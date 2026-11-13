//! Stage 1, calibrate (`stage_calibrate`): each light through its masters, with per-frame artifact reuse.

use super::*;

/// Calibrate-only byte footprint for progress's `bytes_total` (mirrors
/// [`crate::stacking::paths::estimate_bytes`]'s calibrated-frame term, but
/// scoped to just this stage rather than the whole run's estimate). Sums
/// per FRAME rather than per group (owner decision 2026-09-10: a group's
/// members can carry different native geometry now that camera/geometry
/// are not grouping keys, so there is no single group-wide `W x H` any
/// more).
pub(super) fn calibrate_bytes_total(groups: &[IntegrationGroup], excluded: &HashSet<i64>) -> u64 {
    let mut total = 0u64;
    for g in groups {
        let planes: u64 = if g.color_mode == ColorMode::Osc { 3 } else { 1 };
        for f in &g.frames {
            if excluded.contains(&f.frame_id) {
                continue;
            }
            let w = f.width.max(0) as u64;
            let h = f.height.max(0) as u64;
            total += planes * w * h * 4;
        }
    }
    total
}

/// One frame's stage-1 RESOLVE outcome (private to [`stage_calibrate`],
/// perf tier 1 Task 4). Replaces the old `CalibrateOutcome`: `Reused`/
/// `Excluded` are terminal (nothing more to do for this frame), `Job` is a
/// fully-resolved unit of pixel work the fan-out below runs in parallel and
/// [`commit_calibrated`] then writes back.
enum CalibrateResolved {
    /// An existing `calibrated` artifact was fresh; nothing was written.
    /// `bytes` is the reused file's own recorded size (fix round 1, item 5:
    /// a fully-reused stage must report `N/N` progress bytes, not `0/N`).
    Reused { bytes: u64 },
    /// Calibration failed for this frame; `reason` is
    /// [`RunContext::runtime_exclusions`]'s text.
    Excluded { reason: String },
    /// Everything a worker needs to calibrate this frame's pixels, with no
    /// further catalog access — see [`CalibrateJob`].
    Job(Box<CalibrateJob>),
}

/// One frame's fully-resolved stage-1 pixel work (perf tier 1 Task 4):
/// everything [`resolve_calibrate_job`] read from the catalog, handed to a
/// fan-out worker so it never touches `rc`/the DB. Frame order is already
/// recoverable from `jobs[pos]` (the resolve loop below pushes jobs in group
/// order and the results loop walks `jobs` the same way), so there is no
/// `idx` field here (Task 4 fix round — the original field was set but never
/// read).
struct CalibrateJob {
    frame_id: i64,
    /// The frame's `calibrated` / `calibrated_mosaic` rows as the resolve step found them (under the frame-level key),
    /// so the commit can remove a file the rewrite replaced ([`replace_frame_level_file`]).
    old_calibrated: Option<crate::db::stacking::StackingArtifactRow>,
    old_mosaic: Option<crate::db::stacking::StackingArtifactRow>,
    hash: String,
    spec: GenerationSpec,
    out: PathBuf,
    mosaic_out: Option<PathBuf>,
    opts: CalibratedLightOptions,
    /// Keeps the zip-cache files `spec` reads (a zip light, zip masters) from eviction until the job is dropped.
    _pins: Vec<crate::zip_store::cache::Pinned>,
}

/// One group's resolved stage-1 work (Task 13c L4a): the jobs left after cache reuse and exclusions, and the
/// sizing the cross-group queue decision needs.
struct CalibrateGroupPlan {
    /// Index into the run's `plan_groups`.
    group_idx: usize,
    jobs: Vec<CalibrateJob>,
    /// The LARGEST frame's (width, height) in the group.
    plane_px: (u64, u64),
    working_set_bytes: u64,
    /// Preloaded-master bytes counted from the jobs (an upper bound on what the preload will hold).
    shared_bytes_est: u64,
}

/// The distinct master files a group's jobs need preloaded: the dark (the bias when there is no dark — never
/// both) and the flat. Sorted, so the preload order is deterministic.
fn distinct_preload_paths(jobs: &[CalibrateJob]) -> BTreeSet<PathBuf> {
    let mut distinct: BTreeSet<PathBuf> = BTreeSet::new();
    for job in jobs {
        let subtrahend = job
            .spec
            .inputs
            .dark_path
            .clone()
            .or_else(|| job.spec.inputs.bias_path.clone());
        distinct.extend(subtrahend);
        distinct.extend(job.spec.inputs.flat_path.clone());
    }
    distinct
}

/// What one group's phase 1 pushed to `rc.warnings` / `rc.runtime_exclusions`.
#[derive(Default)]
struct GroupListStash {
    warnings: Vec<String>,
    exclusions: Vec<(i64, String)>,
}

/// Move everything pushed to the run's lists since `(warn_mark, excl_mark)` into a stash.
fn take_list_stash(rc: &mut RunContext, warn_mark: usize, excl_mark: usize) -> GroupListStash {
    GroupListStash {
        warnings: rc.warnings.split_off(warn_mark),
        exclusions: rc.runtime_exclusions.split_off(excl_mark),
    }
}

/// Put the stashes of groups `*next..=upto` back onto the run's lists, in group order.
fn replay_list_stash(
    rc: &mut RunContext,
    stash: &mut [GroupListStash],
    next: &mut usize,
    upto: usize,
) {
    while *next <= upto && *next < stash.len() {
        let s = std::mem::take(&mut stash[*next]);
        rc.warnings.extend(s.warnings);
        rc.runtime_exclusions.extend(s.exclusions);
        *next += 1;
    }
}

/// Run the calibrate jobs of one or several groups through ONE [`fan_out_groups`] queue; `plans[k]`, `preloads[k]`
/// and `tickers[k]` belong together. Results per group, in that group's job order.
fn calibrate_queue(
    rc: &RunContext,
    plans: &[&CalibrateGroupPlan],
    preloads: &[&PreloadedMasters],
    tickers: &[&FanOutTicker],
    scratch: &Path,
    gate: &AdmissionGate,
    label: FanOutLabel,
) -> Vec<Vec<Option<Result<GeneratedLight, String>>>> {
    let cancel_ref: &AtomicBool = &rc.cancel;
    let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    // Arcs only — cheap. Every job's own dark was already prebuilt into `rc.hot_maps`, so each worker's clone is
    // a cache HIT for every lookup `execute_generation` makes; it never inserts into its clone.
    let hot_maps_snapshot: HashMap<PathBuf, Arc<HotPixelMapOutcome>> = rc.hot_maps.clone();
    let working_sets: Vec<u64> = plans.iter().map(|p| p.working_set_bytes).collect();
    let groups: Vec<Vec<&CalibrateJob>> = plans.iter().map(|p| p.jobs.iter().collect()).collect();
    fan_out_groups(
        groups,
        &working_sets,
        gate,
        label,
        cancel_ref,
        move |g: usize, job: &CalibrateJob| {
            if stage_one_job_cancelled(&job.out, job.frame_id) {
                tickers[g].tick(Some(job.frame_id));
                return Err(CANCELLED_MARKER.to_string());
            }
            let mut local_maps = hot_maps_snapshot.clone();
            let out = execute_generation(
                &job.spec,
                &job.out,
                job.mosaic_out.as_deref(),
                scratch,
                &job.opts,
                &mut local_maps,
                Some(preloads[g]),
                Some(pool_ref),
                cancel_ref,
            )
            .map_err(|e| {
                if matches!(
                    e.downcast_ref::<IntegrationError>(),
                    Some(IntegrationError::Cancelled)
                ) {
                    CANCELLED_MARKER.to_string()
                } else {
                    format!("calibration failed: {e:#}")
                }
            });
            tickers[g].tick(Some(job.frame_id));
            out
        },
    )
}

