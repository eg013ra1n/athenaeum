//! Dev probe for M2 (local normalization, spec §5.2): against a REAL
//! catalog (a frame set that has already been through a `stacking` plan/run
//! at least once, so `calibrated`/`metrics`/registration rows exist to
//! reuse), builds the group's LN reference from the cached registered rows
//! and measurements, normalizes one named (or the first candidate) frame,
//! and prints the per-channel scale/sigma/matches/cells-rejected plus the
//! residual background on the stride mesh before and after normalization —
//! `median |raw − ref|` (before) vs `median |(A·raw+B) − ref|` (after,
//! evaluated through the SAME bicubic B-spline the engine's band loop uses,
//! read back from the real `.athln` sidecar this run writes).
//!
//! No `ServiceContext` — just the catalog's own `Database` (schema init is
//! idempotent, so opening a real, already-populated catalog is safe) and
//! the DB-row helpers `stacking::run`'s own stage 6 reads. Weight comes
//! from the frame SET's latest stacking run's `stacking_run_frames` rows
//! (the same "cached" data every other input here is read from); a frame
//! set with no run yet has no stored weight, so every candidate falls back
//! to `1.0` (equal ranking) with a printed note.
//!
//! cargo run --release -p athenaeum-core --example ln_probe -- \
//!   --db <catalog.db> --set <id> --group <key> [--frames N] [--scale 1024] \
//!   [--frame <calibrated-file-stem>]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use athenaeum_core::db::stacking::{find_artifact, list_frame_rows, list_runs};
use athenaeum_core::db::Database;
use athenaeum_core::geometry::{Linear, PixelMap};
use athenaeum_core::integration::banded::BandPlanes;
use athenaeum_core::integration::io_policy::IoPolicy;
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::integration::registered_source::{RegisteredFrame, RegisteredSource};
use athenaeum_core::integration::source::FrameSource;
use athenaeum_core::integration::storage_class::StorageClass;
use athenaeum_core::registration::db::get_registration_for_frame_set;
use athenaeum_core::resample::Interpolation;
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::groups::{group_frames, GroupFrame, IntegrationGroup};
use athenaeum_core::stacking::integrate::{
    GroupInput, LocalNormalizationConfig, NormalizationConfig, StackFrame,
};
use athenaeum_core::stacking::ln::{
    background_grid, build_reference, normalize_frame, relative_scale, BackgroundParams,
    LnFrameGrids, LnGrid, LnReferenceForDetection, DEFAULT_PARAMS, TARGET_DEVIATION_SIGMA,
};
use athenaeum_core::stacking::measure::FrameMeasurement;
use athenaeum_core::stacking::psf_signal::PsfModel;
use athenaeum_core::stacking::weights::FrameWeight;

fn usage() -> ! {
    eprintln!(
        "usage: ln_probe --db <catalog.db> --set <id> --group <key> \
[--frames N] [--scale 1024] [--frame <calibrated-file-stem>]"
    );
    std::process::exit(2);
}

struct Args {
    db: PathBuf,
    set_id: i64,
    group_key: String,
    frames: u32,
    scale: u32,
    frame: Option<String>,
}

fn parse_args() -> Args {
    let mut db: Option<PathBuf> = None;
    let mut set_id: Option<i64> = None;
    let mut group_key: Option<String> = None;
    let mut frames: u32 = LocalNormalizationConfig::default().reference_frames;
    let mut scale: u32 = LocalNormalizationConfig::default().scale;
    let mut frame: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--db" => db = Some(value.into()),
            "--set" => set_id = Some(value.parse().unwrap_or_else(|_| usage())),
            "--group" => group_key = Some(value),
            "--frames" => frames = value.parse().unwrap_or_else(|_| usage()),
            "--scale" => scale = value.parse().unwrap_or_else(|_| usage()),
            "--frame" => frame = Some(value),
            _ => usage(),
        }
    }

    Args {
        db: db.unwrap_or_else(|| usage()),
        set_id: set_id.unwrap_or_else(|| usage()),
        group_key: group_key.unwrap_or_else(|| usage()),
        frames,
        scale,
        frame,
    }
}

/// One catalog frame with enough cached state to join the LN reference
/// build: its calibrated (working-dir) path, its stored measurement, its
/// registration transform, and (if any past run recorded one) its weight.
struct Candidate {
    frame: GroupFrame,
    calibrated: PathBuf,
    measurement: FrameMeasurement,
    map: PixelMap,
    weight: f64,
}

