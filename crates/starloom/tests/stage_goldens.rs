//! Stage goldens (spec SL-7): every stage runs on a fixed synthetic input on a 2-thread pool, and its output checksum
//! is tied to the version that owns it. A change that moves a stage's bytes while its version stays is red here, and so
//! is a version bump with no new row, so a numeric change can never reach the caches unversioned.
//!
//! # Recording a row
//!
//! Nothing writes the table. Run
//! `cargo test -p starloom --test stage_goldens -- --nocapture`, copy the literal the failure message (or, on a
//! platform with no rows yet, the `eprintln!`) prints into [`GOLDENS`] by hand, and do it in the same change as the
//! version bump. A change that moves no number records its row with the reason in its commit message.
//!
//! # What a row is valid for
//!
//! A row is valid for the rustafits revision `Cargo.lock` locks (Athenaeum's workspace lock inside Athenaeum; SL-18): the measure, pass-1, register, normalization
//! and integrate stages run rustafits' star fit. A red row right after a rustafits bump means rustafits moved numbers:
//! with its `MEASURE_VERSION` raised the rows ask for a new record; without it, stop the bump (the rustafits rule).
//!
//! # Platforms
//!
//! Every float stage is recorded per platform (`<arch>-<os>`): `exp` and `powf` round one ulp differently from one
//! libm to the next. A platform with no rows of its own checks determinism only (two runs agree) and prints the rows
//! it would record. The one float-free stage, `ln_grid_format`, has the platform `"*"` and is enforced everywhere.
//! A red float row when neither starloom nor rustafits changed points at the system libm first: compare the macOS
//! version recorded beside the rows.
#![cfg(feature = "stack")]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use starloom::calibration_library::cosmetic::{
    apply_hot_pixel_correction, hot_pixel_map_from_dark, HotPixelMapOutcome,
};
use starloom::calibration_library::light_cal::{
    calibrate_light_compute_with_cards, LightCalInputs, NoFlatCards, OUTPUT_SCALE_DIVISOR,
};
use starloom::calibration_library::params::{FlatNormMode, LightCalParams};
use starloom::calibration_library::LIGHT_CAL_VERSION;
use starloom::fits_writer::write_fits_f32;
use starloom::geometry::ransac::SplitMix64;
use starloom::geometry::{Linear, LinearKind, PixelMap};
use starloom::integration::combine::{IntegrationRecipe, Rejection};
use starloom::integration::engine::{
    integrate_bias_like, integrate_flat, EngineProgress, FlatPrecal, ENGINE_VERSION,
};
use starloom::integration::io_policy::IoPolicy;
use starloom::integration::stats::NormalizationPair;
use starloom::resample::{warp_rows, Plane};
use starloom::stacking::config::{DrizzleKernel, DRIZZLE_KERNEL_VERSION};
use starloom::stacking::drizzle::{drizzle_group, DrizzleFrame, DrizzleInput, DrizzleProgress};
use starloom::stacking::integrate::{
    integrate_group, GroupInput, GroupProgress, IntegrationConfig, NormalizationConfig, StackFrame,
    INTEGRATE_VERSION, SNR_GAIN_VERSION,
};
use starloom::stacking::ln::background::{BackgroundModel, TileTable, CLEANING};
use starloom::stacking::ln::grid::{LnFrameGrids, LnGrid, LN_GRID_VERSION};
use starloom::stacking::ln::reference::LnReference;
use starloom::stacking::ln::{normalize_frame, LnReferenceForDetection, LnScaleSeeds, LN_MODEL_VERSION};
use starloom::stacking::measure::{
    measure_frame_full, measure_frame_pass1_gain, MeasureOptions, MeasureTimings, SeedOverride,
    MEASURE_STAGE_VERSION,
};
use starloom::stacking::pass1_artifact::{write_pass1, PASS1_ARTIFACT_VERSION};
use starloom::stacking::psf_signal::PsfModel;
use starloom::stacking::register::align::{SeedPolicy, ALIGN_VERSION};
use starloom::stacking::register::frame::{reference_stars, register_frame};
use starloom::stacking::register::{scale_gate_for, RegistrationConfig};
use starloom::stacking::weights::FrameWeight;
use starloom::star_measure::MEASURE_VERSION;
use starloom::storage_class::StorageClass;
use starloom::test_support::{add_noise, synthetic_star_field};

/// One recorded row: a stage's output checksum on a platform, at the versions it was recorded at.
struct Golden {
    stage: &'static str,
    platform: &'static str,
    versions: &'static [(&'static str, u32)],
    checksum: u64,
}

/// One computed stage. `versions` names the owning version first, then the upstream ones the stage's Athenaeum hash
/// folds. `portable` marks a float-free stage (its row's platform is `"*"`).
struct Computed {
    stage: &'static str,
    versions: &'static [&'static str],
    portable: bool,
    checksum: u64,
}