/// Stage 1's commit loop for ONE group, in the group's own frame order (never completion order):
/// `commit_calibrated`'s artifact-row writes and `rc.warnings` land exactly where the old sequential stage put
/// them.
///
/// Task 4 fix round: the `?` on `commit_calibrated` is a DB failure, not a calibration failure — if it fires at
/// frame k, the run fails and frames k+1.. in this group are already written to disk with no
/// `stacking_artifacts` row. That is not a leak: the next run's freshness check finds no row, regenerates those
/// frames, and the write is the same atomic temp-file + rename `execute_generation` always uses.
///
/// `bytes_done` for a GENERATED frame is only known once `commit_calibrated` returns its file's real size, so
/// the byte count advances at the group boundary, not mid-fan-out.
///
/// The dark guard's per-group tally rides along: a regenerated frame whose dark or bias left
/// [`DARK_SUBTRACTION_LOST_BAR`] or more of its pixels at or below zero is "lost", and a group with any lost
/// frame gets one `warn!` and one run warning ([`warn_subtraction_lost`]). A frame served from the cache was not
/// regenerated, has no count, and is not tallied; counting changes no byte of any frame or any stage hash.
fn commit_calibrate_group(
    rc: &mut RunContext,
    group_key: &str,
    jobs: &[CalibrateJob],
    results: Vec<Option<Result<GeneratedLight, String>>>,
    bytes_done: &mut u64,
) -> Result<(), RunError> {
    let mut counted = 0usize;
    let mut lost: Vec<(i64, f64)> = Vec::new();
    // What the lost frames had subtracted, from the engine's own CALSTAT: "BD…" is a dark, "B…" a bias.
    let (mut lost_dark, mut lost_bias) = (false, false);
    for (pos, res) in results.into_iter().enumerate() {
        let job = &jobs[pos];
        match res {
            None => return Err(RunError::Cancelled),
            Some(Err(msg)) if msg == CANCELLED_MARKER => return Err(RunError::Cancelled),
            Some(Err(msg)) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id = job.frame_id,
                    error = %msg,
                    "calibration failed; frame excluded"
                );
                rc.runtime_exclusions.push((job.frame_id, msg));
            }
            Some(Ok(generated)) => {
                if generated.pixels > 0 {
                    counted += 1;
                    let fraction = generated.non_positive_pixels as f64 / generated.pixels as f64;
                    if subtraction_lost(fraction) {
                        lost.push((job.frame_id, fraction));
                        if generated.calstat.starts_with("BD") {
                            lost_dark = true;
                        } else if generated.calstat.starts_with('B') {
                            lost_bias = true;
                        }
                    }
                }
                *bytes_done += commit_calibrated(rc, job, generated)?;
                rc.cached_calibrated.insert(job.frame_id, false);
            }
        }
    }
    if !lost.is_empty() {
        let subtrahend = match (lost_dark, lost_bias) {
            (true, false) => "dark",
            (false, true) => "bias",
            _ => "dark or bias",
        };
        warn_subtraction_lost(rc, group_key, counted, &lost, subtrahend);
    }
    Ok(())
}

/// The bar for a frame the dark guard calls lost: the share of its pixels at or below zero right after the dark
/// or bias was subtracted. The share is 0.5 exactly when the frame's median sits at zero, and a healthy
/// low-signal frame gets there on its own: a short narrowband sub has a sky of the order of the read noise, so
/// 20-30 % of its pixels are already at or below zero with a matching dark, a pedestal drift of about 1 ADU
/// (temperature, an old dark library) moves it further, and an OSC sensor behind a narrowband filter (three
/// quarters of its sites see almost no light) reaches 0.5. At 0.9 the median is more than 1.28 sigma below
/// zero, which drift cannot produce and a real gain or offset mismatch does (the real case read 98.6-99.9 %).
/// The standard test fixture (500 ADU background against a 100 ADU dark) reads 0 %.
const DARK_SUBTRACTION_LOST_BAR: f64 = 0.9;

/// Whether a frame with this share of pixels at or below zero is "lost" ([`DARK_SUBTRACTION_LOST_BAR`]).
fn subtraction_lost(non_positive_fraction: f64) -> bool {
    non_positive_fraction >= DARK_SUBTRACTION_LOST_BAR
}

/// The median of the lost frames' fractions (`fractions` is non-empty, finite).
fn median_fraction(fractions: &mut [f64]) -> f64 {
    fractions.sort_by(f64::total_cmp);
    let n = fractions.len();
    if n % 2 == 1 {
        fractions[n / 2]
    } else {
        (fractions[n / 2 - 1] + fractions[n / 2]) / 2.0
    }
}

