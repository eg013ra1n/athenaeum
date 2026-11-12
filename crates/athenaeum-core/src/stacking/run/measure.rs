//! Stage 3, measure and select (`stage_measure`).

use super::*;

/// Every `fits` artifact path whose write DEGRADED (final fix wave, ruling
/// C-30, the review's I1) — pushed immediately beside the
/// `"fits artifact write failed; consumers will fall back"` warn that is
/// the production signal, so a test can assert the degrade happened at all
/// AND that it happened exactly once.
///
/// A test-only recorder rather than a captured log line, for the reason
/// `stacking::register::frame::DETECT_FRAME_STARS_LOG` spells out at
/// length: `stacking::run`'s stages execute on the run's own
/// `stacking-run-<id>` thread (and their fan-outs on scoped worker
/// threads), none of which inherits a `set_default`-scoped subscriber from
/// the test thread — only a `set_global_default` would reach them, and that
/// can be installed at most once per test binary. Logging the PATH (not a
/// bare counter) is what lets a test filter the entries down to its own
/// working tempdir, immune to whatever sibling tests are measuring frames
/// at the same moment under `cargo test`'s default parallelism.
#[cfg(test)]
pub(crate) static FITS_WRITE_FAILURE_LOG: std::sync::Mutex<Vec<PathBuf>> =
    std::sync::Mutex::new(Vec::new());

/// Planes of ONE channel resident while a frame is measured (perf tier 1
/// Task 3, measured 2026-09-19 with `measure_probe` under `/usr/bin/time
/// -l` on the real OSC calibrated frame `c_2025-09-14_00-55-28__-10.00_
/// 180.00s_0000_d.fits` (6248×4176, 3 planes, 313 MB) —
/// `cargo build --release --example measure_probe -p athenaeum-core` then
/// `/usr/bin/time -l ./target/release/examples/measure_probe <file>`, run
/// twice: peak RSS ("maximum resident set size") 742,899,712 B and
/// 741,212,160 B, larger kept; baseline (`measure_probe` with no args, its
/// own usage-and-exit path) 6,176,768 B, measured twice, stable, and
/// subtracted first. `(742,899,712 − 6,176,768) / (6248 × 4176 × 4) =
/// 7.06`, rounded up: 8. Higher than the audit's 4–6 guess but at the
/// task's own stop threshold ("more than 8" blocks; 8 does not), and
/// plausible against `measure_plane_with_seeds`: the plane read from disk,
/// its ADU-scaled copy, `background_residual`'s own scratch, `noise_mrs`'s
/// à-trous layers and the detector's copy of the (possibly pre-filtered)
/// plane all coexist. `measure_frame_with_seeds` reads and measures one
/// plane at a time, so the frame's own channel count (3 for this OSC
/// frame) does not multiply in.
///
/// Re-measured (perf tier A Task 3, 2026-09-19) after the detect.rs copy-
/// chain change (`luminance` borrows a single plane, `detect_stars` fuses
/// its max-fold into the ADU-scale pass) — **unchanged, 8, on both a mono
/// and an OSC frame.** `measure_plane_with_seeds`/`measure_frame_with_seeds`
/// never call `register::detect::{luminance, detect_stars}` at all (Measure
/// seeds through `ImageAnalyzer::detect_fast_data` directly), so this
/// constant was never going to move from that change; the re-measurement
/// exists to confirm it and to localize the peak with per-sub-stage
/// `getrusage` checkpoints the way Task 2 (tier 1) asked Task 3 to.
/// Frames: OSC `c_2025-10-18_22-53-18__-10.00_180.00s_0144_d.fits`
/// (6248×4176, 3 planes), mono `c_2025-10-18_22-18-12__-10.00_180.00s_
/// 0058.fits` (6224×4168, 1 plane), both from the LDN1272-test acceptance
/// catalog. `/usr/bin/time -l` on `measure_probe`, 3 runs per frame, larger
/// kept; baseline (no-arg usage-and-exit) 6,406,144 B, stable across 2
/// runs. OSC peak 741,392,384 B: `(741,392,384 − 6,406,144) / (6248 × 4176
/// × 4) = 7.04`, rounds up to 8 — matches the original measurement almost
/// exactly. Mono peak 708,378,624 B: `(708,378,624 − 6,406,144) / (6224 ×
/// 4168 × 4) = 6.77`, rounds up to 7 — the max across both measured frames
/// is kept (8), the same conservative rule the original measurement used,
/// since under-provisioning risks the swap-thrash regression
/// `LN_PLANES_RESIDENT`'s own comment (`normalize.rs`) describes.
///
/// Per-sub-stage `getrusage(RUSAGE_SELF).ru_maxrss` checkpoints (a
/// temporary, uncommitted instrumentation of `measure_plane_with_seeds` and
/// its caller's per-plane loop — `ru_maxrss` is a HIGH-WATER MARK, so every
/// reading is the max reached so far in the whole process, never a
/// snapshot of the currently-live set) found the SAME pattern on both
/// frames, dominated by plane 1 of the run (subsequent planes barely move
/// the mark, confirming the "does not multiply in" claim above): reading
/// one plane sets the mark to ≈ 208 MB (the decode scratch buffer and the
/// decoded `Vec<f32>` transiently coexist inside `PlaneReader::read_plane`,
/// each ≈ one plane); the ADU-scale copy (`scaled`) adds nothing NEW to
/// the mark (the allocator reuses the read scratch's just-freed space);
/// `background_residual` (`astroimage::analysis::background::
/// estimate_background_mesh` plus this crate's own residual pass) pushes it
/// up by ≈ 113 MB; **`noise_mrs` (rustafits' `estimate_noise_mrs`, 4 à-trous
/// layers: `smoothed`, `w1` — held live to the end for the final
/// re-estimation loop — then a `prev_smooth`/`next_smooth` pair per layer)
/// is the single largest jump, ≈ 345 MB**, dwarfing every other stage
/// combined and setting the mark to within ~5 % of the run's eventual
/// peak; detection then adds next to nothing (≈ 0.3 MB — the D5 rustafits
/// change landed exactly here, `run_fast_detection`'s single-channel
/// `Cow::Borrowed` no longer copying `scaled` a second time); the PSF fit
/// adds ≈ 3 MB; the tail (`stratified_sample`/`clip_sample`/
/// `location_scale`/`noise_scale_factors`, all far-sub-plane-sized samples)
/// still shows a further ≈ 33 MB of watermark creep, attributed to
/// allocator fragmentation (freed pages not exactly reused by a
/// differently-sized next allocation) rather than a live buffer, since
/// nothing in that tail is plane-sized. **This explains Task 2's own
/// finding that D4 (skip the dead noise map in `background_residual`) left
/// the overall peak flat**: `noise_mrs` runs AFTER `background_residual`
/// and sets a higher mark regardless of whether `background_residual`'s own
/// noise-map buffer existed, so D4's ~104 MB saving there is masked by a
/// downstream allocation nothing in Tier A touches. `noise_mrs`'s three
/// buffers are entirely inside the `rustafits` submodule (off limits to
/// this task — pinned at `872ae0f` on `perf/stacking-kernels`), and every
/// buffer under this crate's own control (`scaled`, and the caller's
/// `data`) is genuinely read at both the start AND the end of
/// `measure_plane_with_seeds` (`data` again at the final
/// `stratified_sample` call), so neither can be scope-narrowed without
/// either reordering a read (harmless) or restructuring the read/scale
/// split across the `measure_frame_with_seeds`/`measure_plane_with_seeds`
/// boundary (a materially bigger change than a bit-identical drop). No
/// dead-at-peak buffer was found on the core side; the constant stays 8.
pub(crate) const MEASURE_PLANES_RESIDENT: u64 = 8;

/// `MEASURE_PLANES_RESIDENT` planes of one w×h channel, in bytes — Measure's
/// fan-out working set (perf tier 1 Task 3).
pub(crate) fn measure_working_set_bytes(w: u64, h: u64) -> u64 {
    MEASURE_PLANES_RESIDENT * w * h * 4
}

/// Stage 3 (measure & select, spec §4.1-4.3): per group, measure every
/// frame that reached calibration (manually- and stage-1-excluded frames
/// never do — Task 6's calibrate never wrote them a file), reusing a fresh
/// `metrics` artifact when one exists, then compute weights
/// (`cfg.measurement.weight_mode`) and apply the selection filters
/// (`cfg.selection`). Builds `rc.measured` — one [`MeasuredFrame`] per
/// group frame, included or not. A group with fewer than 3 included frames
/// is marked `status = "skipped"` and excluded from later stages; a run
/// with no viable group fails outright.
pub(super) fn stage_measure(rc: &mut RunContext) -> Result<(), RunError> {
    stage_measure_with(rc, None)
}