/// The recorded rows, upstream first. A row is recorded by hand (see the module doc).
const GOLDENS: &[Golden] = &[
    // aarch64-macos, macOS 27.2.1 (sw_vers -productVersion), rustafits 1.9.1 (0edd7d1, the revision `Cargo.lock` locks); the rows were recorded at 1.9.0 (a8e6b79) and hold unchanged.
    Golden { stage: "measure", platform: "aarch64-macos", versions: &[("MEASURE_STAGE_VERSION", 1), ("MEASURE_VERSION", 8)], checksum: 0x15dfd60956642ce3 },
    Golden { stage: "pass1_artifact", platform: "aarch64-macos", versions: &[("MEASURE_STAGE_VERSION", 1), ("PASS1_ARTIFACT_VERSION", 1), ("MEASURE_VERSION", 8)], checksum: 0x312dae5cab562ff8 },
    Golden { stage: "register", platform: "aarch64-macos", versions: &[("ALIGN_VERSION", 1), ("MEASURE_STAGE_VERSION", 1), ("MEASURE_VERSION", 8)], checksum: 0xb6d64ba321698e0a },
    Golden { stage: "warp", platform: "aarch64-macos", versions: &[("ALIGN_VERSION", 1)], checksum: 0x57b84de494726d1c },
    Golden { stage: "ln_model", platform: "aarch64-macos", versions: &[("LN_MODEL_VERSION", 4), ("MEASURE_STAGE_VERSION", 1), ("MEASURE_VERSION", 8)], checksum: 0xe7cffc278266b609 },
    // Float-free, enforced on every platform.
    Golden { stage: "ln_grid_format", platform: "*", versions: &[("LN_GRID_VERSION", 1)], checksum: 0x0c6a45d93ef3d17b },
    Golden { stage: "integrate", platform: "aarch64-macos", versions: &[("INTEGRATE_VERSION", 1), ("ENGINE_VERSION", 1), ("MEASURE_STAGE_VERSION", 1), ("MEASURE_VERSION", 8)], checksum: 0xbeeaee875bbeb3be },
    Golden { stage: "snr_gain", platform: "aarch64-macos", versions: &[("SNR_GAIN_VERSION", 2), ("INTEGRATE_VERSION", 1), ("ENGINE_VERSION", 1), ("MEASURE_STAGE_VERSION", 1), ("MEASURE_VERSION", 8)], checksum: 0xcad258e262200dce },
    Golden { stage: "drizzle", platform: "aarch64-macos", versions: &[("DRIZZLE_KERNEL_VERSION", 2)], checksum: 0xbedfc3aff9989e37 },
    Golden { stage: "master_bias", platform: "aarch64-macos", versions: &[("ENGINE_VERSION", 1)], checksum: 0x9bda271ac4ddb582 },
    Golden { stage: "master_flat", platform: "aarch64-macos", versions: &[("ENGINE_VERSION", 1)], checksum: 0x606511c5b2815ad7 },
    Golden { stage: "light_cal", platform: "aarch64-macos", versions: &[("LIGHT_CAL_VERSION", 1)], checksum: 0x1c7a4c66d0374d8c },
    Golden { stage: "cosmetic", platform: "aarch64-macos", versions: &[("LIGHT_CAL_VERSION", 1)], checksum: 0x7f7b15225c17ae02 },
];

/// The live value of a named version constant.
fn live_version(name: &str) -> u32 {
    match name {
        "MEASURE_STAGE_VERSION" => MEASURE_STAGE_VERSION,
        "MEASURE_VERSION" => MEASURE_VERSION,
        "PASS1_ARTIFACT_VERSION" => u32::from(PASS1_ARTIFACT_VERSION),
        "ALIGN_VERSION" => ALIGN_VERSION,
        "LN_MODEL_VERSION" => LN_MODEL_VERSION,
        "LN_GRID_VERSION" => u32::from(LN_GRID_VERSION),
        "INTEGRATE_VERSION" => INTEGRATE_VERSION,
        "ENGINE_VERSION" => ENGINE_VERSION,
        "SNR_GAIN_VERSION" => SNR_GAIN_VERSION,
        "DRIZZLE_KERNEL_VERSION" => DRIZZLE_KERNEL_VERSION,
        "LIGHT_CAL_VERSION" => LIGHT_CAL_VERSION,
        other => panic!("stage_goldens: unknown version name {other}"),
    }
}

/// xxh3-64 over the parts, each prefixed by its length as `u64` little-endian.
fn digest(parts: &[&[u8]]) -> u64 {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    h.digest()
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn f64_bytes(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_bits().to_le_bytes()).collect()
}

/// An optional plane: a presence byte, then the samples.
fn opt_f32_bytes(v: &Option<Vec<f32>>) -> Vec<u8> {
    match v {
        Some(v) => [&[1u8][..], &f32_bytes(v)].concat(),
        None => vec![0u8],
    }
}