/// One `warn!` and one run warning for a group with lost frames (`counted` is how many of its regenerated frames
/// had a dark or bias subtracted). Advisory: a catalog failure reading the lost lights' gain and offset is a
/// `warn!` and the sentence says "not recorded", never a failed run.
fn warn_subtraction_lost(
    rc: &mut RunContext,
    group_key: &str,
    counted: usize,
    lost: &[(i64, f64)],
    subtrahend: &str,
) {
    use crate::api::lights::{card_text, subtraction_run_sentence};
    let mut offsets: Vec<Option<f64>> = Vec::new();
    let mut gains: Vec<Option<f64>> = Vec::new();
    match db(&rc.ctx) {
        Ok(handle) => {
            let conn = handle.conn();
            for &(id, _) in lost {
                let row: rusqlite::Result<(Option<f64>, Option<f64>)> = conn.query_row(
                    "SELECT offset, gain FROM frames WHERE id = ?1",
                    rusqlite::params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                );
                match row {
                    Ok((offset, gain)) => {
                        offsets.push(offset);
                        gains.push(gain);
                    }
                    Err(e) => {
                        tracing::warn!(
                            run_id = rc.run_id,
                            frame_id = id,
                            error = %e,
                            "gain and offset unreadable; the dark subtraction warning omits them"
                        );
                        offsets.push(None);
                        gains.push(None);
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key,
                error = %e,
                "gain and offset unreadable: catalog unavailable; the dark subtraction warning omits them"
            );
        }
    }
    let offset_text = card_text("OFFSET", &offsets);
    let gain_text = card_text("GAIN", &gains);
    let mut fractions: Vec<f64> = lost.iter().map(|&(_, f)| f).collect();
    let fraction = median_fraction(&mut fractions);
    tracing::warn!(
        run_id = rc.run_id,
        group_key,
        count = lost.len(),
        non_positive_fraction = fraction,
        light_offset = %offset_text,
        light_gain = %gain_text,
        "dark subtraction left most pixels at or below zero"
    );
    rc.warnings.push(subtraction_run_sentence(
        group_key,
        lost.len(),
        counted,
        &format!("{offset_text}, {gain_text}"),
        subtrahend,
    ));
}

/// Stage 1's per-frame RESOLVE phase (perf tier 1 Task 4, spec §9.3): every
/// catalog read `calibrate_one_frame` used to do before handing off to
/// `execute_generation` — the stage-1 hash (through `rc.memo`, which
/// internally resolves the frame's calibration plan the same way the
/// second, generation-time resolution below does — see
/// [`HashMemo::calibration_hash_checked`]'s doc for why that is not wasted
/// work), the freshness lookup, and (on a miss) `resolve_generation_cached`
/// — all still on the run thread, one frame at a time, exactly as before.
/// Never held across [`resolve_generation_cached`]'s master-flat read
/// (matching this codebase's never-hold-a-connection-across-slow-I/O
/// convention), and no longer holds anything at all across
/// [`execute_generation`]'s pixel work — that now happens in a worker,
/// after this function has returned. Returns [`CalibrateResolved::Job`]
/// with everything a worker needs and nothing it does not (no `rc`, no
/// `Connection`); the caller (`stage_calibrate`) fills in `idx`.
fn resolve_calibrate_job(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    group: &IntegrationGroup,
    frame: &GroupFrame,
    scratch: &Path,
) -> Result<CalibrateResolved, RunError> {
    let group_key: &str = group.key.as_str();
    let hash = {
        let conn = db(&rc.ctx)?.conn();
        match rc.memo.calibration_hash_checked(&conn, cfg, frame) {
            Ok(hash) => hash,
            Err(e) => {
                return Ok(CalibrateResolved::Excluded {
                    reason: format!("calibration failed: {e}"),
                })
            }
        }
    };

    // M4d Task 1 (ruling R-M4d-1): does this group's drizzle deposit each
    // colour's own samples? Then this frame also owes a calibrated CFA
    // mosaic, produced by the SAME generation as the debayered artifact.
    let want_mosaic = wants_cfa_mosaic(cfg, group);

    // The frame's rows sit under the frame-level key (spec §5.1), so a regroup finds them again. They are read even
    // under `rerun_from = Calibrate` (no reuse then): the commit removes the file a rewrite replaces.
    let (existing, mosaic_row) = {
        let conn = db(&rc.ctx)?.conn();
        let existing = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            group_key,
            ArtifactKind::Calibrated,
            Some(frame.frame_id),
        )?;
        // Read for every frame, not only under `want_mosaic`: with the debayer off an OSC frame's calibrated file
        // takes the mosaic's name, and a mosaic row left over that file would read fresh over the new pixels.
        let mosaic_row = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            group_key,
            ArtifactKind::CalibratedMosaic,
            Some(frame.frame_id),
        )?;
        (existing, mosaic_row)
    };
    if rc.rerun_from != Some(Stage::Calibrate) {
        if let Some(row) = &existing {
            // The mosaic rides the calibrated artifact's OWN hash (it is the
            // same generation), so `drizzle.bayer` deliberately stays out of
            // `calibration_subtree`: flipping it must not invalidate every
            // calibrated frame in the set. What brings the mosaic into being
            // is this second freshness check — a frame calibrated by an
            // earlier run that wanted no mosaic regenerates BOTH files in one
            // generation, exactly once.
            let mosaic_fresh =
                !want_mosaic || mosaic_row.as_ref().is_some_and(|row| is_fresh(row, &hash));
            if is_fresh(row, &hash) && mosaic_fresh {
                return Ok(CalibrateResolved::Reused {
                    bytes: row.size.unwrap_or(0) as u64,
                });
            }
        }
    }

    let mut calibration_opts = cfg.calibration.clone();
    calibration_opts.keep_mosaic = want_mosaic;
    // R6: the run's calibrated frames are read back by `PlaneReader`
    // (Measure, Register, Integrate), which is FITS-only — the user-facing
    // format choice applies to export and send, never to the working tree.
    // Forced BEFORE `resolve_generation_cached` (not after): that call is
    // what sets `spec.format` from these options, so the override has to
    // land on the options it resolves from, not on a later copy.
    calibration_opts.format = crate::fits_writer::OutputFormat::Fits;
    // Perf tier 1 Task 6: this file is regenerable — the `stacking_artifacts`
    // row above keys on the config hash, the size and a time within
    // 2 s, so a file a crash truncates is a size miss on the next run, never a wrong answer.
    // Skipping the fsync still keeps the write atomic (tmp + rename).
    calibration_opts.skip_fsync = true;

    // Masters inside a zip are extracted BEFORE the catalog borrow below (a connection is never held across
    // an extraction); the resolve reads them from the cache. The pins ride in the job until its fan-out ends.
    let zip_masters = if matches!(
        origin_of(&rc.origins, frame.frame_id),
        Ok(FrameOrigin::Catalog { attested: false, .. })
    ) {
        match crate::export::ZipMasters::extract_for_frames(
            &rc.ctx,
            &[frame.frame_id],
            &rc.cancel,
            &mut |_, _, _| {},
        ) {
            Ok(found) => found,
            Err(e) => {
                rc.check_cancel()?;
                return Ok(CalibrateResolved::Excluded {
                    reason: format!("calibration failed: {e:#}"),
                });
            }
        }
    } else {
        crate::export::ZipMasters::none()
    };
    rc.check_cancel()?;
    let spec = {
        let conn = db(&rc.ctx)?.conn();
        // Dispatched by origin like the stage-1 hash above: a frame whose id
        // is not a catalog `frames.id` must never reach this catalog read.
        match origin_of(&rc.origins, frame.frame_id) {
            // An attested light is prepared, never calibrated (spec §13).
            Ok(FrameOrigin::Catalog { attested: true, .. }) => {
                Err("an attested frame is prepared, never calibrated".to_string())
            }
            Ok(FrameOrigin::Catalog { .. }) => crate::export::resolve_generation_mapped(
                &conn,
                frame.frame_id,
                &calibration_opts,
                scratch,
                rc.memo.divisors_mut(),
                &zip_masters,
            )
            .map_err(|e| format!("{e:#}")),
            // A project frame is prepared, never calibrated (spec §5.1).
            Ok(FrameOrigin::Project { .. }) => {
                Err("a project frame is prepared, never calibrated".to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    };
    let spec = match spec {
        Ok(s) => s,
        Err(e) => {
            return Ok(CalibrateResolved::Excluded {
                reason: format!("calibration failed: {e}"),
            })
        }
    };
    // A light inside a zip is read from its extracted copy; the pin lives in the job (with the masters')
    // until the group's fan-out ends.
    let mut pins = zip_masters.into_pins();
    let spec = {
        let light_catalog_path = spec.inputs.light_path.to_string_lossy().into_owned();
        let in_zip = {
            let conn = db(&rc.ctx)?.conn();
            crate::zip_store::presence::is_zip_resident_path(&conn, &light_catalog_path)
        };
        if in_zip {
            match crate::zip_store::materialize::materialize_one_cancellable(
                &rc.ctx,
                &light_catalog_path,
                &rc.cancel,
            ) {
                Ok((local, pin)) => {
                    pins.extend(pin);
                    spec.with_light_path(local)
                }
                Err(e) => {
                    rc.check_cancel()?;
                    tracing::error!(
                        run_id = rc.run_id,
                        frame_id = frame.frame_id,
                        path = %light_catalog_path,
                        error = %format!("{e:#}"),
                        "light could not be read from its zip"
                    );
                    return Ok(CalibrateResolved::Excluded {
                        reason: format!("calibration failed: cannot read the file: {e:#}"),
                    });
                }
            }
        } else {
            spec
        }
    };

    let stem = frame_file_stem(frame);
    let out = rc
        .layout
        .calibrated_dir()
        .join(spec.output_filename(&format!("{stem}.fits")));
    // M4d Task 1: the mosaic is the same stem under the MONO spelling
    // (`c_<stem>.fits`) — through the one shared naming rule, never a second
    // `format!`, so the mosaic and the debayered sibling can never drift
    // apart. A mono frame never has one (`wants_cfa_mosaic` is false for a
    // mono group, and `execute_generation` writes none for a frame it is not
    // debayering — whose own output already IS this name).
    let mosaic_out = want_mosaic.then(|| {
        rc.layout
            .calibrated_dir()
            .join(crate::export::calibrated_output_filename(
                &format!("{stem}.fits"),
                false,
                // R7: the mosaic is a run-internal artifact, always FITS.
                crate::fits_writer::OutputFormat::Fits,
            ))
    });

    // The write order (a row is gone before the file it names is replaced): a cancelled or killed run then leaves no
    // row of this hash over another config's file. `Reused` and `Excluded` returned above and delete nothing. The
    // mosaic row goes too when the run wants a mosaic or its file is the one `out` overwrites.
    {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::delete_artifact(
            &conn,
            &rc.subject,
            group_key,
            ArtifactKind::Calibrated,
            Some(frame.frame_id),
        )?;
        let mosaic_names_out = mosaic_row
            .as_ref()
            .and_then(|row| row.path.as_deref())
            .is_some_and(|path| Path::new(path) == out.as_path());
        if want_mosaic || mosaic_names_out {
            crate::db::stacking::delete_artifact(
                &conn,
                &rc.subject,
                group_key,
                ArtifactKind::CalibratedMosaic,
                Some(frame.frame_id),
            )?;
        }
        // Any other subject's row over a file this job writes goes too (two sets with one name share a root).
        crate::db::stacking::delete_artifacts_naming_path(&conn, &out.to_string_lossy())?;
        if let Some(mosaic_out) = &mosaic_out {
            crate::db::stacking::delete_artifacts_naming_path(&conn, &mosaic_out.to_string_lossy())?;
        }
    }

    Ok(CalibrateResolved::Job(Box::new(CalibrateJob {
        frame_id: frame.frame_id,
        old_calibrated: existing,
        old_mosaic: mosaic_row,
        hash,
        spec,
        out,
        mosaic_out,
        opts: calibration_opts,
        _pins: pins,
    })))
}

/// Stage 1's per-dark PREBUILD phase (perf tier 1 Task 4): builds every
/// distinct master dark's hot-pixel map that `jobs` will need, on the run
/// thread, BEFORE the fan-out — so no two workers ever race to measure the
/// same dark, and every worker's own `execute_generation` call sees a cache
/// hit (`newly_measured == false`) whatever order the fan-out finishes in.
/// Mirrors `execute_generation`'s own cache-miss branch
/// (`calibrated_generator.rs:511-533`) exactly — same call, same `Refused`
/// mapping, same `warn!` — with ONE difference: because every worker's own
/// call is now guaranteed a cache hit, `execute_generation` can never be the
/// one to push the operator-facing refusal line, so THIS function pushes it
/// instead, still exactly once per distinct dark (whichever job in `jobs`
/// names it first — `jobs` is built by `stage_calibrate`'s resolve loop in
/// group-frame order, so that "first" is deterministic and matches what the
/// old sequential stage would have reported).
///
/// Task 4 fix round: the `seen_this_call` set this loop used to keep beside
/// `rc.hot_maps` was redundant — `rc.hot_maps.insert` below runs in the same
/// iteration that measures a dark, so `rc.hot_maps.contains_key` already
/// short-circuits every later job naming the same dark, within this call and
/// across calls alike.
fn prebuild_hot_maps(
    rc: &mut RunContext,
    jobs: &[CalibrateJob],
    scratch: &Path,
    group_key: &str,
    plane_px: (u64, u64),
    resident_bytes: u64,
) {
    // Distinct darks still to measure, in job order (the order the serial
    // loop visited them, so warnings land in today's group order). Deduped
    // BEFORE spawning: a concurrent run would otherwise write `scratch` per
    // dark twice.
    let mut darks: Vec<PathBuf> = Vec::new();
    for job in jobs {
        if !job.opts.hot_pixel_correction {
            continue;
        }
        let Some(dark) = &job.spec.dark_path else {
            continue;
        };
        if rc.hot_maps.contains_key(dark) || darks.contains(dark) {
            continue;
        }
        darks.push(dark.clone());
    }
    if darks.is_empty() {
        return;
    }
    let gate = preload_gate(rc, plane_px, darks.len(), resident_bytes);
    let results = fan_out(
        darks.clone(),
        &gate,
        FanOutLabel::new(rc, Stage::Calibrate, Some(group_key), "hot-maps"),
        &rc.cancel,
        |dark: PathBuf| -> Result<HotPixelMapOutcome, String> {
            Ok(match hot_pixel_map_from_dark(&dark, scratch) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(
                        path = %dark.display(),
                        error = %format!("{e:#}"),
                        "measuring the master dark failed — hot-pixel correction refused"
                    );
                    HotPixelMapOutcome::Refused(format!("measuring it failed: {e:#}"))
                }
            })
        },
    );
    for (dark, result) in darks.into_iter().zip(results) {
        // `None`: the fan-out saw a cancel before starting this dark; the
        // caller's cancel check right after ends the group.
        let Some(Ok(measured)) = result else { continue };
        if let HotPixelMapOutcome::Refused(reason) = &measured {
            rc.warnings.push(format!(
                "Hot-pixel correction skipped for {}: {reason}",
                dark.display()
            ));
        }
        rc.hot_maps.insert(dark, Arc::new(measured));
    }
}

/// Planes resident per worker while a master is decoded or a hot-pixel map
/// measured (one full plane read plus its scratch/result copy).
const PRELOAD_PLANES_RESIDENT: u64 = 3;

/// The admission gate for the Calibrate preload fan-outs: `PRELOAD_PLANES_RESIDENT`
/// planes of the group's geometry per worker against the live budget.
/// `resident_bytes` is what earlier groups' preloads already hold (the merged Calibrate prelude keeps every
/// group's masters resident at once; `0` for a per-group preload, whose predecessors were dropped).
fn preload_gate(
    rc: &RunContext,
    plane_px: (u64, u64),
    items: usize,
    resident_bytes: u64,
) -> AdmissionGate {
    let working_set = PRELOAD_PLANES_RESIDENT * plane_px.0 * plane_px.1 * 4;
    let budget = fan_budget(rc);
    let n = admission_with_shared(
        resident_bytes,
        working_set,
        rc.ctx.image_pool.current_num_threads().min(items.max(1)),
        budget.bytes,
    );
    AdmissionGate::budgeted(rc, working_set, resident_bytes, n)
}

/// Stage 1's per-group PRELOAD phase (perf tier 1 Task 5): decode every
/// distinct master subtrahend/flat `jobs` will need exactly ONCE, on the
/// run thread, before the fan-out — so instead of every included frame
/// re-opening and re-band-reading its group's shared masters,
/// `execute_generation`'s engine call reads them straight from RAM. Decoded
/// through [`preload_master_plane`], the SAME `BandSource` decode path the
/// engine runs itself, so the samples are bit-identical to the old
/// per-frame reads (pinned by
/// `light_cal::tests::preloaded_masters_calibrate_bit_identically` and
/// `light_cal::tests::preloaded_masters_match_across_multiple_bands`).
///
/// Fix round 1, Important finding 1: the subtrahend collected here mirrors
/// the engine's own dark-OR-bias rule (`light_cal.rs::
/// calibrate_light_compute_inner`'s `subtrahend` — dark preferred, bias
/// only when there is no dark) — `job.spec.inputs.dark_path.or(bias_path)`,
/// never both. Collecting the bias unconditionally alongside a present dark
/// used to decode and hold a plane the engine would never query.
///
/// A preload failure never fails the group: it is logged and the path is
/// simply absent from the returned map, so every worker's own
/// `execute_generation` call falls back to reading that one file itself —
/// exactly the pre-Task-5 behavior for that path alone. A CANCEL is
/// different (ruled-in item 5): `cancel` is checked before each file (so a
/// run cancelled mid-group does not decode every remaining distinct master
/// first) and `preload_master_plane` itself is cancel-aware
/// (`BandSource::open_with_cancel` + `read_band_with_progress`), so a
/// cancel raised mid-file is `RunError::Cancelled` too, not a logged
/// failure. Paths are visited in SORTED order (ruled-in item 6, a
/// `BTreeSet` rather than a `HashSet`) so preload order — and therefore
/// warn order — is deterministic run to run.
fn build_preloaded_masters(
    rc: &RunContext,
    jobs: &[CalibrateJob],
    scratch: &Path,
    group_key: &str,
    plane_px: (u64, u64),
    resident_bytes: u64,
) -> Result<PreloadedMasters, RunError> {
    let cancel: &AtomicBool = &rc.cancel;
    let distinct = distinct_preload_paths(jobs);
    if cancel.load(Ordering::SeqCst) {
        return Err(RunError::Cancelled);
    }
    // Task 13c L1: the distinct masters decode concurrently (each decode is
    // independent, so the samples are bit-identical to the serial loop);
    // results come back by index, in the BTreeSet's sorted order.
    let paths: Vec<PathBuf> = distinct.into_iter().collect();
    let gate = preload_gate(rc, plane_px, paths.len(), resident_bytes);
    let results = fan_out(
        paths.clone(),
        &gate,
        FanOutLabel::new(rc, Stage::Calibrate, Some(group_key), "preload"),
        cancel,
        |path: PathBuf| preload_master_plane(&path, scratch, cancel),
    );
    let mut planes = HashMap::new();
    for (path, result) in paths.into_iter().zip(results) {
        match result {
            // Never started: a cancel was raised before this master's turn.
            None => return Err(RunError::Cancelled),
            Some(Ok(plane)) => {
                planes.insert(path, plane);
            }
            Some(Err(IntegrationError::Cancelled)) => return Err(RunError::Cancelled),
            Some(Err(e)) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "master preload failed; the engine reads it per frame"
                );
            }
        }
    }
    Ok(PreloadedMasters { planes })
}

