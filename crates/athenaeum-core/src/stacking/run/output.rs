//! Stages 7 to 9, integrate, drizzle and output (`stage_output`): per group, the integration, the drizzle, the run
//! summary and the written masters.

use super::*;

/// The `.rej` file stems of one group's rejection-bitmap set, one per included frame, in `included` order: the
/// frame's [`frame_file_stem`], never the stem of its calibrated path. A calibrated row written before the
/// frame-level cache keeps its old `calibrated/<group_key>/c_<name>.fits` path, and a merged group can hold two such
/// frames of one file name from different old folders; the frame's own stem carries its id, so the two never share
/// a `.rej` (ruling R4 (c)). A member with no frame in the group (not expected) falls back to its position and id.
fn rejection_map_stems(group: &IntegrationGroup, included_ids: &[i64]) -> Vec<String> {
    let frames_by_id: HashMap<i64, &GroupFrame> =
        group.frames.iter().map(|f| (f.frame_id, f)).collect();
    included_ids
        .iter()
        .enumerate()
        .map(|(pos, frame_id)| match frames_by_id.get(frame_id) {
            Some(frame) => frame_file_stem(frame),
            None => format!("frame_{pos}_f{frame_id}"),
        })
        .collect()
}

/// One process, one writer: `master_file_name`/`write_master_light`
/// (`resolve_collision`) are check-then-write, so two groups' writes — even
/// across two concurrent runs of the same process — must never race on the
/// same free name (carry-forward).
pub(super) static OUTPUT_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// One included member of a group, snapshotted out of `rc.measured` before
/// the (possibly slow) integration call so nothing below needs a borrow of
/// `rc` to survive it — the fields [`crate::stacking::integrate::StackFrame`]
/// needs, plus `frame_id` (`StackFrame` also carries `frame_id` now, purely
/// for the Task 6a fallback warning — this struct's own copy stays, since a
/// lot of code here reads `member.frame_id` before any `StackFrame` exists).
pub(super) struct GroupMember {
    pub(super) frame_id: i64,
    /// The catalog `files.filename` (fix round 1, item 3) — `ATH_STKF`
    /// names the SOURCE frame, not the calibrated artifact, and this needs
    /// no `c_`/`_d` stripping at all (unlike `calibrated`'s own name).
    pub(super) filename: String,
    calibrated: PathBuf,
    pub(super) map: PixelMap,
    pub(super) measurement: FrameMeasurement,
    pub(super) weight: FrameWeight,
    exposure_s: Option<f64>,
    date_obs: Option<String>,
    /// This frame's stage-5 `registration_results.config_hash` (spec §9.3) —
    /// `String::new()` for a member somehow reaching here without an
    /// `Aligned` outcome (never happens: the loop below only pushes members
    /// whose `registration` matched `Aligned`, see [`process_group_output`]).
    /// Stage 6's own [`normalization_hash_for`] keys a frame's `ln` artifact
    /// on this, so a re-registered frame's sidecar is recomputed too.
    pub(super) registration_hash: String,
    /// Perf tier A Task 6a: this frame's materialized registered artifact —
    /// `Some` when a `stacking_artifacts` row of `kind = "registered"`
    /// exists for it with `config_hash == registration_hash` (a HASH-only
    /// check, deliberately not the disk-`stat`ing [`plan::is_fresh`] — see
    /// the field's own build site); `None` when no such row exists at all,
    /// which falls back SILENTLY (nothing was ever expected here). Every
    /// reader ([`crate::stacking::integrate::integrate_planes`],
    /// `ln::normalize_frame`) then tries `Some` paths for real and falls
    /// back LOUDLY (one `warn!`) only if the file turns out to be missing,
    /// unreadable, or the wrong size at that point — never a run failure
    /// either way.
    registered_path: Option<PathBuf>,
}

/// What became of one group at Output time.
enum GroupOutcome {
    /// Fewer than 3 included frames — per stage 3, possibly narrowed further
    /// by a stage-5 registration exclusion — never attempted.
    Skipped,
    /// `integrate_group`, the master-card build or the write itself failed
    /// (anything other than a cancel). The group row is already `failed`
    /// with the message, a warning already logged/pushed, and a
    /// `SummaryGroup` with no master already pushed — the caller only needs
    /// to know this group did not produce one.
    Failed,
    /// A master was written; the group row and `SummaryGroup` are already
    /// updated/pushed.
    Written,
}

/// The serde name (camelCase, per every stacking config enum's own
/// attribute) of an enum value — `MasterCardInputs::weight_mode`/
/// `::normalization` want the SAME strings the config itself round-trips as
/// (`"psfSignalWeight"`, `"scaleZeroOffset"`, …), not a hand-written mirror
/// that could drift from the enum's own `#[serde(rename_all = "camelCase")]`.
/// Fix round 1, item 10: propagates instead of silently falling back to an
/// empty string — a mis-serialized enum belongs in the run's own failure
/// text, never a blank `ATH_STKW`/`ATH_STKO` card value.
pub(super) fn enum_serde_name<T: Serialize>(v: &T) -> Result<String, RunError> {
    let value = serde_json::to_value(v)
        .map_err(|e| RunError::Other(format!("failed to serialize enum value: {e}")))?;
    value.as_str().map(str::to_string).ok_or_else(|| {
        RunError::Other(format!("enum value did not serialize to a string: {value}"))
    })
}

/// Build one [`SummaryFrame`] from a frame's whole stage 3-5 outcome plus,
/// once integration has run, its `rejected_fraction` (`None` for anything
/// integration never actually combined — excluded before it ever ran, or
/// dropped by `integrate_group`'s OWN min-weight floor, a check independent
/// of `cfg.selection`'s stage-3 floor; `GroupOutput` reports only a COUNT of
/// such drops, not which frames, so `included`/`SummaryFrame.included` here
/// still reflect stage 3-5's decision, not that further drop — a known,
/// narrow gap, not something any of this task's required tests exercises).
pub(super) fn summary_frame_for(entry: &MeasuredFrame, rejected_fraction: Option<f64>) -> SummaryFrame {
    let m = entry.measurement.as_ref();
    let mean_channel =
        |f: fn(&crate::stacking::measure::ChannelMeasurement) -> f64| -> Option<f64> {
            m.map(|meas| {
                if meas.channels.is_empty() {
                    0.0
                } else {
                    meas.channels.iter().map(|c| f(c)).sum::<f64>() / meas.channels.len() as f64
                }
            })
        };
    let (reg_status, reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio, reg_flipped) =
        match &entry.registration {
            Some(RegisteredFrameOutcome::Aligned { record, .. }) => (
                Some(record.status.clone()),
                record.model.clone(),
                Some(record.rms_residual_px),
                Some(record.matched_stars as usize),
                record.inlier_ratio,
                Some(record.flipped),
            ),
            Some(RegisteredFrameOutcome::Failed(_)) => {
                (Some("failed".to_string()), None, None, None, None, None)
            }
            None => (Some("skipped".to_string()), None, None, None, None, None),
        };
    SummaryFrame {
        frame_id: entry.frame.frame_id,
        filename: entry.frame.filename.clone(),
        included: entry.included,
        exclusion_reason: entry.reason.clone(),
        weight: entry.weight.as_ref().map(|w| w.normalized_mean),
        weight_channels: entry
            .weight
            .as_ref()
            .map(|w| w.normalized.clone())
            .unwrap_or_default(),
        fwhm_px: m.map(FrameMeasurement::mean_fwhm_px),
        eccentricity: m.map(FrameMeasurement::mean_eccentricity),
        stars: m.map(FrameMeasurement::min_stars),
        psf_signal_weight: m.map(FrameMeasurement::mean_psf_signal_weight),
        psf_snr: mean_channel(|c| c.psf_snr),
        noise: mean_channel(|c| c.noise),
        reg_status,
        reg_model,
        reg_rms_px,
        reg_inliers,
        reg_inlier_ratio,
        reg_flipped,
        rejected_fraction,
        calibrated_path: entry.calibrated.as_ref().map(|p| p.display().to_string()),
        cached_calibrated: entry.cached_calibrated,
        cached_metrics: entry.cached_metrics,
        cached_registration: matches!(
            entry.registration,
            Some(RegisteredFrameOutcome::Aligned { cached: true, .. })
        ),
        ln_scale: entry.ln_scale,
        cached_ln: entry.cached_ln,
        ln_growth_factor: entry.ln_growth_factor,
        ln_growth_pairs: entry.ln_growth_pairs,
        ln_aperture_correction: entry.ln_aperture_correction,
        // Task 11b: the frame's own β, where the two-pass Measure read one (mean across channels, as `fwhm_px`).
        beta_own: m.and_then(|meas| {
            let own: Vec<f64> = meas.channels.iter().filter_map(|c| c.beta_own).collect();
            (!own.is_empty() && own.len() == meas.channels.len())
                .then(|| own.iter().sum::<f64>() / own.len() as f64)
        }),
        beta_own_at_cap: m.is_some_and(|meas| meas.channels.iter().any(|c| c.beta_own_at_cap)),
        field_width_at_floor: m
            .is_some_and(|meas| meas.channels.iter().any(|c| c.field_width_at_floor)),
    }
}

/// Test hook (plan 6b-3 fix round 1): hand a grid drizzle no covered
/// rectangle, to reach its defensive arm — otherwise unreachable once the
/// master passed its own floor. Only project runs read the rectangle, and
/// those tests hold the grid-counters lock.
#[cfg(test)]
static FORCE_NO_DRIZZLE_STATS_RECT: AtomicBool = AtomicBool::new(false);

/// Test hook (plan 6b-3 D10 fix round 1): raise the run's cancel flag after
/// the next group has written, to cancel between two groups. One-shot, project
/// runs only (see [`FORCE_CROP_FAILURE`]).
#[cfg(test)]
pub(super) static CANCEL_AFTER_NEXT_GROUP: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
fn drizzle_stats_rect(
    rect: Option<crate::stacking::crop::CropRect>,
) -> Option<crate::stacking::crop::CropRect> {
    if FORCE_NO_DRIZZLE_STATS_RECT.load(Ordering::SeqCst) {
        None
    } else {
        rect
    }
}

#[cfg(not(test))]
fn drizzle_stats_rect(
    rect: Option<crate::stacking::crop::CropRect>,
) -> Option<crate::stacking::crop::CropRect> {
    rect
}

/// Push one [`SummaryGroup`] for `group` onto `rc.summary.groups` — called
/// exactly once per plan group, whatever its outcome (skipped/failed/
/// written), so the run's provenance document always accounts for every
/// group. `rejected_by_frame` is empty for a skipped/failed group (nothing
/// was ever combined).
#[allow(clippy::too_many_arguments)]
fn push_summary_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    included_count: usize,
    master_path: Option<String>,
    rejection_low_path: Option<String>,
    rejection_high_path: Option<String>,
    stats: Option<GroupStats>,
    normalization_reference_frame_id: Option<i64>,
    rejected_by_frame: &HashMap<i64, f64>,
    ln_reference_path: Option<String>,
    // M3 Task 5: `None` from every call site except the group-Written path
    // in `process_group_output`, which passes whatever its own drizzle
    // attempt (if any) produced.
    drizzle_path: Option<String>,
    weight_map_path: Option<String>,
    drizzle: Option<DrizzleStats>,
    coverage_path: Option<String>,
    drizzle_coverage_path: Option<String>,
    ln_scale_px: Option<u32>,
) {
    let frames = rc
        .measured
        .get(&group.key)
        .map(|entries| {
            entries
                .iter()
                .map(|e| {
                    let rf = rejected_by_frame.get(&e.frame.frame_id).copied();
                    summary_frame_for(e, rf)
                })
                .collect()
        })
        .unwrap_or_default();

    // M4b ruling R-M4b-4: whichever frame this group actually registered
    // onto — its `GroupGeometry` is the one place that knows, in either
    // mode.
    //
    // Ruling R-T3-3 (fix round 2): reported ONLY for a group whose master
    // was WRITTEN. Stage 4 resolves a `GroupGeometry` for every plan group,
    // and a group can still end up with no master at several later points
    // — fewer than 3 included frames at stage 5, fewer than 3 ALIGNED
    // members at integration, a local-normalization narrowing, an
    // integration or write failure. Every one of those reaches here with
    // `master_path: None`, and reporting a reference for any of them would
    // put a `reference #N` on a Results card with nothing behind it. The
    // written-master flag is the one signal that covers them all; fix
    // round 1's "did stage 5 touch this group" test did not (a `Failed`
    // registration outcome is still an outcome).
    let reference_frame_id = master_path.as_ref().and_then(|_| {
        rc.group_geometry
            .get(&group.key)
            .map(|g| g.reference_frame_id)
    });

    // Perf tier C Task 1: whatever stage 3 resolved for this group, if
    // anything — `None` for a group `push_summary_group` is called for
    // before stage 3 ever ran for it (there is no such call site today,
    // but this reads honestly either way).
    let beta = rc.measured_groups.get(&group.key).and_then(|mg| mg.beta);

    // Perf tier C Task 2 fix round 4 (ruling C-14 item 2): the group's own
    // per-channel seeds calibration, `None` for a group whose LN never ran
    // or whose calibration could not be measured.
    let seeds_calibration = rc
        .group_seeds_calibration
        .get(&group.key)
        .map(|c| c.k.clone());

    // Plan B B0 (PB-2): the LN reference's `c` per plane, `None` for a group whose LN never reached its reference.
    let ln_reference_aperture_correction = rc.group_ln_reference_c.get(&group.key).cloned();

    // Spec 2026-10-04 §8: the estimator rides the summary whenever LN ran
    // for the group; the aperture only under the common aperture.
    let ln_local = rc.config.normalization.local;
    let ln_flux_estimator = ln_scale_px.map(|_| ln_local.flux_estimator);
    let ln_aperture = (ln_scale_px.is_some()
        && ln_local.flux_estimator == crate::stacking::integrate::LnFluxEstimator::CommonAperture)
        .then_some(ln_local.aperture);

    rc.summary.groups.push(SummaryGroup {
        key: group.key.clone(),
        frame_count: group.frames.len(),
        included_count,
        master_path,
        rejection_low_path,
        rejection_high_path,
        stats,
        normalization_reference_frame_id,
        reference_frame_id,
        ln_reference_path,
        drizzle_path,
        weight_map_path,
        coverage_path,
        drizzle_coverage_path,
        drizzle,
        beta,
        seeds_calibration,
        ln_scale_px,
        ln_flux_estimator,
        ln_aperture,
        ln_reference_aperture_correction,
        frames,
    });
}