/// [`stage_measure`] with the cross-group pass-1 queue decision overridable (`Some(true)` = always one queue, `Some(false)` =
/// per-group queues): the tests compare the two shapes.
fn stage_measure_with(rc: &mut RunContext, force_merge: Option<bool>) -> Result<(), RunError> {
    let stage_start = Instant::now();
    let meter = crate::stacking::telemetry::StageMeter::start(rc, Stage::Measure, None);
    // Plan 6b-2 C14: a project's frames without a manifest WCS are solved
    // first; the Solve boundary's exclusions reach `stage1_failed` below.
    solve_project_frames(rc)?;
    let cfg = rc.config.clone();
    let opts = cfg
        .measurement
        .measure_options(cfg.normalization.scale_estimator);
    let groups = rc.plan_groups.clone();
    let excluded_set: HashSet<i64> = rc.excluded.iter().copied().collect();
    let stage1_failed: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Measure);

    let total: usize = groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .filter(|f| !excluded_set.contains(&f.frame_id) && !stage1_failed.contains_key(&f.frame_id))
        .count();

    let mut current = 0usize;
    rc.progress(Stage::Measure, None, current, total, 0, 0, None, None);

    let mut any_group_viable = false;

    let auto = matches!(cfg.measurement.psf_model, psf_signal::PsfModel::Auto);
    if !auto {
        for group in &groups {
            rc.check_cancel()?;
            let (entries, to_measure, gw, gh) =
                measure_group_prepare(rc, group, &excluded_set, &stage1_failed)?;
            measure_group_finish(
                rc,
                group,
                &cfg,
                &opts,
                entries,
                to_measure,
                None,
                force_fresh,
                (gw, gh),
                &excluded_set,
                &stage1_failed,
                &mut current,
                total,
                0,
                &mut any_group_viable,
            )?;
        }
    } else {
        // Plan A Task 11b (owner decision R11b-12): under `Auto` every member is fitted at the group beta in two
        // passes. Task 13c L4c: phase 1 per group in group order (everything up to the pass-1 item list), phase 2
        // ONE pass-1 queue over every group's items when the merged admission lowers no group's (else each group's
        // own queue, run in phase 3), phase 3 per group in group order (group beta, pass 2 and the rest: the beta
        // barrier stays per group).
        struct AutoGroup {
            entries: Vec<MeasuredFrame>,
            state: TwoPassState,
            max_wh: (u64, u64),
            /// What phase 1 pushed to `rc.warnings` / `rc.runtime_exclusions` for this group: replayed at the
            /// group's phase 3, so the run's lists keep the order the per-group loop produced.
            warnings: Vec<String>,
            exclusions: Vec<(i64, String)>,
        }
        let mut prepared: Vec<AutoGroup> = Vec::with_capacity(groups.len());
        // The marks of the group being prepared: an error or cancel mid phase 1 puts the stashed lists back, in
        // group order, ahead of that group's partial entries (Task 13c final review A, M1).
        let mut marks = (rc.warnings.len(), rc.runtime_exclusions.len());
        let phase1: Result<(), RunError> = (|| {
            for group in &groups {
                rc.check_cancel()?;
                marks = (rc.warnings.len(), rc.runtime_exclusions.len());
                let (warn_mark, excl_mark) = marks;
                let (entries, to_measure, gw, gh) =
                    measure_group_prepare(rc, group, &excluded_set, &stage1_failed)?;
                let state =
                    measure_two_pass_prepare(rc, group, &cfg, to_measure, force_fresh, (gw, gh))?;
                prepared.push(AutoGroup {
                    entries,
                    state,
                    max_wh: (gw, gh),
                    warnings: rc.warnings.split_off(warn_mark),
                    exclusions: rc.runtime_exclusions.split_off(excl_mark),
                });
            }
            Ok(())
        })();
        if let Err(e) = phase1 {
            let partial = (
                rc.warnings.split_off(marks.0),
                rc.runtime_exclusions.split_off(marks.1),
            );
            replay_measure_stash(rc, prepared.into_iter().map(|g| (g.warnings, g.exclusions)));
            rc.warnings.extend(partial.0);
            rc.runtime_exclusions.extend(partial.1);
            return Err(e);
        }
        let p1_counts: Vec<usize> = prepared.iter().map(|g| g.state.need_pass1.len()).collect();
        let mut merged_results: Vec<Option<Vec<Option<Result<Pass1Done, String>>>>> =
            (0..groups.len()).map(|_| None).collect();
        let p1_groups: Vec<usize> = (0..groups.len()).filter(|&g| p1_counts[g] > 0).collect();
        let pool_threads = rc.ctx.image_pool.current_num_threads();
        let budget = fan_budget(rc);
        let loads: Vec<GroupLoad> = p1_groups
            .iter()
            .map(|&g| GroupLoad {
                working_set_bytes: prepared[g].state.working_set_bytes,
                shared_bytes: 0,
            })
            .collect();
        let decision = merged_admission(&loads, pool_threads, budget.bytes);
        let merge = p1_groups.len() >= 2 && force_merge.unwrap_or(decision.merge);
        if p1_groups.len() >= 2 {
            tracing::debug!(
                run_id = rc.run_id,
                stage = Stage::Measure.as_str(),
                count = p1_groups.len(),
                admission = decision.merged,
                admission_per_group_min = decision.per_group_min,
                admission_per_group_max = decision.per_group_max,
                merged = merge,
                "cross-group queue decision"
            );
        }
        if merge {
            let working_sets: Vec<u64> = loads.iter().map(|l| l.working_set_bytes).collect();
            let ws_max = working_sets.iter().copied().max().unwrap_or(0);
            let admission_n = admission(ws_max, pool_threads, budget.bytes);
            log_admission(rc, Stage::Measure, "", ws_max, 0, admission_n, budget, "pass1");
            // One ticker per group, all counting into one stage-wide counter (half-frames, scale 2).
            let mut tickers: Vec<FanOutTicker> = Vec::with_capacity(p1_groups.len());
            for &g in &p1_groups {
                let t = FanOutTicker::new(
                    rc,
                    Stage::Measure,
                    Some(groups[g].key.clone()),
                    current * 2,
                    total,
                    0,
                    0,
                )
                .with_scale(2);
                tickers.push(match tickers.first() {
                    Some(first) => t.sharing_state(first),
                    None => t,
                });
            }
            let jobs: Vec<Vec<(i64, PathBuf, PathBuf)>> = p1_groups
                .iter()
                .map(|&g| pass1_jobs(&prepared[g].state))
                .collect();
            let cancel_ref: &AtomicBool = &rc.cancel;
            let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
            let tickers_ref = &tickers;
            let opts_ref = &opts;
            let results = fan_out_groups(
                jobs,
                &working_sets,
                &AdmissionGate::budgeted(rc, ws_max, 0, admission_n),
                FanOutLabel::new(rc, Stage::Measure, None, "pass1"),
                cancel_ref,
                move |k, job| run_pass1_item(job, opts_ref, pool_ref, cancel_ref, &tickers_ref[k]),
            );
            // Every file written gets its row BEFORE a cancel is honoured (R11c-5 (1)), group by group.
            for (&g, res) in p1_groups.iter().zip(&results) {
                if let Err(e) = upsert_pass1_rows(rc, &prepared[g].state, res) {
                    replay_measure_stash(rc, prepared.into_iter().map(|g| (g.warnings, g.exclusions)));
                    return Err(e);
                }
            }
            for (&g, res) in p1_groups.iter().zip(results) {
                merged_results[g] = Some(res);
            }
        }
        // A cancel or an error in the shared tail replays every stash still held before it returns (M1).
        let mut rest = groups.iter().zip(prepared).enumerate();
        let phase3: Result<(), RunError> = (|| {
            while let Some((g, (group, pg))) = rest.next() {
                let AutoGroup {
                    mut entries,
                    mut state,
                    max_wh,
                    warnings,
                    exclusions,
                } = pg;
                rc.warnings.extend(warnings);
                rc.runtime_exclusions.extend(exclusions);
                rc.check_cancel()?;
                let pass1 = match merged_results[g].take() {
                    Some(res) => Some(res),
                    None if state.need_pass1.is_empty() => None,
                    None => {
                        let res = measure_pass1_queue(rc, group, &opts, &state, current, total);
                        upsert_pass1_rows(rc, &state, &res)?;
                        Some(res)
                    }
                };
                let pass1_failed = match pass1 {
                    Some(res) => apply_pass1_results(rc, &mut entries, &mut state, res)?,
                    None => 0,
                };
                // The stage counter holds the pass-1 half-ticks of every group still to finish on the cross-group
                // queue; on a per-group queue, only this group's.
                let pass1_base = if merge {
                    p1_counts[g..].iter().sum()
                } else {
                    p1_counts[g]
                };
                let auto_group_beta = measure_two_pass_rest(
                    rc,
                    group,
                    &cfg,
                    &opts,
                    &mut entries,
                    state,
                    pass1_failed,
                    (current, total),
                    pass1_base,
                    merge,
                )?;
                measure_group_finish(
                    rc,
                    group,
                    &cfg,
                    &opts,
                    entries,
                    Vec::new(),
                    auto_group_beta,
                    force_fresh,
                    max_wh,
                    &excluded_set,
                    &stage1_failed,
                    &mut current,
                    total,
                    if merge { p1_counts[g + 1..].iter().sum() } else { 0 },
                    &mut any_group_viable,
                )?;
            }
            Ok(())
        })();
        if let Err(e) = phase3 {
            replay_measure_stash(rc, rest.map(|(_, (_, g))| (g.warnings, g.exclusions)));
            return Err(e);
        }
    }

    if !any_group_viable {
        return Err(RunError::Other(
            "no group has 3 included frames".to_string(),
        ));
    }

    meter.finish();
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Measure,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

/// Put the phase-1 lists of the groups a failed Measure never reached back onto the run's lists, in group order
/// (Task 13c final review A, M1): a cancel or an error must not drop a warning already raised.
fn replay_measure_stash(
    rc: &mut RunContext,
    stashes: impl IntoIterator<Item = (Vec<String>, Vec<(i64, String)>)>,
) {
    for (warnings, exclusions) in stashes {
        rc.warnings.extend(warnings);
        rc.runtime_exclusions.extend(exclusions);
    }
}