/// Stage 1's per-frame COMMIT phase (perf tier 1 Task 4): everything
/// `calibrate_one_frame` used to do AFTER `execute_generation` returned —
/// file identities, the missing-mosaic warning, and the one-transaction
/// artifact-row pair — back on the run thread. Called from the fan-out's
/// results loop in the group's own frame order (never fan-out completion
/// order), so the artifact rows and `rc.warnings` this pushes land in
/// exactly the order the old sequential stage produced them in. Returns the
/// debayered artifact's own byte size (`bytes_total` for this stage is the
/// debayered footprint — `calibrate_bytes_total` — so counting the mosaic
/// too would push a complete stage past 100%).
fn commit_calibrated(
    rc: &mut RunContext,
    job: &CalibrateJob,
    generated: GeneratedLight,
) -> Result<u64, RunError> {
    rc.warnings.extend(generated.warnings);

    let (size, modified_at) =
        file_identity(&job.out).map_err(|e| RunError::Other(format!("{e:#}")))?;
    // M4d Task 1: recorded only when the generator says it actually wrote one
    // (`mosaic_written`), never by stat-ing the path — a leftover file from an
    // earlier run must not be adopted as this generation's output.
    let mosaic_identity = match (&job.mosaic_out, generated.mosaic_written) {
        (Some(path), true) => Some((
            path.clone(),
            file_identity(path).map_err(|e| RunError::Other(format!("{e:#}")))?,
        )),
        _ => None,
    };
    // Fix round 1 (I1): the one case the group-level `wants_cfa_mosaic` rule
    // cannot see — an OSC group frame whose own `BAYERPAT` the catalog could
    // not parse, so `resolve_generation` left `spec.debayer` false and no
    // mosaic exists to keep. That frame's `calibrated_mosaic` row will be
    // missing on the next run too, which means it recalibrates every run:
    // cheap on one broken frame, but never silent.
    if job.mosaic_out.is_some() && !generated.mosaic_written {
        tracing::warn!(
            run_id = rc.run_id,
            frame_id = job.frame_id,
            "no cfa mosaic could be kept for this frame; it will be recalibrated on every run"
        );
    }

    {
        // One transaction for the pair (fix round 1, m3): a crash between two
        // bare statements would leave the calibrated row alone and the mosaic
        // row missing — recoverable (the next run regenerates both) but it
        // would silently spend a full recalibration of the frame. Committing
        // them together makes the pair as atomic as the generation was.
        let mut conn = db(&rc.ctx)?.conn();
        let tx = conn
            .transaction()
            .map_err(|e| RunError::Other(format!("opening the artifact transaction: {e}")))?;
        upsert_artifact(
            &tx,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: Some(job.frame_id),
                group_key: "",
                kind: ArtifactKind::Calibrated,
                path: job.out.to_str(),
                config_hash: &job.hash,
                size: Some(size),
                modified_at: Some(&modified_at),
                payload_json: None,
            },
        )?;
        if let Some((path, (mosaic_size, mosaic_modified))) = &mosaic_identity {
            upsert_artifact(
                &tx,
                &NewArtifact {
                    subject: &rc.subject,
                    frame_id: Some(job.frame_id),
                    group_key: "",
                    kind: ArtifactKind::CalibratedMosaic,
                    path: path.to_str(),
                    config_hash: &job.hash,
                    size: Some(*mosaic_size),
                    modified_at: Some(mosaic_modified),
                    payload_json: None,
                },
            )?;
        }
        tx.commit()
            .map_err(|e| RunError::Other(format!("committing the artifact rows: {e}")))?;
        // The rows now point at the new files; the files they used to point at go once nothing references them.
        let owned = rc.layout.calibrated_root();
        replace_frame_level_file(&conn, &owned, job.old_calibrated.as_ref(), &job.out);
        if let Some((path, _)) = &mosaic_identity {
            replace_frame_level_file(&conn, &owned, job.old_mosaic.as_ref(), path);
        }
    }

    Ok(size as u64)
}

/// The plane count every member of `group` has after stage 1 PREPARE (plan
/// 6b-2 C13, review I12): one for mono; three for OSC with the debayer on;
/// with it off, three for an `osc__` group (its members are three-plane
/// files, in a project or an attested set) and one for a `cfa__` group
/// (mosaics kept as mosaics).
pub(super) fn expected_stage_one_planes(cfg: &StackingConfig, group: &IntegrationGroup) -> usize {
    crate::stacking::plan::stage_one_planes(cfg, group, true)
}

/// Planes of one w×h channel resident while a fan-out worker calibrates one
/// MONO frame (the light band in, the `out` plane): measured 2026-10-17 on
/// Horse C11 (mono 9576×6388, admission 1) as work 1 393 MiB − shared
/// 933 MiB ≈ 460 MiB = 1.9 planes of 233 MiB; one plane of margin → 3.
/// (Task 13c L1; it was a flat 8, which counted a debayer output even on mono.)
pub(crate) const CALIBRATE_PLANES_RESIDENT_MONO: u64 = 3;