/// Mark a group `skipped` (DB row + `SummaryGroup` with no master) and
/// return [`GroupOutcome::Skipped`] — the shared tail for "fewer than 3
/// included frames reached Output", whether stage 3 already said so or a
/// stage-5 registration exclusion narrowed it further since.
fn skip_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    included_count: usize,
) -> Result<GroupOutcome, RunError> {
    let group_id = *rc.group_ids.get(&group.key).ok_or_else(|| {
        RunError::Other(format!(
            "no stacking_run_groups row for group {}",
            group.key
        ))
    })?;
    {
        let conn = db(&rc.ctx)?.conn();
        update_group(
            &conn,
            group_id,
            &GroupUpdate {
                included_count: Some(included_count as i64),
                status: Some("skipped"),
                ..Default::default()
            },
        )?;
    }
    push_summary_group(
        rc,
        group,
        included_count,
        None,
        None,
        None,
        None,
        None,
        &HashMap::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    Ok(GroupOutcome::Skipped)
}

/// Mark a group `failed` (DB row + `warn!` + `SummaryGroup` with no master)
/// and return [`GroupOutcome::Failed`] — the shared tail of every non-cancel
/// error path in [`process_group_output`] (brief: "any other error → group
/// `failed`, `warn!`, continue with the next group").
fn fail_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    included_count: usize,
    msg: &str,
) -> Result<GroupOutcome, RunError> {
    tracing::warn!(
        run_id = rc.run_id,
        group_key = %group.key,
        error = %msg,
        "group integration failed"
    );
    rc.warnings.push(format!("group {}: {msg}", group.key));
    let group_id = *rc.group_ids.get(&group.key).ok_or_else(|| {
        RunError::Other(format!(
            "no stacking_run_groups row for group {}",
            group.key
        ))
    })?;
    {
        let conn = db(&rc.ctx)?.conn();
        update_group(
            &conn,
            group_id,
            &GroupUpdate {
                included_count: Some(included_count as i64),
                status: Some("failed"),
                error: Some(msg),
                ..Default::default()
            },
        )?;
    }
    push_summary_group(
        rc,
        group,
        included_count,
        None,
        None,
        None,
        None,
        None,
        &HashMap::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    Ok(GroupOutcome::Failed)
}

/// Shared, lock-protected state one group's `Integrate`-stage progress
/// ticks are latched against (fix round 2). A run emits one group's
/// `Integrate` progress at a time (`stage_output`'s group loop is
/// sequential), so a single instance scoped to one [`process_group_output`]
/// call — not a `HashMap<(Stage, Option<String>), _>` spanning the whole
/// run — is enough to key the latch by `(stage, group_key)`: this state
/// itself never sees a second `(stage, group_key)` pair.
struct IntegrateTickState {
    last_emit: Instant,
    /// The highest `percent` emitted so far — monotonic across the WHOLE
    /// group (by construction: `plane` only advances, `frac` is always in
    /// `[0, 1]`, so a stale/out-of-order tick's percent can never exceed
    /// the current watermark).
    last_percent: f64,
    /// Which plane `last_bytes_done` belongs to.
    last_plane: usize,
    /// The highest `bytes_done` emitted so far FOR `last_plane` — scoped to
    /// one plane, not the whole group, because `bytes_done`/`bytes_total`
    /// are the engine's own PER-PLANE pair (`EngineProgress::on_band`'s own
    /// doc: they reset every plane) — latching it across a plane boundary
    /// would silently drop every tick of a new plane, since a fresh plane
    /// always starts back at `bytes_done: 0`.
    last_bytes_done: u64,
    /// The plane's combine progress in ROWS — the one measure that moves
    /// through the whole plane (reading a band is ≈ 10 % of its time,
    /// combining it the rest), so it is what `frac` is built from on both
    /// the read and the combine tick. Before this tracker the combine tick
    /// passed `frac = 1.0` and a one-plane group sat at 100 % for the whole
    /// combine while only the bytes text moved (owner report 2026-09-20).
    rows: CombineRowsTracker,
}

impl IntegrateTickState {
    fn new() -> Self {
        IntegrateTickState {
            last_emit: Instant::now() - Duration::from_millis(PROGRESS_THROTTLE_MS),
            last_percent: f64::NEG_INFINITY,
            last_plane: 0,
            last_bytes_done: 0,
            rows: CombineRowsTracker::default(),
        }
    }
}

/// Rows combined so far in the CURRENT plane, folded from the engine's two
/// callbacks: `on_band(k, …)` fires when band `k` (1-based) has finished
/// READING and is about to combine; `on_combine(done, h)` reports rows
/// combined WITHIN that band (`h` = the band's height, `done` reaches `h`
/// on its last tick and may repeat). A band's rows are committed exactly
/// once — on the first tick that reports `done == h` for a band not yet
/// committed — so a repeated final tick, or a band whose ticks never reach
/// `h`, cannot double-count; `frac` for the plane is `rows() / plane_rows`.
/// Monotone by construction, which is what the max-latch in
/// [`emit_integrate_tick`] needs from both callbacks: the read tick of band
/// `k+1` reports the same `rows()` as band `k`'s last combine tick, never
/// less, so it is never dropped and its bytes text still updates.
#[derive(Default)]
struct CombineRowsTracker {
    /// Rows of every band already committed.
    committed: u64,
    /// The band the last committed rows belong to (0 = none yet).
    committed_band: usize,
    /// The band currently combining, from the last `on_band` tick.
    cur_band: usize,
    /// `done` of the current band's last tick, 0 after a commit.
    cur_done: u64,
}

impl CombineRowsTracker {
    /// A new plane starts: nothing combined yet.
    fn on_plane(&mut self) {
        *self = CombineRowsTracker::default();
    }

    /// Band `band` (1-based) finished reading; a `0` is the engine's forecast
    /// tick before pass 2's first band and names no band.
    fn on_band(&mut self, band: usize) {
        if band > 0 && band != self.cur_band {
            self.cur_band = band;
            self.cur_done = 0;
        }
    }

    /// `done` of `h` rows of the current band are combined.
    fn on_combine(&mut self, done: u64, h: u64) {
        if self.cur_band == 0 || self.committed_band == self.cur_band {
            // No band announced yet, or this band is already committed (a
            // repeated final tick): nothing to add.
            return;
        }
        if done >= h && h > 0 {
            self.committed += h;
            self.committed_band = self.cur_band;
            self.cur_done = 0;
        } else {
            self.cur_done = done.min(h);
        }
    }

    /// Rows combined so far in this plane.
    fn rows(&self) -> u64 {
        self.committed + self.cur_done
    }

    /// `rows() / plane_rows`, clamped to `[0, 1]`; `0` for a plane with no
    /// rows (never true of a real plane).
    fn frac(&self, plane_rows: u64) -> f64 {
        if plane_rows == 0 {
            0.0
        } else {
            (self.rows() as f64 / plane_rows as f64).min(1.0)
        }
    }
}

/// One `Integrate`-stage progress tick, throttled AND max-latched against
/// `state`. Percent = `100 * (plane + frac) / channels`. A free function
/// (not a closure borrowing `RunContext`) so `on_plane`/`on_band`/`on_combine`
/// — `Sync` closures the engine may call from any of its own worker threads
/// — can call it with only cloned-out plain values, and so it is directly
/// unit-testable on its own (see `emit_integrate_ticks_never_race_percent_backwards`
/// below).
///
/// Fix round 1, item 1: the throttle guard (`state.lock()`) stays held
/// THROUGH the emit, not dropped before it — this makes ONE caller's own
/// check/update/compute/emit sequence indivisible, so two ticks can never
/// interleave or land torn. It does NOT, on its own, stop a worker that
/// read a LOWER `(plane, frac)` and was then descheduled from taking the
/// lock AFTER a higher tick already went out and emitting the lower value —
/// atomicity of one critical section says nothing about the relative order
/// of two SEPARATE critical sections whose inputs were computed outside
/// the lock. Fix round 2 closes that: a max-latch, held under the SAME
/// lock as the throttle state, drops (never clamps — clamping would emit
/// a duplicate percent under a fresh, misleadingly-newer `bytes_done`) any
/// tick whose `percent` (whole-group scope) or `bytes_done` (current-plane
/// scope) regresses relative to what has already been emitted for this
/// `(stage, group_key)`. `force` bypasses the THROTTLE only — the latch
/// always applies, so the stage's first (`force: true`, plane 0/frac 0) and
/// last (`force: true`, `percent: 100.0`) events are still guaranteed to be
/// the true minimum/maximum rather than merely unthrottled. Emitting under
/// the lock is cheap (a channel send / IPC call); a slow emitter only slows
/// the engine's own callbacks, the same tradeoff `OUTPUT_WRITE_LOCK` makes
/// for the master write.
#[allow(clippy::too_many_arguments)]
fn emit_integrate_tick(
    state: &Mutex<IntegrateTickState>,
    emitter: &dyn ProgressEmitter,
    run_id: i64,
    subject: &StackSubject,
    group_key: &str,
    channels: usize,
    plane: usize,
    frac: f64,
    bytes_done: u64,
    bytes_total: u64,
    force: bool,
    message: Option<String>,
) {
    let now = Instant::now();
    let percent = 100.0 * (plane as f64 + frac) / channels.max(1) as f64;
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());

    // Max-latch (fix round 2), checked BEFORE the throttle so a dropped
    // tick never resets `last_emit` either. `percent` regressing drops the
    // tick outright, whatever plane it claims to be from. Otherwise: moving
    // to a strictly LATER plane re-arms the bytes_done watermark at 0 (a
    // fresh plane's own counter); staying on the SAME plane still latches
    // `bytes_done`; a tick claiming an EARLIER plane than what is already
    // recorded has, by construction, a `percent` that cannot exceed the
    // current watermark (plane only advances, frac in [0, 1]) — it is
    // already caught by the percent check above and never reaches here.
    if percent < s.last_percent {
        return;
    }
    if plane > s.last_plane {
        s.last_plane = plane;
        s.last_bytes_done = 0;
    } else if bytes_done < s.last_bytes_done {
        return;
    }

    if !force && now.duration_since(s.last_emit) < Duration::from_millis(PROGRESS_THROTTLE_MS) {
        return;
    }

    s.last_emit = now;
    s.last_percent = percent;
    s.last_bytes_done = bytes_done;
    // After the throttle: a dropped tick never builds the subject pair.
    let (set_id, project_id) = wire_subject(subject);
    // `s` (the `MutexGuard`) stays alive through the emit below — fix round
    // 1's own point still holds: the watermark update above only decides
    // WHICH tick is allowed to claim the next slot, not what order two
    // already-claimed emits reach the recorder in. Dropping the guard here
    // would let a second thread claim the NEXT slot and race this thread's
    // own `emit_event` call, reopening exactly the interleaving fix round 1
    // closed (a higher, later-claimed tick landing before this one's).

    emit_event(
        emitter,
        STACKING_PROGRESS_EVENT,
        &StackingProgressEvent {
            run_id,
            set_id,
            project_id,
            stage: Stage::Integrate,
            group_key: Some(group_key.to_string()),
            current: plane,
            total: channels,
            percent,
            bytes_done,
            bytes_total,
            frame_id: None,
            message,
        },
    );
}

/// The Integrate row's own words for a tick (v0.6.3): which plane of how
/// many, a `pass 2` marker when large-scale rejection integrates the group
/// twice (`plane >= channels`), and the band or the combine step — the
/// row used to show a bare percentage for a stage that takes twenty
/// minutes on a real set.
fn integrate_tick_message(
    plane: usize,
    planes_total: usize,
    channels: usize,
    step: &str,
) -> String {
    let channels = channels.max(1);
    let pass = if planes_total > channels && plane >= channels {
        " · pass 2"
    } else {
        ""
    };
    format!("plane {}/{channels}{pass} · {step}", plane % channels + 1)
}