/// Everything `stage_measure` does for one group before its measurement: the group's frames as [`MeasuredFrame`]s
/// (manual and stage-1 exclusions, the calibrated artifact, its planes) and the list of frames to measure. Returns
/// `(entries, to_measure, group_max_w, group_max_h)`.
#[allow(clippy::type_complexity)]
fn measure_group_prepare(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    excluded_set: &HashSet<i64>,
    stage1_failed: &HashMap<i64, String>,
) -> Result<(Vec<MeasuredFrame>, Vec<(usize, GroupFrame, PathBuf)>, u64, u64), RunError> {
    let mut entries: Vec<MeasuredFrame> = Vec::with_capacity(group.frames.len());
    let mut to_measure: Vec<(usize, GroupFrame, PathBuf)> = Vec::new();
    // Owner decision 2026-09-10: a group's members can carry different
    // native geometry now, so admission sizing below uses the LARGEST
    // frame in the group rather than a (now nonexistent) single
    // group-wide width/height — conservative, never under-admits.
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

    for frame in &group.frames {
        if excluded_set.contains(&frame.frame_id) {
            entries.push(MeasuredFrame {
                frame: frame.clone(),
                calibrated: None,
                planes: 0,
                measurement: None,
                weight: None,
                included: false,
                reason: Some("excluded manually".to_string()),
                registration: None,
                cached_calibrated: false,
                cached_metrics: false,
                ln_scale: None,
                cached_ln: false,
                ln_growth_factor: None,
                ln_growth_pairs: None,
                ln_aperture_correction: None,
                measure_floored: false,
            });
            continue;
        }
        if let Some(reason) = stage1_failed.get(&frame.frame_id) {
            entries.push(MeasuredFrame {
                frame: frame.clone(),
                calibrated: None,
                planes: 0,
                measurement: None,
                weight: None,
                included: false,
                reason: Some(reason.clone()),
                registration: None,
                cached_calibrated: false,
                cached_metrics: false,
                ln_scale: None,
                cached_ln: false,
                ln_growth_factor: None,
                ln_growth_pairs: None,
                ln_aperture_correction: None,
                measure_floored: false,
            });
            continue;
        }

        let calibrated_path: Option<PathBuf> = {
            let conn = db(&rc.ctx)?.conn();
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group.key,
                ArtifactKind::Calibrated,
                Some(frame.frame_id),
            )?
            .and_then(|a| a.path)
            .map(PathBuf::from)
        };
        let Some(calibrated_path) = calibrated_path else {
            let reason = "measurement failed: no calibrated artifact on record".to_string();
            rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
            entries.push(MeasuredFrame {
                frame: frame.clone(),
                calibrated: None,
                planes: 0,
                measurement: None,
                weight: None,
                included: false,
                reason: Some(reason),
                registration: None,
                cached_calibrated: false,
                cached_metrics: false,
                ln_scale: None,
                cached_ln: false,
                ln_growth_factor: None,
                ln_growth_pairs: None,
                ln_aperture_correction: None,
                measure_floored: false,
            });
            continue;
        };

        // Plan 6b-2 C9: a project frame whose landed file changed since
        // the frame source read it leaves the run here.
        if let Some(reason) = changed_during_run(rc, frame.frame_id)? {
            tracing::warn!(run_id = rc.run_id, frame_id = frame.frame_id, reason = %reason, "frame excluded");
            rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
            entries.push(MeasuredFrame {
                frame: frame.clone(),
                calibrated: None,
                planes: 0,
                measurement: None,
                weight: None,
                included: false,
                reason: Some(reason),
                registration: None,
                cached_calibrated: false,
                cached_metrics: false,
                ln_scale: None,
                cached_ln: false,
                ln_growth_factor: None,
                ln_growth_pairs: None,
                ln_aperture_correction: None,
                measure_floored: false,
            });
            continue;
        }

        let planes = match PlaneReader::open(&calibrated_path) {
            Ok(r) => Ok(r.channels()),
            Err(e) => Err(format!("calibrated frame unreadable: {e}")),
        };
        let planes = match planes {
            Ok(p) if p > 0 => p,
            Ok(_) => {
                let reason = "calibrated frame has no planes".to_string();
                rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
                let cached_calibrated = rc
                    .cached_calibrated
                    .get(&frame.frame_id)
                    .copied()
                    .unwrap_or(false);
                entries.push(MeasuredFrame {
                    frame: frame.clone(),
                    calibrated: Some(calibrated_path),
                    planes: 0,
                    measurement: None,
                    weight: None,
                    included: false,
                    reason: Some(reason),
                    registration: None,
                    cached_calibrated,
                    cached_metrics: false,
                    ln_scale: None,
                    cached_ln: false,
                    ln_growth_factor: None,
                    ln_growth_pairs: None,
                    ln_aperture_correction: None,
                    measure_floored: false,
                });
                continue;
            }
            Err(reason) => {
                rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
                let cached_calibrated = rc
                    .cached_calibrated
                    .get(&frame.frame_id)
                    .copied()
                    .unwrap_or(false);
                entries.push(MeasuredFrame {
                    frame: frame.clone(),
                    calibrated: Some(calibrated_path),
                    planes: 0,
                    measurement: None,
                    weight: None,
                    included: false,
                    reason: Some(reason),
                    registration: None,
                    cached_calibrated,
                    cached_metrics: false,
                    ln_scale: None,
                    cached_ln: false,
                    ln_growth_factor: None,
                    ln_growth_pairs: None,
                    ln_aperture_correction: None,
                    measure_floored: false,
                });
                continue;
            }
        };

        let cached_calibrated = rc
            .cached_calibrated
            .get(&frame.frame_id)
            .copied()
            .unwrap_or(false);
        entries.push(MeasuredFrame {
            frame: frame.clone(),
            calibrated: Some(calibrated_path.clone()),
            planes,
            measurement: None,
            weight: None,
            included: false,
            reason: None,
            registration: None,
            cached_calibrated,
            cached_metrics: false,
            ln_scale: None,
            cached_ln: false,
            ln_growth_factor: None,
            ln_growth_pairs: None,
            ln_aperture_correction: None,
            measure_floored: false,
        });
        to_measure.push((entries.len() - 1, frame.clone(), calibrated_path));
    }
    Ok((entries, to_measure, group_max_w, group_max_h))
}