/// The same for a DEBAYERED OSC frame (mosaic in + 3 debayered planes out):
/// measured on set 204 (OSC 6248×4176, admission 1) as work 600 MiB − shared
/// 199 MiB ≈ 400 MiB = 3.8 planes of 104 MiB (at admission 4: 1 798 − 199 MiB,
/// 400 MiB per worker, consistent); one plane of margin → 5.
pub(crate) const CALIBRATE_PLANES_RESIDENT_OSC: u64 = 5;

/// Stage-1-worker cancellation marker (perf tier 1 Task 4): a fan-out
/// worker cannot return `RunError` (the closure's error type is `String`,
/// same as every other fan-out in the run), so `execute_generation`
/// reporting `IntegrationError::Cancelled` is encoded as this sentinel
/// string instead of the ordinary "calibration failed: …" text; the
/// results loop right after `fan_out` returns maps it (and a plain
/// `rc.check_cancel()` failure) back to [`RunError::Cancelled`]. Never
/// shown to a user — a cancelled run's `last_error`/summary never reaches
/// this string, since [`RunError::Cancelled`] is handled before it would be
/// formatted.
pub(super) const CANCELLED_MARKER: &str = "__cancelled__";

/// Test hook: a stage-1 worker (Calibrate or Prepare) whose output lies under a listed directory and whose frame id
/// matches returns [`CANCELLED_MARKER`] without work, the shape of "the first unfinished job" in the commit loop. The
/// cancel flag stays clear, so the later jobs still write their files.
#[cfg(test)]
pub(super) static STAGE_ONE_JOB_CANCELLED: std::sync::Mutex<Vec<(PathBuf, i64)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(super) fn stage_one_job_cancelled(out: &Path, frame_id: i64) -> bool {
    STAGE_ONE_JOB_CANCELLED
        .lock()
        .map(|hooks| hooks.iter().any(|(dir, id)| *id == frame_id && out.starts_with(dir)))
        .unwrap_or(false)
}

#[cfg(not(test))]
pub(super) fn stage_one_job_cancelled(_out: &Path, _frame_id: i64) -> bool {
    false
}

/// Said once per run when Bayer drizzle is asked for but the OSC debayer is
/// off, so stage 1 keeps no calibrated CFA mosaic (both stage-1 kinds).
pub(super) fn warn_bayer_drizzle_ignored(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    groups: &[IntegrationGroup],
) {
    if cfg.drizzle.enabled
        && cfg.drizzle.bayer
        && !cfg.calibration.debayer_osc
        && groups.iter().any(|g| g.color_mode == ColorMode::Osc)
    {
        tracing::warn!(
            run_id = rc.run_id,
            set_id = rc.subject.frame_set_id(),
            project_id = rc.subject.project_id(),
            "bayer drizzle ignored: the osc debayer is off for this run"
        );
        rc.warnings.push(
            "Bayer drizzle is ignored: the OSC debayer is off for this run, so no calibrated CFA \
             mosaic is kept and drizzle deposits each frame's calibrated planes as they are."
                .to_string(),
        );
    }
}

/// Which stage 1 a run takes (plan 6b-2 C11, C12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageOneKind {
    Calibrate,
    Prepare,
}

/// Stage 1's kind for this run: Prepare for a project and for an attested
/// frame set, Calibrate otherwise. One stage-1 path per run: frames that
/// disagree refuse the run before any file is read. A frame with no origin
/// counts for neither; the stage that reads it fails on it: Calibrate
/// excludes the frame (6b-1), Prepare's `changed_during_run(..)?` propagates
/// `origin_of`'s error and fails the run, as Measure does.
fn stage_one_kind(rc: &RunContext) -> Result<StageOneKind, RunError> {
    let (mut prepare, mut calibrate) = (0usize, 0usize);
    for f in rc.plan_groups.iter().flat_map(|g| g.frames.iter()) {
        match rc.origins.get(&f.frame_id) {
            Some(origin) if crate::stacking::origin::needs_prepare(origin) => prepare += 1,
            Some(_) => calibrate += 1,
            None => {}
        }
    }
    if prepare > 0 && calibrate > 0 {
        return Err(RunError::Other(format!(
            "a run cannot mix prepared and calibrated frames ({prepare} prepared, {calibrate} calibrated)"
        )));
    }
    Ok(if prepare > 0 || rc.subject.project_id().is_some() {
        StageOneKind::Prepare
    } else {
        StageOneKind::Calibrate
    })
}

/// Spec 2026-10-04 §5.1-§5.3 (P-11): one run warning per group whose lights
/// link a flat but no dark or bias, with the flat's darkest corner over its
/// normalization divisor. Advisory: a failure to tally or to read a corner
/// is a `warn!` and the stage continues, never a failed run.
fn warn_flat_only_group(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    group: &IntegrationGroup,
    frame_ids: &[i64],
    scratch: &Path,
) {
    use crate::api::lights::{
        card_text, flat_only_lights, offset_gain_text, pedestal_run_sentence,
    };
    let handle = match db(&rc.ctx) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                error = %e,
                "pedestal check skipped: catalog unavailable"
            );
            return;
        }
    };
    let conn = handle.conn();
    let tally = match flat_only_lights(&conn, frame_ids) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                error = %e,
                "pedestal check skipped: link read failed"
            );
            return;
        }
    };
    if tally.frame_ids.is_empty() {
        return;
    }
    // The smallest ratio over the distinct flat masters the affected lights use.
    let mut corner: Option<f64> = None;
    let mut seen: HashSet<String> = HashSet::new();
    for &id in &tally.frame_ids {
        let inputs = match crate::calibration_library::light_resolve::resolve_frame_inputs(
            &conn,
            id,
            cfg.calibration.flat_norm,
        ) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id = id,
                    error = %e,
                    "flat corner ratio unreadable; the pedestal warning omits it"
                );
                continue;
            }
        };
        let Some(m) = inputs.flat else { continue };
        let phase = inputs
            .cfa_geometry
            .map(|g| (g.xoff.rem_euclid(2), g.yoff.rem_euclid(2)));
        if !seen.insert(format!("{}|{phase:?}", m.path)) {
            continue;
        }
        // A master flat inside a zip is read from its extracted copy; the pin
        // lives until this flat's divisor and corner ratio are read.
        let (flat_path, _flat_pin) =
            match crate::zip_store::materialize::materialize_one_cancellable(&rc.ctx, &m.path, &rc.cancel) {
                Ok(found) => found,
                Err(e) => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        path = %m.path,
                        error = %format!("{e:#}"),
                        "flat corner ratio unreadable; the pedestal warning omits it"
                    );
                    continue;
                }
            };
        let flat_local = flat_path.to_string_lossy().into_owned();
        // The corner ratio is corner over centre whatever the light division
        // does, so the divisor is always the generator's resolved constant.
        let divisor = match crate::export::calibrated_generator::flat_norm_divisor_cached(
            rc.memo.divisors_mut(),
            &flat_local,
            scratch,
            cfg.calibration.flat_norm_mode,
            &cfg.calibration.params,
            inputs.cfa_geometry,
        ) {
            Ok(d) => d.global_value(),
            Err(e) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    path = %m.path,
                    error = %e,
                    "flat corner ratio unreadable; the pedestal warning omits it"
                );
                continue;
            }
        };
        match crate::calibration_library::light_cal::flat_corner_ratio(
            &flat_path,
            scratch,
            divisor,
        ) {
            Ok(v) => corner = Some(corner.map_or(v, |c| c.min(v))),
            Err(e) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    path = %m.path,
                    error = %e,
                    "flat corner ratio unreadable; the pedestal warning omits it"
                );
            }
        }
    }
    let offset_text = card_text("OFFSET", &tally.offsets);
    let gain_text = card_text("GAIN", &tally.gains);
    let flat_ids_text = tally
        .flat_set_ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    tracing::warn!(
        run_id = rc.run_id,
        group_key = %group.key,
        count = tally.frame_ids.len(),
        light_offset = %offset_text,
        light_gain = %gain_text,
        flat_set_ids = %flat_ids_text,
        flat_corner_ratio = corner.unwrap_or(f64::NAN),
        "flat without a dark or bias: the offset pedestal is amplified in vignetted corners"
    );
    rc.warnings.push(pedestal_run_sentence(
        &group.key,
        tally.frame_ids.len(),
        &offset_gain_text(&tally),
        corner,
    ));
}

/// The one progress message of the input check.
pub(super) const INPUTS_MESSAGE: &str = "checking the inputs";