/// Throttle state for the Drizzle stage's per-frame progress ticks (M3 Task
/// 5, fix round 1, Important I1). Unlike [`IntegrateTickState`], no
/// max-latch is needed: `drizzle_group` calls `DrizzleProgress::on_frame`
/// strictly sequentially, from one control-flow thread — only the per-frame
/// pixel DEPOSIT fans out over `pool`, never `on_frame` itself (see
/// `drizzle::mod::drizzle_group`'s own per-frame loop) — so `done_units`
/// only ever increases, unlike Integrate's per-band ticks racing across
/// worker threads. `Mutex` only because `DrizzleProgress::on_frame` must be
/// `Sync` to satisfy the trait bound.
/// Which per-frame rejection bitmaps the Drizzle stage reads (M4c Task 3,
/// fix round 1, ruling R-T3-1): the bits the MASTER was built with, always.
///
/// With the large-scale second pass on, that is the SECOND pass's own set
/// (`rej/run-<id>/<group>/pass2/…`, [`RejBitmapSet::second_pass_path`]) —
/// the forced structures AND whatever pass 2's own per-pixel tests rejected,
/// i.e. exactly the samples the returned master left out. Without it, the
/// first pass's set is that record, unchanged from M3. The processed
/// `.rejl` files are neither: they are the intermediate the forced set was
/// derived FROM, and carry no algorithmic rejection at all.
///
/// `second_pass_rej_ok` false with the second pass having run means those
/// files are missing or incomplete — the caller treats that as a bitmap
/// failure and skips drizzle for the group rather than handing it pass 1's
/// bits, which describe a different integration.
fn drizzle_rejection_paths(
    set: Option<&RejBitmapSet>,
    included: usize,
    second_pass_rej_ok: bool,
) -> Vec<Option<PathBuf>> {
    (0..included)
        .map(|k| {
            set.map(|s| {
                if second_pass_rej_ok {
                    s.second_pass_path(k)
                } else {
                    s.path(k).to_path_buf()
                }
            })
        })
        .collect()
}

struct DrizzleTickState {
    last_emit: Instant,
}

impl DrizzleTickState {
    fn new() -> Self {
        DrizzleTickState {
            last_emit: Instant::now() - Duration::from_millis(PROGRESS_THROTTLE_MS),
        }
    }
}

/// One `Drizzle`-stage progress tick, throttled like every other stage's
/// (`PROGRESS_THROTTLE_MS`; `force` bypasses it). A free function, not
/// `RunContext::progress`, because `DrizzleProgress::on_frame` is a `Sync`
/// closure `drizzle_group` calls with no `&mut RunContext` in reach (the
/// drizzle block re-borrows `rc.cancel`/`rc.ctx.image_pool` immutably for
/// the call, so nothing here may need `&mut rc` either) — mirrors
/// [`emit_integrate_tick`]'s shape, minus the max-latch (see
/// [`DrizzleTickState`]'s own doc for why one isn't needed here).
#[allow(clippy::too_many_arguments)]
fn emit_drizzle_tick(
    state: &Mutex<DrizzleTickState>,
    emitter: &dyn ProgressEmitter,
    run_id: i64,
    subject: &StackSubject,
    group_key: &str,
    current: usize,
    total: usize,
    frame_id: Option<i64>,
    force: bool,
) {
    let now = Instant::now();
    let percent = if total == 0 {
        100.0
    } else {
        100.0 * current as f64 / total as f64
    };
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    if !force && now.duration_since(s.last_emit) < Duration::from_millis(PROGRESS_THROTTLE_MS) {
        return;
    }
    s.last_emit = now;
    let (set_id, project_id) = wire_subject(subject);
    emit_event(
        emitter,
        STACKING_PROGRESS_EVENT,
        &StackingProgressEvent {
            run_id,
            set_id,
            project_id,
            stage: Stage::Drizzle,
            group_key: Some(group_key.to_string()),
            current,
            total,
            percent,
            bytes_done: 0,
            bytes_total: 0,
            frame_id,
            message: None,
        },
    );
}

/// The drizzle stage's own failure classification (M3 Task 5), folding
/// every fallible step past `drizzle_group` itself (scaling the WCS,
/// building the cards, writing the file) into the SAME two-way split
/// ruling R-M3-7 draws for `drizzle_group`'s own [`DrizzleError`]:
/// `Cancelled` propagates as the run's cancel, everything else is a
/// per-group warning — the master is already written and stays untouched
/// either way.
enum DrizzleFailure {
    Cancelled,
    Other(String),
}

impl From<DrizzleError> for DrizzleFailure {
    fn from(e: DrizzleError) -> Self {
        match e {
            DrizzleError::Cancelled => DrizzleFailure::Cancelled,
            other => DrizzleFailure::Other(other.to_string()),
        }
    }
}

/// The result of one `pick_reference_idx` call (ruling R-M3-17 v2 + its
/// coverage addendum, fix round 1 Important 3): `idx` is always a valid
/// index into the `members` slice the closure was called with — a group
/// with `>= 3` members always gets an anchor, restoring the pre-this-
/// ruling guarantee `best_by_weight` used to provide unconditionally.
/// `coverage_dropped`/`sky_fallback` are `Some(count)` when the closure
/// had to relax ITS OWN filter to reach that guarantee (fewer than 3
/// members covered the reference geometry; or every covering member's
/// background was non-finite/non-positive) — the caller (which alone holds
/// `&mut rc`) turns either into a `warn!` + a run warning.
struct AnchorPick {
    idx: usize,
    coverage_dropped: Option<usize>,
    sky_fallback: Option<usize>,
}