/// Everything `stage_measure` does for one group after its pass-1/pass-2 fits: the single-pass measurement of
/// `Moffat4`/`Fixed` (a no-op under `Auto`, whose frames the two passes took), weights, selection, the group β, the
/// `stacking_run_groups` row, `rc.measured` and the stage progress. `auto_group_beta` is the two-pass group β.
#[allow(clippy::too_many_arguments)]
fn measure_group_finish(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    cfg: &StackingConfig,
    opts: &MeasureOptions,
    mut entries: Vec<MeasuredFrame>,
    to_measure: Vec<(usize, GroupFrame, PathBuf)>,
    auto_group_beta: Option<f64>,
    force_fresh: bool,
    (group_max_w, group_max_h): (u64, u64),
    excluded_set: &HashSet<i64>,
    stage1_failed: &HashMap<i64, String>,
    current_ref: &mut usize,
    total: usize,
    ahead_halves: usize,
    any_group_viable: &mut bool,
) -> Result<(), RunError> {
    let mut current = *current_ref;
    let auto = matches!(cfg.measurement.psf_model, psf_signal::PsfModel::Auto);
    // Freshness check (skipped entirely under a Measure-or-earlier
    // `rerun_from`): a `metrics` artifact whose hash matches this
    // frame's CURRENT stage-1 hash is reused straight from
    // `payload_json` — but ONLY when every one of its planes' `fits`
    // rows is ALSO fresh (Tier C Task 1 fix round 1, the same
    // `fits_artifact_fresh` rule `plan.rs`'s own gate uses): a
    // `metrics`-only hit (a `fits` file removed by hand between runs)
    // is re-measured, which rewrites BOTH artifacts for that frame —
    // never a partial "keep the stale metrics, hope Task 2 falls back"
    // outcome. Without this a plain run (no `rerun_from`) could never
    // heal a missing `fits` file on its own; only "Re-run from Measure"
    // would.
    let group_measurable = to_measure.len();
    let mut needing_measure: Vec<(usize, GroupFrame, PathBuf)> = Vec::new();
    for (idx, frame, path) in to_measure {
        let calib_hash = {
            let conn = db(&rc.ctx)?.conn();
            rc.memo.calibration_hash_checked(&conn, &cfg, &frame)?
        };
        let expected_hash = measurement_hash_for(&cfg, &calib_hash, None);

        let mut reused = false;
        if !force_fresh {
            let existing = {
                let conn = db(&rc.ctx)?.conn();
                crate::db::stacking::find_artifact(
                    &conn,
                    &rc.subject,
                    &group.key,
                    ArtifactKind::Metrics,
                    Some(frame.frame_id),
                )?
            };
            if let Some(row) = existing {
                if row.config_hash == expected_hash {
                    if let Some(payload) = &row.payload_json {
                        match serde_json::from_str::<FrameMeasurement>(payload) {
                            Ok(m) => {
                                let fits_fresh = {
                                    let conn = db(&rc.ctx)?.conn();
                                    fits_artifact_fresh(
                                        &conn,
                                        &rc.subject,
                                        frame.frame_id,
                                        Some(payload.as_str()),
                                        &expected_hash,
                                    )?
                                };
                                if fits_fresh {
                                    entries[idx].measurement = Some(m);
                                    entries[idx].cached_metrics = true;
                                    reused = true;
                                } else {
                                    tracing::debug!(
                                        run_id = rc.run_id,
                                        frame_id = frame.frame_id,
                                        "cached metrics found but its fits sidecar is missing \
                                         or stale; re-measuring"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    error = %e,
                                    "stored metrics payload failed to parse; re-measuring"
                                );
                            }
                        }
                    }
                }
            }
        }
        if !reused {
            needing_measure.push((idx, frame, path));
        }
    }

    // Cached frames count as done the moment the group starts (they
    // cost nothing), so the bar shows the reuse straight away.
    let reused_in_group = group_measurable - needing_measure.len();
    if reused_in_group > 0 {
        rc.progress(
            Stage::Measure,
            Some(group.key.clone()),
            current + reused_in_group,
            total,
            0,
            0,
            None,
            None,
        );
    }

    if !needing_measure.is_empty() {
        let working_set_bytes = measure_working_set_bytes(group_max_w, group_max_h);
        let budget = fan_budget(rc);
        let admission_n = admission(
            working_set_bytes,
            rc.ctx.image_pool.current_num_threads(),
            budget.bytes,
        );
        log_admission(
            rc,
            Stage::Measure,
            &group.key,
            working_set_bytes,
            0,
            admission_n,
            budget,
            "measure",
        );
        let meta: Vec<(usize, GroupFrame)> = needing_measure
            .iter()
            .map(|(idx, f, _)| (*idx, f.clone()))
            .collect();
        let items: Vec<(i64, PathBuf)> = needing_measure
            .into_iter()
            .map(|(_, f, p)| (f.frame_id, p))
            .collect();

        let ticker = FanOutTicker::new(
            rc,
            Stage::Measure,
            Some(group.key.clone()),
            current + reused_in_group,
            total,
            0,
            0,
        );
        let cancel_ref: &AtomicBool = &rc.cancel;
        let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
        let ticker_ref = &ticker;
        let gate = AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n);
        let label = FanOutLabel::new(rc, Stage::Measure, Some(&group.key), "measure");
        let results = fan_out(items, &gate, label, cancel_ref, move |(frame_id, path)| {
            let out = measure_frame_with_fits(&path, &opts, Some(pool_ref), cancel_ref)
                .map_err(|e| format!("measurement failed: {e}"));
            ticker_ref.tick(Some(frame_id));
            out
        });

        rc.check_cancel()?;

        for (pos, res) in results.into_iter().enumerate() {
            let (idx, frame) = &meta[pos];
            let idx = *idx;
            match res {
                None => return Err(RunError::Cancelled),
                Some(Err(msg)) => {
                    rc.runtime_exclusions.push((frame.frame_id, msg.clone()));
                    entries[idx].included = false;
                    entries[idx].reason = Some(msg.clone());
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = frame.frame_id,
                        reason = %msg,
                        "frame excluded"
                    );
                }
                Some(Ok((m, fits))) => {
                    let calib_hash = {
                        let conn = db(&rc.ctx)?.conn();
                        rc.memo.calibration_hash_checked(&conn, &cfg, frame)?
                    };
                    let expected_hash = measurement_hash_for(&cfg, &calib_hash, None);
                    persist_measurement(rc, group, frame, &m, &fits, &expected_hash)?;
                    entries[idx].measurement = Some(m);
                }
            }
        }
    }

    // Plan B B0 (PB-1, F-11): the aperture correction `c` each channel of the group resolved, one info per channel after
    // the fan-out, beside the group β line. Over every MEASURED member (fresh or reused), never narrowed to `included`,
    // as the group β is. A `metrics` row reused from before B0 carries no `c` and reads as a plane at floor here; the
    // before-runs measure on fresh work dirs.
    {
        let channels = entries
            .iter()
            .filter_map(|e| e.measurement.as_ref())
            .map(|m| m.channels.len())
            .max()
            .unwrap_or(0);
        for plane in 0..channels {
            // R-PB0-4: a plane with no `c` and no fit at all is left out, so `count` is the planes where the
            // "fewer than 5" warn fired.
            let kept = channel_c_inputs(
                entries
                    .iter()
                    .filter_map(|e| e.measurement.as_ref())
                    .filter_map(|m| m.channels.get(plane)),
            );
            let c = channel_c_summary(&kept);
            match (c.median, c.min, c.max) {
                (Some(median), Some(min), Some(max)) => tracing::info!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    plane,
                    planes = c.planes,
                    count = c.count,
                    aperture_correction = median,
                    aperture_correction_min = min,
                    aperture_correction_max = max,
                    "measure: aperture correction resolved per channel"
                ),
                _ => tracing::info!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    plane,
                    planes = c.planes,
                    count = c.count,
                    "measure: aperture correction resolved per channel"
                ),
            }
            // Plan B B4 floor (i), R-PB4-5: a plane at the fit-call floor takes this median (before the weights, so
            // `compute_weights` and the run summary read the stand-in). The planes were persisted before this point
            // and the `metrics` payload keeps the module's values; the fold re-derives the median every run.
            let mut at_plane: Vec<&mut ChannelMeasurement> = entries
                .iter_mut()
                .filter_map(|e| e.measurement.as_mut())
                .filter_map(|m| m.channels.get_mut(plane))
                .collect();
            if let Some((m, count)) = substitute_group_c(&mut at_plane) {
                tracing::info!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    plane,
                    count,
                    aperture_correction = m,
                    "measure: floored planes took the group channel's median aperture correction"
                );
                for e in entries.iter() {
                    let took = e
                        .measurement
                        .as_ref()
                        .and_then(|m| m.channels.get(plane))
                        .and_then(|c| c.aperture_correction_group);
                    if let Some(aperture_correction) = took {
                        tracing::debug!(
                            run_id = rc.run_id,
                            group_key = %group.key,
                            frame_id = e.frame.frame_id,
                            plane,
                            aperture_correction,
                            "measure: floored plane took the group channel's median aperture correction"
                        );
                    }
                }
            }
        }
    }


    // Carry-forward: a non-finite location/scale in any channel excludes
    // the frame from weighing entirely, rather than failing the group.
    let mut weigh_indices: Vec<usize> = Vec::new();
    for (i, entry) in entries.iter_mut().enumerate() {
        let Some(m) = &entry.measurement else {
            continue;
        };
        let non_finite = m
            .channels
            .iter()
            .any(|c| !c.location.is_finite() || !c.scale.is_finite());
        if non_finite {
            let reason = "measurement invalid: non-finite location/scale".to_string();
            rc.runtime_exclusions
                .push((entry.frame.frame_id, reason.clone()));
            entry.included = false;
            entry.reason = Some(reason);
            continue;
        }
        weigh_indices.push(i);
    }

    if !weigh_indices.is_empty() {
        let keyword_values: Vec<Option<f64>> = weigh_indices
            .iter()
            .map(|&i| {
                if cfg.measurement.weight_mode == WeightMode::Keyword {
                    entries[i].calibrated.as_deref().and_then(|p| {
                        FitsHeader::from_path(p)
                            .ok()
                            .and_then(|h| h.get_f64(&cfg.measurement.keyword))
                    })
                } else {
                    None
                }
            })
            .collect();

        let (weights, selection_reasons) = {
            let inputs: Vec<WeightInput> = weigh_indices
                .iter()
                .zip(keyword_values.iter())
                .map(|(&i, &kw)| WeightInput {
                    measurement: entries[i].measurement.as_ref().unwrap(),
                    exposure_s: entries[i].frame.exposure_s,
                    keyword_value: kw,
                })
                .collect();
            let excluded_flags = vec![false; inputs.len()];
            let w = compute_weights(
                &inputs,
                cfg.measurement.weight_mode,
                &cfg.measurement.formula,
                &excluded_flags,
            );
            let manual = vec![false; inputs.len()];
            // Plan D D1w (PD-5): under the PSF-family modes the 5 % floor is applied to the frame-common weight after
            // Register (`reweigh_groups`); here only a weight of zero is refused.
            let sel = if defers_weight_gate(cfg.measurement.weight_mode) {
                select_frames_deferring_weight_gate(&inputs, &w, &manual, &cfg.selection)
            } else {
                select_frames(&inputs, &w, &manual, &cfg.selection)
            };
            (w, sel)
        };

        for (i, &idx) in weigh_indices.iter().enumerate() {
            entries[idx].weight = Some(weights[i].clone());
            match &selection_reasons[i] {
                Some(reason) => {
                    entries[idx].included = false;
                    entries[idx].reason = Some(reason.clone());
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = entries[idx].frame.frame_id,
                        reason = %reason,
                        "frame excluded"
                    );
                }
                None => {
                    entries[idx].included = true;
                    // Plan D D1w (PD-18): a frame the deferred floor will judge after Register is registered and
                    // reweighed, but the reference pick must not see it. Stored here, because the reweigh overwrites
                    // `weight`.
                    entries[idx].measure_floored = defers_weight_gate(cfg.measurement.weight_mode)
                        && weight_gate(&weights[i], &cfg.selection).is_some();
                }
            }
            tracing::debug!(
                run_id = rc.run_id,
                frame_id = entries[idx].frame.frame_id,
                weight = weights[i].normalized_mean,
                included = entries[idx].included,
                "frame measured"
            );
        }
    }

    let included_count = entries.iter().filter(|e| e.included).count();

    // Perf tier C Task 1 (ruling C-1/C-1a): one β for the whole group,
    // the lower median of every MEASURED member's own `Auto` pick —
    // every channel of every frame this stage actually measured
    // (fresh or reused from a cached `metrics` row), never narrowed to
    // `included` (weighing/selection happens above, over the SAME
    // `Auto` β regardless of which frames end up combined). `None`
    // when the group measured nothing at all. Recorded only — nothing
    // consumes it yet (Task 2 will).
    // Task 11b: under `Auto` the value the two passes resolved and fitted
    // every member at — the same lower median, over the members' OWN βs.
    let group_beta_value: Option<f64> = if auto {
        auto_group_beta
    } else {
        let betas: Vec<f64> = entries
            .iter()
            .filter_map(|e| e.measurement.as_ref())
            .flat_map(|m| m.channels.iter().map(|c| c.beta))
            .collect();
        if betas.is_empty() {
            None
        } else {
            Some(psf_signal::group_beta(&betas))
        }
    };
    rc.measured_groups.insert(
        group.key.clone(),
        MeasuredGroup {
            beta: group_beta_value,
        },
    );
    if let Some(beta) = group_beta_value {
        tracing::debug!(
            run_id = rc.run_id,
            group_key = %group.key,
            beta,
            "stacking group beta resolved"
        );
    }

    {
        let conn = db(&rc.ctx)?.conn();
        let group_id = *rc.group_ids.get(&group.key).ok_or_else(|| {
            RunError::Other(format!(
                "no stacking_run_groups row for group {}",
                group.key
            ))
        })?;
        if included_count < 3 {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                count = included_count,
                "stacking group skipped: fewer than 3 included frames"
            );
            rc.warnings.push(format!(
                "group {} skipped: fewer than 3 included frames",
                group.key
            ));
            update_group(
                &conn,
                group_id,
                &GroupUpdate {
                    included_count: Some(included_count as i64),
                    status: Some("skipped"),
                    beta: group_beta_value,
                    ..Default::default()
                },
            )?;
        } else {
            *any_group_viable = true;
            update_group(
                &conn,
                group_id,
                &GroupUpdate {
                    included_count: Some(included_count as i64),
                    beta: group_beta_value,
                    ..Default::default()
                },
            )?;
        }
    }

    rc.measured.insert(group.key.clone(), entries);

    let group_reachable = group
        .frames
        .iter()
        .filter(|f| {
            !excluded_set.contains(&f.frame_id) && !stage1_failed.contains_key(&f.frame_id)
        })
        .count();
    current += group_reachable;
    // `ahead_halves`: the pass-1 half-ticks of the groups still to come on the cross-group queue, already in the
    // stage counter; the bar never steps back below them (Task 13c L4 review I1).
    rc.progress(
        Stage::Measure,
        Some(group.key.clone()),
        current + ahead_halves / 2,
        total,
        0,
        0,
        None,
        None,
    );
    *current_ref = current;
    Ok(())
}