/// The median of only the finite values of `v` — mirrors
/// `stacking::ln::mod`'s own private `median_of_finite` (not exported past
/// the crate boundary), needed here for the same reason: a warped frame's
/// off-canvas strip is NaN-filled by `RegisteredSource::fill_frame`, and
/// this probe replicates `normalize_frame`'s own NaN handling on the copy
/// it feeds to `relative_scale` directly (it also calls `normalize_frame`
/// itself, which does its own, identical sanitization internally).
fn median_of_finite(v: &[f32]) -> f32 {
    let mut finite: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if finite.is_empty() {
        return f32::NAN;
    }
    finite.sort_by(|a, b| a.total_cmp(b));
    finite[finite.len() / 2]
}

fn sanitize(v: &mut [f32]) {
    let location = median_of_finite(v);
    for x in v.iter_mut() {
        if !x.is_finite() {
            *x = location;
        }
    }
}

/// Warps one channel of `frame` into the group's reference geometry — the
/// same `RegisteredSource` + `BandPlanes` round trip `normalize_frame`
/// itself runs internally (its own warp is not exposed, so a probe that
/// wants the raw warped plane — for its own `relative_scale`/background
/// calls, independent of what `normalize_frame` returns — has to run it
/// too).
fn warp_channel(
    frame: &StackFrame,
    width: usize,
    height: usize,
    channel: usize,
    interpolation: Interpolation,
    clamping: f32,
) -> Vec<f32> {
    let registered = RegisteredFrame {
        path: frame.path.clone(),
        map: frame.map.clone(),
    };
    let src = RegisteredSource::open(
        &[registered],
        width,
        height,
        channel,
        interpolation,
        clamping,
    )
    .unwrap_or_else(|e| {
        eprintln!("warp failed: {e}");
        std::process::exit(1);
    });
    let mut band = BandPlanes::new(&src);
    let no_progress = |_: u64| {};
    src.read_band_with_progress(
        0,
        height,
        &mut band,
        1,
        &no_progress,
        &AtomicBool::new(false),
    )
    .unwrap_or_else(|e| {
        eprintln!("warp failed: {e}");
        std::process::exit(1);
    });
    let mut plane = vec![0f32; width * height];
    band.decode_frame_into(0, &mut plane);
    plane
}