/// Every path the run reads as an input: each catalog light of the plan groups (a manually excluded
/// one too: it is still a member of the plan, and a later regroup may include it) and, for a light
/// that is calibrated, every master applied to it. A project's frames give nothing. Sorted, once each.
fn run_input_paths(rc: &RunContext, conn: &rusqlite::Connection) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for frame in rc.plan_groups.iter().flat_map(|g| g.frames.iter()) {
        let Some(origin) = rc.origins.get(&frame.frame_id) else {
            continue;
        };
        let attested = match origin {
            FrameOrigin::Catalog { attested, .. } => *attested,
            FrameOrigin::Project { .. } => continue,
        };
        paths.push(frame.path.clone());
        if attested {
            continue;
        }
        match crate::export::applied_master_paths(conn, frame.frame_id, &rc.config.calibration) {
            Ok(masters) => paths.extend(masters.into_iter().map(|p| p.to_string_lossy().into_owned())),
            // The stage-1 hash reports it for the frame.
            Err(error) => tracing::debug!(
                frame_id = frame.frame_id,
                %error,
                "masters of a frame cannot be named for the input check"
            ),
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Whether a stage of THIS run reads the cached row a rebase candidate stands for: the one rule that decides which
/// rows the "not found" warning counts (a row no stage reads is not "recomputed", and counting it would keep the
/// warning for ever). A frame-level row counts when its frame is a member of a plan group and is not manually
/// excluded; the calibrated mosaic only when its group keeps one ([`wants_cfa_mosaic`]); an LN row only when local
/// normalization is on, for a member of the row's own group, and the LN reference when that group is in the plan.
/// This may count fewer rows than the plan gate does (it never reads a row the run does not), never more.
struct RunReads {
    ln_active: bool,
    /// Group key to (whether the group keeps a CFA mosaic, its frames that are not manually excluded).
    groups: HashMap<String, (bool, HashSet<i64>)>,
    frames: HashSet<i64>,
}

impl RunReads {
    fn of(rc: &RunContext) -> Self {
        let mut groups = HashMap::new();
        let mut frames = HashSet::new();
        for g in &rc.plan_groups {
            let members: HashSet<i64> = g
                .frames
                .iter()
                .map(|f| f.frame_id)
                .filter(|id| !rc.excluded.contains(id))
                .collect();
            frames.extend(members.iter().copied());
            groups.insert(g.key.clone(), (wants_cfa_mosaic(&rc.config, g), members));
        }
        Self {
            ln_active: rc.config.normalization.local.enabled
                || rc.config.normalization.rejection == RejectionNormalization::Local,
            groups,
            frames,
        }
    }

    fn reads(&self, c: &crate::stacking::rebase::Candidate) -> bool {
        let Some(frame) = c.frame_id else {
            // The one group-level row with a file: the LN reference.
            return c.kind == ArtifactKind::LnReference.as_db_str()
                && self.ln_active
                && self.groups.contains_key(&c.group_key);
        };
        if c.kind == ArtifactKind::Ln.as_db_str() {
            return self.ln_active
                && self.groups.get(&c.group_key).is_some_and(|(_, members)| members.contains(&frame));
        }
        if c.kind == ArtifactKind::LnReference.as_db_str() {
            return self.ln_active && self.groups.contains_key(&c.group_key);
        }
        if c.kind == ArtifactKind::CalibratedMosaic.as_db_str() {
            return self.groups.values().any(|(mosaic, members)| *mosaic && members.contains(&frame));
        }
        self.frames.contains(&frame)
    }
}

/// Point the subject's cached rows at the CURRENT working folder when their files moved with it (file identity
/// §4.10), once per run, before the inputs are sampled and before the stage-1 dispatch. The three phases of the
/// input check: the candidates are read under the connection, every mapped file is probed WITHOUT it (one listing
/// per mapped folder, then one stat per listed file; cancel checked per file) and the rows that passed are committed
/// in one transaction. Nothing on disk is moved, copied or deleted. A failure to read the candidates or to commit is
/// logged and the run goes on: a row left where it is costs a recompute, never a wrong file. A row whose file is
/// still at the old place is read there; the warning counts the rows left AND nowhere, and only those a stage of this
/// run reads ([`RunReads`]).
///
/// Progress is ONE Calibrate event at `current = 0`, as the input check's: a tick per file would reach 100 % and
/// the stage's own first event would put the row back to 0.
pub(super) fn rebase_working_rows(rc: &mut RunContext) -> Result<(), RunError> {
    use crate::stacking::rebase;

    // The rows are read under the connection; the canonicalisation of the old roots runs after the guard is dropped.
    let rows = {
        let conn = db(&rc.ctx)?.conn();
        match rebase::candidate_rows(&conn, &rc.subject, &rc.layout.root) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    error = %format!("{error:#}"),
                    "working folder rows not read for the rebase; the run goes on"
                );
                return Ok(());
            }
        }
    };
    let candidates = rebase::resolve_candidates(rows, &rc.layout.root);
    if candidates.is_empty() {
        return Ok(());
    }
    rc.progress(
        Stage::Calibrate,
        None,
        0,
        candidates.len(),
        0,
        0,
        None,
        Some(rebase::REBASE_MESSAGE.to_string()),
    );
    let mut prober = rebase::Prober::new(&candidates, false);
    let mut moved: Vec<&rebase::Candidate> = Vec::new();
    let mut left: Vec<&rebase::Candidate> = Vec::new();
    for c in &candidates {
        rc.check_cancel()?;
        let check = prober.check(c);
        let Some(reason) = check.left_reason() else {
            moved.push(c);
            continue;
        };
        tracing::debug!(
            path = %c.mapped.display(),
            kind = %c.kind,
            reason,
            "cached file not rebased"
        );
        left.push(c);
    }
    rc.check_cancel()?;
    let mut changed: HashSet<i64> = HashSet::new();
    if !moved.is_empty() {
        let mut conn = db(&rc.ctx)?.conn();
        match rebase::commit_rows(&mut conn, &moved) {
            Ok(rows) => changed = rows,
            Err(error) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    count = moved.len(),
                    error = %format!("{error:#}"),
                    "working folder rows not rebased; the run goes on"
                );
                // Their files are at the old place still, as every other left row's.
                left.append(&mut moved);
            }
        }
    }
    let mut roots: Vec<&Path> = Vec::new();
    for c in &candidates {
        if !roots.contains(&c.old_root.as_path()) {
            roots.push(c.old_root.as_path());
        }
    }
    for root in roots {
        let of_root = candidates.iter().filter(|c| c.old_root == root).count();
        let moved_here = candidates
            .iter()
            .filter(|c| c.old_root == root && changed.contains(&c.row_id))
            .count();
        tracing::info!(
            run_id = rc.run_id,
            old_root = %root.display(),
            new_root = %rc.layout.root.display(),
            moved = moved_here,
            left = of_root - moved_here,
            "working folder rows rebased"
        );
    }
    // A row whose file is still at the old place is read there; only one that is nowhere, and that a stage of this
    // run reads, is counted. One stat each, for those rows alone.
    let reads = RunReads::of(rc);
    let mut gone = rebase::LeftGone::default();
    for c in left {
        rc.check_cancel()?;
        if reads.reads(c) && rebase::is_gone_at_old_place(c, false) {
            gone.add(c);
        }
    }
    if let Some(warning) = rebase::left_warning(&gone, &rc.layout.root) {
        rc.warnings.push(warning);
    }
    Ok(())
}

/// Sample every input of the run ONCE, before any stage-1 hash (spec 4.4): read the rows under the
/// connection, stat and read the files WITHOUT it, bank what may be banked under it in one
/// transaction. Idempotent: a run whose book is filled returns at once. Runs after `stage_masters`, so
/// a master rebuilt in the run is sampled after its build.
///
/// Progress is ONE Calibrate event at `current = 0`; a tick per file would reach 100 % and the stage's
/// own first event would put the row back to 0.
pub(super) fn ensure_source_samples(rc: &mut RunContext) -> Result<(), RunError> {
    use crate::stacking::source_identity::{InputIdentity, SourceSamples};

    if rc.memo.samples_resolved() {
        return Ok(());
    }
    let started = Instant::now();
    let rows = {
        let conn = db(&rc.ctx)?.conn();
        let inputs = run_input_paths(rc, &conn);
        SourceSamples::read_rows(&conn, &inputs).map_err(|e| {
            tracing::error!(run_id = rc.run_id, error = %e, "stacking inputs cannot be read");
            RunError::Other(format!("{e:#}"))
        })?
    };
    let count = rows.len();
    if count > 0 {
        rc.progress(
            Stage::Calibrate,
            None,
            0,
            count,
            0,
            0,
            None,
            Some(INPUTS_MESSAGE.to_string()),
        );
    }
    let samplings = SourceSamples::sample_rows(rows, &rc.cancel);
    rc.check_cancel()?;
    let sampled = samplings
        .iter()
        .filter(|(_, s)| !matches!(s.identity, InputIdentity::NotSampled(_)))
        .count();
    let banked = {
        let mut conn = db(&rc.ctx)?.conn();
        match SourceSamples::bank(&mut conn, &samplings) {
            Ok(n) => n,
            Err(error) => {
                tracing::warn!(run_id = rc.run_id, %error, "input samples not banked; the run goes on");
                0
            }
        }
    };
    let mut book = SourceSamples::for_run();
    book.fill_run(samplings);
    rc.memo.set_run_samples(book);
    tracing::info!(
        run_id = rc.run_id,
        count,
        sampled,
        banked,
        duration_ms = started.elapsed().as_millis() as u64,
        "stacking inputs checked"
    );
    Ok(())
}