/// Where a member's first pass comes from in phase 3.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass1Source {
    /// No valid artifact: pass 1 runs in the pass-2 worker (and writes the artifact).
    Owed,
    /// The artifact on disk, validated by the controller (row, head, stamp).
    Artifact,
}
/// What a phase-1 worker hands back: the own β, and whether the artifact was written (the records are dropped).
struct Pass1Done {
    own: Vec<f64>,
    written: bool,
}
/// What a phase-3 worker hands back besides the measurement: whether it wrote the artifact (it ran pass 1).
struct Pass2Done {
    measurement: FrameMeasurement,
    fits: Vec<Vec<psf_signal::StarFit>>,
    written: bool,
}
/// What a phase-3 worker hands back when the member failed: the exclusion text, and whether the worker had already
/// written the member's first-pass artifact (it ran pass 1 and pass 2 then failed). The artifact is the work of an
/// expensive pass and is valid on its own, so its row is recorded either way (ruling R10 M3).
struct Pass2Failed {
    message: String,
    written: bool,
}
/// Whether `calibrated` is a file Athenaeum writes and replaces itself: under the layout's `calibrated/` or `prepared/`
/// root. A first pass keeps its stamp's modification time out of the comparison for those only (R12 (a)); a path
/// spelled differently from the roots compares as not owned, the safe side.
fn owns_calibrated_file(layout: &WorkingLayout, calibrated: &Path) -> bool {
    calibrated.starts_with(layout.calibrated_root()) || calibrated.starts_with(layout.prepared_root())
}
/// One frame of the group on its way through the two passes.
struct MeasureMember {
    idx: usize,
    frame: GroupFrame,
    path: PathBuf,
    calib_hash: String,
    /// The stored row whose first pass is valid: its measurement and its key.
    cached: Option<(FrameMeasurement, String)>,
    /// The frame's own β per plane (pass 1, read or reused).
    own: Option<Vec<f64>>,
    /// Where the first-pass artifact is READ from: the fresh row's own path (spec §5.7: a row written before the
    /// frame-level key keeps its old folder), else [`Self::layout_spill`].
    spill: PathBuf,
    /// Where a first-pass artifact is WRITTEN: the current layout's path for this frame (flat, frame-only stem).
    /// A refused or stale artifact is rewritten here, never at the old row path, so the write's
    /// [`replace_frame_level_file`] retires a legacy `measured/<old_key>/` file (ruling R6 M1).
    layout_spill: PathBuf,
    /// The artifact's key ([`pass1_hash_for`]).
    pass1_key: String,
    source: Pass1Source,
    /// The calibrated file is not one Athenaeum writes (not under the layout's `calibrated/` or `prepared/`: an attested
    /// light or a project replica used in place), so its modification time is part of the first pass's stamp (R12 (a)).
    strict_mtime: bool,
    /// A fresh artifact row exists but its head is not validated yet: the member's own β came from its `metrics`
    /// row, so the artifact is only validated once the member is known to need pass 2 (R11c-5 (3)).
    spill_pending: bool,
    /// The frame's `measured` row as it stood when the step-0 reads ran (read always, also under `force_fresh`). It is
    /// deleted before pass 1 replaces its file unless it is fresh and kept, and handed to [`upsert_pass1_row`] so the
    /// write retires a legacy file the row named.
    old_measured: Option<crate::db::stacking::StackingArtifactRow>,
}
/// The artifact's head validated against the calibrated file now: usable, or `None` after the log line that says
/// why (stale before use `debug!`, any other refusal `warn!`).
fn validate_pass1_spill(
    run_id: i64,
    frame_id: i64,
    spill: &Path,
    path: &Path,
    strict_mtime: bool,
) -> Option<Pass1Head> {
    let usable = read_pass1_head(spill)
        .and_then(|head| check_pass1_head(&head, path, strict_mtime).map(|()| head));
    match usable {
        Ok(head) => Some(head),
        // Found stale before use: the calibrated file is not the one the artifact was read from.
        Err(e @ Pass1ArtifactError::Stale { .. }) => {
            tracing::debug!(
                run_id,
                frame_id,
                path = %spill.display(),
                error = %e,
                "measure: first-pass artifact is stale; pass 1 re-runs"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                run_id,
                frame_id,
                path = %spill.display(),
                error = %e,
                "measure: first-pass artifact refused; pass 1 re-runs"
            );
            None
        }
    }
}
fn exclude_measured_member(rc: &mut RunContext, entry: &mut MeasuredFrame, msg: String) {
    rc.runtime_exclusions
        .push((entry.frame.frame_id, msg.clone()));
    entry.included = false;
    tracing::warn!(
        run_id = rc.run_id,
        frame_id = entry.frame.frame_id,
        reason = %msg,
        "frame excluded"
    );
    entry.reason = Some(msg);
}
/// The first pass of one frame, run in a worker, and its artifact written best-effort: a failed write is a
/// `warn!` (the caller keeps the records only for as long as it needs them).
fn run_pass1_and_spill(
    frame_id: i64,
    path: &Path,
    spill: &Path,
    opts: &MeasureOptions,
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
) -> Result<(FramePass1, bool), crate::integration::IntegrationError> {
    let pass1 = measure_frame_pass1(path, opts, Some(pool), cancel)?;
    let written = match write_pass1(spill, &pass1) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                frame_id,
                path = %spill.display(),
                error = %e,
                "measure: first-pass artifact write failed; the first pass re-runs when it is needed"
            );
            false
        }
    };
    Ok((pass1, written))
}

/// The members of one group on their way through the two passes, and the sizing its fan-outs share (Task 13c L4c:
/// the prepare step, the pass-1 queue and the rest used to be one function, so several groups' pass-1 items can share
/// one queue).
struct TwoPassState {
    members: Vec<MeasureMember>,
    /// Indices into `members` owed pass 1.
    need_pass1: Vec<usize>,
    working_set_bytes: u64,
}