/// The row literal for `c` on `platform`, as it is pasted into [`GOLDENS`].
fn literal(platform: &str, c: &Computed, live: &dyn Fn(&str) -> u32) -> String {
    let versions: Vec<String> = c
        .versions
        .iter()
        .map(|n| format!("(\"{n}\", {})", live(n)))
        .collect();
    format!(
        "Golden {{ stage: \"{}\", platform: \"{platform}\", versions: &[{}], checksum: {:#018x} }},",
        c.stage,
        versions.join(", "),
        c.checksum
    )
}

/// A table row as it is written in [`GOLDENS`] (the literal a failure message about the row carries).
fn row_literal(g: &Golden) -> String {
    let versions: Vec<String> = g.versions.iter().map(|(n, v)| format!("(\"{n}\", {v})")).collect();
    format!(
        "Golden {{ stage: \"{}\", platform: \"{}\", versions: &[{}], checksum: {:#018x} }},",
        g.stage,
        g.platform,
        versions.join(", "),
        g.checksum
    )
}

/// Compares the computed stages with the table on `platform`. A float stage on a platform with no rows checks
/// determinism only (the caller did that) and passes; a `portable` stage is checked against its `"*"` row everywhere.
/// A row of the table that reads as coverage but covers nothing fails too: a row whose stage is no longer computed
/// (a renamed or dropped stage) and a second row for the same (stage, platform). That holds for the rows of `platform`
/// when it has rows and for the `"*"` rows on every platform.
fn check(
    table: &[Golden],
    platform: &str,
    computed: &[Computed],
    live: &dyn Fn(&str) -> u32,
) -> Result<(), String> {
    let has_rows = table.iter().any(|g| g.platform == platform);
    let mut failures = Vec::new();
    for c in computed {
        if !c.portable && !has_rows {
            continue;
        }
        let row_platform = if c.portable { "*" } else { platform };
        let lit = literal(row_platform, c, live);
        let Some(row) = table
            .iter()
            .find(|g| g.stage == c.stage && g.platform == row_platform)
        else {
            failures.push(format!(
                "{} ({row_platform}): no golden row; record: {lit}",
                c.stage
            ));
            continue;
        };
        let moved = c.versions.iter().find_map(|name| {
            let now = live(name);
            let recorded = row.versions.iter().find(|(n, _)| n == name).map(|&(_, v)| v);
            (recorded != Some(now)).then_some((*name, now, recorded))
        });
        if let Some((name, now, recorded)) = moved {
            let recorded = recorded.map_or("none".to_string(), |v| v.to_string());
            failures.push(format!(
                "{} ({row_platform}): {name} is {now}, the row was recorded at {recorded}: record the new row in the \
                 same change. New row: {lit}",
                c.stage
            ));
        } else if row.checksum != c.checksum {
            let own = c.versions[0];
            failures.push(format!(
                "{} ({row_platform}): the output changed while {own} stayed {}: bump {own} in the same change (SL-7) \
                 and record the new row; a change that moves no number records the row with the reason in its \
                 commit. New row: {lit}",
                c.stage,
                live(own)
            ));
        }
    }
    for (at, row) in table.iter().enumerate() {
        let star = row.platform == "*";
        if !star && !(has_rows && row.platform == platform) {
            continue;
        }
        let key = (row.stage, row.platform);
        if table[..at].iter().any(|g| (g.stage, g.platform) == key) {
            failures.push(format!(
                "{} ({}): a second row for the same stage and platform; the first one is the one checked: remove \
                 one. Row: {}",
                row.stage,
                row.platform,
                row_literal(row)
            ));
        }
        if !computed.iter().any(|c| c.stage == row.stage && c.portable == star) {
            failures.push(format!(
                "{} ({}): the table has a row for a stage that is no longer computed: remove it or restore the \
                 stage. Row: {}",
                row.stage,
                row.platform,
                row_literal(row)
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Fixtures. Every input is built from public items; a fixture never changes once recorded.
// ---------------------------------------------------------------------------------------------------------------

fn io() -> IoPolicy {
    IoPolicy {
        band_budget_bytes: 64 << 20,
        read_concurrency: 2,
        storage: StorageClass::Local,
        prefetch_local: false,
    }
}

fn write_mono(dir: &Path, name: &str, w: usize, h: usize, data: &[f32]) -> PathBuf {
    let p = dir.join(name);
    write_fits_f32(&p, w, h, 1, data, &[]).unwrap();
    p
}

fn shift_map(dx: f64, dy: f64) -> PixelMap {
    let fwd = Linear {
        kind: LinearKind::Affine,
        m: [[1.0, 0.0, -dx], [0.0, 1.0, -dy], [0.0, 0.0, 1.0]],
    };
    PixelMap::linear(fwd).unwrap()
}

/// `n` stars on a `size`-square plane: `x = lo + span·u`, `y = lo + span·u`, `amp = 0.1 + 0.4·u`, drawn in that order
/// from `SplitMix64(seed)`.
fn random_stars(seed: u64, n: usize, lo: f64, span: f64) -> Vec<(f64, f64, f64)> {
    let mut rng = SplitMix64(seed);
    (0..n)
        .map(|_| {
            let x = lo + span * rng.next_f64();
            let y = lo + span * rng.next_f64();
            let amp = 0.1 + 0.4 * rng.next_f64();
            (x, y, amp)
        })
        .collect()
}

fn no_progress() -> (impl Fn(usize, usize) + Sync, impl Fn(usize, usize, u64, u64) + Sync) {
    (|_: usize, _: usize| {}, |_: usize, _: usize, _: u64, _: u64| {})
}

fn weight(w: f64) -> FrameWeight {
    FrameWeight {
        channels: vec![w],
        normalized: vec![w],
        mean: w,
        normalized_mean: w,
        missing: None,
    }
}

/// Stage rows in upstream-first order. Each stage runs inside `pool.install`.
fn compute(pool: &Arc<rayon::ThreadPool>) -> Vec<Computed> {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let cancel = AtomicBool::new(false);
    let mut rows = Vec::new();

    // measure + pass1_artifact: one 768x512 one-plane frame.
    let (field, w, h) = starloom::test_support::fixture_field(7, 1.0, 0.002);
    let measured = write_mono(d, "measure.fits", w, h, &field);
    let opts = MeasureOptions::default();
    let (mut m, _) = pool
        .install(|| {
            measure_frame_full(&measured, &opts, Some(pool), &cancel, SeedOverride::Fast, Some(1.0))
        })
        .unwrap();
    m.duration_ms = 0;
    for c in &mut m.channels {
        c.timings = MeasureTimings::default();
    }
    rows.push(Computed {
        stage: "measure",
        versions: &["MEASURE_STAGE_VERSION", "MEASURE_VERSION"],
        portable: false,
        checksum: digest(&[&serde_json::to_vec(&m).unwrap()]),
    });

    let mut pass1 = pool
        .install(|| measure_frame_pass1_gain(&measured, &opts, Some(pool), &cancel, Some(1.0)))
        .unwrap();
    pass1.fix_stamp_and_zero_timings();
    let athm = d.join("pass1.athm");
    write_pass1(&athm, &pass1).unwrap();
    rows.push(Computed {
        stage: "pass1_artifact",
        versions: &["MEASURE_STAGE_VERSION", "PASS1_ARTIFACT_VERSION", "MEASURE_VERSION"],
        portable: false,
        checksum: digest(&[&std::fs::read(&athm).unwrap()]),
    });

    // register: 40 stars, the subject shifted by (3.25, -2.5).
    let stars = random_stars(11, 40, 16.0, 224.0);
    let shifted: Vec<(f64, f64, f64)> = stars.iter().map(|&(x, y, a)| (x + 3.25, y - 2.5, a)).collect();
    let reference_plane = synthetic_star_field(256, 256, &stars, 3.0, 0.002, 11);
    let subject_plane = synthetic_star_field(256, 256, &shifted, 3.0, 0.002, 12);
    let ref_path = write_mono(d, "reference.fits", 256, 256, &reference_plane);
    let sub_path = write_mono(d, "subject.fits", 256, 256, &subject_plane);
    let cfg = RegistrationConfig::default();
    let reg = pool.install(|| {
        let reference = reference_stars(&ref_path, &cfg, Some(pool), None).unwrap();
        register_frame(
            &reference,
            &sub_path,
            &cfg,
            Some(pool),
            &cancel,
            None,
            SeedPolicy::QuadFirst,
            scale_gate_for(None, None),
        )
        .unwrap()
    });
    let alignment = reg.outcome.expect("the register fixture aligns");
    rows.push(Computed {
        stage: "register",
        versions: &["ALIGN_VERSION", "MEASURE_STAGE_VERSION", "MEASURE_VERSION"],
        portable: false,
        checksum: digest(&[
            alignment.map.to_json().as_bytes(),
            &(alignment.inliers as u64).to_le_bytes(),
            &f64_bytes(&[alignment.rms_px]),
        ]),
    });

    // warp: the register reference through a similarity (0.3 deg, shift (0.37, -0.61)).
    let (s, c) = 0.3f64.to_radians().sin_cos();
    let similarity = Linear {
        kind: LinearKind::Similarity,
        m: [[c, -s, 0.37], [s, c, -0.61], [0.0, 0.0, 1.0]],
    };
    let map = PixelMap::linear(similarity).unwrap();
    let mut warped = vec![0f32; 256 * 256];
    pool.install(|| {
        warp_rows(
            &Plane::full(&reference_plane, 256, 256),
            &map,
            256,
            0,
            256,
            cfg.interpolation,
            cfg.clamping_threshold,
            &mut warped,
        )
    });
    rows.push(Computed {
        stage: "warp",
        versions: &["ALIGN_VERSION"],
        portable: false,
        checksum: digest(&[&f32_bytes(&warped)]),
    });

    // ln_model: a 256x256 reference and a target with the same stars, x1.1 + 0.01.
    let ln_stars: Vec<(f64, f64, f64)> = (0..8)
        .flat_map(|row| {
            (0..8).map(move |col| {
                let x = 18.0 + col as f64 * 31.0 + ((row * 7 + col) % 5) as f64;
                let y = 18.0 + row as f64 * 31.0 + ((row * 3 + col) % 4) as f64;
                (x, y, 0.10 + ((row * 8 + col) % 9) as f64 * 0.02)
            })
        })
        .collect();
    let ln_ref_plane = synthetic_star_field(256, 256, &ln_stars, 4.2, 0.002, 11);
    let ln_target: Vec<f32> = ln_ref_plane.iter().map(|&v| v * 1.1 + 0.01).collect();
    let ln_path = write_mono(d, "ln_member.fits", 256, 256, &ln_target);
    let ln_reference = LnReference {
        width: 256,
        height: 256,
        planes: vec![ln_ref_plane],
        frames_used: Vec::new(),
    };
    let ln_cfg = starloom::stacking::integrate::LocalNormalizationConfig {
        enabled: true,
        scale: 128,
        ..Default::default()
    };
    let ln_measure = MeasureOptions {
        psf_model: PsfModel::Moffat4,
        max_stars: 200,
        ..Default::default()
    };
    let ln_sidecar = d.join("ln_member.athln");
    pool.install(|| {
        let for_detection = LnReferenceForDetection::build(&ln_reference, PsfModel::Moffat4, 200, Some(pool), None);
        let ref_models: Vec<BackgroundModel> = ln_reference
            .planes
            .iter()
            .map(|p| {
                BackgroundModel::fit(
                    TileTable::measure(p, ln_reference.width, ln_reference.height, &CLEANING, Some(pool)),
                    ln_cfg.scale,
                )
                .expect("the fixture reference has a background")
            })
            .collect();
        let frame = StackFrame {
            path: ln_path.clone(),
            map: PixelMap::linear(Linear::identity()).unwrap(),
            measurement: starloom::stacking::measure::FrameMeasurement {
                width: 256,
                height: 256,
                channels: vec![Default::default()],
                duration_ms: 0,
            },
            weight: weight(1.0),
            exposure_s: 180.0,
            date_obs: None,
            frame_id: 1,
            registered_path: None,
        };
        normalize_frame(
            &ln_reference,
            &for_detection,
            &ref_models,
            &frame,
            LnScaleSeeds::ForcedDetection,
            &ln_cfg,
            ln_cfg.scale,
            &ln_measure,
            cfg.interpolation,
            cfg.clamping_threshold,
            &ln_sidecar,
            None,
            Some(pool),
            &cancel,
        )
        .expect("the LN fixture member normalizes");
    });
    rows.push(Computed {
        stage: "ln_model",
        versions: &["LN_MODEL_VERSION", "MEASURE_STAGE_VERSION", "MEASURE_VERSION"],
        portable: false,
        checksum: digest(&[&std::fs::read(&ln_sidecar).unwrap()]),
    });

    // ln_grid_format: one grid of literal values (gw = gh = 3), no float arithmetic.
    let grid = LnGrid {
        ref_width: 33,
        ref_height: 33,
        scale: 128,
        gw: 3,
        gh: 3,
        a: (0..9).map(|i| 1.0 + 0.0625 * i as f32).collect(),
        b: (0..9).map(|i| -0.25 + 0.03125 * i as f32).collect(),
        global_scale: 1.0625,
        location_ref: 0.25,
        location_tgt: 0.1875,
    };
    let grid_path = d.join("literal.athln");
    pool.install(|| LnFrameGrids { channels: vec![grid] }.write(&grid_path).unwrap());
    rows.push(Computed {
        stage: "ln_grid_format",
        versions: &["LN_GRID_VERSION"],
        portable: true,
        checksum: digest(&[&std::fs::read(&grid_path).unwrap()]),
    });

    // integrate + snr_gain: 5 frames 128x128, identity maps, one with a planted hot column.
    let int_stars = random_stars(21, 24, 8.0, 112.0);
    let int_paths: Vec<PathBuf> = (0..5u64)
        .map(|i| {
            let mut plane = synthetic_star_field(128, 128, &int_stars, 3.0, 0.003, 100 + i);
            if i == 2 {
                for y in 0..128 {
                    plane[y * 128 + 40] += 0.5;
                }
            }
            write_mono(d, &format!("int{i}.fits"), 128, 128, &plane)
        })
        .collect();
    let int_frames: Vec<StackFrame> = int_paths
        .iter()
        .map(|p| StackFrame {
            path: p.clone(),
            map: PixelMap::linear(Linear::identity()).unwrap(),
            measurement: pool
                .install(|| {
                    measure_frame_full(p, &opts, Some(pool), &cancel, SeedOverride::Fast, None)
                })
                .unwrap()
                .0,
            weight: weight(1.0),
            exposure_s: 60.0,
            date_obs: None,
            frame_id: 1,
            registered_path: None,
        })
        .collect();
    let integration = IntegrationConfig::default();
    let normalization = NormalizationConfig::default();
    let input = GroupInput {
        frames: &int_frames,
        reference: 0,
        width: 128,
        height: 128,
        channels: 1,
        interpolation: RegistrationConfig::default().interpolation,
        clamping: 0.3,
        integration: &integration,
        normalization: &normalization,
        ln: None,
        rej: None,
        second_pass_bitmaps: false,
        coverage_map: false,
        stats_on_covered: false,
        common_stars: None,
    };
    let (on_plane, on_band) = no_progress();
    let progress = GroupProgress {
        on_plane: &on_plane,
        engine: EngineProgress {
            on_band: &on_band,
            on_combine: &on_band,
        },
    };
    let out = pool
        .install(|| integrate_group(&input, &opts, pool, &cancel, &progress, io()))
        .unwrap();
    rows.push(Computed {
        stage: "integrate",
        versions: &["INTEGRATE_VERSION", "ENGINE_VERSION", "MEASURE_STAGE_VERSION", "MEASURE_VERSION"],
        portable: false,
        checksum: digest(&[
            &f32_bytes(&out.data),
            &opt_f32_bytes(&out.rejection_low),
            &opt_f32_bytes(&out.rejection_high),
        ]),
    });
    rows.push(Computed {
        stage: "snr_gain",
        versions: &[
            "SNR_GAIN_VERSION",
            "INTEGRATE_VERSION",
            "ENGINE_VERSION",
            "MEASURE_STAGE_VERSION",
            "MEASURE_VERSION",
        ],
        portable: false,
        checksum: digest(&[
            &f64_bytes(&out.stats.snr_gain),
            &f64_bytes(&out.stats.master_psf_snr),
            &f64_bytes(&out.stats.best_sub_psf_snr),
        ]),
    });

    // drizzle: 3 frames 96x96, scale 2, the default kernel, weights on, rejection and LN off.
    let dz_stars = random_stars(31, 20, 8.0, 80.0);
    let shifts = [(0.0, 0.0), (0.37, -0.21), (-0.43, 0.29)];
    let dz_paths: Vec<PathBuf> = shifts
        .iter()
        .enumerate()
        .map(|(i, &(dx, dy))| {
            let moved: Vec<(f64, f64, f64)> = dz_stars.iter().map(|&(x, y, a)| (x + dx, y + dy, a)).collect();
            let plane = synthetic_star_field(96, 96, &moved, 3.0, 0.002, 200 + i as u64);
            write_mono(d, &format!("dz{i}.fits"), 96, 96, &plane)
        })
        .collect();
    let dz_maps: Vec<PixelMap> = shifts.iter().map(|&(dx, dy)| shift_map(dx, dy)).collect();
    let dz_weight = [1.0f64];
    let dz_pair = [NormalizationPair::IDENTITY];
    let dz_frames: Vec<DrizzleFrame<'_>> = dz_paths
        .iter()
        .zip(&dz_maps)
        .map(|(p, m)| DrizzleFrame {
            path: p,
            map: m,
            weight: &dz_weight,
            output_pair: &dz_pair,
            ln: None,
            rej: None,
            cfa: None,
        })
        .collect();
    let dz_input = DrizzleInput {
        frames: &dz_frames,
        width: 96,
        height: 96,
        channels: 1,
        scale: 2,
        drop_shrink: 0.9,
        kernel: DrizzleKernel::default(),
        use_weights: true,
        use_rejection: false,
        use_local_normalization: false,
        write_weight_map: true,
        write_coverage: false,
        stats_on_covered: false,
        stats_rect: None,
        measure: &opts,
        ram_total_bytes: Some(8 << 30),
        force_exact_overlap_for_measurement: false,
    };
    let (dz_on_frame, _) = no_progress();
    let dz = pool
        .install(|| {
            drizzle_group(&dz_input, pool, &cancel, &DrizzleProgress { on_frame: &dz_on_frame })
        })
        .unwrap();
    rows.push(Computed {
        stage: "drizzle",
        versions: &["DRIZZLE_KERNEL_VERSION"],
        portable: false,
        checksum: digest(&[&f32_bytes(&dz.data), &opt_f32_bytes(&dz.weight)]),
    });

    // master_bias: 7 frames 64x64 at 500 DN plus noise, one hot pixel.
    let bias_paths: Vec<PathBuf> = (0..7u64)
        .map(|i| {
            let mut plane = vec![500.0f32; 64 * 64];
            add_noise(&mut plane, 2.0, 300 + i);
            if i == 3 {
                plane[17 * 64 + 23] = 9000.0;
            }
            write_mono(d, &format!("bias{i}.fits"), 64, 64, &plane)
        })
        .collect();
    let (_, on_band) = no_progress();
    let bias = pool
        .install(|| {
            integrate_bias_like(
                &bias_paths,
                IntegrationRecipe::average(Rejection::WinsorizedSigma {
                    sigma_low: 3.0,
                    sigma_high: 3.0,
                }),
                pool,
                d,
                &cancel,
                EngineProgress {
                    on_band: &on_band,
                    on_combine: &on_band,
                },
                io(),
            )
        })
        .unwrap();
    rows.push(Computed {
        stage: "master_bias",
        versions: &["ENGINE_VERSION"],
        portable: false,
        checksum: digest(&[&f32_bytes(&bias.data), &f64_bytes(&[bias.rejected_fraction])]),
    });

    // master_flat: 5 flats 64x64, vignetting and exposure drift, a synthetic bias of 500 DN.
    let drift = [1.0f32, 1.4, 2.0, 2.6, 3.5];
    let flat_paths: Vec<PathBuf> = drift
        .iter()
        .enumerate()
        .map(|(i, &k)| {
            let mut plane = vec![0f32; 64 * 64];
            for y in 0..64usize {
                for x in 0..64usize {
                    let (dx, dy) = (x as f32 - 31.5, y as f32 - 31.5);
                    let vignette = 1.0 - 0.0002 * (dx * dx + dy * dy);
                    plane[y * 64 + x] = 500.0 + 4000.0 * k * vignette;
                }
            }
            add_noise(&mut plane, 3.0, 400 + i as u64);
            write_mono(d, &format!("flat{i}.fits"), 64, 64, &plane)
        })
        .collect();
    let flat = pool
        .install(|| {
            integrate_flat(
                &flat_paths,
                &FlatPrecal::SyntheticBias(500.0),
                IntegrationRecipe::median(Rejection::None),
                pool,
                d,
                &cancel,
                EngineProgress {
                    on_band: &on_band,
                    on_combine: &on_band,
                },
                io(),
            )
        })
        .unwrap();
    rows.push(Computed {
        stage: "master_flat",
        versions: &["ENGINE_VERSION"],
        portable: false,
        checksum: digest(&[&f32_bytes(&flat.data), &f64_bytes(&flat.flat_norm.into_iter().collect::<Vec<_>>())]),
    });

    // light_cal: light, dark, flat and bias 64x64 as FITS, no caller card, the flat normalized.
    let ramp = |base: f32, slope: f32, sigma: f32, seed: u64| {
        let mut plane: Vec<f32> = (0..64 * 64).map(|k| base + slope * (k % 64) as f32).collect();
        add_noise(&mut plane, sigma, seed);
        plane
    };
    let light_path = write_mono(d, "cal_light.fits", 64, 64, &ramp(8000.0, 20.0, 6.0, 500));
    let dark_path = write_mono(d, "cal_dark.fits", 64, 64, &ramp(1000.0, 0.0, 2.0, 501));
    let bias_path = write_mono(d, "cal_bias.fits", 64, 64, &ramp(990.0, 0.0, 1.0, 503));
    let flat_path = write_mono(d, "cal_flat.fits", 64, 64, &ramp(30000.0, -60.0, 8.0, 502));
    let cal_inputs = LightCalInputs {
        light_path,
        dark_path: Some(dark_path),
        bias_path: Some(bias_path),
        flat_path: Some(flat_path),
        flat_norm: true,
        flat_norm_mode: FlatNormMode::CentralThird,
        cfa_geometry: None,
        params: LightCalParams::default(),
        scale_divisor: OUTPUT_SCALE_DIVISOR,
        output_path: d.join("cal_out.fits"),
        cards: vec![],
        scratch_dir: d.to_path_buf(),
    };
    let (calibrated, _) = pool
        .install(|| calibrate_light_compute_with_cards(&cal_inputs, None, &NoFlatCards, &cancel))
        .unwrap();
    rows.push(Computed {
        stage: "light_cal",
        versions: &["LIGHT_CAL_VERSION"],
        portable: false,
        checksum: digest(&[&f32_bytes(&calibrated.data)]),
    });

    // cosmetic: a 64x64 dark and a light with the same 6 spikes.
    let spikes = [101usize, 777, 1500, 2222, 3001, 4000];
    let mut cos_dark = vec![300.0f32; 64 * 64];
    add_noise(&mut cos_dark, 2.0, 600);
    let mut cos_light = vec![1000.0f32; 64 * 64];
    add_noise(&mut cos_light, 5.0, 601);
    for &k in &spikes {
        cos_dark[k] = 5000.0;
        cos_light[k] = 4000.0;
    }
    let cos_dark_path = write_mono(d, "cos_dark.fits", 64, 64, &cos_dark);
    let (map, corrected_count) = pool.install(|| {
        let map = match hot_pixel_map_from_dark(&cos_dark_path, d).unwrap() {
            HotPixelMapOutcome::Map(m) => m,
            HotPixelMapOutcome::Refused(why) => panic!("the cosmetic fixture's dark was refused: {why}"),
        };
        let corrected_count = apply_hot_pixel_correction(&mut cos_light, 64, 64, &map, None);
        (map, corrected_count)
    });
    rows.push(Computed {
        stage: "cosmetic",
        versions: &["LIGHT_CAL_VERSION"],
        portable: false,
        checksum: digest(&[
            &f32_bytes(&cos_light),
            &(map.len() as u64).to_le_bytes(),
            &corrected_count.to_le_bytes(),
        ]),
    });

    rows
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn the_pool() -> Arc<rayon::ThreadPool> {
    Arc::new(rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap())
}

/// The goldens: every stage's checksum equals its row, and two runs on the fixed pool agree.
#[test]
fn stage_outputs_match_their_goldens() {
    assert!(
        std::env::var_os("STARLOOM_SCALAR_SELECTION").is_none(),
        "goldens are recorded on the default kernel selection; unset STARLOOM_SCALAR_SELECTION"
    );
    let pool = the_pool();
    let first = compute(&pool);
    let second = compute(&pool);
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(
            a.checksum, b.checksum,
            "{}: two runs gave {:#018x} and {:#018x}: not deterministic on a fixed 2-thread pool",
            a.stage, a.checksum, b.checksum
        );
    }
    let platform = platform();
    if !GOLDENS.iter().any(|g| g.platform == platform) {
        eprintln!("no golden rows for {platform}: determinism checked only");
        for c in &first {
            eprintln!("{}", literal(if c.portable { "*" } else { &platform }, c, &live_version));
        }
    }
    if let Err(report) = check(GOLDENS, &platform, &first, &live_version) {
        panic!("{report}");
    }
}

fn one_row_table() -> Vec<Golden> {
    vec![Golden {
        stage: "measure",
        platform: "p",
        versions: &[("MEASURE_VERSION", 8)],
        checksum: 1,
    }]
}

fn measure_computed(checksum: u64) -> Vec<Computed> {
    vec![Computed {
        stage: "measure",
        versions: &["MEASURE_VERSION"],
        portable: false,
        checksum,
    }]
}

#[test]
fn a_moved_checksum_under_the_same_version_asks_for_a_bump() {
    let err = check(&one_row_table(), "p", &measure_computed(2), &|_| 8).unwrap_err();
    assert!(err.contains("bump MEASURE_VERSION"), "{err}");
    assert!(err.contains("New row:"), "{err}");
}

#[test]
fn a_moved_version_asks_for_a_new_row() {
    let err = check(&one_row_table(), "p", &measure_computed(1), &|_| 9).unwrap_err();
    assert!(err.contains("MEASURE_VERSION is 9"), "{err}");
    assert!(err.contains("New row:"), "{err}");
}

#[test]
fn a_platform_without_rows_checks_determinism_only() {
    assert!(check(&one_row_table(), "q", &measure_computed(2), &|_| 8).is_ok());
}

#[test]
fn a_star_row_is_enforced_on_every_platform() {
    let table = [Golden {
        stage: "ln_grid_format",
        platform: "*",
        versions: &[("LN_GRID_VERSION", 1)],
        checksum: 1,
    }];
    let computed = [Computed {
        stage: "ln_grid_format",
        versions: &["LN_GRID_VERSION"],
        portable: true,
        checksum: 2,
    }];
    let err = check(&table, "q", &computed, &|_| 1).unwrap_err();
    assert!(err.contains("bump LN_GRID_VERSION"), "{err}");
}

#[test]
fn a_row_for_a_stage_no_longer_computed_fails() {
    let mut table = one_row_table();
    table.push(Golden {
        stage: "dropped_stage",
        platform: "p",
        versions: &[("MEASURE_VERSION", 8)],
        checksum: 3,
    });
    let err = check(&table, "p", &measure_computed(1), &|_| 8).unwrap_err();
    assert!(err.contains("no longer computed"), "{err}");
    assert!(err.contains("stage: \"dropped_stage\", platform: \"p\""), "{err}");
    // A star row for a stage that is gone fails on every platform, even one with no rows of its own.
    let star = [Golden {
        stage: "dropped_grid_2",
        platform: "*",
        versions: &[("LN_GRID_VERSION", 1)],
        checksum: 4,
    }];
    let err = check(&star, "q", &measure_computed(1), &|_| 8).unwrap_err();
    assert!(err.contains("no longer computed"), "{err}");
    assert!(err.contains("platform: \"*\""), "{err}");
}

#[test]
fn a_second_row_for_one_stage_and_platform_fails() {
    let mut table = one_row_table();
    table.push(Golden {
        stage: "measure",
        platform: "p",
        versions: &[("MEASURE_VERSION", 8)],
        checksum: 5,
    });
    let err = check(&table, "p", &measure_computed(1), &|_| 8).unwrap_err();
    assert!(err.contains("a second row"), "{err}");
    assert!(err.contains("checksum: 0x0000000000000005"), "{err}");
    let star = |checksum| Golden {
        stage: "ln_grid_format",
        platform: "*",
        versions: &[("LN_GRID_VERSION", 1)],
        checksum,
    };
    let table = [star(1), star(2)];
    let computed = [Computed {
        stage: "ln_grid_format",
        versions: &["LN_GRID_VERSION"],
        portable: true,
        checksum: 1,
    }];
    let err = check(&table, "q", &computed, &|_| 1).unwrap_err();
    assert!(err.contains("a second row"), "{err}");
}