/// Normalize (stage 6, M2 Task 5 — [`run_group_normalization`]), integrate
/// and write the master for one group. LN DISABLED keeps the exact pre-M2
/// shape: a static 1/1 "Normalize" progress tick and no other effect (spec
/// ruling 14). LN enabled resolves/caches the group's LN reference and
/// per-frame `.athln` sidecars (real per-frame progress); a frame LN could
/// not measure is excluded here when LN drives OUTPUT normalization,
/// narrowing `members` before integration ever sees it (see
/// [`run_group_normalization`]'s own doc). Returns the outcome plus
/// (normalize, integrate, output) wall time for [`stage_output`]'s own
/// per-stage [`crate::stacking::provenance::StageTiming`] totals — ruling R5
/// (reverses fix round 1, item 6): the `normalize` component is
/// `run_group_normalization`'s own wall time (reference build + fan-out),
/// timed here because the acceptance run found it the single most expensive
/// stage and therefore not one `RunSummary.stages` can leave invisible;
/// `stage_output` only pushes a `Normalize` `StageTiming` when the group's
/// own config had LN active (disabled stays a bare progress tick, nothing
/// worth reporting).
/// `Err(RunError::Cancelled)` — from `IntegrationError::Cancelled` or a
/// cancel noticed before this group started — is the ONLY error that
/// propagates; everything else becomes `Ok((GroupOutcome::Failed, ..))` via
/// [`fail_group`], per ruling 12 (a cancelled run keeps its artifacts and
/// writes no master) vs. the brief's per-group failure policy.
fn process_group_output(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    measure_opts: &MeasureOptions,
    wcs: Option<&PlateSolveRecord>,
) -> Result<(GroupOutcome, Duration, Duration, Duration, Duration), RunError> {
    let zero = || {
        (
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        )
    };

    // Plan 6b-2 C9: the Output boundary — the member snapshot below reads
    // the files Normalize, Integrate and Drizzle work from.
    exclude_changed_members(rc, group)?;

    let included_count = rc
        .measured
        .get(&group.key)
        .map(|v| v.iter().filter(|e| e.included).count())
        .unwrap_or(0);
    if included_count < 3 {
        let (n, i, d, o) = zero();
        return Ok((skip_group(rc, group, included_count)?, n, i, d, o));
    }

    // Snapshot the included members' data — nothing below needs `rc` again
    // until the write completes, so no borrow of it needs to survive the
    // (potentially slow) integration call.
    let mut members: Vec<GroupMember> = Vec::with_capacity(included_count);
    if let Some(entries) = rc.measured.get(&group.key) {
        for e in entries {
            if !e.included {
                continue;
            }
            let Some(calibrated) = e.calibrated.clone() else {
                continue;
            };
            let Some(measurement) = e.measurement.clone() else {
                continue;
            };
            let (map, registration_hash) = match &e.registration {
                Some(RegisteredFrameOutcome::Aligned {
                    map,
                    registered_hash,
                    ..
                }) => (map.clone(), registered_hash.clone()),
                _ => continue,
            };
            let weight = e.weight.clone().unwrap_or(FrameWeight {
                channels: Vec::new(),
                normalized: Vec::new(),
                mean: 0.0,
                normalized_mean: 0.0,
                missing: None,
            });
            // Perf tier A Task 6a (fix round 1: both register fan-out
            // closures — the align-and-write one and the cache-hit
            // rewrite-only one, see `register_group_pass`): this snapshot is
            // taken AFTER stage 5 has already written (or refreshed) every
            // included, aligned member's registered artifact, so a fresh
            // row here is the ordinary case; `None` (the write failed, or a
            // config predating this task never ran stage 5 again) just
            // means every reader below falls back to the on-the-fly warp
            // for this one frame, silently (nothing was ever expected on
            // disk).
            //
            // Deliberately HASH-ONLY, not the full [`is_fresh`] disk check
            // stage 5's own [`registered_artifact_is_fresh`] uses to decide
            // whether a REWRITE is needed: the actual "is the file usable
            // right now" question belongs to
            // `RegisteredSource::open_materialized`'s own `PlaneReader::open`
            // attempt at read time, which is what turns a file that
            // vanished AFTER this snapshot (deleted mid-run, a cleanup, a
            // foreign edit) into the loud, warned fallback rather than a
            // silent one — re-`stat`-ing here would only race that same
            // check and answer it twice.
            let registered_path = {
                let conn = db(&rc.ctx)?.conn();
                crate::db::stacking::find_artifact(
                    &conn,
                    &rc.subject,
                    &group.key,
                    ArtifactKind::Registered,
                    Some(e.frame.frame_id),
                )?
                .filter(|row| row.config_hash == registration_hash)
                .and_then(|row| row.path.map(PathBuf::from))
            };
            members.push(GroupMember {
                frame_id: e.frame.frame_id,
                filename: e.frame.filename.clone(),
                calibrated,
                map,
                measurement,
                weight,
                exposure_s: e.frame.exposure_s,
                date_obs: e.frame.date_obs.clone(),
                registered_path,
                registration_hash,
            });
        }
    }

    if members.len() < 3 {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = members.len(),
            "stacking group skipped: fewer than 3 registered members reached integration"
        );
        let (n, i, d, o) = zero();
        return Ok((skip_group(rc, group, members.len())?, n, i, d, o));
    }

    // Ruling R-M3-17 v2 (supersedes Ruling 7): the set's reference (manual
    // pin or auto best-by-weight, `rc.reference_frame_id`) is the
    // REGISTRATION reference only — it no longer anchors normalization just
    // because it happens to be a member of this group. Every group's
    // NORMALIZATION anchor is chosen sky-penalized instead
    // (`sky_penalized_order`, spec §4.4): among the group's admissible
    // members, the one with the highest `weight / sqrt(background)` wins —
    // faint-signal SNR scales as `1/sqrt(background)`, so a bright-sky
    // top-weight member no longer outranks a slightly-lower-weight
    // dark-sky one (LDN 1272, 2026-09-10: the OSC group's normalization
    // reference was the top-PSF-weight member of a 3x-brighter-sky night;
    // the master inherited that night's sky level).
    //
    // Addendum: a member whose registered footprint does not cover the
    // reference rectangle (`reference_coverage` < `MIN_REFERENCE_COVERAGE`
    // — a rotated frame, a mismatched camera angle, a badly offset one) is
    // not admissible either — normalizing to it (or modelling the LN
    // reference's background on it) would carry its uncovered corners/
    // edges into every other frame. The coverage filter itself is dropped
    // for a group where fewer than 3 members would pass it (a `warn!` +
    // run warning), rather than ranking among too few candidates to mean
    // anything.
    //
    // Factored into a closure (not just a one-shot call) because stage 6's
    // LN pass, below, can narrow `members` further (a `TooFewMatches`
    // exclusion when LN drives OUTPUT normalization) — the anchor is
    // re-derived from whichever members survive that. `run_id`/`ref_width`/
    // `ref_height`/`group` are captured by copy/shared reference (never
    // `rc` itself), so the closure holds no live borrow of `rc` across the
    // mutable uses of it between the two call sites below — the coverage
    // filter's "dropped" case is instead returned to the caller, which
    // pushes the run warning itself (the closure can only `tracing::warn!`,
    // never touch `rc.warnings`).
    let run_id = rc.run_id;
    let (ref_width, ref_height) = {
        let geometry = rc.geometry_of(&group.key);
        (geometry.width, geometry.height)
    };
    let ln_field = group_ln_field_for(rc, group)?;
    let pick_reference_idx = |members: &[GroupMember]| -> Option<AnchorPick> {
        let weights: Vec<FrameWeight> = members.iter().map(|m| m.weight.clone()).collect();
        let star_counts: Vec<usize> = members.iter().map(|m| m.measurement.min_stars()).collect();
        let sky: Vec<f64> = members
            .iter()
            .map(|m| m.measurement.mean_median())
            .collect();

        let coverage = member_coverages(ln_field.as_ref(), members, ref_width, ref_height);
        let coverage_mask: Vec<bool> = coverage
            .iter()
            .map(|&c| c >= MIN_REFERENCE_COVERAGE)
            .collect();
        let admissible_count = coverage_mask.iter().filter(|&&ok| ok).count();
        let (included_mask, coverage_dropped) = if admissible_count >= 3 {
            (coverage_mask, None)
        } else {
            (vec![true; members.len()], Some(admissible_count))
        };
        if let Some(count) = coverage_dropped {
            tracing::warn!(
                run_id,
                group_key = %group.key,
                count,
                min_coverage = MIN_REFERENCE_COVERAGE,
                "reference-coverage filter dropped for this group: fewer than 3 members pass"
            );
        }

        // Fix round 1, Important 3: `sky_penalized_order` returning empty
        // (every admissible member's background non-finite/non-positive —
        // e.g. a narrowband set whose dark slightly over-subtracts every
        // frame) used to fail the WHOLE GROUP here. A group with `>= 3`
        // members always got an anchor before this ruling existed
        // (`best_by_weight` never refuses a non-empty candidate set); this
        // restores that guarantee by falling back to it over the SAME
        // `included_mask` (still coverage-filtered) the sky-penalized rank
        // used, rather than failing the group over a background
        // measurement problem the anchor pick itself can't fix.
        let ordered = sky_penalized_order(&weights, &included_mask, &star_counts, &sky);
        let (idx, sky_fallback) = match ordered.into_iter().next() {
            Some(idx) => (idx, None),
            None => {
                let count = included_mask.iter().filter(|&&ok| ok).count();
                tracing::warn!(
                    run_id,
                    group_key = %group.key,
                    count,
                    "no admissible member has a usable measured background; \
                     anchoring by weight alone"
                );
                let idx = best_by_weight(&weights, &included_mask, &star_counts)?;
                (idx, Some(count))
            }
        };
        tracing::info!(
            run_id,
            group_key = %group.key,
            frame_id = members[idx].frame_id,
            weight = weights[idx].normalized_mean,
            background = sky[idx],
            "normalization anchor chosen"
        );
        Some(AnchorPick {
            idx,
            coverage_dropped,
            sky_fallback,
        })
    };
    // Same shape as `StackFrame`'s own construction from a `GroupMember`
    // list — a closure, not a one-shot `.map()`, because the LN pass can
    // force this to run a second time over a narrowed `members`.
    let build_stack_frames = |members: &[GroupMember]| -> Vec<StackFrame> {
        members
            .iter()
            .map(|m| StackFrame {
                path: m.calibrated.clone(),
                map: m.map.clone(),
                measurement: m.measurement.clone(),
                weight: m.weight.clone(),
                exposure_s: m.exposure_s.unwrap_or(0.0),
                date_obs: m.date_obs.clone(),
                frame_id: m.frame_id,
                registered_path: m.registered_path.clone(),
            })
            .collect()
    };

    let mut reference_idx = match pick_reference_idx(&members) {
        Some(pick) => {
            if let Some(count) = pick.coverage_dropped {
                rc.warnings.push(format!(
                    "group {}: reference-coverage filter dropped for normalization anchoring — \
                     only {count} member(s) cover the reference geometry (need >= 3)",
                    group.key
                ));
            }
            if let Some(count) = pick.sky_fallback {
                rc.warnings.push(format!(
                    "group {}: no admissible member has a usable measured background — \
                     anchored by weight alone among {count} candidate(s)",
                    group.key
                ));
            }
            pick.idx
        }
        None => {
            let (n, i, d, o) = zero();
            return Ok((
                fail_group(
                    rc,
                    group,
                    members.len(),
                    "no included frame to anchor normalization",
                )?,
                n,
                i,
                d,
                o,
            ));
        }
    };
    let mut normalization_reference_frame_id = members[reference_idx].frame_id;

    // Ruling 6, generalized by M4b ruling R-M4b-5: every group's master
    // adopts ITS OWN reference's geometry — the run-wide reference's in
    // co-registered mode (where every group's entry is that same one), the
    // group's own in native mode. The group's OWN plane count (mono vs.
    // debayered OSC) is whatever this group's own measurements carry.
    let mut channels = members[reference_idx].measurement.channels.len();
    let (width, height) = (ref_width, ref_height);

    let mut stack_frames: Vec<StackFrame> = build_stack_frames(&members);

    // Maps memory check (carry-forward): turn maps off for THIS group with a
    // warning rather than fail, when they would not fit the budget.
    let mut group_integration = rc.config.integration.clone();
    if group_integration.write_rejection_maps {
        let maps_bytes = 3u64 * channels as u64 * width as u64 * height as u64 * 4;
        // Fix round 1, item 5: unknown RAM is treated as NOT fitting
        // (conservative — the same reading `admission`'s own "unknown → 1"
        // fallback takes), not as fitting.
        let fits = total_ram_bytes()
            .map(|total| maps_bytes <= total / 4)
            .unwrap_or(false);
        if !fits {
            group_integration.write_rejection_maps = false;
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                "rejection maps disabled for this group: over the memory budget"
            );
            rc.warnings.push(format!(
                "group {}: rejection maps disabled — would exceed the memory budget",
                group.key
            ));
        }
    }
    let normalization_cfg = rc.config.normalization.clone();

    let input = GroupInput {
        frames: &stack_frames,
        reference: reference_idx,
        width,
        height,
        channels,
        interpolation: rc.config.registration.interpolation,
        clamping: rc.config.registration.clamping_threshold,
        integration: &group_integration,
        normalization: &normalization_cfg,
        // Stage 6 has not run yet at this point — nothing to hand
        // `integrate_group` until the LN pass below (if any) resolves real
        // sidecars; the second `GroupInput` further down carries them.
        ln: None,
        // M3 Task 2: this `input` only ever reaches `run_group_normalization`
        // (the LN pass), never `integrate_group` — the real `GroupInput`
        // further down is the one a `RejBitmapSet` (Task 5) will attach to.
        rej: None,
        second_pass_bitmaps: false,
        coverage_map: false,
        stats_on_covered: false,
        // Plan D D1b: this `input` only reaches the LN pass, which reads no statistics - the real one below carries the set.
        common_stars: None,
    };

    let paths: Vec<PathBuf> = members.iter().map(|m| m.calibrated.clone()).collect();
    let io: IoPolicy = {
        let conn = db(&rc.ctx)?.conn();
        crate::integration::io_policy::resolve(
            &conn,
            &rc.ctx.settings,
            &paths,
            rc.ctx.image_pool.current_num_threads(),
        )?
        // Task 13c L3: registered artifacts are positional file reads, so
        // Integrate and the LN reference build prefetch on every class.
        .with_local_prefetch()
    };

    // Stage 6 (local normalization, M2 Task 5): resolves/caches the group's
    // LN reference, models its backgrounds once, then fans `normalize_frame`
    // out over the included members. When LN drives OUTPUT normalization
    // (`normalization.local.enabled`) a frame whose relative scale could not
    // be measured is excluded here — that narrows `members` below what
    // `reference_idx`/`stack_frames`/`input` above were built from, so all
    // three are rebuilt from the surviving members before integration ever
    // sees them. LN DISABLED keeps the exact pre-M2 behaviour: a static
    // 1/1 "Normalize" progress tick and nothing else (spec ruling 14).
    // Ruling R5 (reverses fix round 1, item 6): the acceptance run measured
    // LN as the run's single most expensive stage (≈28 of 49 min) — a stage
    // that costs more than every other one combined cannot stay invisible
    // in `RunSummary.stages`, so its wall time IS timed here
    // (`normalize_start`, wrapping the whole match below — reference build +
    // fan-out, i.e. `run_group_normalization`'s own wall time, never folded
    // into `integrate_dur`) and pushed by `stage_output` as a `Normalize`
    // `StageTiming`, but only when `ln_active` (disabled stays invisible —
    // the pre-M2 shape — since it is genuinely a no-op tick).
    let ln_active = normalization_cfg.local.enabled
        || normalization_cfg.rejection == RejectionNormalization::Local;
    let normalize_start = Instant::now();
    let mut normalize_meter = ln_active.then(|| {
        crate::stacking::telemetry::StageMeter::start(rc, Stage::Normalize, Some(&group.key))
    });
    let mut ln_reference_path: Option<String> = None;
    let mut ln_scale_px: Option<u32> = None;
    // M2 Task 7 (fix round 1, item 3): every member `run_group_normalization`
    // itself confirmed a READABLE `ln` artifact for THIS run — verified at
    // the source (the cache-hit branch re-reads the artifact row's own file
    // before trusting it; the fan-out branch re-reads what it just wrote),
    // so nothing past this point ever opens a file or handles a read
    // failure: an unreadable sidecar never reaches `process_group_output`
    // at all — it is either re-normalized in the same run or already
    // folded into `excluded_frame_ids` below. `None` (never populated)
    // whenever LN never actually ran for the group at all
    // (`LnGroupOutcome.reference_path` is `None`: disabled, or a
    // ruling-R3-shaped fallback to global normalization — carry-over (a),
    // Task 5's re-review: `Some` means "LN ran", full stop).
    let mut ln_sidecar_grids: Option<HashMap<i64, LnFrameGrids>> = None;
    if ln_active {
        match run_group_normalization(
            rc,
            group,
            &members,
            &input,
            measure_opts,
            io,
            ln_field.as_ref(),
        ) {
            Ok(outcome) => {
                ln_reference_path = outcome.reference_path.clone();
                ln_scale_px = outcome.scale_px;
                if outcome.reference_path.is_some() {
                    ln_sidecar_grids = Some(outcome.sidecar_grids);
                }
                if !outcome.excluded_frame_ids.is_empty() {
                    let excluded: HashSet<i64> = outcome.excluded_frame_ids.into_iter().collect();
                    members.retain(|m| !excluded.contains(&m.frame_id));
                    if members.len() < 3 {
                        tracing::warn!(
                            run_id = rc.run_id,
                            group_key = %group.key,
                            count = members.len(),
                            "stacking group skipped: local normalization left fewer than 3 members"
                        );
                        finish_meter(&mut normalize_meter);
                        let n = normalize_start.elapsed();
                        let (_, i, d, o) = zero();
                        return Ok((skip_group(rc, group, members.len())?, n, i, d, o));
                    }
                    reference_idx = match pick_reference_idx(&members) {
                        Some(pick) => {
                            if let Some(count) = pick.coverage_dropped {
                                rc.warnings.push(format!(
                                    "group {}: reference-coverage filter dropped for \
                                     normalization anchoring — only {count} member(s) cover \
                                     the reference geometry (need >= 3)",
                                    group.key
                                ));
                            }
                            if let Some(count) = pick.sky_fallback {
                                rc.warnings.push(format!(
                                    "group {}: no admissible member has a usable measured \
                                     background — anchored by weight alone among {count} \
                                     candidate(s)",
                                    group.key
                                ));
                            }
                            pick.idx
                        }
                        None => {
                            finish_meter(&mut normalize_meter);
                            let n = normalize_start.elapsed();
                            let (_, i, d, o) = zero();
                            return Ok((
                                fail_group(
                                    rc,
                                    group,
                                    members.len(),
                                    "no included frame to anchor normalization",
                                )?,
                                n,
                                i,
                                d,
                                o,
                            ));
                        }
                    };
                    normalization_reference_frame_id = members[reference_idx].frame_id;
                    channels = members[reference_idx].measurement.channels.len();
                    stack_frames = build_stack_frames(&members);
                }
            }
            Err(RunError::Cancelled) => return Err(RunError::Cancelled),
            // Carry-over (b), Task 5's re-review: this can only reach
            // `process_group_output` after `exclude_frame_and_persist`
            // already flipped a frame's in-memory state — the group cannot
            // safely continue as if that exclusion never happened, so it
            // fails outright (same shape every other group-fatal condition
            // in this function uses) rather than falling into the generic
            // "warn and continue with global normalization" arm below.
            Err(RunError::ExclusionPersistFailed(msg)) => {
                finish_meter(&mut normalize_meter);
                let n = normalize_start.elapsed();
                let (_, i, d, o) = zero();
                return Ok((
                    fail_group(
                        rc,
                        group,
                        members.len(),
                        &format!("local normalization: persisting a frame exclusion failed: {msg}"),
                    )?,
                    n,
                    i,
                    d,
                    o,
                ));
            }
            Err(RunError::Other(msg)) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %msg,
                    "local normalization failed for this group; continuing with global normalization"
                );
                rc.warnings.push(ln_fallback_warning(
                    &group.key,
                    !normalization_cfg.local.enabled,
                    &LnFallback::GroupFailed { msg: &msg },
                ));
            }
        }
    } else {
        rc.progress(
            Stage::Normalize,
            Some(group.key.clone()),
            1,
            1,
            0,
            0,
            None,
            None,
        );
    }
    finish_meter(&mut normalize_meter);
    let normalize_dur = normalize_start.elapsed();

    // M2 Task 7 (fix round 1, item 3): `GroupInput.ln`, aligned with
    // `members`' (possibly LN-narrowed) order, built directly from the
    // already-verified grids `run_group_normalization` handed back — no
    // file I/O and no exclusion handling here, both moved to the source
    // (see `ln_sidecar_grids`'s own doc above). A member absent from the
    // map (kept with a warning while LN drives rejection only, after its
    // OWN `normalize_frame` call failed this run, or a plan 6b-3 D7
    // low-overlap member that keeps global normalization) gets `None`. `None`
    // (the whole field) whenever LN never ran for the group at all,
    // matching every M1 caller byte-for-byte.
    let ln_grids: Option<Vec<Option<LnFrameGrids>>> = ln_sidecar_grids
        .map(|mut grids| members.iter().map(|m| grids.remove(&m.frame_id)).collect());

    // M3 Task 5 (spec Section 6.2, ruling R-M3-8): when drizzle wants the
    // per-frame rejection survivor mask, the RUN — not `integrate_group` —
    // creates the bitmap set, sized and ordered to the SAME included set
    // `integrate_group` will compute internally via the extracted
    // `included_after_min_weight` rule (called here with the exact same
    // `stack_frames`/`reference_idx`/`min_weight` `integrate_group` itself
    // will use, so the two can never disagree about which frames survive
    // the floor). A `create` failure skips this group's drizzle entirely,
    // with a warning below (ruling text: "the master still integrates,
    // rej: None") — `rej_set_failure` carries the reason for that later
    // warning.
    //
    // M4c Task 3 (ruling R-M4c-4) widens the condition: large-scale
    // rejection derives its structures from the SAME bitmaps, so it needs
    // the set even with drizzle off. `group_integration` (not
    // `rc.config.integration`) is read for the flag on purpose — it is the
    // config `integrate_group` itself will see, so its own
    // `large_scale_passes` answer can never disagree with this one.
    let bitmaps_for_drizzle = rc.config.drizzle.enabled && rc.config.drizzle.use_rejection;
    let bitmaps_for_large_scale = group_integration.large_scale.enabled;
    // Which consumer(s) a bitmap failure would cost, for an honest warning.
    let bitmap_consumers = match (bitmaps_for_drizzle, bitmaps_for_large_scale) {
        (true, true) => "drizzle and large-scale rejection",
        (true, false) => "drizzle",
        _ => "large-scale rejection",
    };
    let mut rej_set: Option<RejBitmapSet> = None;
    let mut rej_set_failure: Option<String> = None;
    if bitmaps_for_drizzle || bitmaps_for_large_scale {
        let included_for_rej =
            included_after_min_weight(&stack_frames, reference_idx, group_integration.min_weight);
        let included_ids: Vec<i64> =
            included_for_rej.iter().map(|&i| stack_frames[i].frame_id).collect();
        let stems = rejection_map_stems(group, &included_ids);
        match RejBitmapSet::create(
            &rc.layout.rej_dir(rc.run_id, &group.key),
            &stems,
            width,
            height,
            channels,
        ) {
            Ok(set) => rej_set = Some(set),
            Err(e) => {
                let msg = format!("failed to create the rejection-bitmap set: {e:#}");
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %msg,
                    consumers = bitmap_consumers,
                    "rejection bitmaps unavailable for this group"
                );
                rc.warnings.push(format!(
                    "group {}: {bitmap_consumers} skipped — {msg}",
                    group.key
                ));
                rej_set_failure = Some(msg);
            }
        }
    }

    // Plan D D1b: the set is cloned out of `rc` (a few KB) so `input` holds no borrow of it.
    let common_stars = rc.common_stars.get(&group.key).cloned();
    // `input` borrowed the STACK_FRAMES built above; when the LN pass
    // narrowed `members` (and rebuilt `stack_frames`), that borrow must be
    // dropped and rebuilt before anything below reads it — always rebuilding
    // here (cheap: a handful of references/copies) is simpler than tracking
    // whether anything actually changed.
    let input = GroupInput {
        frames: &stack_frames,
        reference: reference_idx,
        width,
        height,
        channels,
        interpolation: rc.config.registration.interpolation,
        clamping: rc.config.registration.clamping_threshold,
        integration: &group_integration,
        normalization: &normalization_cfg,
        ln: ln_grids.as_deref(),
        // M3 Task 2/5: the bitmap set created just above (`None` when
        // drizzle doesn't want rejection bitmaps this run, or `create`
        // itself failed — `integrate_group` runs fine either way, it just
        // never gets bitmaps to write).
        rej: rej_set.as_ref(),
        // Final fix wave: the large-scale SECOND pass writes a bitmap set
        // of its own only when the Drizzle stage — its one reader — is
        // going to open it. `bitmaps_for_drizzle` is the same condition
        // that decides whether drizzle reads bitmaps at all, so a
        // standalone large-scale run writes two sets per frame instead of
        // three and nothing downstream misses the third.
        second_pass_bitmaps: bitmaps_for_drizzle,
        coverage_map: rc.config.output.coverage_map || rc.config.output.crop_to_coverage > 0,
        stats_on_covered: rc.grid.is_some(),
        // Plan D D1b: the master PSF SNR and the SNR gain read the group's frame-common star set (`reweigh_groups`).
        common_stars: common_stars.as_deref(),
    };

    // Progress plumbing: `on_plane`/`on_band`/`on_combine` are `Sync`
    // closures the engine may call from any of its own worker threads — they
    // take no `&mut RunContext`, only cloned-out plain values plus an
    // `AtomicUsize`/`Mutex<Instant>` for the shared "which plane, when did we
    // last emit" state (same pattern as `api::masters::run_build`).
    // Percent = `100 * (plane + band_fraction) / planes`: `on_plane` stamps
    // the current plane (fired at plane start, `band_fraction = 0`);
    // `on_band`/`on_combine` read it back and derive `band_fraction` from
    // the engine's own `bytes_done/bytes_total` pair for that plane.
    //
    // M4c Task 3: the plane space is `channels × passes` wide — large-scale
    // rejection integrates the group TWICE, and `integrate_group` reports
    // pass 2's planes offset by `channels` (see `large_scale_passes`, the
    // one place that rule lives; called here with the same inputs
    // `integrate_group` will use, so the denominator below and the plane
    // indices it receives can never disagree).
    let integrate_planes_total =
        channels * large_scale_passes(&group_integration, rej_set.is_some());
    let current_plane = std::sync::atomic::AtomicUsize::new(0);
    let tick_state: Mutex<IntegrateTickState> = Mutex::new(IntegrateTickState::new());
    // The plane's row count for the combine-progress fraction: the group's
    // own (reference) geometry, the same rows the engine combines.
    let plane_rows = rc.geometry_of(&group.key).height as u64;
    let emitter = rc.emitter.clone();
    let run_id = rc.run_id;
    let tick_subject = rc.subject.clone();
    let group_key_for_progress = group.key.clone();

    let on_plane = |p: usize, _total: usize| {
        current_plane.store(p, Ordering::Relaxed);
        tick_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rows
            .on_plane();
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            &tick_subject,
            &group_key_for_progress,
            integrate_planes_total,
            p,
            0.0,
            0,
            0,
            true,
            Some(integrate_tick_message(
                p,
                integrate_planes_total,
                channels,
                "reading",
            )),
        );
    };
    let on_band = |band: usize, bands: usize, bytes_done: u64, bytes_total: u64| {
        let plane = current_plane.load(Ordering::Relaxed);
        // Percent follows ROWS COMBINED, not bytes read: a band's read is
        // ≈ 10 % of its time and the bytes pair is what the text shows
        // anyway. Reporting the bytes fraction here used to put the bar
        // AHEAD of the combine tick, whose rows fraction the max-latch then
        // dropped for the rest of the band.
        let frac = {
            let mut st = tick_state.lock().unwrap_or_else(|e| e.into_inner());
            st.rows.on_band(band);
            st.rows.frac(plane_rows)
        };
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            &tick_subject,
            &group_key_for_progress,
            integrate_planes_total,
            plane,
            frac,
            bytes_done,
            bytes_total,
            false,
            Some(integrate_tick_message(
                plane,
                integrate_planes_total,
                channels,
                &format!("band {band}/{bands}"),
            )),
        );
    };
    let on_combine = |rows: usize, rows_total: usize, bytes_done: u64, bytes_total: u64| {
        let plane = current_plane.load(Ordering::Relaxed);
        let frac = {
            let mut st = tick_state.lock().unwrap_or_else(|e| e.into_inner());
            st.rows.on_combine(rows as u64, rows_total as u64);
            st.rows.frac(plane_rows)
        };
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            &tick_subject,
            &group_key_for_progress,
            integrate_planes_total,
            plane,
            frac,
            bytes_done,
            bytes_total,
            false,
            Some(integrate_tick_message(
                plane,
                integrate_planes_total,
                channels,
                "combining",
            )),
        );
    };
    let progress = GroupProgress {
        on_plane: &on_plane,
        engine: EngineProgress {
            on_band: &on_band,
            on_combine: &on_combine,
        },
    };

    let integrate_start = Instant::now();
    let mut integrate_meter = Some(crate::stacking::telemetry::StageMeter::start(
        rc,
        Stage::Integrate,
        Some(&group.key),
    ));
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "integrate",
        &group.key,
        "integrate band plan",
    );
    let cancel: &AtomicBool = &rc.cancel;
    let pool: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    // The span names the group on the integration's own events (the master
    // statistics rectangle among them); logging only.
    let integrated = {
        let _span = tracing::debug_span!("integrate_group", group_key = %group.key).entered();
        integrate_group(&input, measure_opts, pool, cancel, &progress, io)
    };
    let mut output = match integrated {
        Ok(o) => o,
        Err(IntegrationError::Cancelled) => return Err(RunError::Cancelled),
        Err(e) => {
            finish_meter(&mut integrate_meter);
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("group integration failed: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_start.elapsed(),
                Duration::ZERO,
                Duration::ZERO,
            ));
        }
    };
    emit_integrate_tick(
        &tick_state,
        emitter.as_ref(),
        run_id,
        &tick_subject,
        &group_key_for_progress,
        integrate_planes_total,
        integrate_planes_total,
        0.0,
        0,
        0,
        true,
        None,
    );
    finish_meter(&mut integrate_meter);
    let integrate_dur = integrate_start.elapsed();

    // B3 (M3 final fix wave, I3): integration itself completed correctly —
    // `rej_set`'s own `failure()` is only ever set by a WRITE fault the
    // engine's band loop hit mid-integration (`RejPlaneSink::record_band`
    // latches it and returns `Ok`, per ruling: never fail a run because
    // drizzle-support machinery failed). Folded into `rej_set_failure` here
    // so the SAME check below (which already handles a `create` failure)
    // treats both the same way: drizzle skipped for this group, the master
    // stays untouched.
    if let Some(reason) = rej_set.as_ref().and_then(|s| s.failure()) {
        rc.warnings.push(format!(
            "{bitmap_consumers} skipped for {}: rejection bitmaps could not be written: {reason}",
            group.key
        ));
        rej_set_failure = Some(reason);
    }

    // M4c Task 3: large-scale rejection was asked for but did not run for
    // this group — `integrate_group` logged WHY (an unusable first-pass
    // bitmap set, or a processing failure) and kept the first pass's
    // master; the user is told here, since the warning list is the run's
    // own user-visible channel. The bitmap-write fault above already has
    // its own line, so this one covers the remaining causes only.
    if bitmaps_for_large_scale
        && output.stats.large_scale_rejected_fraction.is_none()
        && rej_set_failure.is_none()
    {
        rc.warnings.push(format!(
            "group {}: large-scale rejection did not run — the master is the per-pixel result \
             (see the log for the reason)",
            group.key
        ));
    }

    // Fix round 1 (ruling R-T3-1): the second pass ran but left no usable
    // bitmap set of its own, so nothing on disk describes THIS master's
    // rejected samples — drizzle is skipped for the group rather than
    // pointed at pass 1's bits, which describe the integration the second
    // pass replaced. The master itself is untouched and already written.
    //
    // Final fix wave: gated on `bitmaps_for_drizzle`, because the whole
    // degradation this branch describes is drizzle's. With drizzle off the
    // second pass deliberately writes no set of its own (`GroupInput::
    // second_pass_bitmaps`), so `second_pass_rej_ok` is false by design and
    // warning here told the user about a drizzle that was never going to
    // run. `rej_set_failure` — the only thing this branch sets — is itself
    // read only under `rc.config.drizzle.enabled`, so nothing else loses
    // information.
    if bitmaps_for_drizzle
        && output.stats.large_scale_rejected_fraction.is_some()
        && !output.second_pass_rej_ok
        && rej_set_failure.is_none()
    {
        let msg = "the second pass's rejection bitmaps are unavailable".to_string();
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            error = %msg,
            consumers = "drizzle",
            "rejection bitmaps unavailable for this group"
        );
        rc.warnings
            .push(format!("drizzle skipped for {}: {msg}", group.key));
        rej_set_failure = Some(msg);
    }

    // Plan 6b-3 D10 (spec §5.9): with `cropToCoverage = K > 0` this group's
    // outputs are written as FITS into the run's scratch folder and wait for
    // the run's rectangle (`finish_crop`). The group's own `coverage >= K` box
    // is taken here, from the count the engine kept for it; when the
    // coverage map itself is off, that count is dropped before the write.
    let crop_k = rc.config.output.crop_to_coverage;
    let crop_mode = crop_k > 0;
    let group_rect = if crop_mode {
        output.coverage.as_deref().and_then(|c| {
            crate::stacking::crop::coverage_rect(c, output.width, output.height, crop_k)
        })
    } else {
        None
    };
    if crop_mode && !rc.config.output.coverage_map {
        output.coverage = None;
    }
    let (out_dir, out_format) = if crop_mode {
        (
            rc.layout.uncropped_dir(rc.run_id),
            crate::stacking::config::OutputFormat::Fits,
        )
    } else {
        (rc.output_dir.clone(), rc.config.output.format)
    };

    let output_start = Instant::now();

    // Spec §5.9, plan 6b-3 D3, D11: what a grid master states — cloned out
    // so the drizzle block below can read it while `rc` is borrowed.
    let grid_stamp: Option<(Arc<crate::stacking::grid_map::GridRun>, String)> = rc
        .grid
        .clone()
        .zip(rc.subject.project_id().map(str::to_string));

    let group_reference_calibrated = members[reference_idx].calibrated.clone();
    let reference_cards = match source_cards_from_file(&group_reference_calibrated) {
        Ok(c) => c,
        Err(e) => {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to read the group reference's header: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    };

    let date_obs_first = output
        .included
        .iter()
        .filter_map(|&i| members[i].date_obs.as_deref())
        .min();
    let date_obs_last = output
        .included
        .iter()
        .filter_map(|&i| members[i].date_obs.as_deref())
        .max();

    // Fix round 1, item 3: `ATH_STKF` names the SOURCE frame, not the
    // calibrated artifact — Checkpoint B's own master reads
    // `ATH_STKF = '2025-09-14_02-19-02__-9.90_180.00s_0019'`, the raw
    // `files.filename` stem, never `c_`/`_d`-stripped (the calibrated
    // file's own stem needed stripping in the probe only because the probe
    // had no `GroupFrame.filename` to read directly).
    let reference_id = Path::new(&members[reference_idx].filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("reference")
        .to_string();
    let weight_mode_str = enum_serde_name(&rc.config.measurement.weight_mode)?;
    let normalization_str = format!(
        "{}/{}",
        enum_serde_name(&rc.config.normalization.output)?,
        enum_serde_name(&rc.config.normalization.rejection)?
    );
    let run_id_str = rc.run_id.to_string();

    let mut cards = match build_master_light_cards(&MasterCardInputs {
        reference_cards: &reference_cards,
        wcs,
        frames: output.stats.included,
        weighted_exposure_s: output.stats.weighted_exposure_s,
        date_obs_first,
        date_obs_last,
        recipe: &output.stats.recipe,
        weight_mode: &weight_mode_str,
        normalization: &normalization_str,
        reference_id: &reference_id,
        group_key: &group.key,
        cameras: &group.cameras,
        run_id: &run_id_str,
        app_version: &rc.app_version,
        geometry: rc.config.registration.geometry,
    }) {
        Ok(c) => c,
        Err(e) => {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to build the master header: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    };
    if grid_stamp.is_some()
        && group
            .filter
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        // Review M4: a grid master never carries the reference's raw FILTER.
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            "the group's canonical filter is empty; the grid master carries no FILTER"
        );
        rc.warnings.push(format!(
            "group {}: its canonical filter is empty, so the master carries no FILTER card",
            group.key
        ));
    }
    if let Some((grid, project_id)) = &grid_stamp {
        if let Err(e) = stamp_grid_cards(
            &mut cards,
            &GridStamp {
                project_id,
                grid_hash: &grid.hash,
                filter: group.filter.as_deref(),
            },
        ) {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to build the master header: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    }

    // Plan 6c D9: the exposures of the frames the master integrated
    // (`output.included` indexes `members`), not the group's label alone.
    let exposure = exposure_label(
        rc.config.grouping.exposure_mode,
        group.exposure_s,
        output.included.iter().map(|&i| members[i].exposure_s),
    );
    let name = master_file_name(
        &rc.set_name,
        group.filter.as_deref(),
        group.color_mode,
        group.binning,
        exposure,
        output.stats.included,
    );

    let written = {
        // Fix round 1, item 2: `unwrap_or_else(|e| e.into_inner())`, not
        // `.unwrap()` — this is a process-wide `static`, so one panic
        // inside a PRIOR `write_master_light` call (any group, any run)
        // would otherwise poison it for the rest of the process's life,
        // failing every later master write with no write ever attempted.
        // Safe to recover: the guarded section only claims a name
        // (`resolve_collision`) and writes through `write_fits_f32` or, for
        // `output.format = xisf` (M4d Task 2), `write_xisf_f32` — both of
        // which write to a sibling temp file and atomically rename it into
        // place (`fits_writer::writer::write_fits_f32`,
        // `fits_writer::xisf_writer::write_xisf_f32`) — so a panic mid-write
        // leaves an orphaned `*.fits.tmp.<pid>.<seq>` (or
        // `*.xisf.tmp.<pid>.<seq>`) file, never a partial file AT the
        // resolved name itself; a later `resolve_collision` call still sees
        // that name correctly free and reuses it, and there is no partial
        // state under this lock for a later writer to misread.
        let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_master_light(&out_dir, &name, &output, &cards, out_format)
    };
    let written = match written {
        Ok(w) => w,
        Err(e) => {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to write the master: {e:#}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    };

    let stats_json = serde_json::to_string(&output.stats)
        .map_err(|e| RunError::Other(format!("failed to serialize group stats: {e}")))?;
    let group_id = *rc.group_ids.get(&group.key).ok_or_else(|| {
        RunError::Other(format!(
            "no stacking_run_groups row for group {}",
            group.key
        ))
    })?;
    let master_path_str = written.master.display().to_string();
    let rejection_low_str = written
        .rejection_low
        .as_ref()
        .map(|p| p.display().to_string());
    let rejection_high_str = written
        .rejection_high
        .as_ref()
        .map(|p| p.display().to_string());
    let coverage_path_str = written.coverage.as_ref().map(|p| p.display().to_string());

    // M4d Task 3 (ruling R-M4d-4): the `master_lights` row's own facts.
    // `frames` is the group's included count — the SAME number the group row
    // and the master's `ATH_STKN` card carry, so the three can never
    // disagree. `total_exposure_s` is the PLAIN sum of those frames'
    // exposures (not `weighted_exposure_s`, which is the integration's own
    // weighted figure): `Option`'s own `Sum` collapses to `None` the moment
    // one member carries no `EXPTIME`, which is exactly the nullable column
    // the schema asks for — a partial total would be a lie.
    let output_format_str = enum_serde_name(&out_format)?;
    let total_exposure_s: Option<f64> = output
        .included
        .iter()
        .map(|&i| members[i].exposure_s)
        .sum::<Option<f64>>();

    let mut rejected_by_frame: HashMap<i64, f64> = HashMap::with_capacity(output.included.len());
    {
        let conn = db(&rc.ctx)?.conn();
        update_group(
            &conn,
            group_id,
            &GroupUpdate {
                included_count: Some(output.stats.included as i64),
                master_path: Some(&master_path_str),
                rejection_low_path: rejection_low_str.as_deref(),
                rejection_high_path: rejection_high_str.as_deref(),
                stats_json: Some(&stats_json),
                status: Some("done"),
                ..Default::default()
            },
        )?;

        // M4d Task 3: one row per WRITTEN output, alongside the group-row
        // update that records the same master — the master light is
        // cataloged here and nowhere else (it never becomes a `frames`
        // row). `width`/`height`/`channels` are what the writer actually
        // wrote: this group's own reference geometry and plane count.
        insert_master_light(
            &conn,
            &NewMasterLight {
                subject: &rc.subject,
                run_id: rc.run_id,
                group_key: &group.key,
                kind: MasterLightKind::Master.as_db_str(),
                path: &master_path_str,
                format: &output_format_str,
                width: width as i64,
                height: height as i64,
                channels: channels as i64,
                frames: output.stats.included as i64,
                total_exposure_s,
            },
        )?;
        // Plan 6b-3 D9: the coverage map's own row, in the same DB
        // block as the master's.
        if let Some(coverage) = coverage_path_str.as_deref() {
            insert_master_light(
                &conn,
                &NewMasterLight {
                    subject: &rc.subject,
                    run_id: rc.run_id,
                    group_key: &group.key,
                    kind: MasterLightKind::Coverage.as_db_str(),
                    path: coverage,
                    format: "fits",
                    width: width as i64,
                    height: height as i64,
                    channels: 1,
                    frames: output.stats.included as i64,
                    total_exposure_s,
                },
            )?;
        }

        for (k, &idx) in output.included.iter().enumerate() {
            if let Some(frac) = output.stats.rejected_fraction_per_frame.get(k).copied() {
                let frame_id = members[idx].frame_id;
                set_frame_rejected_fraction(&conn, &rc.subject, rc.run_id, frame_id, frac)?;
                rejected_by_frame.insert(frame_id, frac);
            }
        }
    }

    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        path = %master_path_str,
        "master written"
    );

    // M4d Task 2 fix round 1 (ruling R-T2-1): XISF has no row-order concept
    // — row 0 IS the top row for every XISF reader — while this master's
    // array is simply its source frames' order, carried through every stage
    // unflipped. For a bottom-up set (or one whose frames carry no
    // `ROWORDER` at all, the astronomical default) the XISF master therefore
    // displays mirrored relative to the FITS master of the same run. The
    // pixels are deliberately not flipped (that would have to transform the
    // WCS and its SIP terms — a follow-up); the XISF header states the
    // effective order explicitly and this says so once per group. The
    // group's drizzled master and weight map share this array order, so
    // they are covered by this one warning rather than repeating it.
    if rc.config.output.format == crate::stacking::config::OutputFormat::Xisf
        && cards_row_order_is_bottom_up(&cards)
    {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            path = %master_path_str,
            "xisf output keeps the frames' bottom-up row order"
        );
        rc.warnings.push(format!(
            "group {}: XISF output keeps the frames' bottom-up row order; XISF viewers \
             will show it flipped relative to the FITS master",
            group.key
        ));
    }

    // M3 Task 5 (spec Stage::Drizzle, ruling R-M3-11): runs AFTER the master
    // is written and its DB row updated, in this SAME call, so a drizzle
    // failure of any kind can never turn a successful master build into a
    // reported group failure (ruling R-M3-7) — the master above is already
    // good either way. `drizzle_path`/`weight_map_path`/`drizzle_stats` feed
    // `push_summary_group` below; every early-return path above this point
    // passes `None` for all three via the existing three-`None` call sites.
    let mut drizzle_dur = Duration::ZERO;
    let mut drizzle_path: Option<String> = None;
    let mut weight_map_path: Option<String> = None;
    let mut drizzle_coverage_path: Option<String> = None;
    let mut drizzle_stats: Option<DrizzleStats> = None;
    let mut crop_drizzle: Option<CropDrizzle> = None;
    if rc.config.drizzle.enabled {
        if let Some(reason) = &rej_set_failure {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                error = %reason,
                "drizzle skipped for this group; the master is unaffected"
            );
            // B7 (M3 final fix wave, M4): drizzle was never attempted for
            // this group — no `0/total` starting tick was ever emitted
            // either, so without a terminal tick here the Drizzle row's
            // last observed event is whatever the PREVIOUS group (or stage)
            // left it at, forever. `0/0` (not `total/total` — there is no
            // `total` to report; nothing was attempted) with the skip
            // reason as `message` gives the row an honest, final state.
            rc.progress(
                Stage::Drizzle,
                Some(group.key.clone()),
                0,
                0,
                0,
                0,
                None,
                Some(format!("rejection bitmaps could not be written: {reason}")),
            );
        } else {
            // Fix round 1, Minor M6: counts as "attempted" from here —
            // bitmap creation (if wanted) already succeeded, so the only
            // remaining outcomes are a shape refusal, `drizzle_group`
            // itself, or the post-`drizzle_group` write steps; every one
            // of those is a real attempt for `stage_output`'s own
            // `Stage::Drizzle` `StageTiming` gate to count (never
            // `any_master`, which is only a proxy — see that gate's own
            // doc).
            rc.drizzle_attempted += 1;

            // Fix round 1, Minor M2: the run-wide `dropShrink` clamp
            // (ruling R-M3-10) now happens ONCE, in `stage_output` before
            // the group loop — `rc.config.drizzle.drop_shrink` is already
            // in `[0.5, 1.0]` by the time any group reads it here, so
            // reading it straight off `rc.config` (not clamping again) can
            // never repeat the run warning once per group.
            let drizzle_cfg = rc.config.drizzle.clone();
            let scale = drizzle_cfg.scale;
            let drop_shrink = drizzle_cfg.drop_shrink;

            // M4d Task 1 (ruling R-M4d-2): Bayer drizzle's per-frame source.
            // Resolved HERE, before the immutable borrows below, because it
            // needs a DB connection: the frame's own `calibrated_mosaic`
            // artifact (stage 1 wrote it in the same generation as the
            // debayered frame) plus the mosaic's phase-corrected pattern,
            // read through the SAME two functions the generator used, so the
            // deposit and the debayer can never disagree about which pixel is
            // red. A frame missing either falls back to the debayered deposit
            // with a named warning — an interpolated frame in the stack is
            // honest and useful; refusing the whole group over one frame is
            // not.
            // A 3-channel group is the only one a mosaic can be routed into
            // (`drizzle_group` refuses the combination outright), and
            // `channels` is this GROUP's own plane count — an OSC group whose
            // frames somehow measured one plane is not one.
            let bayer_wanted = wants_cfa_mosaic(&rc.config, group) && channels == 3;
            let mut bayer_sources: Vec<Option<(PathBuf, astroimage::BayerPattern)>> =
                vec![None; output.included.len()];
            if bayer_wanted {
                let conn = db(&rc.ctx)?.conn();
                for (k, &idx) in output.included.iter().enumerate() {
                    let frame_id = members[idx].frame_id;
                    let path = crate::db::stacking::find_artifact(
                        &conn,
                        &rc.subject,
                        &group.key,
                        ArtifactKind::CalibratedMosaic,
                        Some(frame_id),
                    )?
                    .and_then(|a| a.path)
                    .map(PathBuf::from);
                    let pattern = match origin_of(&rc.origins, frame_id)
                        .map_err(anyhow::Error::from)
                        .and_then(|origin| cfa_geometry(&conn, origin, frame_id))
                    {
                        Ok(geom) => geom.map(crate::export::calibrated_generator::bayer_for),
                        Err(error) => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id,
                                error = %format!("{error:#}"),
                                "bayer drizzle: reading the frame's mosaic phase failed"
                            );
                            None
                        }
                    };
                    bayer_sources[k] = match (path, pattern) {
                        (Some(path), Some(pattern)) => Some((path, pattern)),
                        _ => None,
                    };
                }
            }
            let bayer_missing = bayer_sources.iter().filter(|s| s.is_none()).count();
            if bayer_wanted && bayer_missing > 0 {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    count = bayer_missing,
                    "bayer drizzle: frames without a cfa mosaic fall back to the debayered deposit"
                );
                rc.warnings.push(format!(
                    "group {}: Bayer drizzle deposited {bayer_missing} frame(s) from their \
                     debayered planes — no calibrated CFA mosaic was available for them",
                    group.key
                ));
            }

            let drizzle_progress_total = output.included.len() * channels;
            rc.progress(
                Stage::Drizzle,
                Some(group.key.clone()),
                0,
                drizzle_progress_total,
                0,
                0,
                None,
                None,
            );
            let drizzle_start = Instant::now();
            let mut drizzle_meter = Some(crate::stacking::telemetry::StageMeter::start(
                rc,
                Stage::Drizzle,
                Some(&group.key),
            ));

            // Fix round 1, Minor M4: `output.output_pairs[p][k]` below is a
            // bare double index — a shape mismatch would otherwise panic
            // inside `process_group_output`, get caught by `run_thread`'s
            // `catch_unwind`, and fail the WHOLE RUN, exactly what R-M3-7
            // forbids for a drizzle-only problem. Guaranteed safe today
            // (`integrate_planes` pushes one `output_pairs` entry per
            // plane, one inner entry per included frame — `integrate.rs`),
            // checked anyway as defense-in-depth: a mismatch degrades to
            // `DrizzleFailure::Other`, same as any other post-`drizzle_group`
            // step's error.
            let output_pairs_shape_ok = output.output_pairs.len() == channels
                && output
                    .output_pairs
                    .iter()
                    .all(|p| p.len() == output.included.len());

            let drizzle_outcome: Result<(WrittenDrizzle, DrizzleStats, Vec<Card>), DrizzleFailure> =
                if !output_pairs_shape_ok {
                    Err(DrizzleFailure::Other(format!(
                        "output_pairs shape does not match {channels} channel(s) x {} included frame(s)",
                        output.included.len()
                    )))
                } else {
                    // Per included frame (`output.included`, engine order —
                    // the SAME order `rej_set` (when created) was
                    // sized/stemmed to), transpose `output.output_pairs`
                    // (outer: plane, inner: included frame) into a
                    // per-frame, per-plane slice — the shape
                    // `DrizzleFrame::output_pair` wants.
                    let output_pairs_by_frame: Vec<Vec<NormalizationPair>> =
                        (0..output.included.len())
                            .map(|k| (0..channels).map(|p| output.output_pairs[p][k]).collect())
                            .collect();
                    // M4c Task 3, fix round 1 (ruling R-T3-1): drizzle reads
                    // exactly the bits the master was built with — see
                    // `drizzle_rejection_paths`.
                    let rej_paths = drizzle_rejection_paths(
                        rej_set.as_ref(),
                        output.included.len(),
                        output.second_pass_rej_ok,
                    );
                    let drizzle_frames: Vec<DrizzleFrame> = output
                        .included
                        .iter()
                        .enumerate()
                        .map(|(k, &idx)| DrizzleFrame {
                            path: stack_frames[idx].path.as_path(),
                            map: &stack_frames[idx].map,
                            weight: &stack_frames[idx].weight.normalized,
                            output_pair: &output_pairs_by_frame[k],
                            ln: ln_grids
                                .as_ref()
                                .and_then(|g| g.get(idx))
                                .and_then(|o| o.as_ref()),
                            rej: rej_paths[k].as_deref(),
                            // M4d Task 1: indexed by `k` like `rej_paths` —
                            // the included-frame order, not `idx`.
                            cfa: bayer_sources[k].as_ref().map(|(path, pattern)| {
                                crate::stacking::drizzle::CfaSource {
                                    path: path.as_path(),
                                    pattern: *pattern,
                                }
                            }),
                        })
                        .collect();

                    let drizzle_input = DrizzleInput {
                        frames: &drizzle_frames,
                        width,
                        height,
                        channels,
                        scale,
                        drop_shrink,
                        kernel: drizzle_cfg.kernel,
                        use_weights: drizzle_cfg.use_weights,
                        use_rejection: drizzle_cfg.use_rejection && rej_set.is_some(),
                        // The drizzle follows the plain master's OUTPUT
                        // normalization: LN grids computed for rejection
                        // alone (`rejection = Local`, `local.enabled =
                        // false` - the shipped default) never normalize
                        // the drizzled master, or it would be locally
                        // normalized while the master it sits beside is not.
                        use_local_normalization: drizzle_cfg.use_local_normalization
                            && rc.config.normalization.local.enabled,
                        write_weight_map: drizzle_cfg.write_weight_map,
                        write_coverage: rc.config.output.coverage_map,
                        // Plan 6b-3 ruling (b): a grid drizzle's statistics ride
                        // the master's own covered rectangle; `None` there is
                        // the defensive arm (the drizzle fails, the master stays).
                        stats_on_covered: rc.grid.is_some(),
                        stats_rect: drizzle_stats_rect(output.measure_rect),
                        measure: measure_opts,
                        ram_total_bytes: None,
                        // Tier C item C2: the run always drizzles through
                        // whichever overlap arm ruling C-3 resolves to; only
                        // the `drizzle_probe` forces the exact clip.
                        force_exact_overlap_for_measurement: false,
                    };

                    // Fix round 1, Important I1: real per-frame progress —
                    // `drizzle_group` calls `on_frame` once per (frame,
                    // plane) unit, strictly sequentially (see
                    // `DrizzleTickState`'s own doc), so a throttled
                    // `emit_drizzle_tick` reaches the SAME `stacking-progress`
                    // channel `rc.progress` uses, via cloned-out plain
                    // values (`emitter`/`run_id`/`subject`/`group_key`) since
                    // the closure cannot hold `&mut rc`. `frame_id` is the
                    // k-th drizzled frame's own id — `done` is 1-based
                    // (incremented before the call, `drizzle/mod.rs`), so
                    // `(done - 1) % n` recovers `k` regardless of which
                    // plane `done` is currently in (channels > 1 wraps
                    // `done` past `n` once per plane).
                    let drizzle_frame_ids: Vec<i64> = output
                        .included
                        .iter()
                        .map(|&idx| members[idx].frame_id)
                        .collect();
                    let drizzle_tick_state: Mutex<DrizzleTickState> =
                        Mutex::new(DrizzleTickState::new());
                    let drizzle_tick_emitter = rc.emitter.clone();
                    let drizzle_tick_run_id = rc.run_id;
                    let drizzle_tick_subject = rc.subject.clone();
                    let drizzle_tick_group_key = group.key.clone();
                    let drizzle_on_frame = |done: usize, total: usize| {
                        let n = drizzle_frame_ids.len();
                        let frame_id = if n == 0 {
                            None
                        } else {
                            drizzle_frame_ids.get(done.saturating_sub(1) % n).copied()
                        };
                        emit_drizzle_tick(
                            &drizzle_tick_state,
                            drizzle_tick_emitter.as_ref(),
                            drizzle_tick_run_id,
                            &drizzle_tick_subject,
                            &drizzle_tick_group_key,
                            done,
                            total,
                            frame_id,
                            false,
                        );
                    };
                    let drizzle_progress = DrizzleProgress {
                        on_frame: &drizzle_on_frame,
                    };

                    // Fresh, tightly-scoped borrows of
                    // `rc.cancel`/`rc.ctx.image_pool` — the outer
                    // `cancel`/`pool` (defined once, before
                    // `integrate_group`'s own call) are NOT reused here:
                    // this block runs well after several `&mut rc` accesses
                    // (`rc.progress`, `rc.drizzle_attempted += 1`) the outer
                    // borrow would otherwise have to span, which the borrow
                    // checker refuses (an immutable borrow of `rc` cannot be
                    // held live across a `&mut rc` use). Re-taken here,
                    // after the last `&mut rc` access before this point (the
                    // `Stage::Drizzle` progress tick above), the borrow only
                    // needs to live to the `drizzle_group` call a few lines
                    // down — no `&mut rc` happens in between.
                    let cancel: &AtomicBool = &rc.cancel;
                    let pool: &rayon::ThreadPool = rc.ctx.image_pool.as_ref();
                    // A labeled block, not a closure: every fallible step
                    // below needs `pool`/`cancel` (the fresh borrows just
                    // above) and `rc.output_dir`/`cards`/`written` (plain
                    // reads) — no `&mut rc` access happens until after the
                    // block ends, so this stays a plain immutable borrow the
                    // same way `integrate_group`'s own call above does.
                    'attempt: {
                        let drz =
                            match drizzle_group(&drizzle_input, pool, cancel, &drizzle_progress) {
                                Ok(o) => o,
                                Err(e) => break 'attempt Err(e.into()),
                            };
                        let scaled = match wcs.map(|w| scale_plate_solve(w, scale)).transpose() {
                            Ok(s) => s,
                            Err(e) => break 'attempt Err(DrizzleFailure::Other(e.to_string())),
                        };
                        let mut drz_cards = match build_drizzle_cards(
                            &cards,
                            scaled.as_ref(),
                            scale,
                            drop_shrink,
                            drizzle_cfg.kernel,
                            rc.config.registration.geometry,
                        ) {
                            Ok(c) => c,
                            Err(e) => break 'attempt Err(DrizzleFailure::Other(e.to_string())),
                        };
                        if let Some((grid, project_id)) = &grid_stamp {
                            if let Err(e) = stamp_grid_cards(
                                &mut drz_cards,
                                &GridStamp {
                                    project_id,
                                    grid_hash: &grid.hash,
                                    filter: group.filter.as_deref(),
                                },
                            ) {
                                break 'attempt Err(DrizzleFailure::Other(e.to_string()));
                            }
                        }
                        let master_stem = written
                            .master
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("master")
                            .to_string();
                        // Same "one process, one writer" concern
                        // `OUTPUT_WRITE_LOCK` guards the master write
                        // against: `write_drizzled_master` is
                        // check-then-write (`resolve_collision`) into the
                        // SAME output dir.
                        let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                        match write_drizzled_master(
                            &out_dir,
                            &master_stem,
                            &drz,
                            &drz_cards,
                            // A plain immutable read of `rc`, like
                            // `out_dir` above: the drizzled master
                            // and its weight map land in the SAME
                            // container as the master (ruling R-M4d-3).
                            out_format,
                        ) {
                            Ok(wd) => Ok((wd, drz.stats, drz_cards)),
                            Err(e) => Err(DrizzleFailure::Other(format!("{e:#}"))),
                        }
                    }
                };
            finish_meter(&mut drizzle_meter);
            drizzle_dur = drizzle_start.elapsed();
            crate::stacking::telemetry::memory_checkpoint(
                rc.run_id,
                "drizzle",
                &group.key,
                "drizzle buffers dropped",
            );

            match drizzle_outcome {
                Ok((wd, stats, drz_cards)) => {
                    let drizzle_path_str = wd.drizzle.display().to_string();
                    let weight_map_path_str =
                        wd.weight_map.as_ref().map(|p| p.display().to_string());
                    let drizzle_coverage_path_str =
                        wd.coverage.as_ref().map(|p| p.display().to_string());
                    {
                        let conn = db(&rc.ctx)?.conn();
                        update_group(
                            &conn,
                            group_id,
                            &GroupUpdate {
                                drizzle_path: Some(&drizzle_path_str),
                                ..Default::default()
                            },
                        )?;
                        // M4d Task 3 (ruling R-M4d-4): the drizzled master
                        // and — when one was written — its weight map are
                        // outputs of this run just as the master is, and
                        // get their own rows in the same DB touch as the
                        // group-row update that records the drizzle path.
                        // Their geometry is the drizzle's OWN output grid
                        // (`scale x` the master's), which the stats report;
                        // the weight map shares it exactly. `frames` and
                        // `total_exposure_s` are the same group figures the
                        // master row carries — a drizzle deposits the same
                        // included frames.
                        insert_master_light(
                            &conn,
                            &NewMasterLight {
                                subject: &rc.subject,
                                run_id: rc.run_id,
                                group_key: &group.key,
                                kind: MasterLightKind::Drizzle.as_db_str(),
                                path: &drizzle_path_str,
                                format: &output_format_str,
                                width: stats.out_width as i64,
                                height: stats.out_height as i64,
                                channels: channels as i64,
                                frames: output.stats.included as i64,
                                total_exposure_s,
                            },
                        )?;
                        if let Some(weight_map) = weight_map_path_str.as_deref() {
                            insert_master_light(
                                &conn,
                                &NewMasterLight {
                                    subject: &rc.subject,
                                    run_id: rc.run_id,
                                    group_key: &group.key,
                                    kind: MasterLightKind::WeightMap.as_db_str(),
                                    path: weight_map,
                                    format: &output_format_str,
                                    width: stats.out_width as i64,
                                    height: stats.out_height as i64,
                                    channels: channels as i64,
                                    frames: output.stats.included as i64,
                                    total_exposure_s,
                                },
                            )?;
                        }
                        // Plan 6b-3 D9 (spec §5.8): drizzle's own coverage —
                        // always FITS, the drizzle's own grid.
                        if let Some(coverage) = drizzle_coverage_path_str.as_deref() {
                            insert_master_light(
                                &conn,
                                &NewMasterLight {
                                    subject: &rc.subject,
                                    run_id: rc.run_id,
                                    group_key: &group.key,
                                    kind: MasterLightKind::DrizzleCoverage.as_db_str(),
                                    path: coverage,
                                    format: "fits",
                                    width: stats.out_width as i64,
                                    height: stats.out_height as i64,
                                    channels: channels as i64,
                                    frames: output.stats.included as i64,
                                    total_exposure_s,
                                },
                            )?;
                        }
                    }
                    // Fix round 1, Important I1: the final, forced
                    // `current == total` tick — `rc.progress` is available
                    // again here (the labeled block above has ended, the
                    // `pool`/`cancel` borrows are dropped), and `current ==
                    // total` is one of `RunContext::progress`'s own two
                    // unconditional-force cases, so this always reaches the
                    // recorder even under the 300 ms throttle.
                    rc.progress(
                        Stage::Drizzle,
                        Some(group.key.clone()),
                        drizzle_progress_total,
                        drizzle_progress_total,
                        0,
                        0,
                        None,
                        None,
                    );
                    tracing::info!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        path = %drizzle_path_str,
                        drizzle_scale = scale,
                        out_width = stats.out_width,
                        out_height = stats.out_height,
                        duration_ms = drizzle_dur.as_millis() as u64,
                        "drizzled master written"
                    );
                    drizzle_path = Some(drizzle_path_str);
                    weight_map_path = weight_map_path_str;
                    drizzle_coverage_path = drizzle_coverage_path_str;
                    if crop_mode {
                        crop_drizzle = Some(CropDrizzle {
                            written: wd.clone(),
                            cards: drz_cards,
                            scale,
                        });
                    }
                    drizzle_stats = Some(stats);
                }
                Err(DrizzleFailure::Cancelled) => return Err(RunError::Cancelled),
                Err(DrizzleFailure::Other(msg)) => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        error = %msg,
                        "drizzle failed for this group; the master is unaffected"
                    );
                    rc.warnings
                        .push(format!("group {}: drizzle failed: {msg}", group.key));
                    // B7 (M3 final fix wave, M4): a forced `total/total`
                    // terminal tick (the starting `0/total` tick already
                    // went out above) — without it the row's last observed
                    // event stays at whatever partial percent the failure
                    // landed on, forever. Carries `msg` so a live listener
                    // can show why the row completed without a
                    // `drizzlePath`.
                    rc.progress(
                        Stage::Drizzle,
                        Some(group.key.clone()),
                        drizzle_progress_total,
                        drizzle_progress_total,
                        0,
                        0,
                        None,
                        Some(msg),
                    );
                }
            }
        }
    }

    if crop_mode {
        rc.crop_items.push(CropItem {
            group_key: group.key.clone(),
            group_id,
            rect: group_rect,
            width: output.width,
            height: output.height,
            channels,
            frames: output.stats.included,
            total_exposure_s,
            cards: cards.clone(),
            wcs: wcs.cloned(),
            master: written.clone(),
            drizzle: crop_drizzle,
        });
    }

    push_summary_group(
        rc,
        group,
        output.stats.included,
        Some(master_path_str),
        rejection_low_str,
        rejection_high_str,
        Some(output.stats.clone()),
        Some(normalization_reference_frame_id),
        &rejected_by_frame,
        ln_reference_path,
        drizzle_path,
        weight_map_path,
        drizzle_stats,
        coverage_path_str,
        drizzle_coverage_path,
        ln_scale_px,
    );

    // B2 (M3 final fix wave, I2): `output_start` was taken BEFORE the
    // drizzle block above, which runs entirely inside this same timer's
    // span — without subtracting it, `StageTiming{Output}` double-counted
    // every second of `StageTiming{Drizzle}` (on the acceptance set, ~14
    // minutes of drizzle reported as the master-write stage's own
    // duration). Drizzle and Output are meant to be disjoint spans (ruling
    // R-M3-11 places Drizzle strictly between Integrate and Output); this
    // is the correction.
    let output_dur = output_start.elapsed().saturating_sub(drizzle_dur);
    drop(members);
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "output",
        &group.key,
        "group members dropped",
    );
    rc.progress(
        Stage::Output,
        Some(group.key.clone()),
        1,
        1,
        0,
        0,
        None,
        None,
    );

    Ok((
        GroupOutcome::Written,
        normalize_dur,
        integrate_dur,
        drizzle_dur,
        output_dur,
    ))
}