/// Stage 1: calibrate every non-manually-excluded LIGHT frame, reusing a
/// fresh `calibrated` artifact when one exists. Ruling 4's "sequentially,
/// exactly as the calibrated-lights export does today" is retired as of
/// 2026-09-19 (perf tier 1 Task 4): the per-frame DB lookups and freshness
/// check ([`resolve_calibrate_job`]) still run one at a time, on the run
/// thread, in the group's own frame order — but the pixel work
/// ([`execute_generation`]) now fans out across `rc.ctx.image_pool`'s width
/// via [`fan_out`], with every distinct dark's hot-pixel map measured once
/// up front ([`prebuild_hot_maps`]) so no worker ever races another to
/// build it. Results are written back ([`commit_calibrated`]: artifact
/// rows, `rc.warnings`) on the run thread, in the fan-out's ORIGINAL
/// (frame) order rather than completion order — so the FILES and artifact
/// rows are byte-identical to the old sequential stage's, which is what
/// `calibrate_fan_out_matches_the_sequential_stage_byte_for_byte` pins.
/// Warning ORDER is not: [`prebuild_hot_maps`] pushes every hot-pixel-map
/// refusal for a group up front, before that group's fan-out even starts,
/// where the old sequential stage would have interleaved the same warning
/// at whichever frame first triggered that dark's measurement — the SET of
/// warnings is unchanged, only their position relative to the per-frame
/// calibration warnings that follow. The calibrated-lights EXPORT path
/// (`export::file_organizer::GenerationBatch`) is untouched and keeps its
/// own sequential loop — this fan-out is stage 1
/// of the stacking run only. A per-frame failure excludes that frame
/// (`runtime_exclusions` + a `warn!`) rather than failing the run; only
/// `IntegrationError::Cancelled` (surfaced by a worker as
/// [`CANCELLED_MARKER`]) propagates as [`RunError::Cancelled`].
///
/// A project or an attested set takes [`stage_prepare`] instead (plan 6b-2
/// C11, C12).
pub(super) fn stage_calibrate(rc: &mut RunContext) -> Result<(), RunError> {
    stage_calibrate_with(rc, None)
}