/// Median absolute value of a per-cell residual over a `BackgroundGrid`
/// pair (both `gw × gh`, row-major) — the "before"/"after" summary the
/// brief asks for, computed ON THE MESH (the same node positions the LN
/// grid's own A/B live at), not over raw pixels.
fn median_abs(values: &[f32]) -> f64 {
    let mut v: Vec<f64> = values.iter().map(|&x| x as f64).collect();
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn main() {
    let args = parse_args();

    // `Database::new` creates a fresh (empty, schema-initialised) catalog
    // at a path that doesn't exist yet — exactly wrong for a probe meant to
    // read an EXISTING real catalog, so a typo'd `--db` would otherwise
    // silently produce a throwaway DB and fail later with a confusing
    // "group not found" instead of the real problem.
    if !args.db.exists() {
        eprintln!("{} does not exist", args.db.display());
        std::process::exit(1);
    }
    let db = Database::new(args.db.clone()).unwrap_or_else(|e| {
        eprintln!("opening {}: {e}", args.db.display());
        std::process::exit(1);
    });
    let conn = db.conn();

    let cfg = StackingConfig::default();
    let groups = group_frames(&conn, args.set_id, &cfg.grouping).unwrap_or_else(|e| {
        eprintln!("grouping set {}: {e}", args.set_id);
        std::process::exit(1);
    });
    let group: &IntegrationGroup = groups
        .iter()
        .find(|g| g.key == args.group_key)
        .unwrap_or_else(|| {
            eprintln!(
                "group {:?} not found in set {} (have: {:?})",
                args.group_key,
                args.set_id,
                groups.iter().map(|g| &g.key).collect::<Vec<_>>()
            );
            std::process::exit(1);
        });

    // Weight: the frame SET's latest stacking run's own `stacking_run_frames`
    // rows (cached, like everything else this probe reads) — `None` (no run
    // yet) leaves every candidate at an equal 1.0.
    let weight_by_frame: HashMap<i64, f64> = match list_runs(&conn, args.set_id, 1)
        .ok()
        .and_then(|r| r.into_iter().next())
    {
        Some(run) => list_frame_rows(&conn, run.id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| r.weight.map(|w| (r.frame_id, w)))
            .collect(),
        None => {
            eprintln!("note: no prior stacking run for this set — every candidate weighs 1.0");
            HashMap::new()
        }
    };

    let registration_by_frame = get_registration_for_frame_set(&conn, args.set_id)
        .unwrap_or_else(|e| {
            eprintln!("reading registration rows: {e}");
            std::process::exit(1);
        })
        .into_iter()
        .map(|r| (r.frame_id, r))
        .collect::<HashMap<_, _>>();

    let mut candidates: Vec<Candidate> = Vec::new();
    for gf in &group.frames {
        let calibrated = match find_artifact(
            &conn,
            args.set_id,
            &group.key,
            "calibrated",
            Some(gf.frame_id),
        ) {
            Ok(Some(row)) => match row.path {
                Some(p) => PathBuf::from(p),
                None => continue,
            },
            _ => continue,
        };
        let measurement =
            match find_artifact(&conn, args.set_id, &group.key, "metrics", Some(gf.frame_id)) {
                Ok(Some(row)) => match row
                    .payload_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<FrameMeasurement>(s).ok())
                {
                    Some(m) => m,
                    None => continue,
                },
                _ => continue,
            };
        let Some(reg) = registration_by_frame.get(&gf.frame_id) else {
            continue;
        };
        if reg.status == "failed" {
            continue;
        }
        // Fix round 1, item 6: a `transform_json` that FAILS TO PARSE is a
        // corrupt/unexpected row, not the same thing as "no transform_json
        // at all" — silently falling back to identity for it would feed
        // `build_reference`/`normalize_frame` a WRONG map for a frame that
        // has a real (if unreadable) one, never named or logged. Skip the
        // candidate loudly instead; a row with no `transform_json` at all
        // (a genuinely absent value) keeps the identity fallback.
        let map = match reg.transform_json.as_deref() {
            Some(j) => match PixelMap::from_json(j) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "frame {}: transform_json failed to parse ({e}), skipping",
                        gf.frame_id
                    );
                    continue;
                }
            },
            None => PixelMap::linear(Linear::identity()).unwrap_or_else(|| {
                eprintln!("frame {}: no usable transform, skipping", gf.frame_id);
                std::process::exit(1);
            }),
        };
        let weight = weight_by_frame.get(&gf.frame_id).copied().unwrap_or(1.0);
        candidates.push(Candidate {
            frame: gf.clone(),
            calibrated,
            measurement,
            map,
            weight,
        });
    }

    if candidates.len() < 3 {
        eprintln!(
            "only {} usable candidates in group {:?} (need calibrated + metrics + a non-failed \
registration row for each — run stacking through Register first); found {}",
            candidates.len(),
            args.group_key,
            candidates.len()
        );
        std::process::exit(1);
    }

    let channels = candidates[0].measurement.channels.len();
    candidates.retain(|c| {
        let ok = c.measurement.channels.len() == channels;
        if !ok {
            eprintln!(
                "frame {} has {} channels, group has {channels}; skipping",
                c.frame.frame_id,
                c.measurement.channels.len()
            );
        }
        ok
    });
    if candidates.len() < 3 {
        eprintln!("fewer than 3 candidates share the group's own channel count");
        std::process::exit(1);
    }

    // Best-weighted first, same ranking `stacking::run::run_group_normalization`
    // uses for the real reference member selection.
    candidates.sort_by(|a, b| b.weight.total_cmp(&a.weight));

    let stack_frames: Vec<StackFrame> = candidates
        .iter()
        .map(|c| StackFrame {
            path: c.calibrated.clone(),
            map: c.map.clone(),
            measurement: c.measurement.clone(),
            weight: FrameWeight {
                channels: vec![c.weight; channels],
                normalized: vec![c.weight; channels],
                mean: c.weight,
                normalized_mean: c.weight,
                missing: None,
            },
            exposure_s: c.frame.exposure_s.unwrap_or(0.0),
            date_obs: c.frame.date_obs.clone(),
        })
        .collect();

    // Owner decision 2026-09-10: `IntegrationGroup` no longer carries a
    // group-level width/height (a group's members can now differ in native
    // geometry). This probe's own "reference" is `stack_frames[0]` (the
    // best-weighted candidate, sorted above) — the same frame the real
    // pipeline would pick a reference geometry from — so read ITS
    // calibrated file's own dimensions, mirroring how `stacking::run`
    // resolves `reference_width`/`reference_height` from the real
    // reference frame.
    let reference_plane = PlaneReader::open(&candidates[0].calibrated).unwrap_or_else(|e| {
        eprintln!(
            "opening reference candidate {}: {e}",
            candidates[0].calibrated.display()
        );
        std::process::exit(1);
    });
    let width = reference_plane.width();
    let height = reference_plane.height();
    let interpolation = cfg.registration.interpolation;
    let clamping = cfg.registration.clamping_threshold;

    let integration_cfg = cfg.integration.clone();
    let normalization_cfg = NormalizationConfig::default();
    // Moved up from just before its own first use (final fix wave, I2):
    // `LnReferenceForDetection::build` now needs `local_cfg.psf_model`/
    // `measure_opts.max_stars` too, since it hoists the reference-side
    // detection + fit that used to happen inside `relative_scale` itself.
    let local_cfg = LocalNormalizationConfig {
        enabled: true,
        scale: args.scale,
        reference_frames: args.frames,
        psf_model: PsfModel::Auto,
        local_scale: false,
    };
    let measure_opts = cfg
        .measurement
        .measure_options(cfg.normalization.scale_estimator);
    let group_input = GroupInput {
        frames: &stack_frames,
        reference: 0, // best-weighted candidate, index 0 after the sort above
        width,
        height,
        channels,
        interpolation,
        clamping,
        integration: &integration_cfg,
        normalization: &normalization_cfg,
        ln: None,
        rej: None,
        second_pass_bitmaps: false,
    };

    let n = (args.frames as usize).min(stack_frames.len());
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .expect("build a thread pool");
    let io = IoPolicy {
        band_budget_bytes: 1 << 30,
        read_concurrency: 4,
        storage: StorageClass::Local,
    };
    let cancel = AtomicBool::new(false);
    let on_progress = |plane: usize, total: usize| eprintln!("reference: plane {plane}/{total}");

    let included: Vec<usize> = (0..stack_frames.len()).collect();
    let reference = build_reference(&group_input, &included, n, &pool, &cancel, io, &on_progress)
        .unwrap_or_else(|e| {
            eprintln!("building the LN reference: {e}");
            std::process::exit(1);
        });
    eprintln!(
        "reference: {} of {} candidates, {}x{}, {channels} channel(s)",
        reference.frames_used.len(),
        stack_frames.len(),
        reference.width,
        reference.height
    );

    let ref_for_detection =
        LnReferenceForDetection::build(&reference, local_cfg.psf_model, measure_opts.max_stars);
    let ref_params = BackgroundParams {
        scale: args.scale,
        ..DEFAULT_PARAMS
    };
    let ref_backgrounds: Vec<_> = reference
        .planes
        .iter()
        .map(|p| background_grid(p, reference.width, reference.height, &ref_params))
        .collect();

    // The target frame: the named stem, or the first candidate (index 0 of
    // `stack_frames`, matching this probe's own ordering — the brief's
    // "first included frame"). Matches either the CALIBRATED file's own
    // stem (`c_<stem>`) or the original frame's (the calibrated-lights
    // export convention strips a `c_` prefix — see CLAUDE.md's Calibrated-
    // Lights Export section), so `--frame f0` and `--frame c_f0` both work.
    let target_index = match &args.frame {
        Some(want) => candidates
            .iter()
            .position(|c| {
                c.calibrated
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s == want || s.trim_start_matches("c_") == want.as_str())
                    .unwrap_or(false)
            })
            .unwrap_or_else(|| {
                eprintln!("no candidate calibrated file with stem {want:?}");
                std::process::exit(1);
            }),
        None => 0,
    };
    let target = &stack_frames[target_index];
    eprintln!(
        "target: frame {} ({})",
        candidates[target_index].frame.frame_id,
        target.path.display()
    );

    let sidecar_dir = tempfile::tempdir().expect("create a tempdir for the sidecar");
    let sidecar_path = sidecar_dir.path().join("probe.athln");

    let outcome = normalize_frame(
        &reference,
        &ref_for_detection,
        &ref_backgrounds,
        target,
        &local_cfg,
        &measure_opts,
        interpolation,
        clamping,
        &sidecar_path,
        &cancel,
    )
    .unwrap_or_else(|e| {
        eprintln!("normalize_frame failed: {e}");
        std::process::exit(1);
    });

    let grids = LnFrameGrids::read(&sidecar_path).unwrap_or_else(|e| {
        eprintln!("reading back the sidecar this run just wrote: {e:#}");
        std::process::exit(1);
    });

    let mut per_channel = Vec::with_capacity(channels);
    for p in 0..channels {
        let target_plane = warp_channel(
            target,
            reference.width,
            reference.height,
            p,
            interpolation,
            clamping,
        );
        let mut sanitized_target = target_plane.clone();
        sanitize(&mut sanitized_target);

        let scale_result = relative_scale(
            &ref_for_detection.sanitized_planes[p],
            &sanitized_target,
            reference.width,
            reference.height,
            local_cfg.psf_model,
            measure_opts.max_stars,
            4.0,
            0.3,
            local_cfg.local_scale,
        );
        if let Err(e) = &scale_result {
            // Should not happen — `normalize_frame` above already ran the
            // identical fit successfully for this same frame/channel — but
            // report it rather than silently printing nulls if it does.
            eprintln!("channel {p}: relative_scale: {e}");
        }

        let target_params = BackgroundParams {
            scale: args.scale,
            deviation_sigma: TARGET_DEVIATION_SIGMA,
            ..DEFAULT_PARAMS
        };
        let target_bg = background_grid(
            &target_plane,
            reference.width,
            reference.height,
            &target_params,
        );
        let ref_bg = &ref_backgrounds[p];

        // Before: raw per-cell background mismatch. After: the SAME cells,
        // corrected through the fitted grid the engine's own band loop
        // would apply — evaluated via `evaluate_row` (the bicubic B-spline
        // reconstruction of the coarse `A`/`B` grid), at the mesh's own
        // node positions (`(i·stride, j·stride)`, clamped into the plane —
        // `background_grid`'s own convention, see its module doc), not the
        // grid's raw stored node values (which would trivially reproduce
        // `B_ref` by construction and prove nothing).
        let grid: &LnGrid = &grids.channels[p];
        // Fix round 1, item 6: the grid's own stride, not a re-derivation
        // of the `(scale / 8).max(2)` formula from `args.scale` — the two
        // agree today (this probe builds `local_cfg.scale = args.scale`),
        // but reading it off the grid itself can never drift from what
        // `evaluate_row` actually uses internally.
        let stride = grid.stride();
        let mut before = Vec::with_capacity(target_bg.gw * target_bg.gh);
        let mut after = Vec::with_capacity(target_bg.gw * target_bg.gh);
        let mut a_row = vec![0f32; reference.width];
        let mut b_row = vec![0f32; reference.width];
        // The trailing node on each axis overshoots the plane by design,
        // so its address is clamped to the last pixel — the same rule
        // `ln::background::background_grid` windows that node's cell with
        // and `ln::a_grid` samples the local-scale spline at (review m7),
        // so all three read the mesh at one and the same pixel.
        for j in 0..target_bg.gh {
            let node_y = (j * stride).min(reference.height.saturating_sub(1));
            grid.evaluate_row(node_y, &mut a_row, &mut b_row);
            for i in 0..target_bg.gw {
                let node_x = (i * stride).min(reference.width.saturating_sub(1));
                let idx = j * target_bg.gw + i;
                let raw = target_bg.cells[idx];
                let refv = ref_bg.cells[idx];
                before.push((raw - refv).abs());
                let corrected = LnGrid::apply(a_row[node_x], b_row[node_x], raw);
                after.push((corrected - refv).abs());
            }
        }

        // `ScaleResult` is no longer `Copy` (M4c: it carries the optional
        // local-scale spline), so the fields are read through a borrow.
        let scale_ok = scale_result.ok();
        let scale_ok = scale_ok.as_ref();
        per_channel.push(serde_json::json!({
            "channel": p,
            "scale": scale_ok.map(|r| r.scale),
            "sigma": scale_ok.map(|r| r.sigma),
            "matches": scale_ok.map(|r| r.matches),
            "rcrRejected": scale_ok.map(|r| r.rejected),
            "beta": scale_ok.map(|r| r.beta),
            "pass": scale_ok.map(|r| r.pass),
            "localNodes": scale_ok.map(|r| r.local.as_ref().map_or(0, |s| s.nodes.len())),
            "cellsRejected": target_bg.invalid_cells,
            "residualBefore": median_abs(&before),
            "residualAfter": median_abs(&after),
        }));
    }

    let out = serde_json::json!({
        "set": args.set_id,
        "group": args.group_key,
        "candidates": stack_frames.len(),
        "referenceFramesUsed": reference.frames_used.len(),
        "target": {
            "frameId": candidates[target_index].frame.frame_id,
            "path": target.path.display().to_string(),
        },
        "sidecar": {
            "scale": outcome.scale,
            "matches": outcome.matches,
            "cellsRejected": outcome.cells_rejected,
            "path": outcome.sidecar.display().to_string(),
        },
        "perChannel": per_channel,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