/// Stage 3 under `measurement.psfModel = Auto` (Plan A Task 11b, owner decision R11b-12; the task's design ruling 2
/// §1/§3): every member of `group` is fitted at ONE β, the group β — [`psf_signal::group_beta`] (the lower median) of
/// the members' own βs, the very value `stacking_run_groups.beta` records and stage 6 fits its reference and every
/// member's seeds at (`PsfModel::Fixed(group β)`) — so no frame's F is compared across the group at a β of its own,
/// and Measure's F and LN's F sit at one β. Two passes over `star_measure`'s `PlanePrep` seam, with the first pass
/// SPILLED to the work dir (Plan A Task 11c, R11c-1 .. R11c-6):
///
/// 0. per member, on the controller: the own β from the frame's `metrics` row when its first pass is still valid
///    ([`pass1_own_betas`]), else from its first-pass artifact — the `measured` row fresh at [`pass1_hash_for`] and
///    its head ([`read_pass1_head`]) describing the calibrated file as it is now ([`check_pass1_head`]) — else pass 1
///    is owed;
/// 1. fanned out, for the members owed pass 1: the verdict pass and the frame's own β, F-4's E5 estimator unchanged
///    ([`measure_frame_pass1`]), written to the artifact ([`write_pass1`]) and DROPPED in the worker — which returns
///    the own β and whether the file was written; the controller upserts the `measured` rows at once, so a run
///    cancelled after this phase leaves the work for the next one;
/// 2. the group β, once;
/// 3. per member that needs its second pass, fanned out: every fit, F, `.athf`, `c` and F-6 statistic at
///    `PsfModel::Fixed(group β)` ([`measure_frame_pass2`]) — unless the frame's stored row is keyed on this very group
///    β with fresh `fits` rows (a cache hit). The first pass comes from the artifact ([`read_pass1`]: the checksum,
///    the stamp), or — a member with no valid artifact (the upgrade ripple), a refused read, a failed write — from
///    pass 1 run in this worker, which writes the artifact too.
///
/// A group of one is fitted at its own β (the lower median of its planes' own βs, which is its own β on a mono
/// frame). Every `metrics` row is keyed on `measurement_hash_for(cfg, calib, Some(group β))`: the group β moving (a
/// member excluded, added or re-measured to another own β) re-runs pass 2 for the whole group and pass 1 for no
/// member whose artifact is valid. The artifact is keyed on the calibration hash and the measurement subtree, NOT the
/// group β. `force_fresh` (`rerun_from` ≤ Measure) skips the artifact as it skips the `metrics` row: pass 1 runs and
/// the artifact is overwritten in place. Returns the group β, `None` when no member was measured.
///
/// Residency: each phase holds at most one frame's first pass per admitted worker — `fan_out`'s phase-1 results carry
/// the own β only, its phase-3 results `(measurement, fits)`. A write failure (disk full) is a `warn!`, no row, the
/// records dropped and pass 1 re-run in that member's pass-2 worker: twice only on failure, never held (holding is
/// refused: on a full disk the machine cannot swap), so the bound holds unconditionally.
///
/// The group β is the lower median over every member whose pass 1 succeeded, including one whose pass 2 then fails and
/// is excluded: that member still counted in the median. No re-computation loop; every `.athf` written is at the one
/// group β regardless.
///
/// Progress: the bar moves from the first frame. Each frame is two half-ticks on a doubled scale (pass 1, pass 2:
/// [`FanOutTicker::with_scale`]), so the stage keeps its `total` and lands on it; a member whose pass 1 ran in phase 1
/// ticks its second half only in phase 3, any other member both halves there, a cache hit both at once.
///
/// Split by Task 13c L4c into [`measure_two_pass_prepare`] (step 0, the controller reads), the pass-1 queue
/// ([`measure_pass1_queue`] per group, or one queue over several groups in `stage_measure`), [`apply_pass1_results`]
/// and [`measure_two_pass_rest`] (the group β and pass 2); the order of one group's steps is unchanged.
fn measure_two_pass_prepare(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    cfg: &StackingConfig,
    to_measure: Vec<(usize, GroupFrame, PathBuf)>,
    force_fresh: bool,
    (group_max_w, group_max_h): (u64, u64),
) -> Result<TwoPassState, RunError> {
    // Step 0: the own β from the stored rows (skipped under a Measure-or-earlier `rerun_from`, as the single pass's
    // reuse is): the `metrics` row first, else the first-pass artifact.
    let mut members: Vec<MeasureMember> = Vec::with_capacity(to_measure.len());
    for (idx, frame, path) in to_measure {
        let calib_hash = {
            let conn = db(&rc.ctx)?.conn();
            rc.memo.calibration_hash_checked(&conn, cfg, &frame)?
        };
        let stem = frame_file_stem(&frame);
        let mut member = MeasureMember {
            idx,
            frame,
            strict_mtime: !owns_calibrated_file(&rc.layout, &path),
            path,
            spill: rc.layout.measured_path(&stem),
            layout_spill: rc.layout.measured_path(&stem),
            pass1_key: pass1_hash_for(cfg, &calib_hash),
            calib_hash,
            cached: None,
            own: None,
            source: Pass1Source::Owed,
            spill_pending: false,
            old_measured: None,
        };
        // The `measured` row is read for every member, `force_fresh` included: a member left owed deletes it before
        // pass 1 replaces its file, whatever the reason it is owed.
        let (existing, spilled) = {
            let conn = db(&rc.ctx)?.conn();
            (
                if force_fresh {
                    None
                } else {
                    crate::db::stacking::find_artifact(
                        &conn,
                        &rc.subject,
                        &group.key,
                        ArtifactKind::Metrics,
                        Some(member.frame.frame_id),
                    )?
                },
                crate::db::stacking::find_artifact(
                    &conn,
                    &rc.subject,
                    &group.key,
                    ArtifactKind::Measured,
                    Some(member.frame.frame_id),
                )?,
            )
        };
        member.old_measured = spilled.clone();
        if !force_fresh {
            if let Some(payload) = existing.as_ref().and_then(|row| row.payload_json.as_ref()) {
                match serde_json::from_str::<FrameMeasurement>(payload) {
                    Ok(m) => {
                        let row = existing.as_ref().expect("a payload has its row");
                        if let Some(own) =
                            pass1_own_betas(cfg, &member.calib_hash, &row.config_hash, &m)
                        {
                            member.own = Some(own);
                            member.cached = Some((m, row.config_hash.clone()));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            run_id = rc.run_id,
                            frame_id = member.frame.frame_id,
                            error = %e,
                            "stored metrics payload failed to parse; re-measuring"
                        );
                    }
                }
            }
            if let Some(row) = spilled.filter(|row| is_fresh(row, &member.pass1_key)) {
                // The row's own path is the artifact (spec §5.7: a row written before the frame-level key keeps its
                // `measured/<group_key>/` path, and a frame's stem may have changed since): the layout path is only
                // for a new write. A fresh row without a path reads as absent (`is_fresh` requires one).
                if let Some(path) = row.path.as_deref() {
                    member.spill = PathBuf::from(path);
                }
                if member.own.is_none() {
                    if let Some(head) = validate_pass1_spill(
                        rc.run_id,
                        member.frame.frame_id,
                        &member.spill,
                        &member.path,
                        member.strict_mtime,
                    ) {
                        member.source = Pass1Source::Artifact;
                        member.own = Some(head.beta_own);
                    } else {
                        // Refused or stale: pass 1 re-runs and writes the current layout's path.
                        member.spill = member.layout_spill.clone();
                    }
                } else {
                    member.spill_pending = true;
                }
            } else {
                tracing::debug!(
                    run_id = rc.run_id,
                    frame_id = member.frame.frame_id,
                    path = %member.spill.display(),
                    "measure: no usable first-pass artifact row"
                );
            }
        }
        members.push(member);
    }

    let working_set_bytes = measure_working_set_bytes(group_max_w, group_max_h);
    let need_pass1: Vec<usize> = (0..members.len())
        .filter(|&i| members[i].own.is_none())
        .collect();
    // The write order (a row is gone before the file it names is replaced): a member left owed without a fresh
    // artifact row will run pass 1 and rewrite its `measured` file, so a cancelled or killed run must not leave the
    // old row over it. That is every member of `need_pass1`, and a member whose own β came from its `metrics` row
    // while its `measured` row is not fresh at its key (it is not in `need_pass1`, but needs pass 2 -> pass 1 writes
    // the file under `layout_spill`, under another key than the row's). A `spill_pending` member keeps its fresh row.
    {
        let conn = db(&rc.ctx)?.conn();
        for member in &members {
            if member.source == Pass1Source::Owed && !member.spill_pending && member.old_measured.is_some() {
                crate::db::stacking::delete_artifact(
                    &conn,
                    &rc.subject,
                    &group.key,
                    ArtifactKind::Measured,
                    Some(member.frame.frame_id),
                )?;
            }
        }
    }
    Ok(TwoPassState {
        members,
        need_pass1,
        working_set_bytes,
    })
}

/// The pass-1 jobs of one group: `(frame id, calibrated file, artifact path)` per member owed pass 1.
fn pass1_jobs(state: &TwoPassState) -> Vec<(i64, PathBuf, PathBuf)> {
    state
        .need_pass1
        .iter()
        .map(|&i| {
            (
                state.members[i].frame.frame_id,
                state.members[i].path.clone(),
                state.members[i].layout_spill.clone(),
            )
        })
        .collect()
}

/// One pass-1 item, run in a worker: the first pass, spilled and dropped; ticks half a frame.
fn run_pass1_item(
    (frame_id, path, spill): (i64, PathBuf, PathBuf),
    opts: &MeasureOptions,
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
    ticker: &FanOutTicker,
) -> Result<Pass1Done, String> {
    let out = run_pass1_and_spill(frame_id, &path, &spill, opts, pool, cancel)
        .map(|(pass1, written)| Pass1Done {
            own: pass1.beta_own().to_vec(),
            written,
        })
        .map_err(|e| format!("measurement failed: {e}"));
    ticker.tick_units(Some(frame_id), 1);
    out
}