/// [`stage_calibrate`] with the cross-group queue decision overridable (`Some(true)` = always one queue,
/// `Some(false)` = always per group; tests pin the two against each other).
fn stage_calibrate_with(rc: &mut RunContext, force_merge: Option<bool>) -> Result<(), RunError> {
    rebase_working_rows(rc)?;
    ensure_source_samples(rc)?;
    // Plan 6b-2 C11, C12: a project, or an attested set, prepares instead.
    if stage_one_kind(rc)? == StageOneKind::Prepare {
        return stage_prepare(rc);
    }
    // Calibrate only ever runs for a frame set; the id feeds the log lines.
    let Some(set_id) = rc.subject.frame_set_id() else {
        return Err(RunError::Other(
            "stage 1 calibrates a frame set only; a project prepares".into(),
        ));
    };
    let stage_start = Instant::now();
    let meter = crate::stacking::telemetry::StageMeter::start(rc, Stage::Calibrate, None);
    let excluded_set: HashSet<i64> = rc.excluded.iter().copied().collect();
    let groups = rc.plan_groups.clone();
    let cfg = rc.config.clone();

    let total: usize = groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .filter(|f| !excluded_set.contains(&f.frame_id))
        .count();
    let bytes_total = calibrate_bytes_total(&groups, &excluded_set);

    tracing::info!(
        run_id = rc.run_id,
        set_id,
        count = total,
        groups = groups.len(),
        config_hash = %rc.hash,
        "stacking run started"
    );

    // M4d Task 1 (fix round 1, I1): Bayer drizzle needs the calibrated CFA
    // mosaic, and stage 1 can only keep one for a frame it is DEBAYERING —
    // with the debayer off, an OSC group's calibrated frame IS the mosaic and
    // the group is single-plane, so there is nothing to route per colour.
    // Said once per run, here, where the toggle first matters: the
    // alternative (silence) is a run whose Drizzle row claims Bayer while
    // every plane is deposited exactly as it would have been without it.
    warn_bayer_drizzle_ignored(rc, &cfg, &groups);

    let scratch = rc.layout.tmp_root();
    std::fs::create_dir_all(&scratch)
        .map_err(|e| RunError::Other(format!("failed to create scratch dir: {e}")))?;

    let mut current = 0usize;
    let mut bytes_done = 0u64;
    rc.progress(
        Stage::Calibrate,
        None,
        current,
        total,
        bytes_done,
        bytes_total,
        None,
        None,
    );

    // Task 13c L4a. Phase 1 (group order, sequential): resolve every group's jobs and name its flat-only
    // lights. Phase 2: when ONE queue over every group's jobs does not lower any group's admission
    // (`merged_admission`), the jobs of all groups run through it, longest first; otherwise each group runs
    // its own queue as before. Phase 3 (group order): commit. Scheduling only — the files, artifact rows and
    // their commit order (group, then frame) are the per-group loop's.
    let mut plans: Vec<CalibrateGroupPlan> = Vec::new();
    // Phase 1 runs every group before any of them commits, so its entries in `rc.warnings` and
    // `rc.runtime_exclusions` would land ahead of the earlier groups' commit warnings. Each group's are set aside
    // here and replayed at that group's phase 3, which restores the per-group loop's order (Task 13c L4 review M2).
    let (warn_mark, excl_mark) = (rc.warnings.len(), rc.runtime_exclusions.len());
    let mut stash: Vec<GroupListStash> = Vec::with_capacity(groups.len());
    let mut replayed = 0usize;
    // A cancel or an error anywhere below replays every stash still held before it returns (Task 13c final review A, M1).
    let phase1: Result<(), RunError> = (|| {
        for (group_idx, group) in groups.iter().enumerate() {
            rc.check_cancel()?;
            if group_idx > 0 {
                stash.push(take_list_stash(rc, warn_mark, excl_mark));
            }
            std::fs::create_dir_all(rc.layout.calibrated_dir())
                .map_err(|e| RunError::Other(format!("failed to create calibrated dir: {e}")))?;

            // Owner decision 2026-09-10 (same rule `stage_measure` uses): a
            // group's members can carry different native geometry (M4b — a
            // second camera, a bin-2 member), so admission sizing below budgets
            // for the LARGEST frame in the group rather than a single
            // group-wide width/height.
            let group_max_w = group
                .frames
                .iter()
                .map(|f| f.width.max(0) as u64)
                .max()
                .unwrap_or(0);
            let group_max_h = group
                .frames
                .iter()
                .map(|f| f.height.max(0) as u64)
                .max()
                .unwrap_or(0);

            let mut jobs: Vec<CalibrateJob> = Vec::new();
            for frame in &group.frames {
                if excluded_set.contains(&frame.frame_id) {
                    continue;
                }
                rc.check_cancel()?;

                match resolve_calibrate_job(rc, &cfg, group, frame, &scratch)? {
                    CalibrateResolved::Reused { bytes } => {
                        bytes_done += bytes;
                        rc.cached_calibrated.insert(frame.frame_id, true);
                        current += 1;
                        rc.progress(
                            Stage::Calibrate,
                            Some(group.key.clone()),
                            current,
                            total,
                            bytes_done,
                            bytes_total,
                            Some(frame.frame_id),
                            None,
                        );
                    }
                    CalibrateResolved::Excluded { reason } => {
                        tracing::warn!(
                            run_id = rc.run_id,
                            frame_id = frame.frame_id,
                            error = %reason,
                            "calibration failed; frame excluded"
                        );
                        rc.runtime_exclusions.push((frame.frame_id, reason));
                        current += 1;
                        // Task 4 fix round: the same progress tick `Reused`
                        // emits above — an all-`Excluded` group used to emit
                        // nothing here and leave the bar short of `total`.
                        rc.progress(
                            Stage::Calibrate,
                            Some(group.key.clone()),
                            current,
                            total,
                            bytes_done,
                            bytes_total,
                            Some(frame.frame_id),
                            None,
                        );
                    }
                    CalibrateResolved::Job(job) => {
                        jobs.push(*job);
                    }
                }
            }
            // Spec 2026-10-04 §5.1: evaluated here, per group, before the
            // cached-group `continue`, so a run whose stage 1 is fully cached
            // still names its flat-only lights.
            let visited: Vec<i64> = group
                .frames
                .iter()
                .map(|f| f.frame_id)
                .filter(|id| !excluded_set.contains(id))
                .filter(|id| !rc.runtime_exclusions.iter().any(|(e, _)| e == id))
                .collect();
            warn_flat_only_group(rc, &cfg, group, &visited, &scratch);
            if jobs.is_empty() {
                continue;
            }
            let group_debayers = jobs.iter().any(|j| j.spec.debayer);
            let working_set_bytes = calibrate_working_set_bytes(
                group_max_w,
                group_max_h,
                group.color_mode,
                group_debayers,
            );
            // The planes this group will preload, counted from its jobs (the preload itself runs later and can only
            // hold fewer, when a decode fails): the cross-group decision needs them before any master is decoded.
            let shared_bytes_est =
                distinct_preload_paths(&jobs).len() as u64 * group_max_w * group_max_h * 4;
            plans.push(CalibrateGroupPlan {
                group_idx,
                jobs,
                plane_px: (group_max_w, group_max_h),
                working_set_bytes,
                shared_bytes_est,
            });
        }
        Ok(())
    })();
    if let Err(e) = phase1 {
        stash.push(take_list_stash(rc, warn_mark, excl_mark));
        replay_list_stash(rc, &mut stash, &mut replayed, usize::MAX);
        return Err(e);
    }
    if !groups.is_empty() {
        stash.push(take_list_stash(rc, warn_mark, excl_mark));
    }

    let phase23: Result<(), RunError> = (|| {
        let pool_threads = rc.ctx.image_pool.current_num_threads();
        // The stage-start read feeds the merge decision only; each fan-out below re-reads the live budget.
        let budget = fan_budget(rc);
        let loads: Vec<GroupLoad> = plans
            .iter()
            .map(|p| GroupLoad {
                working_set_bytes: p.working_set_bytes,
                shared_bytes: p.shared_bytes_est,
            })
            .collect();
        let decision = merged_admission(&loads, pool_threads, budget.bytes);
        let merge = plans.len() >= 2 && force_merge.unwrap_or(decision.merge);
        if plans.len() >= 2 {
            tracing::debug!(
                run_id = rc.run_id,
                stage = Stage::Calibrate.as_str(),
                count = plans.len(),
                admission = decision.merged,
                admission_per_group_min = decision.per_group_min,
                admission_per_group_max = decision.per_group_max,
                merged = merge,
                "cross-group queue decision"
            );
        }

        if merge {
            // Every group's masters are resident at once (`shared_sum`), hot maps and scratch are built per group
            // and never concurrently.
            let mut preloads: Vec<PreloadedMasters> = Vec::with_capacity(plans.len());
            // What the earlier groups' preloads hold while this one decodes (M4).
            let mut resident_bytes = 0u64;
            for plan in &plans {
                rc.check_cancel()?;
                let key = groups[plan.group_idx].key.as_str();
                let warn_before = rc.warnings.len();
                prebuild_hot_maps(rc, &plan.jobs, &scratch, key, plan.plane_px, resident_bytes);
                stash[plan.group_idx]
                    .warnings
                    .extend(rc.warnings.split_off(warn_before));
                let pre = build_preloaded_masters(
                    rc,
                    &plan.jobs,
                    &scratch,
                    key,
                    plan.plane_px,
                    resident_bytes,
                )?;
                resident_bytes += pre.planes.len() as u64 * plan.plane_px.0 * plan.plane_px.1 * 4;
                preloads.push(pre);
            }
            let shared_sum: u64 = plans
                .iter()
                .zip(&preloads)
                .map(|(p, pre)| pre.planes.len() as u64 * p.plane_px.0 * p.plane_px.1 * 4)
                .sum();
            let ws_max = plans.iter().map(|p| p.working_set_bytes).max().unwrap_or(0);
            let budget = fan_budget(rc);
            let admission_n = admission_with_shared(shared_sum, ws_max, pool_threads, budget.bytes);
            log_admission(rc, Stage::Calibrate, "", ws_max, shared_sum, admission_n, budget, "calibrate");
            // One ticker per group, all counting into one stage-wide counter, so `current` never runs backwards
            // however the groups' items interleave.
            let mut tickers: Vec<FanOutTicker> = Vec::with_capacity(plans.len());
            for plan in &plans {
                let t = FanOutTicker::new(
                    rc,
                    Stage::Calibrate,
                    Some(groups[plan.group_idx].key.clone()),
                    current,
                    total,
                    bytes_done,
                    bytes_total,
                );
                tickers.push(match tickers.first() {
                    Some(first) => t.sharing_state(first),
                    None => t,
                });
            }
            let results = calibrate_queue(
                rc,
                &plans.iter().collect::<Vec<_>>(),
                &preloads.iter().collect::<Vec<_>>(),
                &tickers.iter().collect::<Vec<_>>(),
                &scratch,
                &AdmissionGate::budgeted(rc, ws_max, shared_sum, admission_n),
                FanOutLabel::new(rc, Stage::Calibrate, None, "calibrate"),
            );
            current = tickers.first().map_or(current, FanOutTicker::done);
            // Per group, in group order: a group's completed results commit (frame order) before a cancel or an
            // error in a LATER group is honoured, as the per-group loop did.
            for (plan, res) in plans.iter().zip(results) {
                replay_list_stash(rc, &mut stash, &mut replayed, plan.group_idx);
                commit_calibrate_group(rc, &groups[plan.group_idx].key, &plan.jobs, res, &mut bytes_done)?;
                rc.progress(
                    Stage::Calibrate,
                    Some(groups[plan.group_idx].key.clone()),
                    current,
                    total,
                    bytes_done,
                    bytes_total,
                    None,
                    None,
                );
            }
            rc.check_cancel()?;
        } else {
            for plan in &plans {
                rc.check_cancel()?;
                let group = &groups[plan.group_idx];
                replay_list_stash(rc, &mut stash, &mut replayed, plan.group_idx);
                prebuild_hot_maps(rc, &plan.jobs, &scratch, &group.key, plan.plane_px, 0);
                // Perf tier 1 Task 5: decode this group's shared master subtrahend
                // (dark, or bias when there is no dark — mirroring the engine's own
                // rule, fix round 1 Important finding 1) and flat once, on the run
                // thread, before the fan-out — every worker's `execute_generation`
                // call below then reads them from RAM instead of re-opening and
                // re-band-reading them per frame.
                let preloaded =
                    build_preloaded_masters(rc, &plan.jobs, &scratch, &group.key, plan.plane_px, 0)?;

                // Perf tier 1 Task 5 fix round 1, Important finding 2: the preloaded
                // planes are resident ONCE per group, not once per worker, so they
                // are charged separately from the per-worker working set —
                // `shared_bytes` is the REAL count of planes this group actually
                // preloaded: one subtrahend (dark or bias, never both) and one flat
                // PER DISTINCT MASTER the group's jobs resolve to, not a hard-coded
                // 2, so a group missing a link (or a preload failure) doesn't
                // over-charge the budget, and native-mode grouping's several
                // distinct masters per group don't under-charge it either.
                // `admission_with_shared` subtracts this from the RAM budget BEFORE
                // dividing by the per-worker working set.
                let shared_bytes = preloaded.planes.len() as u64 * plan.plane_px.0 * plan.plane_px.1 * 4;
                // The live budget, read right before this group's fan-out (Task 13c L4 review I2).
                let budget = fan_budget(rc);
                let admission_n = admission_with_shared(
                    shared_bytes,
                    plan.working_set_bytes,
                    pool_threads,
                    budget.bytes,
                );
                log_admission(
                    rc,
                    Stage::Calibrate,
                    &group.key,
                    plan.working_set_bytes,
                    shared_bytes,
                    admission_n,
                    budget,
                    "calibrate",
                );
                let ticker = FanOutTicker::new(
                    rc,
                    Stage::Calibrate,
                    Some(group.key.clone()),
                    current,
                    total,
                    bytes_done,
                    bytes_total,
                );
                let mut results = calibrate_queue(
                    rc,
                    &[plan],
                    &[&preloaded],
                    &[&ticker],
                    &scratch,
                    &AdmissionGate::budgeted(rc, plan.working_set_bytes, shared_bytes, admission_n),
                    FanOutLabel::new(rc, Stage::Calibrate, Some(&group.key), "calibrate"),
                );

                rc.check_cancel()?;
                current = ticker.done();
                commit_calibrate_group(
                    rc,
                    &group.key,
                    &plan.jobs,
                    results.pop().unwrap_or_default(),
                    &mut bytes_done,
                )?;
                rc.progress(
                    Stage::Calibrate,
                    Some(group.key.clone()),
                    current,
                    total,
                    bytes_done,
                    bytes_total,
                    None,
                    None,
                );
            }
        }
        Ok(())
    })();
    if let Err(e) = phase23 {
        replay_list_stash(rc, &mut stash, &mut replayed, usize::MAX);
        return Err(e);
    }

    // Groups with nothing to calibrate (fully cached, or all excluded) after the last plan.
    replay_list_stash(rc, &mut stash, &mut replayed, groups.len().saturating_sub(1));
    meter.finish();
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Calibrate,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

/// The calibrate planes (mono [`CALIBRATE_PLANES_RESIDENT_MONO`], debayered
/// OSC [`CALIBRATE_PLANES_RESIDENT_OSC`]) of one w×h channel, in bytes — ONE
/// worker's own Calibrate fan-out footprint (perf tier 1 Task 5 fix round
/// 1, Important finding 2; per colour mode since Task 13c L1). `debayer` is
/// whether this group's frames are actually debayered; a mono group (or an
/// OSC group kept as mosaics) never holds the 3-plane output. The group's
/// shared preloaded master planes are NOT folded in here — they are
/// resident once per group, not once per worker, so they are charged
/// through `admission_with_shared`'s `shared_bytes` instead.
pub(crate) fn calibrate_working_set_bytes(
    w: u64,
    h: u64,
    colour_mode: ColorMode,
    debayer: bool,
) -> u64 {
    let planes = if colour_mode == ColorMode::Osc && debayer {
        CALIBRATE_PLANES_RESIDENT_OSC
    } else {
        CALIBRATE_PLANES_RESIDENT_MONO
    };
    planes * w * h * 4
}

#[cfg(test)]
#[path = "calibrate_tests.rs"]
mod tests;