/// Stages 6 (normalize), 7 (integrate) and 9 (output). Per group, in
/// `rc.plan_groups` (plan) order: [`process_group_output`] recomputes
/// viability from `rc.measured`'s FINAL state (stage 5 can have narrowed a
/// group below 3 included since stage 3's own check), integrates through
/// Plan 4's `integrate_group`, and writes the master + optional rejection
/// maps. A cancel (from `IntegrationError::Cancelled` or noticed between
/// groups) aborts the whole run — ruling 12: a cancelled run keeps its
/// artifacts and writes no master. Any other integration/write failure fails
/// only that group; a run in which NO group ever wrote a master ends the
/// whole run `failed` too — there is nothing to hand back either way. Once
/// every group has been attempted and at least one wrote a master, the
/// configured cleanup policy runs exactly once.
///
/// Plan 6b-3 D10 (fix round 1): when the stage ends in an error or a cancel
/// with groups still waiting in a crop run's scratch folder, they are
/// finalized uncropped ([`finalize_unfinished_crop`]) so no master is left
/// only in scratch.
pub(super) fn stage_output(rc: &mut RunContext) -> Result<(), RunError> {
    let result = stage_output_groups(rc);
    if result.is_err() {
        finalize_unfinished_crop(rc);
    }
    result
}