/// Step 1 for ONE group, fanned out on its own queue (the per-group shape; also the fallback of the cross-group
/// queue). `current` is the stage-wide count of frames finished before this group.
fn measure_pass1_queue(
    rc: &RunContext,
    group: &IntegrationGroup,
    opts: &MeasureOptions,
    state: &TwoPassState,
    current: usize,
    total: usize,
) -> Vec<Option<Result<Pass1Done, String>>> {
    // The live budget, read right before this fan-out (Task 13c L4 review I2).
    let budget = fan_budget(rc);
    let admission_n = admission(
        state.working_set_bytes,
        rc.ctx.image_pool.current_num_threads(),
        budget.bytes,
    );
    log_admission(
        rc,
        Stage::Measure,
        &group.key,
        state.working_set_bytes,
        0,
        admission_n,
        budget,
        "pass1",
    );
    // Half a frame per pass-1 frame (scale 2), so the bar moves from the first frame.
    let ticker1 = FanOutTicker::new(
        rc,
        Stage::Measure,
        Some(group.key.clone()),
        current * 2,
        total,
        0,
        0,
    )
    .with_scale(2);
    let ticker1_ref = &ticker1;
    let cancel_ref: &AtomicBool = &rc.cancel;
    let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    fan_out(
        pass1_jobs(state),
        &AdmissionGate::budgeted(rc, state.working_set_bytes, 0, admission_n),
        FanOutLabel::new(rc, Stage::Measure, Some(&group.key), "pass1"),
        cancel_ref,
        move |job| run_pass1_item(job, opts, pool_ref, cancel_ref, ticker1_ref),
    )
}

/// Every pass-1 file written gets its row BEFORE a cancel is honoured: a cancelled run's re-run reuses it (R11c-5 (1)).
fn upsert_pass1_rows(
    rc: &RunContext,
    state: &TwoPassState,
    results: &[Option<Result<Pass1Done, String>>],
) -> Result<(), RunError> {
    for (&i, res) in state.need_pass1.iter().zip(results) {
        if let Some(Ok(Pass1Done { written: true, .. })) = res {
            upsert_pass1_row(
                rc,
                &state.members[i].frame,
                &state.members[i].layout_spill,
                &state.members[i].pass1_key,
                state.members[i].old_measured.as_ref(),
            )?;
        }
    }
    Ok(())
}

/// Take a group's pass-1 results: honour a cancel, exclude a failed member, record the own β of the rest. Returns
/// how many members failed pass 1.
fn apply_pass1_results(
    rc: &mut RunContext,
    entries: &mut [MeasuredFrame],
    state: &mut TwoPassState,
    results: Vec<Option<Result<Pass1Done, String>>>,
) -> Result<usize, RunError> {
    rc.check_cancel()?;
    let mut pass1_failed = 0usize;
    for (&i, res) in state.need_pass1.iter().zip(results) {
        match res {
            None => return Err(RunError::Cancelled),
            Some(Err(msg)) => {
                pass1_failed += 1;
                exclude_measured_member(rc, &mut entries[state.members[i].idx], msg)
            }
            Some(Ok(done)) => {
                state.members[i].own = Some(done.own);
                if done.written {
                    state.members[i].source = Pass1Source::Artifact;
                }
            }
        }
    }
    Ok(pass1_failed)
}

/// Steps 2 and 3 of the two-pass measure (the group β, then pass 2 per member) once pass 1 is done.
/// `pass1_base` is the number of pass-1 half-ticks the stage counter already holds beyond `current * 2` when this
/// group's pass 2 starts: the group's own pass-1 members on a per-group queue, every not-yet-finished group's on the
/// cross-group queue.
#[allow(clippy::too_many_arguments)]
fn measure_two_pass_rest(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    cfg: &StackingConfig,
    opts: &MeasureOptions,
    entries: &mut [MeasuredFrame],
    state: TwoPassState,
    pass1_failed: usize,
    (current, total): (usize, usize),
    pass1_base: usize,
    pass1_shared: bool,
) -> Result<Option<f64>, RunError> {
    let TwoPassState {
        mut members,
        need_pass1,
        working_set_bytes,
    } = state;
    let pool = rc.ctx.image_pool.clone();
    let pool_ref: &Arc<rayon::ThreadPool> = &pool;

    // Step 2, the group β, once: the lower median of every measured member's own βs (every plane of every frame, as
    // stage 6's group β always was).
    let measured: Vec<usize> = (0..members.len())
        .filter(|&i| members[i].own.is_some())
        .collect();
    let own_all: Vec<f64> = measured
        .iter()
        .flat_map(|&i| members[i].own.clone().unwrap_or_default())
        .collect();
    if own_all.is_empty() {
        return Ok(None);
    }
    let group_beta = psf_signal::group_beta(&own_all);
    let (own_min, own_max) = own_all
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &b| {
            (lo.min(b), hi.max(b))
        });
    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        beta = group_beta,
        count = measured.len(),
        beta_own_min = own_min,
        beta_own_max = own_max,
        "measure: group beta resolved; every member is fitted at it"
    );
    for &i in &measured {
        for (plane, &b) in members[i].own.iter().flatten().enumerate() {
            tracing::debug!(
                run_id = rc.run_id,
                frame_id = members[i].frame.frame_id,
                plane,
                beta_own = b,
                beta = group_beta,
                "measure: frame own beta"
            );
        }
    }

    // Step 3: a cache hit where the stored row is keyed on this group β with fresh `fits` rows, else fitted.
    let mut reused = 0usize;
    let mut need_pass2: Vec<usize> = Vec::new();
    for &i in &measured {
        let expected = measurement_hash_for(cfg, &members[i].calib_hash, Some(group_beta));
        if let Some((m, key)) = &members[i].cached {
            if *key == expected {
                let fresh = {
                    let conn = db(&rc.ctx)?.conn();
                    fits_artifact_fresh_for_planes(
                        &conn,
                        &rc.subject,
                        members[i].frame.frame_id,
                        m.channels.len(),
                        &expected,
                    )?
                };
                if fresh {
                    entries[members[i].idx].measurement = Some(m.clone());
                    entries[members[i].idx].cached_metrics = true;
                    reused += 1;
                    continue;
                }
                tracing::debug!(
                    run_id = rc.run_id,
                    frame_id = members[i].frame.frame_id,
                    "cached metrics found but its fits sidecar is missing or stale; re-measuring"
                );
            }
        }
        need_pass2.push(i);
    }
    if need_pass2.is_empty() {
        // Nothing left to fit: every member of the group is done (reused, or excluded in pass 1).
        if reused > 0 || !need_pass1.is_empty() {
            rc.progress(
                Stage::Measure,
                Some(group.key.clone()),
                // The pass-1 half-ticks of the groups still to come on the shared queue are already in the stage
                // counter; the bar never steps back below them.
                current + members.len() + (pass1_base - need_pass1.len()) / 2,
                total,
                0,
                0,
                None,
                None,
            );
        }
        return Ok(Some(group_beta));
    }
    // A member whose own β came from its `metrics` row and that needs pass 2 now: its artifact is validated here, for
    // the first time (R11c-5 (3)); a refused one leaves it owed.
    for &i in &need_pass2 {
        if members[i].spill_pending {
            members[i].spill_pending = false;
            if validate_pass1_spill(
                rc.run_id,
                members[i].frame.frame_id,
                &members[i].spill,
                &members[i].path,
                members[i].strict_mtime,
            )
            .is_some()
            {
                members[i].source = Pass1Source::Artifact;
            } else {
                // Refused or stale: pass 1 re-runs here and writes the current layout's path.
                members[i].spill = members[i].layout_spill.clone();
            }
        }
    }
    // The live budget, read right before this group's pass-2 fan-out (Task 13c L4 review I2). A group whose
    // pass 1 ran on the shared queue gets its own line here too (M3): it has none of its own yet.
    let budget = fan_budget(rc);
    let admission_n = admission(working_set_bytes, pool.current_num_threads(), budget.bytes);
    if need_pass1.is_empty() || pass1_shared {
        log_admission(
            rc,
            Stage::Measure,
            &group.key,
            working_set_bytes,
            0,
            admission_n,
            budget,
            "pass2",
        );
    }
    // Per job: the frame, its calibrated file, the artifact it reads, the layout path a re-run first pass writes,
    // where its first pass comes from, and whether its first half was already ticked by phase 1.
    let jobs: Vec<(i64, PathBuf, PathBuf, PathBuf, Pass1Source, bool, bool)> = need_pass2
        .iter()
        .map(|&i| {
            (
                members[i].frame.frame_id,
                members[i].path.clone(),
                members[i].spill.clone(),
                members[i].layout_spill.clone(),
                members[i].source,
                need_pass1.contains(&i),
                members[i].strict_mtime,
            )
        })
        .collect();
    let ticker = FanOutTicker::new(
        rc,
        Stage::Measure,
        Some(group.key.clone()),
        // Units (scale 2): the earlier groups, the pass-1 half-ticks of this group (a failed frame completed at once,
        // it never reaches pass 2) and the cache hits' two halves.
        current * 2 + pass1_base + pass1_failed + reused * 2,
        total,
        0,
        0,
    )
    .with_scale(2);
    let cancel_ref: &AtomicBool = &rc.cancel;
    let ticker_ref = &ticker;
    let results = fan_out(
        jobs,
        &AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n),
        FanOutLabel::new(rc, Stage::Measure, Some(&group.key), "pass2"),
        cancel_ref,
        move |(frame_id, path, spill, layout_spill, source, first_half_ticked, strict_mtime)| {
            // The first pass: the artifact (the checksum, the stamp), else pass 1 here — which also writes the
            // artifact — for a member owed it or whose artifact is refused now.
            let from_artifact = match source {
                Pass1Source::Artifact => match read_pass1(&spill, &path, opts, strict_mtime) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(
                            frame_id,
                            path = %spill.display(),
                            error = %e,
                            "measure: first-pass artifact refused after it was validated; pass 1 re-runs"
                        );
                        None
                    }
                },
                Pass1Source::Owed => None,
            };
            let first = match from_artifact {
                Some(p) => Ok((p, false)),
                None => run_pass1_and_spill(frame_id, &path, &layout_spill, opts, pool_ref, cancel_ref),
            };
            // One β per plane of THIS read (a group of one's planes included): every plane at the group β. A failure
            // keeps `written` so the controller still records the artifact a failed pass 2 left behind.
            let out = match first {
                Err(e) => Err(Pass2Failed {
                    message: format!("measurement failed: {e}"),
                    written: false,
                }),
                Ok((p, written)) => {
                    let betas = vec![group_beta; p.beta_own().len()];
                    measure_frame_pass2(&path, p, &betas, opts, Some(pool_ref), cancel_ref)
                        .map(|(measurement, fits)| Pass2Done {
                            measurement,
                            fits,
                            written,
                        })
                        .map_err(|e| Pass2Failed {
                            message: format!("measurement failed: {e}"),
                            written,
                        })
                }
            };
            // Phase 1 already ticked the first half of a member it ran; any other member ticks both halves here.
            ticker_ref.tick_units(Some(frame_id), if first_half_ticked { 1 } else { 2 });
            out
        },
    );
    // Every artifact a worker wrote gets its row BEFORE a cancel is honoured, as in phase 1.
    for (&i, res) in need_pass2.iter().zip(&results) {
        if let Some(Ok(Pass2Done { written: true, .. }) | Err(Pass2Failed { written: true, .. })) = res {
            upsert_pass1_row(
                rc,
                &members[i].frame,
                &members[i].layout_spill,
                &members[i].pass1_key,
                members[i].old_measured.as_ref(),
            )?;
        }
    }
    rc.check_cancel()?;
    for (&i, res) in need_pass2.iter().zip(results) {
        match res {
            None => return Err(RunError::Cancelled),
            Some(Err(failed)) => {
                exclude_measured_member(rc, &mut entries[members[i].idx], failed.message)
            }
            Some(Ok(done)) => {
                let expected = measurement_hash_for(cfg, &members[i].calib_hash, Some(group_beta));
                persist_measurement(
                    rc,
                    group,
                    &members[i].frame,
                    &done.measurement,
                    &done.fits,
                    &expected,
                )?;
                entries[members[i].idx].measurement = Some(done.measurement);
            }
        }
    }
    Ok(Some(group_beta))
}