fn stage_output_groups(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let measure_opts = cfg
        .measurement
        .measure_options(cfg.normalization.scale_estimator);

    // M4b ruling R-M4b-8: a master carries ITS OWN reference's WCS — the
    // run-wide reference's in co-registered mode (where every group names
    // the same frame, so this resolves once and the missing-solve warning
    // is emitted once, exactly as before), the group's own in native mode.
    // Keyed by frame id so the lookup AND the warning happen once per
    // distinct reference, never once per group.
    let mut solves: HashMap<i64, Option<PlateSolveRecord>> = HashMap::new();
    let mut warned_missing_wcs: HashSet<i64> = HashSet::new();

    // Fix round 1, Minor M2: the `dropShrink` clamp (ruling R-M3-10) runs
    // ONCE here, for the whole run, instead of once per group — `drizzle.
    // dropShrink` is run-wide config, so a multi-group run under an
    // out-of-range value used to put the identical warning into
    // `summary.warnings` once per group. Mutating `rc.config.drizzle.
    // drop_shrink` in place means every group's later `rc.config.drizzle.
    // clone()` (inside `process_group_output`) already sees the clamped
    // value — nothing there needs to clamp (or warn) again.
    if cfg.drizzle.enabled {
        let configured = rc.config.drizzle.drop_shrink;
        if !(0.5..=1.0).contains(&configured) {
            let clamped = configured.clamp(0.5, 1.0);
            tracing::warn!(
                run_id = rc.run_id,
                drop_shrink = configured,
                clamped,
                "drizzle drop shrink out of [0.5, 1.0]; clamped"
            );
            rc.warnings.push(format!(
                "drizzle drop shrink {configured} out of [0.5, 1.0]; clamped to {clamped}"
            ));
            rc.config.drizzle.drop_shrink = clamped;
            // B10 (M3 final fix wave, M7): `rc.summary.config` was captured
            // in `start_stacking`, BEFORE this clamp ever runs — without
            // this, the persisted `RunSummary.config` (`summary_json` /
            // `runs/run-<id>.json`) would keep naming the PRE-clamp value
            // forever, disagreeing with the value the run actually used
            // (and with the drizzled master's own `ATH_DRZP` card, which is
            // built from `rc.config.drizzle.drop_shrink` after this point).
            rc.summary.config.drizzle.drop_shrink = clamped;
        }
    }

    let groups = rc.plan_groups.clone();
    let mut any_master = false;
    let mut normalize_total = Duration::ZERO;
    let mut integrate_total = Duration::ZERO;
    let mut drizzle_total = Duration::ZERO;
    let mut output_total = Duration::ZERO;
    let solved_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // Same expression `process_group_output` uses per group to decide
    // whether its own LN pass ran at all — one run has one `normalization`
    // config, so this is the same answer for every group.
    let ln_active = cfg.normalization.local.enabled
        || cfg.normalization.rejection == RejectionNormalization::Local;

    for group in &groups {
        rc.check_cancel()?;
        let reference_frame_id = rc.geometry_of(&group.key).reference_frame_id;
        // Fix round 1 (m2): a group `process_group_output` is about to skip
        // never writes a master, so neither the solve lookup nor the
        // missing-WCS warning belongs to it. `included_count_of` is the
        // same count that function's `included_count < 3` early return
        // reads, but it is read BEFORE the Output boundary's landed-file
        // check (`exclude_changed_members`) can lower it, so a project group
        // may still skip after passing this gate (the solve lookup is then
        // merely wasted, never wrong).
        let wcs = if included_count_of(rc, &group.key) >= 3 {
            match &rc.grid {
                // Spec §5.9, plan 6b-3 D11: every master of a project run
                // carries the grid's TAN.
                Some(grid) => Some(crate::stacking::grid_map::grid_plate_solve(
                    &grid.grid, &solved_at,
                )),
                None => {
                    if let std::collections::hash_map::Entry::Vacant(slot) =
                        solves.entry(reference_frame_id)
                    {
                        let conn = db(&rc.ctx)?.conn();
                        let origin = origin_of(&rc.origins, reference_frame_id)?;
                        slot.insert(stored_solve(&conn, origin, reference_frame_id)?);
                    }
                    solves
                        .get(&reference_frame_id)
                        .and_then(|s| s.as_ref())
                        .cloned()
                }
            }
        } else {
            None
        };
        let (outcome, n, i, d, o) = process_group_output(rc, group, &measure_opts, wcs.as_ref())?;
        normalize_total += n;
        integrate_total += i;
        drizzle_total += d;
        output_total += o;
        #[cfg(test)]
        if rc.subject.project_id().is_some()
            && CANCEL_AFTER_NEXT_GROUP.swap(false, Ordering::SeqCst)
        {
            rc.cancel.store(true, Ordering::SeqCst);
        }
        if matches!(outcome, GroupOutcome::Written) {
            any_master = true;
            // … and the warning belongs to a master that was ACTUALLY
            // written without a WCS — once per distinct reference frame, so
            // co-registered mode (where every group names the same one)
            // still warns exactly once, as it always has.
            if wcs.is_none() && warned_missing_wcs.insert(reference_frame_id) {
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id = reference_frame_id,
                    "no plate solve on the reference frame; the master has no WCS"
                );
                rc.warnings.push(
                    "no plate solve on the reference frame; the master has no WCS".to_string(),
                );
            }
        }
    }

    // Plan 6b-3 D10: the crop pass, once every group has written.
    if cfg.output.crop_to_coverage > 0 {
        let crop_start = Instant::now();
        finish_crop(rc)?;
        output_total += crop_start.elapsed();
    }

    // Ruling R5 (reverses fix round 1, item 6): pushed BEFORE `Integrate`
    // (stage order) and only when this run's own config had LN active — LN
    // disabled keeps the pre-M2 shape (no entry at all, not a zero one).
    if ln_active {
        rc.timings.push(crate::stacking::provenance::StageTiming {
            stage: Stage::Normalize,
            duration_ms: normalize_total.as_millis() as u64,
        });
    }
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Integrate,
        duration_ms: integrate_total.as_millis() as u64,
    });
    // M3 Task 5 (ruling R-M3-11, fix round 1 Minor M6): pushed BEFORE
    // `Output`, only when this run's own config had drizzle enabled AND at
    // least one group actually ATTEMPTED it (`rc.drizzle_attempted`, an
    // exact per-group counter incremented inside `process_group_output`
    // itself — not the `any_master` proxy this used to read, which would
    // still push a `0 ms` timing if every group's own drizzle attempt was
    // refused before `drizzle_group` ever ran, e.g. every `RejBitmapSet::
    // create` failing). Drizzle off, or a run where no group ever attempted
    // it, keeps the pre-M3 shape: no entry at all, not a zero one.
    if cfg.drizzle.enabled && rc.drizzle_attempted > 0 {
        rc.timings.push(crate::stacking::provenance::StageTiming {
            stage: Stage::Drizzle,
            duration_ms: drizzle_total.as_millis() as u64,
        });
    }
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Output,
        duration_ms: output_total.as_millis() as u64,
    });

    if !any_master {
        return Err(RunError::Other("no group produced a master".to_string()));
    }

    match cfg.output.cleanup {
        CleanupPolicy::KeepAll => {}
        CleanupPolicy::DeleteRegistered => {
            let freed = {
                let conn = db(&rc.ctx)?.conn();
                cleanup_work(&conn, &rc.subject, &rc.layout, CleanupWhat::Registered)?
            };
            tracing::info!(run_id = rc.run_id, freed_bytes = freed, "cleanup applied");
        }
        CleanupPolicy::DeleteIntermediates => {
            let freed = {
                let conn = db(&rc.ctx)?.conn();
                let sweep = !other_stack_active(&rc.ctx, &rc.subject);
                cleanup_work_with(
                    &conn,
                    &rc.subject,
                    &rc.layout,
                    CleanupWhat::Intermediates,
                    sweep,
                )?
            };
            tracing::info!(run_id = rc.run_id, freed_bytes = freed, "cleanup applied");
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