/// A frame's first-pass artifact row, upserted right after its file was written (Plan A Task 11c, R11c-5: a cancelled
/// run's re-run reuses it). The identity recorded is the file's own, as `fits.<plane>`'s is.
///
/// `old` is the row the step-0 reads found before the write (deleted before the file was replaced, unless it was
/// fresh and kept): its file goes once nothing points to it (a frame whose stem or folder changed since the old row
/// was written; a legacy `measured/<key>/` file).
fn upsert_pass1_row(
    rc: &RunContext,
    frame: &GroupFrame,
    spill: &Path,
    key: &str,
    old: Option<&crate::db::stacking::StackingArtifactRow>,
) -> Result<(), RunError> {
    let (size, modified_at) = file_identity(spill)?;
    let conn = db(&rc.ctx)?.conn();
    upsert_artifact(
        &conn,
        &NewArtifact {
            subject: &rc.subject,
            frame_id: Some(frame.frame_id),
            group_key: "",
            kind: ArtifactKind::Measured,
            path: spill.to_str(),
            config_hash: key,
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: None,
        },
    )?;
    replace_frame_level_file(&conn, &rc.layout.measured_root(), old, spill);
    Ok(())
}

/// A frame's measurement hash after stage 3 — [`measurement_hash_for`] at the group β stage 3 resolved for its group
/// (`RunContext::measured_groups`; Plan A Task 11b: under `Auto` every member's `.athf` was fitted at it, so every
/// stage that folds the measurement hash — Register's and Normalize's reads of the `fits` artifact, C-17 / C-18 —
/// folds it too, and the plan gate resolves the same value, `build_plan`'s `gate_measure_betas`). Under `Moffat4`
/// the β is not folded at all.
pub(super) fn group_measurement_hash(
    rc: &RunContext,
    cfg: &StackingConfig,
    group_key: &str,
    calibrated_hash: &str,
) -> String {
    measurement_hash_for(
        cfg,
        calibrated_hash,
        rc.measured_groups.get(group_key).and_then(|g| g.beta),
    )
}

/// Stage 3's write of one measured frame (factored out of [`stage_measure`] by Plan A Task 11b, which writes from two
/// paths): the `metrics` row (`payload_json` = the measurement) and, per plane, its accepted fits as one `.athf` file
/// and one `stacking_artifacts` row, every one keyed on `expected_hash`.
fn persist_measurement(
    rc: &RunContext,
    group: &IntegrationGroup,
    frame: &GroupFrame,
    m: &FrameMeasurement,
    fits: &[Vec<psf_signal::StarFit>],
    expected_hash: &str,
) -> Result<(), RunError> {
    let payload = serde_json::to_string(&m)
        .map_err(|e| RunError::Other(format!("failed to serialize measurement: {e}")))?;
    {
        let conn = db(&rc.ctx)?.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: Some(frame.frame_id),
                group_key: &group.key,
                kind: ArtifactKind::Metrics,
                path: None,
                config_hash: expected_hash,
                size: None,
                modified_at: None,
                payload_json: Some(&payload),
            },
        )?;
    }
    // Tier C Task 1 (spec §2.2.1): persist every plane's
    // accepted star fits beside `metrics`, one
    // `.athf` file and one `stacking_artifacts` row per
    // plane, keyed on the SAME `expected_hash` — the two
    // are a product of the same computation and share
    // its staleness (see `plan.rs`'s per-frame loop).
    let stem = frame_file_stem(frame);
    for (plane, plane_fits) in fits.iter().enumerate() {
        let fits_path = rc.layout.fits_path(&stem, plane);
        // Final fix wave (ruling C-30, the whole-branch
        // review's I1): a `.athf` write failure DEGRADES,
        // it does not fail the run. Every consumer of
        // this artifact already falls back on its own —
        // Register to detection with a warn
        // (`resolve_register_fits`), Normalize to
        // detection (`fits_paths`' `None` arm), the plan
        // gate to "Measure is stale"
        // (`fits_artifact_fresh`) — so a full stop here
        // would be the ONE place in the seam that treats
        // a missing optimization as a correctness
        // failure, and it would throw away the whole
        // stage's real output (`metrics`, which IS
        // written) over a cache file. Same shape as
        // `.rej`'s latched write failure (M3's
        // `RejBitmapSet`) and the LN sidecar's per-frame
        // exclusion: loud, never silent, never fatal.
        //
        // The write order: the plane's row is deleted BEFORE the file is replaced, and upserted after, so a failed
        // write leaves no row — never a row of the old key over a file another config wrote, and never a row for a
        // file that is not on disk. A consumer falls back as it does for a missing row.
        let kind = ArtifactKind::Fits(plane);
        // The row this write replaces, read BEFORE the delete (see `replace_frame_level_file`).
        let old = {
            let conn = db(&rc.ctx)?.conn();
            let old = crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group.key,
                kind,
                Some(frame.frame_id),
            )?;
            crate::db::stacking::delete_artifact(
                &conn,
                &rc.subject,
                &group.key,
                kind,
                Some(frame.frame_id),
            )?;
            old
        };
        let timed = tracing::enabled!(tracing::Level::DEBUG);
        let (write_t, write_cpu0) = (
            std::time::Instant::now(),
            if timed {
                crate::star_measure::substeps::thread_cpu_ns()
            } else {
                0
            },
        );
        let written = fits_artifact::write_fits(&fits_path, plane_fits);
        // Plan A Task 13b (13-P): the write's wall and this thread's CPU, timers only.
        if timed {
            tracing::debug!(
                run_id = rc.run_id,
                group_key = %group.key,
                frame_id = frame.frame_id,
                plane,
                count = plane_fits.len(),
                athf_write_us = write_t.elapsed().as_micros() as u64,
                athf_write_cpu_us = crate::star_measure::substeps::thread_cpu_ns()
                    .saturating_sub(write_cpu0)
                    / 1_000,
                "fits artifact write timed"
            );
        }
        if let Err(e) = written {
            #[cfg(test)]
            FITS_WRITE_FAILURE_LOG
                .lock()
                .unwrap()
                .push(fits_path.clone());
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                frame_id = frame.frame_id,
                plane,
                path = %fits_path.display(),
                error = %e,
                "fits artifact write failed; consumers will fall back"
            );
            continue;
        }
        let (size, modified_at) = file_identity(&fits_path)?;
        let conn = db(&rc.ctx)?.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: Some(frame.frame_id),
                group_key: &group.key,
                kind,
                path: fits_path.to_str(),
                config_hash: expected_hash,
                size: Some(size),
                modified_at: Some(&modified_at),
                payload_json: None,
            },
        )?;
        replace_frame_level_file(&conn, &rc.layout.fits_root(), old.as_ref(), &fits_path);
    }
    Ok(())
}

#[cfg(test)]
#[path = "measure_tests.rs"]
mod tests;
