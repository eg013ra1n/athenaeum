//! The stacking run thread (spec §8: "the permit is acquired first, so a
//! queued run waits behind an analysis or a master build"; ruling 2:
//! `start_stacking` returns `{ runId }` only). [`start_stacking`] validates
//! the plan, inserts the run + its group rows, registers a cancel handle on
//! [`crate::services::ServiceContext::active_stacks`] and spawns a dedicated
//! `stacking-run-<id>` thread; [`run_thread`] is that thread's single body —
//! catch-unwind around [`run_pipeline`], handle removal, `finish_run`,
//! writing `runs/run-<id>.json`, the terminal log lines, and exactly one
//! `stacking-complete` event, in that order, regardless of how the pipeline
//! ended. [`cancel_stacking`] just flips the registered cancel flag — the
//! thread notices it (inside the queue wait, or at [`RunContext::check_cancel`]
//! between frames/groups) and unwinds through [`RunError::Cancelled`].
//!
//! Plan 5a Task 6 lands the run thread itself, the progress/complete events,
//! and stage 1 (calibrate) — the calibrated-lights export's own generator
//! (`export::calibrated_generator`), reused verbatim, with per-frame
//! `stacking_artifacts` reuse keyed by [`crate::stacking::plan::calibration_hash_for`]'s
//! SAME hash the plan gate computes (ruling: a run and the plan that
//! preceded it must never disagree about what "fresh" means). Tasks 7-8 add
//! stages 3-9 to the SAME [`run_pipeline`]/[`RunContext`] this task defines —
//! see each item's doc comment for exactly what is provisioned now and
//! consumed later.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::api::{db, ApiError, PathPolicy};
use crate::calibration_library::cosmetic::HotPixelMapOutcome;
use crate::db::stacking::{
    finish_run, get_run, insert_group, insert_run, set_frame_rejected_fraction, set_run_reference,
    set_run_status, update_group, upsert_artifact, upsert_frame_row, GroupUpdate, NewArtifact,
    NewFrameRow, NewGroup, NewRun,
};
use crate::events::{emit_event, ProgressEmitter};
use crate::export::{execute_generation, resolve_generation_cached};
use crate::fits_parser::FitsHeader;
use crate::fits_writer::wcs::scale_plate_solve;
use crate::fits_writer::{Card, CardValue};
use crate::geometry::PixelMap;
use crate::integration::band_budget::total_ram_bytes;
use crate::integration::engine::EngineProgress;
use crate::integration::io_policy::IoPolicy;
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::{NormalizationPair, RejectionNormalization};
use crate::integration::IntegrationError;
use crate::plate_solve::storage::{get_plate_solve, PlateSolveRecord};
use crate::registration::db::{
    get_frame_set_reference, get_registration_for_frame_set, upsert_registration,
    RegistrationRecord,
};
use crate::services::compute_queue::ComputeJobKind;
use crate::services::{ServiceContext, StackHandle};
#[cfg(test)]
use crate::stacking::config::config_hash;
use crate::stacking::config::{CleanupPolicy, ReferenceMode, StackingConfig};
use crate::stacking::drizzle::{
    drizzle_group, DrizzleError, DrizzleFrame, DrizzleInput, DrizzleProgress, DrizzleStats,
};
use crate::stacking::groups::{group_frames, set_slug, ColorMode, GroupFrame, IntegrationGroup};
use crate::stacking::integrate::{
    included_after_min_weight, integrate_group, GroupInput, GroupProgress, GroupStats, StackFrame,
};
use crate::stacking::ln::{
    background_grid, build_reference as build_ln_reference, normalize_frame, read_reference,
    write_reference, BackgroundGrid, BackgroundParams, LnFrameGrids, LnReference,
    LnReferenceForDetection, DEFAULT_PARAMS,
};
use crate::stacking::master_cards::{
    build_drizzle_cards, build_master_light_cards, master_file_name, write_drizzled_master,
    write_master_light, MasterCardInputs, WrittenDrizzle,
};
use crate::stacking::measure::{measure_frame, FrameMeasurement, MeasureOptions};
use crate::stacking::paths::{cleanup_work, CleanupWhat, WorkingLayout};
use crate::stacking::plan::{
    build_plan, is_fresh, measurement_hash_for, normalization_hash_for, registration_hash_for,
    registration_row_is_fresh, HashMemo, LnReferencePayload, MasterWork, PlanMaster, Stage,
};
use crate::stacking::provenance::{
    MasterBuilt, RunSummary, SummaryFrame, SummaryGroup, SummaryMeasurement, SummaryReference,
};
use crate::stacking::register::frame::{
    identity_registration, reference_stars, register_frame, to_record,
};
use crate::stacking::register::writer::{
    build_registered_cards, source_cards_from_file, write_registered_frame, RegisteredCards,
};
use crate::stacking::rej::RejBitmapSet;
use crate::stacking::weights::{
    best_by_weight, compute_weights, select_frames, FrameWeight, WeightInput, WeightMode,
};

/// Wire event for `stacking-progress`. `percent` is `100 * current / total`
/// (`100.0` when `total == 0`); `bytes_done`/`bytes_total` describe the
/// current STAGE's byte footprint, not the whole run's.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingProgressEvent {
    pub run_id: i64,
    pub set_id: i64,
    pub stage: Stage,
    pub group_key: Option<String>,
    pub current: usize,
    pub total: usize,
    pub percent: f64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub frame_id: Option<i64>,
    pub message: Option<String>,
}

/// One group's written master, for `StackingCompleteEvent::masters` (ruling
/// 15: per-group masters, listing every group that actually wrote one).
/// Populated by Task 8's Output stage — always empty in this task, since
/// stage 1 (calibrate) never writes a master.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingMasterRef {
    pub group_key: String,
    pub path: String,
    pub drizzle_path: Option<String>,
}

/// Wire event for `stacking-complete` — emitted exactly once per run, from
/// [`run_thread`]'s single exit path, regardless of success/cancel/failure/
/// panic.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingCompleteEvent {
    pub run_id: i64,
    pub set_id: i64,
    pub success: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub masters: Vec<StackingMasterRef>,
}

pub const STACKING_PROGRESS_EVENT: &str = "stacking-progress";
pub const STACKING_COMPLETE_EVENT: &str = "stacking-complete";
pub const PROGRESS_THROTTLE_MS: u64 = 300;

/// [`start_stacking`]'s success payload. Ruling 2: the `ComputeQueue` permit
/// is acquired INSIDE the run thread, so no queue job id exists yet when
/// this returns — the sidebar finds the job by its `"Stacking · <set name>"`
/// label in the queue snapshot instead.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StartedStacking {
    pub run_id: i64,
}

/// A stacking run's terminal outcome, as far as `run_pipeline`/its stages are
/// concerned. `Other`'s `String` is the human-readable failure — already
/// formatted (`{e:#}`-style) by whichever stage produced it, since
/// [`RunContext`] never carries the original error TYPE across a stage
/// boundary, only its text.
#[derive(Debug)]
pub(crate) enum RunError {
    Cancelled,
    Other(String),
    /// M2 Task 7 (carry-over (b) from Task 5's re-review): a DB error while
    /// persisting a stage-6 exclusion (`exclude_frame_and_persist`), raised
    /// AFTER that call already flipped the frame's in-memory
    /// `MeasuredFrame::included` — the frame's state is now inconsistent
    /// (excluded in memory, possibly not on disk), so the caller must fail
    /// the group outright rather than treat this like any other recoverable
    /// local-normalization error and continue with global normalization as
    /// if the frame were still a normal member. Constructed nowhere else;
    /// never escapes `process_group_output`, which converts it to a
    /// `fail_group` call — the arm on the top-level status match below is
    /// exhaustiveness only.
    ExclusionPersistFailed(String),
}

impl From<ApiError> for RunError {
    fn from(e: ApiError) -> Self {
        RunError::Other(e.to_string())
    }
}

impl From<anyhow::Error> for RunError {
    fn from(e: anyhow::Error) -> Self {
        RunError::Other(format!("{e:#}"))
    }
}

/// One stacking run's whole mutable state, held by the dedicated
/// `stacking-run-<id>` thread for its entire life. Built once in
/// [`start_stacking`] (or, in tests, by [`test_context`]) and threaded
/// through every stage function by `&mut` — this is the "RunContext" Tasks
/// 7-8 extend with their own stages, reading/writing the SAME fields.
///
/// Fields beyond the task-6 brief's literal interface (documented individually
/// below): `group_ids`, `memo`, `hot_maps`, `runtime_exclusions`, and the
/// `#[cfg(test)]` `fail_after_stage` hook.
pub(crate) struct RunContext {
    pub(crate) ctx: Arc<ServiceContext>,
    pub(crate) emitter: Arc<dyn ProgressEmitter>,
    pub(crate) run_id: i64,
    pub(crate) set_id: i64,
    pub(crate) set_name: String,
    pub(crate) config: StackingConfig,
    pub(crate) hash: String,
    pub(crate) plan_groups: Vec<IntegrationGroup>,
    /// Stage 0.5's work list (spec §2 row 0.5, owner requirement 2026-09-09),
    /// as resolved by the plan gate at `start_stacking` time — the SAME list
    /// `stage_masters` executes, in order. Empty when there is nothing to
    /// build or rebuild.
    pub(crate) masters_to_build: Vec<PlanMaster>,
    /// Manually-excluded frame ids (spec §9.1's `stacking_set_config.excluded_frame_ids`,
    /// already resolved by the plan gate) — a frame in here is skipped by
    /// every stage, never calibrated/measured/registered/integrated.
    pub(crate) excluded: Vec<i64>,
    /// `group_key -> stacking_run_groups.id`, populated by [`start_stacking`]
    /// right after [`insert_run`] so every later stage (`upsert_frame_row`'s
    /// `group_id`, `update_group`) has the row id without a second DB
    /// round-trip. Read by stages 3 and 5 (Task 7's `stage_measure`/
    /// `write_frame_rows`).
    pub(crate) group_ids: HashMap<String, i64>,
    pub(crate) layout: WorkingLayout,
    /// The run's output folder — every group's master (and, when requested,
    /// its rejection maps) lands directly here (Task 8's Output stage).
    pub(crate) output_dir: PathBuf,
    pub(crate) cancel: Arc<AtomicBool>,
    /// Seeds `summary.app_version` at construction; Task 8's master-card
    /// headers (`MasterCardInputs::app_version`, the `SWCREATE` card) read it
    /// again.
    pub(crate) app_version: String,
    pub(crate) warnings: Vec<String>,
    pub(crate) timings: Vec<crate::stacking::provenance::StageTiming>,
    pub(crate) summary: RunSummary,
    pub(crate) last_emit: Instant,
    pub(crate) rerun_from: Option<Stage>,
    /// This run's ONE [`HashMemo`] (decision 3): the calibrate stage's stage-1
    /// hash lookups AND its `resolve_generation_cached` calls share the same
    /// [`crate::export::DivisorCache`] via [`HashMemo::divisors_mut`].
    pub(crate) memo: HashMemo,
    /// This run's ONE hot-pixel-map cache, keyed by resolved master dark path
    /// (decision 4) — a dark shared by every frame in a group pays the
    /// measurement once.
    pub(crate) hot_maps: HashMap<PathBuf, Arc<HotPixelMapOutcome>>,
    /// `frame_id -> whether stage 1 REUSED an existing `calibrated` artifact
    /// for it` (Task 8's own addition — the brief's `SummaryFrame.cached_calibrated`
    /// has no field on [`MeasuredFrame`] to read it from otherwise, since
    /// `CalibrateOutcome` is transient and stage 3 builds `MeasuredFrame`
    /// fresh from the DB, blind to which run wrote the artifact it found).
    /// Populated once per frame by [`stage_calibrate`]; read by
    /// [`stage_measure`] when it builds each frame's [`MeasuredFrame`].
    pub(crate) cached_calibrated: HashMap<i64, bool>,
    /// Frames excluded AT RUN TIME (as opposed to `excluded`'s manual,
    /// pre-run exclusions), with why — stage 1 pushes `(frame_id,
    /// "calibration failed: …")` here; stages 3 and 5 (Task 7) push their
    /// own failures the same way; `write_frame_rows` (end of stage 5) reads
    /// the union back to build `stacking_run_frames` rows.
    pub(crate) runtime_exclusions: Vec<(i64, String)>,
    /// Group key -> every one of that group's frames' stage 3-5 lifecycle
    /// (Task 7's own addition — the brief's `MeasuredFrame`/
    /// `RegisteredFrameOutcome` shapes, see their doc comments below for the
    /// deviations from the brief's literal, non-`Option` field types). Built
    /// once per group at the end of stage 3, mutated in place by stage 5 with
    /// each frame's registration outcome. Task 8's integrate stage reads this
    /// directly: the per-group INCLUDED frames' calibrated paths,
    /// measurements and `PixelMap`s all live here.
    pub(crate) measured: HashMap<String, Vec<MeasuredFrame>>,
    /// The run's chosen reference (stage 4) — `None` until stage 4 runs.
    pub(crate) reference_frame_id: Option<i64>,
    /// The reference frame's own calibrated file (stage 4 looks this up from
    /// `measured`, once, so stage 5 does not need to search for it again).
    pub(crate) reference_calibrated: Option<PathBuf>,
    /// The reference's own measured geometry (ruling 6: every group's master
    /// adopts this geometry) — set by stage 5 from `reference_stars`'s own
    /// read of the reference's calibrated file (the natural place: stage 5
    /// is what calls `reference_stars`; stage 4 only picks WHICH frame is
    /// the reference, not its pixel geometry).
    pub(crate) reference_width: usize,
    pub(crate) reference_height: usize,
    /// M3 Task 5, fix round 1 (Minor M6): incremented once per group that
    /// actually reaches an attempt at `drizzle_group` (i.e. drizzle is on
    /// AND the group's `RejBitmapSet` — when wanted — was created
    /// successfully), regardless of whether the attempt then succeeds,
    /// fails, or is refused by the `output_pairs` shape check. `stage_output`
    /// reads it after the group loop to decide whether to push a
    /// `Stage::Drizzle` `StageTiming` — an EXACT count, unlike the `any_master`
    /// proxy that used to gate it (a run where every group's bitmap set
    /// failed to create would push a `0 ms` `Drizzle` timing under that
    /// proxy despite `drizzle_group` never having run at all).
    pub(crate) drizzle_attempted: usize,
    /// Test-only fault injection: [`run_pipeline`] panics right after the
    /// named stage completes, so [`run_thread`]'s catch-unwind/single-exit-path
    /// contract can be exercised without a real failure anywhere in the
    /// pipeline itself.
    #[cfg(test)]
    pub(crate) fail_after_stage: Option<Stage>,
}

impl RunContext {
    /// `Err(RunError::Cancelled)` iff the run's cancel flag is set. Called
    /// between frames and between groups so a cancel lands promptly without
    /// polling inside the hot per-frame loop.
    fn check_cancel(&self) -> Result<(), RunError> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(RunError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Emit `stacking-progress`, throttled (decision 6): `current == 0`,
    /// `current == total`, or at least [`PROGRESS_THROTTLE_MS`] since the
    /// last emission for ANY stage — so the first and last events of every
    /// stage always go out even if the whole stage runs in under 300 ms.
    #[allow(clippy::too_many_arguments)]
    fn progress(
        &mut self,
        stage: Stage,
        group_key: Option<String>,
        current: usize,
        total: usize,
        bytes_done: u64,
        bytes_total: u64,
        frame_id: Option<i64>,
        message: Option<String>,
    ) {
        let percent = if total == 0 {
            100.0
        } else {
            100.0 * current as f64 / total as f64
        };
        let now = Instant::now();
        let force = current == 0 || current == total;
        if !force
            && now.duration_since(self.last_emit) < Duration::from_millis(PROGRESS_THROTTLE_MS)
        {
            return;
        }
        self.last_emit = now;
        emit_event(
            self.emitter.as_ref(),
            STACKING_PROGRESS_EVENT,
            &StackingProgressEvent {
                run_id: self.run_id,
                set_id: self.set_id,
                stage,
                group_key,
                current,
                total,
                percent,
                bytes_done,
                bytes_total,
                frame_id,
                message,
            },
        );
    }
}

fn color_mode_wire(mode: ColorMode) -> &'static str {
    match mode {
        ColorMode::Mono => "mono",
        ColorMode::Osc => "osc",
    }
}

fn reference_mode_wire(mode: ReferenceMode) -> &'static str {
    match mode {
        ReferenceMode::Auto => "auto",
        ReferenceMode::Manual => "manual",
    }
}

/// `stacking_run_groups.width`/`height` at insert time (owner decision
/// 2026-09-10: `IntegrationGroup` no longer carries a group-level
/// width/height — a group's members can differ in native geometry now that
/// camera/geometry are not grouping keys). The run's actual reference
/// geometry (`RunContext::reference_width`/`height`) is not known this
/// early — it is only resolved once stage 5 (register) picks the ONE
/// run-wide reference frame — so this reads the group's own
/// reference-anchor member's native `NAXIS1`/`NAXIS2` instead: the same
/// "first member, `(date_obs, id)` order" convention `IntegrationGroup.instrume`
/// already uses for its own display value. `None` only for an (unreachable
/// in practice) empty group — the DB column stays nullable either way.
///
/// `pub(crate)` since M3 Task 5: `stacking::plan`'s own `PlanGroup.anchor_width`/
/// `anchor_height` reuse this SAME convention rather than re-deriving it — a
/// plan and the run it precedes must never disagree about which member
/// anchors a group's geometry.
pub(crate) fn group_anchor_geometry(g: &IntegrationGroup) -> (Option<i64>, Option<i64>) {
    match g.frames.first() {
        Some(f) => (Some(f.width), Some(f.height)),
        None => (None, None),
    }
}

/// `(size, modified_at)` for a just-written file, in the scanner's own shape
/// (`scanner/mod.rs`'s `modified_dt.to_rfc3339()`) — the same identity
/// [`crate::stacking::plan::calibration_hash_for`] reads back off a
/// `stacking_artifacts` row's `size`/`modified_at` when deciding freshness.
fn file_identity(path: &Path) -> anyhow::Result<(i64, String)> {
    let meta = std::fs::metadata(path)?;
    let modified = meta.modified()?;
    let modified_at = chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339();
    Ok((meta.len() as i64, modified_at))
}

/// Heal every `stacking_runs` row a crashed or killed process left
/// `planning`/`running` forever (final fix wave, Critical item 1): such a
/// row has no live cancel handle in [`ServiceContext::active_stacks`] (that
/// map is process-memory only — a restart always starts it empty), so
/// [`start_stacking`]'s own "already active" check and
/// [`crate::api::stacking::cleanup_stacking_work`]'s `Conflict` guard would
/// otherwise refuse forever with no command able to clear the row. Finishes
/// each stuck row as `"failed"` (no summary JSON — the run's own thread is
/// long gone, there is nothing to snapshot) with
/// `error = "interrupted by a restart"`, one `warn!` per row.
///
/// Deliberately entry-point-driven, not host-startup-driven (unlike
/// [`crate::api::sync_prepare::heal_interrupted_preparations`], which the
/// two hosts call once at boot): a stuck stacking run only ever matters to a
/// caller about to read or act on its frame set, and every such caller
/// already goes through one of the five `api::stacking` handlers this is
/// wired into (`get_stacking_plan`, `start_stacking`, `get_stacking_runs`,
/// `get_stacking_run`, `cleanup_stacking_work`) — a sixth process-wide wire-up
/// would heal rows nobody is about to look at, for no benefit over healing
/// on demand.
///
/// A row WITH a live handle (the normal case — this process's own run still
/// actually running) is left alone: `active` is checked before anything is
/// touched, and only ids missing from it are ever finished.
pub(crate) fn heal_interrupted_runs(
    ctx: &ServiceContext,
    conn: &rusqlite::Connection,
) -> Result<usize, ApiError> {
    let unfinished = crate::db::stacking::list_unfinished_runs(conn)?;
    if unfinished.is_empty() {
        return Ok(0);
    }

    let stuck: Vec<(i64, i64)> = {
        let active = ctx.active_stacks.lock().unwrap();
        unfinished
            .into_iter()
            .filter(|(run_id, _)| !active.contains_key(run_id))
            .collect()
    };

    let mut healed = 0usize;
    for (run_id, frames_set_id) in stuck {
        tracing::warn!(
            run_id,
            frames_set_id,
            "stacking run interrupted by a restart"
        );
        finish_run(
            conn,
            run_id,
            "failed",
            None,
            Some("interrupted by a restart"),
        )?;
        healed += 1;
    }
    Ok(healed)
}

/// RAII guard for [`start_stacking`]'s `active_stacks` handle (Plan 5b final
/// fix wave, review finding B2) — same shape as
/// [`crate::api::masters::ActiveBuildGuard`]. Armed the instant the handle is
/// registered, covering everything from there through the thread spawn:
/// `get_run`, the per-group `insert_group` loop, and the spawn itself can
/// all still fail. Before this guard existed, such a failure returned via
/// `?` straight past the handle-removal / row-finishing cleanup the
/// spawn-failure path already had — the handle stayed in `active_stacks`
/// and the `stacking_runs` row stayed `"planning"` forever, so
/// [`heal_interrupted_runs`] (which only heals rows ABSENT from
/// `active_stacks`) would never touch it and every later `start_stacking`
/// for the same set answered `Conflict` until the process restarted.
///
/// Dropped while still armed: removes the handle and finishes the run row
/// as `"failed"`, with [`Self::fail`]'s message when one was recorded (every
/// call site on the risky path calls it right before returning `Err`) or a
/// generic fallback otherwise. Disarmed once `spawn_result` is `Ok` — from
/// there, [`run_thread`]'s own single exit path owns both.
struct StartStackingGuard<'a> {
    ctx: &'a ServiceContext,
    run_id: i64,
    armed: bool,
    error: Option<String>,
}

impl<'a> StartStackingGuard<'a> {
    fn new(ctx: &'a ServiceContext, run_id: i64) -> Self {
        Self {
            ctx,
            run_id,
            armed: true,
            error: None,
        }
    }

    /// Record the error that is about to unwind past this guard via `?` —
    /// `Drop::drop` has no other way to see it.
    fn fail(&mut self, error: impl std::fmt::Display) {
        self.error = Some(error.to_string());
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StartStackingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.ctx.active_stacks.lock().unwrap().remove(&self.run_id);
        let error = self.error.clone().unwrap_or_else(|| {
            "stacking run setup failed before the run thread could start".to_string()
        });
        match db(self.ctx) {
            Ok(database) => {
                let conn = database.conn();
                if let Err(e) = finish_run(&conn, self.run_id, "failed", None, Some(&error)) {
                    tracing::warn!(
                        run_id = self.run_id,
                        error = %e,
                        "failed to mark stacking run failed after a setup failure"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    run_id = self.run_id,
                    error = %e,
                    "failed to open a connection to mark stacking run failed after a setup failure"
                );
            }
        }
    }
}

/// Start a stacking run for `frames_set_id`: build the plan, refuse on the
/// first blocker or an already-active run, insert the run + its group rows,
/// register a cancel handle, and spawn the dedicated thread. Returns as soon
/// as the thread is spawned — the queue permit is acquired INSIDE the thread
/// (ruling 2).
pub fn start_stacking(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    policy: &PathPolicy,
    app_version: String,
    frames_set_id: i64,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, ApiError> {
    let db_handle = db(&ctx)?;
    let conn = db_handle.conn();

    // Critical fix, item 1: heal any row a crashed/killed process left
    // `planning`/`running` BEFORE the plan's own `active_run_id` read below —
    // otherwise a stuck row from a previous process would read as "already
    // active" forever, with no command able to clear it.
    heal_interrupted_runs(&ctx, &conn)?;

    let plan = build_plan(&conn, &ctx.settings, policy, frames_set_id, config)?;

    if let Some(blocker) = plan.blockers.first() {
        return Err(ApiError::Invalid(blocker.message.clone()));
    }
    // Advisory only (fix round 1, item 3): catches a row left `planning`/
    // `running` by a crashed process. The REAL guard against a same-process
    // double start is the `active_stacks` lock below — this read can still
    // race against another `start_stacking` call reaching that lock first
    // (both could pass this check), and that is fine: the lock is what
    // actually serializes the run-row insert and the handle registration.
    if let Some(active_id) = plan.active_run_id {
        return Err(ApiError::Conflict(format!(
            "a stacking run (id {active_id}) is already active for frame set {frames_set_id}"
        )));
    }

    let working_dir = plan.working_dir.clone().ok_or_else(|| {
        ApiError::Internal("stacking plan reported no blockers but no working folder".to_string())
    })?;
    let output_dir_str = plan.output_dir.clone().ok_or_else(|| {
        ApiError::Internal("stacking plan reported no blockers but no output folder".to_string())
    })?;

    // Final fix wave, item 5: the plan itself validates in `ValidateMode::Plan`
    // — no filesystem write, since `build_plan` above runs on every debounced
    // config edit, not just a real start. A run that is actually about to
    // write into these folders needs them to exist, so `start_stacking` is
    // the ONE place that calls `ValidateMode::Save` — creating both folders
    // (and probing them writable) right before the thread spawns. From here
    // on, `working_dir`/`output_dir_str` are Save mode's own canonicalized,
    // post-creation paths, not the plan's lexical ones.
    let validated_dirs = crate::stacking::paths::validate_dirs(
        &conn,
        policy,
        &working_dir,
        &output_dir_str,
        crate::stacking::paths::ValidateMode::Save,
    )?;
    let working_dir = validated_dirs.working.to_string_lossy().into_owned();
    let output_dir_str = validated_dirs.output.to_string_lossy().into_owned();

    let plan_groups = group_frames(&conn, frames_set_id, &plan.config.grouping)?;

    let config_json = serde_json::to_string(&plan.config)
        .map_err(|e| ApiError::Internal(format!("failed to serialize stacking config: {e}")))?;

    // Fix round 1, item 3: the "already running" check AND the run-row
    // insert + handle registration happen under ONE `active_stacks` lock —
    // lock; scan for a handle already covering this set → `Conflict`;
    // `insert_run` (a short insert) while still holding the lock; insert
    // the handle; unlock. Two concurrent `start_stacking` calls for the
    // SAME set can never both pass: whichever reaches the lock second sees
    // the first's handle before it can insert its own run row.
    let cancel = Arc::new(AtomicBool::new(false));
    let run_id = {
        let mut active = ctx.active_stacks.lock().unwrap();
        if active.values().any(|h| h.frames_set_id == frames_set_id) {
            return Err(ApiError::Conflict(format!(
                "a stacking run is already active for frame set {frames_set_id}"
            )));
        }
        let run_id = insert_run(
            &conn,
            &NewRun {
                frames_set_id,
                config_json: &config_json,
                config_hash: &plan.config_hash,
                reference_frame_id: plan.reference.frame_id,
                reference_mode: reference_mode_wire(plan.reference.mode),
                working_dir: &working_dir,
                output_dir: &output_dir_str,
            },
        )?;
        active.insert(
            run_id,
            StackHandle {
                cancel_flag: cancel.clone(),
                frames_set_id,
            },
        );
        run_id
    };

    // Fix round: Plan 5b final fix wave, review finding B2. Armed from here
    // through the spawn below — see `StartStackingGuard`'s own doc comment.
    let mut guard = StartStackingGuard::new(&ctx, run_id);

    let started_at = match get_run(&conn, run_id) {
        Ok(row) => row
            .map(|r| r.started_at)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        Err(e) => {
            guard.fail(&e);
            return Err(e.into());
        }
    };

    let included_by_key: HashMap<&str, usize> = plan
        .groups
        .iter()
        .map(|g| (g.key.as_str(), g.included_count))
        .collect();

    let mut group_ids: HashMap<String, i64> = HashMap::with_capacity(plan_groups.len());
    for g in &plan_groups {
        let included_count = included_by_key
            .get(g.key.as_str())
            .copied()
            .unwrap_or(g.frames.len());
        let (anchor_width, anchor_height) = group_anchor_geometry(g);
        let group_id = match insert_group(
            &conn,
            &NewGroup {
                run_id,
                group_key: &g.key,
                instrume: g.instrume.as_deref(),
                color_mode: color_mode_wire(g.color_mode),
                filter: g.filter.as_deref(),
                binning: Some(g.binning),
                width: anchor_width,
                height: anchor_height,
                exposure: g.exposure_s,
                frame_count: g.frames.len() as i64,
                included_count: included_count as i64,
            },
        ) {
            Ok(id) => id,
            Err(e) => {
                guard.fail(&e);
                return Err(e.into());
            }
        };
        group_ids.insert(g.key.clone(), group_id);
    }

    let set_slug_str = set_slug(&plan.set_name);
    let layout = WorkingLayout::new(Path::new(&working_dir), &set_slug_str);
    let output_dir = PathBuf::from(&output_dir_str);

    let summary = RunSummary {
        run_id,
        set_id: frames_set_id,
        set_name: plan.set_name.clone(),
        app_version: app_version.clone(),
        started_at,
        finished_at: None,
        status: "planning".to_string(),
        config: plan.config.clone(),
        config_hash: plan.config_hash.clone(),
        reference: SummaryReference {
            frame_id: plan.reference.frame_id,
            filename: plan.reference.filename.clone(),
            mode: plan.reference.mode,
            weight: None,
        },
        measurement: SummaryMeasurement {
            seed_source: "fast".to_string(),
            scale_estimator: plan.config.normalization.scale_estimator,
        },
        groups: Vec::new(),
        masters_built: Vec::new(),
        stages: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };

    let rc = RunContext {
        ctx: ctx.clone(),
        emitter: emitter.clone(),
        run_id,
        set_id: frames_set_id,
        set_name: plan.set_name,
        config: plan.config,
        hash: plan.config_hash,
        plan_groups,
        masters_to_build: plan.masters_to_build,
        excluded: plan.excluded_frame_ids,
        group_ids,
        layout,
        output_dir,
        cancel,
        app_version,
        warnings: Vec::new(),
        timings: Vec::new(),
        summary,
        last_emit: Instant::now(),
        rerun_from,
        memo: HashMemo::new(),
        hot_maps: HashMap::new(),
        cached_calibrated: HashMap::new(),
        runtime_exclusions: Vec::new(),
        measured: HashMap::new(),
        reference_frame_id: None,
        reference_calibrated: None,
        reference_width: 0,
        reference_height: 0,
        drizzle_attempted: 0,
        #[cfg(test)]
        fail_after_stage: None,
    };

    let spawn_result = std::thread::Builder::new()
        .name(format!("stacking-run-{run_id}"))
        .spawn(move || {
            run_thread(rc);
        });

    match spawn_result {
        Ok(_) => {
            // The thread started — from here on it owns handle removal /
            // `finish_run` / `stacking-complete` via its own single exit
            // path (`run_thread`). Disarm so this guard's `Drop` is a no-op.
            guard.disarm();
        }
        Err(e) => {
            // The thread never started, so nothing will ever remove the
            // handle, finish the run row, or emit stacking-complete —
            // `guard`'s `Drop` (still armed) does that cleanup below.
            let msg = format!("failed to spawn stacking thread: {e}");
            guard.fail(&msg);
            return Err(ApiError::Internal(msg));
        }
    }

    Ok(StartedStacking { run_id })
}

/// Cancel an active stacking run (queued-in-compute-queue or running).
pub fn cancel_stacking(ctx: &ServiceContext, run_id: i64) -> Result<(), ApiError> {
    let active = ctx.active_stacks.lock().unwrap();
    if let Some(handle) = active.get(&run_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err(ApiError::NotFound(format!(
            "no active stacking run {run_id}"
        )))
    }
}

/// Runs on the dedicated `stacking-run-{run_id}` thread. The single exit
/// path for the whole run: `finish_run`, handle removal, the provenance
/// snapshot, the terminal log lines, and `stacking-complete` ALWAYS happen
/// here, exactly once, regardless of how [`run_pipeline`] ended (including a
/// panic inside it). Fix round 1, item 4: `finish_run` runs BEFORE the
/// handle is removed from `active_stacks` — a `cancel_stacking` call that
/// lands in between sees the row already terminal (never `NotFound` while
/// the row still reads `running`); its flag-set is simply never read by
/// anything afterward, which is harmless. The old order (handle removed
/// first) could show a run nowhere in `active_stacks` while its DB row
/// still said `running`, which is the confusing state this avoids.
fn run_thread(mut rc: RunContext) {
    let run_id = rc.run_id;
    let set_id = rc.set_id;
    let run_started = Instant::now();

    let result =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_pipeline(&mut rc))) {
            Ok(r) => r,
            Err(panic) => {
                let detail = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown".to_string());
                let msg = format!("stacking run panicked: {detail}");
                tracing::error!(run_id, error = %msg, "stacking run thread panicked");
                Err(RunError::Other(msg))
            }
        };

    let (status, success, cancelled, error): (&str, bool, bool, Option<String>) = match &result {
        Ok(()) => ("done", true, false, None),
        Err(RunError::Cancelled) => {
            tracing::info!(run_id, "stacking run cancelled");
            ("cancelled", false, true, None)
        }
        Err(RunError::Other(msg)) | Err(RunError::ExclusionPersistFailed(msg)) => {
            tracing::error!(run_id, error = %msg, "stacking run failed");
            ("failed", false, false, Some(msg.clone()))
        }
    };

    // Ruling 15 / brief: `masters` lists every group that actually wrote one
    // (`SummaryGroup.master_path` is `Some` only then); `success` additionally
    // requires at least one — `stage_output` already fails the whole run when
    // no group ever wrote a master, so this is a redundant-but-cheap safety
    // net, not the primary gate.
    let masters: Vec<StackingMasterRef> = rc
        .summary
        .groups
        .iter()
        .filter_map(|g| {
            g.master_path.clone().map(|path| StackingMasterRef {
                group_key: g.key.clone(),
                path,
                drizzle_path: g.drizzle_path.clone(),
            })
        })
        .collect();
    let success = success && !masters.is_empty();

    rc.summary.status = status.to_string();
    rc.summary.finished_at =
        Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    rc.summary.warnings = rc.warnings.clone();
    rc.summary.stages = rc.timings.clone();
    rc.summary.error = error.clone();

    let summary_json = match serde_json::to_string(&rc.summary) {
        Ok(json) => Some(json),
        Err(e) => {
            tracing::warn!(run_id, error = %e, "failed to serialize stacking run summary");
            None
        }
    };

    match db(&rc.ctx) {
        Ok(db_handle) => {
            let conn = db_handle.conn();
            if let Err(e) = finish_run(
                &conn,
                run_id,
                status,
                summary_json.as_deref(),
                error.as_deref(),
            ) {
                tracing::warn!(run_id, error = %e, "failed to persist stacking run completion");
            }
        }
        Err(e) => {
            tracing::warn!(run_id, error = %e, "failed to persist stacking run completion: database unavailable");
        }
    }

    rc.ctx.active_stacks.lock().unwrap().remove(&run_id);

    if let Some(json) = &summary_json {
        if let Err(e) = std::fs::create_dir_all(rc.layout.runs_dir()) {
            tracing::warn!(run_id, error = %e, "failed to create the runs folder for the provenance snapshot");
        } else if let Err(e) = std::fs::write(rc.layout.run_json(run_id), json) {
            tracing::warn!(
                run_id,
                path = %rc.layout.run_json(run_id).display(),
                error = %e,
                "failed to write the stacking run provenance snapshot"
            );
        }
    }

    for t in &rc.timings {
        tracing::info!(
            run_id,
            stage = t.stage.as_str(),
            duration_ms = t.duration_ms,
            "stacking stage finished"
        );
    }

    let duration_ms = run_started.elapsed().as_millis() as u64;
    tracing::info!(
        run_id,
        set_id,
        duration_ms,
        outcome = status,
        "stacking run finished"
    );

    // M3 Task 5 (ruling R-M3-8): `.rej` bitmaps are per-run temporaries —
    // removed HERE, at the run's single exit path, for EVERY outcome
    // (success, cancel, failure, panic-recovery all reach this line) unless
    // the user asked to keep everything. A missing dir (drizzle never ran
    // for this run, or it never wanted rejection bitmaps) is not an error; a
    // removal failure is a `warn!`, never fails the already-decided run.
    if rc.config.output.cleanup != CleanupPolicy::KeepAll {
        let rej_run_dir = rc.layout.rej_run_dir(run_id);
        match std::fs::remove_dir_all(&rej_run_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    run_id,
                    path = %rej_run_dir.display(),
                    error = %e,
                    "failed to remove the run's rejection-bitmap temporaries"
                );
            }
        }
    }

    emit_event(
        rc.emitter.as_ref(),
        STACKING_COMPLETE_EVENT,
        &StackingCompleteEvent {
            run_id,
            set_id,
            success,
            cancelled,
            error,
            warnings: rc.warnings.clone(),
            masters,
        },
    );
}

/// The whole pipeline, run in order. Queue admission happens FIRST (ruling
/// 2 / decision 1): a `QueueCancelled` while queued becomes
/// [`RunError::Cancelled`] before `set_run_status(running)` is ever called,
/// so a run cancelled while still waiting in line never shows as `running`
/// (see the `cancel_before_admission_finishes_cancelled` test). Tasks 7-8
/// append their own stages after stage 1's `#[cfg(test)]` fault-injection
/// check below.
fn run_pipeline(rc: &mut RunContext) -> Result<(), RunError> {
    let label = format!("Stacking · {}", rc.set_name);
    let (_permit, _job_id) = rc
        .ctx
        .compute_queue
        .acquire(ComputeJobKind::Stacking, &label, rc.cancel.clone())
        .map_err(|_queue_cancelled| RunError::Cancelled)?;

    {
        let conn = db(&rc.ctx)?.conn();
        set_run_status(&conn, rc.run_id, "running")?;
    }

    stage_masters(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Masters) {
        panic!("injected test failure after stage masters");
    }

    stage_calibrate(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Calibrate) {
        panic!("injected test failure after stage calibrate");
    }

    stage_measure(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Measure) {
        panic!("injected test failure after stage measure");
    }

    stage_reference(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Reference) {
        panic!("injected test failure after stage reference");
    }

    stage_register(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Register) {
        panic!("injected test failure after stage register");
    }

    stage_output(rc)?;

    Ok(())
}

// ── Stage 0.5: the run builds/rebuilds its own masters ──────────────────────
//
// Spec §2 row 0.5, owner requirement 2026-09-09 ("the pipeline should build
// the calibration masters itself when they are missing"). The plan gate
// (`stacking::plan::build_plan`) already resolved WHICH masters to build —
// `RunContext::masters_to_build` is that SAME list, carried over verbatim
// from `start_stacking`'s own plan (never re-derived here) — this stage just
// executes it, in order, before calibration ever runs.

/// Build or rebuild every [`PlanMaster`] in `rc.masters_to_build`, in order
/// (bias/darkflat before dark before flat — the list is already sorted that
/// way by [`crate::stacking::plan::collect_masters_to_build`]'s
/// `type_build_rank` sort, mirroring the manual batch-build's own dependency
/// order). A no-op — no progress event, no timing entry — when there is
/// nothing to build (`plan.mastersToBuild` empty): most runs never touch
/// this stage at all.
///
/// Runs entirely on THIS thread via [`crate::api::masters::build_master_inline`]
/// under `Admission::Inherited` — `run_pipeline`'s own `ComputeJobKind::Stacking`
/// permit (acquired above, held for the whole pipeline) covers the pixel
/// work, so this never touches the `ComputeQueue` a second time (see
/// `Admission`'s own doc comment for why a second acquire here would be
/// wrong, not just redundant).
///
/// A build/rebuild failure is FATAL to the whole run (`RunError::Other`) —
/// the run cannot calibrate a light against a master that was never built;
/// a cancel (either the run's own cancel flag, checked between items, or one
/// `build_master_inline` itself observes mid-build) unwinds through
/// `RunError::Cancelled`, same as every other stage.
///
/// A rebuilt master's file changes size/mtime on disk, which the stage-1
/// hash (`calibration_hash_for`) reads straight back via `resolved_master_paths`
/// — a light whose calibration plan resolves to a master this stage just
/// rebuilt therefore computes a DIFFERENT stage-1 hash than any previous
/// run recorded, so its `calibrated` artifact (if any existed) reads as
/// stale automatically and stage 1 regenerates it. By design: a rebuilt
/// master's pixels really did change, so anything calibrated against the
/// old ones must not be reused as if nothing happened.
fn stage_masters(rc: &mut RunContext) -> Result<(), RunError> {
    let items = rc.masters_to_build.clone();
    if items.is_empty() {
        return Ok(());
    }

    let stage_start = Instant::now();
    let total = items.len();

    // Fix round 1, item 3: the opener — forced (`current == 0` bypasses
    // `RunContext::progress`'s throttle unconditionally), so this stage
    // visibly starts even if the run's own `last_emit` timestamp (set at
    // `RunContext` construction) is still inside the 300ms throttle window
    // by the time `stage_masters` gets to run.
    rc.progress(Stage::Masters, None, 0, total, 0, 0, None, None);

    for (i, item) in items.iter().enumerate() {
        rc.check_cancel()?;
        // Emitted BEFORE the build starts, naming the master about to be
        // built/rebuilt, at the COUNT OF ITEMS ALREADY DONE (0-based `i`,
        // never `total`) — a multi-minute integration otherwise leaves the
        // Stacking tab showing the previous item's label for the whole
        // duration (research §8's "a multi-minute operation that logs
        // nothing is indistinguishable from a hung one", same reasoning
        // `api::masters::log_build_started` was added for). Fix round 1,
        // item 3: using `i` rather than `i + 1` here is what keeps the row
        // from ever reading `N/N` while the LAST master is still
        // integrating — that misleading tick only fires once, explicitly,
        // after the loop below.
        rc.progress(
            Stage::Masters,
            None,
            i,
            total,
            0,
            0,
            None,
            Some(item.label.clone()),
        );

        let item_start = Instant::now();

        // `MasterWork::Build`'s target is `BuildTarget::New` directly — the
        // item's own `set_id` IS the raw source set. `MasterWork::Rebuild`'s
        // `set_id` is the MASTER set id (see `PlanMaster`'s doc comment), so
        // it must be resolved into a `BuildTarget::Rebuild` (+ its own
        // source set id) first — the SAME resolution the manual "Rebuild"
        // action uses (`crate::api::masters::rebuild_master`), via the
        // shared helper both now call.
        let (source_set_id, target) = match item.kind {
            MasterWork::Build => (item.set_id, crate::api::masters::BuildTarget::New),
            MasterWork::Rebuild => {
                let conn = db(&rc.ctx)?.conn();
                crate::api::masters::resolve_rebuild_target(&conn, item.set_id).map_err(|e| {
                    RunError::Other(format!("master build failed for set {}: {e}", item.set_id))
                })?
            }
        };

        let build_result = crate::api::masters::build_master_inline(
            &rc.ctx,
            rc.emitter.as_ref(),
            &rc.app_version,
            source_set_id,
            target,
            &rc.cancel,
        );

        let master_set_id = match build_result {
            Ok((id, warning)) => {
                if let Some(w) = warning {
                    rc.warnings.push(w);
                }
                id
            }
            Err(crate::api::masters::BuildStepError::Cancelled) => return Err(RunError::Cancelled),
            Err(crate::api::masters::BuildStepError::Other(msg)) => {
                return Err(RunError::Other(format!(
                    "master build failed for set {}: {msg}",
                    item.set_id
                )));
            }
        };

        // Fix round 1, item 2: a missing file row after a build that itself
        // reported success would be a silent data-integrity gap — never
        // swallow it, even though the run itself does not fail over it (the
        // pixels ARE on disk and registered; only this summary path is
        // empty).
        let path = {
            let conn = db(&rc.ctx)?.conn();
            match crate::api::masters::master_file_path(&conn, master_set_id)? {
                Some((_, p)) => p,
                None => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        set_id = master_set_id,
                        "master file row missing after build"
                    );
                    String::new()
                }
            }
        };

        let duration_ms = item_start.elapsed().as_millis() as u64;
        tracing::info!(
            run_id = rc.run_id,
            set_id = item.set_id,
            kind = item.kind.as_str(),
            duration_ms,
            "master built"
        );

        rc.summary.masters_built.push(MasterBuilt {
            set_id: item.set_id,
            kind: item.kind,
            master_set_id,
            path,
            duration_ms,
        });
    }

    // Fix round 1, item 3: the closer — forced (`current == total`), emitted
    // only AFTER every item's build has actually finished, so the stage's
    // true completion is never lost even if an intermediate per-item tick
    // above was swallowed by the throttle.
    rc.progress(Stage::Masters, None, total, total, 0, 0, None, None);

    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Masters,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

/// Calibrate-only byte footprint for progress's `bytes_total` (mirrors
/// [`crate::stacking::paths::estimate_bytes`]'s calibrated-frame term, but
/// scoped to just this stage rather than the whole run's estimate). Sums
/// per FRAME rather than per group (owner decision 2026-09-10: a group's
/// members can carry different native geometry now that camera/geometry
/// are not grouping keys, so there is no single group-wide `W x H` any
/// more).
fn calibrate_bytes_total(groups: &[IntegrationGroup], excluded: &HashSet<i64>) -> u64 {
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

/// One frame's stage-1 outcome (private to [`stage_calibrate`]).
enum CalibrateOutcome {
    /// An existing `calibrated` artifact was fresh; nothing was written.
    /// `bytes` is the reused file's own recorded size (fix round 1, item 5:
    /// a fully-reused stage must report `N/N` progress bytes, not `0/N`).
    Reused { bytes: u64 },
    /// A fresh `calibrated` file was written; `bytes` is its size.
    Generated { bytes: u64 },
    /// Calibration failed for this frame; `reason` is
    /// [`RunContext::runtime_exclusions`]'s text.
    Excluded { reason: String },
}

/// The collision-safe calibrated-file STEM for one frame of a group (fix
/// round 1, Critical): the plain, extension-free source stem
/// (`Path::file_stem` of `frame.filename`) when it is unique within the
/// group; `<stem>_f<frame_id>` for EVERY frame that shares that stem with
/// another member of the group (all of them, not just the later ones) —
/// deterministic and order-independent, since it depends only on group
/// membership, never on iteration order.
///
/// Why this exists: `layout.calibrated_dir(key).join(spec.output_filename(&frame.filename))`
/// used to collide whenever two frames of one group shared a source
/// basename (a capture program restarting its file counter on a different
/// night is routine) — the second write silently clobbered the first, both
/// artifact rows pointed at one file of identical size, and `is_fresh` then
/// kept the FIRST frame's now-wrong row "fresh" forever, so a later stage
/// used the second frame's pixels twice and lost the first one silently.
///
/// Callers append the generator's own `_d` debayer marker via
/// `GenerationSpec::output_filename` — never insert it here, so the marker
/// always stays LAST in the filename (`c_<stem>_f<id>_d.fits`, never
/// `c_<stem>_d_f<id>.fits`). Task 7's registered-frame writer
/// ([`write_registered_artifact`]/[`registered_file_name`]) calls this SAME
/// function for its own `r_<stem>[_d].fits` naming, so the two stay
/// consistent by construction rather than by one parsing the other's
/// output.
pub(crate) fn calibrated_file_stem(group: &IntegrationGroup, frame: &GroupFrame) -> String {
    fn stem_of(f: &GroupFrame) -> &str {
        Path::new(&f.filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&f.filename)
    }
    let this_stem = stem_of(frame);
    let collisions = group
        .frames
        .iter()
        .filter(|f| stem_of(f) == this_stem)
        .count();
    if collisions > 1 {
        format!("{this_stem}_f{}", frame.frame_id)
    } else {
        this_stem.to_string()
    }
}

/// Stage 1 (calibrate) for one frame (spec §9.3, decision 4).
///
/// Three DB connections, each opened, used and dropped in its own scope: one
/// for the stage-1 hash (through `rc.memo`, which internally resolves the
/// frame's calibration plan the same way the second, generation-time
/// resolution below does — see [`HashMemo::calibration_hash_checked`]'s doc
/// for why that is not wasted work), one for the freshness lookup, one more
/// (opened AFTER the pixel work) to record the fresh artifact row. Never
/// held across [`resolve_generation_cached`]'s master-flat read or
/// [`execute_generation`]'s pixel work, matching this codebase's
/// never-hold-a-connection-across-slow-I/O convention (decision 4: "open a
/// conn, …, DROP the conn, then execute_generation").
fn calibrate_one_frame(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    group: &IntegrationGroup,
    frame: &GroupFrame,
    scratch: &Path,
) -> Result<CalibrateOutcome, RunError> {
    let group_key: &str = group.key.as_str();
    let hash = {
        let conn = db(&rc.ctx)?.conn();
        match rc.memo.calibration_hash_checked(&conn, cfg, frame) {
            Ok(hash) => hash,
            Err(e) => {
                return Ok(CalibrateOutcome::Excluded {
                    reason: format!("calibration failed: {e}"),
                })
            }
        }
    };

    if rc.rerun_from != Some(Stage::Calibrate) {
        let existing = {
            let conn = db(&rc.ctx)?.conn();
            crate::db::stacking::find_artifact(
                &conn,
                rc.set_id,
                group_key,
                "calibrated",
                Some(frame.frame_id),
            )?
        };
        if let Some(row) = &existing {
            if is_fresh(row, &hash) {
                return Ok(CalibrateOutcome::Reused {
                    bytes: row.size.unwrap_or(0) as u64,
                });
            }
        }
    }

    let spec = {
        let conn = db(&rc.ctx)?.conn();
        resolve_generation_cached(
            &conn,
            frame.frame_id,
            &cfg.calibration,
            scratch,
            rc.memo.divisors_mut(),
        )
    };
    let spec = match spec {
        Ok(s) => s,
        Err(e) => {
            return Ok(CalibrateOutcome::Excluded {
                reason: format!("calibration failed: {e:#}"),
            })
        }
    };

    let stem = calibrated_file_stem(group, frame);
    let out = rc
        .layout
        .calibrated_dir(group_key)
        .join(spec.output_filename(&format!("{stem}.fits")));

    let generated = execute_generation(
        &spec,
        &out,
        scratch,
        &cfg.calibration,
        &mut rc.hot_maps,
        &rc.cancel,
    );
    let generated = match generated {
        Ok(g) => g,
        Err(e) => {
            if matches!(
                e.downcast_ref::<IntegrationError>(),
                Some(IntegrationError::Cancelled)
            ) {
                return Err(RunError::Cancelled);
            }
            return Ok(CalibrateOutcome::Excluded {
                reason: format!("calibration failed: {e:#}"),
            });
        }
    };
    rc.warnings.extend(generated.warnings);

    let (size, modified_at) = file_identity(&out).map_err(|e| RunError::Other(format!("{e:#}")))?;

    {
        let conn = db(&rc.ctx)?.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                frames_set_id: rc.set_id,
                frame_id: Some(frame.frame_id),
                group_key,
                kind: "calibrated",
                path: out.to_str(),
                config_hash: &hash,
                size: Some(size),
                modified_at: Some(&modified_at),
                payload_json: None,
            },
        )?;
    }

    Ok(CalibrateOutcome::Generated { bytes: size as u64 })
}

/// Stage 1: calibrate every non-manually-excluded LIGHT frame, sequentially
/// (ruling 4 — exactly as the calibrated-lights export does today), reusing
/// a fresh `calibrated` artifact when one exists. A per-frame failure
/// excludes that frame (`runtime_exclusions` + a `warn!`) rather than
/// failing the run; only `IntegrationError::Cancelled` propagates as
/// [`RunError::Cancelled`].
fn stage_calibrate(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
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
        set_id = rc.set_id,
        count = total,
        groups = groups.len(),
        config_hash = %rc.hash,
        "stacking run started"
    );

    let scratch = rc.layout.root.join("tmp");
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

    for group in &groups {
        rc.check_cancel()?;
        std::fs::create_dir_all(rc.layout.calibrated_dir(&group.key))
            .map_err(|e| RunError::Other(format!("failed to create calibrated dir: {e}")))?;

        for frame in &group.frames {
            if excluded_set.contains(&frame.frame_id) {
                continue;
            }
            rc.check_cancel()?;

            match calibrate_one_frame(rc, &cfg, group, frame, &scratch)? {
                CalibrateOutcome::Reused { bytes } => {
                    bytes_done += bytes;
                    rc.cached_calibrated.insert(frame.frame_id, true);
                }
                CalibrateOutcome::Generated { bytes } => {
                    bytes_done += bytes;
                    rc.cached_calibrated.insert(frame.frame_id, false);
                }
                CalibrateOutcome::Excluded { reason } => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = frame.frame_id,
                        error = %reason,
                        "calibration failed; frame excluded"
                    );
                    rc.runtime_exclusions.push((frame.frame_id, reason));
                }
            }

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
    }

    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Calibrate,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

// ── Stages 3-5 (Task 7): measure & select, reference, register ─────────────

/// One frame's stage 3-5 lifecycle, tracked per group
/// (`RunContext::measured`, keyed by `group_key`) across those three stages
/// — Task 8's integrate stage reads this directly: every INCLUDED frame's
/// calibrated path, measurement and (after stage 5) `PixelMap` all live
/// here.
///
/// Deviation from the brief's literal shape (`calibrated: PathBuf`,
/// `measurement: FrameMeasurement`, both non-`Option`): a manually- or
/// stage-1-excluded frame never reaches stage 3 at all (calibrate never
/// wrote it a file — Task 6), and a frame whose own measurement failed has
/// none either, so both are `Option` here. One entry exists for EVERY frame
/// of every group, included or not, so `write_frame_rows` (end of stage 5)
/// has one place to read every frame's whole outcome from — including the
/// ones excluded before stage 3 (or stage 3 itself) ever ran.
pub(crate) struct MeasuredFrame {
    frame: GroupFrame,
    calibrated: Option<PathBuf>,
    #[allow(dead_code)] // read by Task 8 (admission sizing for later stages)
    planes: usize,
    measurement: Option<FrameMeasurement>,
    weight: Option<FrameWeight>,
    included: bool,
    reason: Option<String>,
    registration: Option<RegisteredFrameOutcome>,
    /// Whether stage 1 REUSED an existing `calibrated` artifact for this
    /// frame rather than writing a fresh one (Task 8's own addition, from
    /// `RunContext::cached_calibrated` — `false` for a frame excluded before
    /// reaching that lookup, which is never wrong for those since they never
    /// had anything to reuse). Feeds `SummaryFrame::cached_calibrated`.
    cached_calibrated: bool,
    /// Whether stage 3 reused an existing `metrics` artifact for this frame
    /// rather than measuring it fresh (Task 8's own addition — set inline,
    /// in the same branch that already decides "reused", right below).
    /// Feeds `SummaryFrame::cached_metrics`.
    cached_metrics: bool,
    /// Stage 6 (local normalization, M2): this frame's own relative scale
    /// (mean across channels), `None` until `run_group_normalization` sets
    /// it (or forever, when LN never ran for this group). Feeds
    /// `SummaryFrame::ln_scale`.
    ln_scale: Option<f64>,
    /// Whether stage 6 REUSED an existing `ln` artifact for this frame
    /// rather than normalizing it fresh. Feeds `SummaryFrame::cached_ln`.
    cached_ln: bool,
}

/// One frame's stage 5 outcome. `cached: true` means an existing
/// `registration_results` row was reused verbatim (ruling 10) — no
/// `register_frame` call, no `upsert_registration` write; `record` is the
/// EXISTING row in that case.
pub(crate) enum RegisteredFrameOutcome {
    Aligned {
        #[allow(dead_code)] // read by Task 8 (per-frame resampling)
        map: PixelMap,
        record: RegistrationRecord,
        #[allow(dead_code)] // read by Task 8/9 (provenance)
        cached: bool,
    },
    /// The failure text — kept alongside (not read back today: the SAME
    /// text is already stored on the frame's `MeasuredFrame::reason`, which
    /// is what `write_frame_rows` and the caller's own `warn!` read) so a
    /// later consumer of `RegisteredFrameOutcome` on its own (Task 8/9,
    /// without also holding the `MeasuredFrame`) still has it.
    Failed(#[allow(dead_code)] String),
}

/// `frame_id`'s [`GroupFrame`], searched across every group of this run's
/// plan — a small local duplicate of `plan.rs`'s own private
/// `find_group_frame` (not worth making that one `pub(crate)` for this one
/// lookup).
fn find_frame_in_groups(groups: &[IntegrationGroup], frame_id: i64) -> Option<GroupFrame> {
    groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .find(|f| f.frame_id == frame_id)
        .cloned()
}

/// Whether stage `stage` must skip its own freshness check and redo its
/// work unconditionally, given the run's `rerun_from` choice: "re-run from
/// X" means every stage from X onward (in pipeline order) is forced fresh,
/// stages before it may still reuse. [`Stage`]'s declaration order IS
/// pipeline order (spec §10.2), so the fieldless enum's own discriminant is
/// the rank. Generalizes decision 4's literal `rc.rerun_from != Some(Stage::Calibrate)`
/// check (Task 6's calibrate stage, left as-is — the two are equivalent for
/// `stage == Calibrate`) to stages 3 and 5.
fn stage_forces_fresh(rerun_from: Option<Stage>, stage: Stage) -> bool {
    rerun_from.is_some_and(|from| stage as u8 >= from as u8)
}

/// Memory-budgeted worker count for a fan-out stage (decision 3):
/// `clamp(budget / working_set, 1, cores)`. `budget = total_ram_bytes() / 4`;
/// when the total is unknown, the budget is treated as exhausted (admission
/// 1 — the conservative, single-frame-at-a-time fallback) rather than
/// guessed. `cores` falls back to 1 when `available_parallelism` fails.
fn admission(working_set_bytes: u64) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let working_set = working_set_bytes.max(1);
    let n = match total_ram_bytes() {
        Some(total) => (total / 4) / working_set,
        None => 1,
    };
    n.clamp(1, cores as u64) as usize
}

/// Fan `items` out across `min(admission, items.len()).max(1)` worker
/// threads pulling from one shared FIFO queue (`std::thread::scope`) —
/// never more workers than there is work, and always at least one so a
/// non-empty `items` makes progress even when `admission` itself is 0.
/// `cancel` checked before each item is pulled — never mid-item, since
/// only the item's own function (a caller that needs per-item
/// cancellation captures its OWN `&AtomicBool` into `f`) can decide that.
/// Results land at their item's ORIGINAL index in `items`; an index whose
/// item was never started because `cancel` fired first is `None` —
/// callers check `cancel` once, right after this returns, rather than
/// inspecting every entry for that case.
fn fan_out<T, R, F>(
    items: Vec<T>,
    admission: usize,
    cancel: &AtomicBool,
    f: F,
) -> Vec<Option<Result<R, String>>>
where
    T: Send,
    R: Send,
    F: Fn(T) -> Result<R, String> + Sync,
{
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let queue: Mutex<VecDeque<(usize, T)>> = Mutex::new(items.into_iter().enumerate().collect());
    let results: Mutex<Vec<Option<Result<R, String>>>> = Mutex::new((0..n).map(|_| None).collect());

    std::thread::scope(|scope| {
        for _ in 0..admission.min(n).max(1) {
            scope.spawn(|| loop {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                let next = queue.lock().unwrap().pop_front();
                let Some((idx, item)) = next else {
                    return;
                };
                let out = f(item);
                results.lock().unwrap()[idx] = Some(out);
            });
        }
    });

    results.into_inner().unwrap()
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
fn stage_measure(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
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

    for group in &groups {
        rc.check_cancel()?;

        let mut entries: Vec<MeasuredFrame> = Vec::with_capacity(group.frames.len());
        let mut to_measure: Vec<(usize, GroupFrame, PathBuf)> = Vec::new();
        let mut max_planes = 1usize;
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
                });
                continue;
            }

            let calibrated_path: Option<PathBuf> = {
                let conn = db(&rc.ctx)?.conn();
                crate::db::stacking::find_artifact(
                    &conn,
                    rc.set_id,
                    &group.key,
                    "calibrated",
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
                });
                continue;
            };

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
                    });
                    continue;
                }
            };
            max_planes = max_planes.max(planes);

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
            });
            to_measure.push((entries.len() - 1, frame.clone(), calibrated_path));
        }

        // Freshness check (skipped entirely under a Measure-or-earlier
        // `rerun_from`): a `metrics` artifact whose hash matches this
        // frame's CURRENT stage-1 hash is reused straight from
        // `payload_json`, never re-measured.
        let mut needing_measure: Vec<(usize, GroupFrame, PathBuf)> = Vec::new();
        for (idx, frame, path) in to_measure {
            let calib_hash = {
                let conn = db(&rc.ctx)?.conn();
                rc.memo.calibration_hash_checked(&conn, &cfg, &frame)?
            };
            let expected_hash = measurement_hash_for(&cfg, &calib_hash);

            let mut reused = false;
            if !force_fresh {
                let existing = {
                    let conn = db(&rc.ctx)?.conn();
                    crate::db::stacking::find_artifact(
                        &conn,
                        rc.set_id,
                        &group.key,
                        "metrics",
                        Some(frame.frame_id),
                    )?
                };
                if let Some(row) = existing {
                    if row.config_hash == expected_hash {
                        if let Some(payload) = &row.payload_json {
                            match serde_json::from_str::<FrameMeasurement>(payload) {
                                Ok(m) => {
                                    entries[idx].measurement = Some(m);
                                    entries[idx].cached_metrics = true;
                                    reused = true;
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

        if !needing_measure.is_empty() {
            let admission_n = admission(8 * max_planes as u64 * group_max_w * group_max_h * 4);
            let meta: Vec<(usize, GroupFrame)> = needing_measure
                .iter()
                .map(|(idx, f, _)| (*idx, f.clone()))
                .collect();
            let items: Vec<PathBuf> = needing_measure.into_iter().map(|(_, _, p)| p).collect();

            let cancel_ref: &AtomicBool = &rc.cancel;
            let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
            let results = fan_out(items, admission_n, cancel_ref, move |path| {
                measure_frame(&path, &opts, Some(pool_ref), cancel_ref)
                    .map_err(|e| format!("measurement failed: {e}"))
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
                    Some(Ok(m)) => {
                        let calib_hash = {
                            let conn = db(&rc.ctx)?.conn();
                            rc.memo.calibration_hash_checked(&conn, &cfg, frame)?
                        };
                        let expected_hash = measurement_hash_for(&cfg, &calib_hash);
                        let payload = serde_json::to_string(&m).map_err(|e| {
                            RunError::Other(format!("failed to serialize measurement: {e}"))
                        })?;
                        {
                            let conn = db(&rc.ctx)?.conn();
                            upsert_artifact(
                                &conn,
                                &NewArtifact {
                                    frames_set_id: rc.set_id,
                                    frame_id: Some(frame.frame_id),
                                    group_key: &group.key,
                                    kind: "metrics",
                                    path: None,
                                    config_hash: &expected_hash,
                                    size: None,
                                    modified_at: None,
                                    payload_json: Some(&payload),
                                },
                            )?;
                        }
                        entries[idx].measurement = Some(m);
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
                let sel = select_frames(&inputs, &w, &manual, &cfg.selection);
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
                        ..Default::default()
                    },
                )?;
            } else {
                any_group_viable = true;
                update_group(
                    &conn,
                    group_id,
                    &GroupUpdate {
                        included_count: Some(included_count as i64),
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
        rc.progress(
            Stage::Measure,
            Some(group.key.clone()),
            current,
            total,
            0,
            0,
            None,
            None,
        );
    }

    if !any_group_viable {
        return Err(RunError::Other(
            "no group has 3 included frames".to_string(),
        ));
    }

    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Measure,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

/// Stage 4 (reference, spec §4.4): pick the run's ONE reference frame.
/// `Manual` reads the Analysis page's stored choice (the plan already
/// verified it exists); `Auto` is the best-weighted frame (ties by star
/// count, via [`best_by_weight`]) in the largest included group (ties by
/// total exposure). Stores the choice (`set_run_reference`,
/// `rc.summary.reference`, `rc.reference_frame_id`/`reference_calibrated`)
/// and emits a single `Reference 1/1` progress event.
fn stage_reference(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
    let cfg = rc.config.clone();

    let (reference_frame_id, filename, weight_val, calibrated) = match cfg.reference.mode {
        ReferenceMode::Manual => {
            let row = {
                let conn = db(&rc.ctx)?.conn();
                get_frame_set_reference(&conn, rc.set_id)?
            };
            let row =
                row.ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;
            let frame_id = row.reference_frame_id;

            let location = rc.measured.iter().find_map(|(key, v)| {
                v.iter()
                    .position(|e| e.frame.frame_id == frame_id)
                    .map(|idx| (key.clone(), idx))
            });
            let (group_key, idx) = location.ok_or_else(|| {
                RunError::Other("reference frame is not part of any group".to_string())
            })?;

            // Fix round 1, item 1: a manual reference excluded in stage 1
            // (calibration failed) or by the user's own manual exclusion
            // list — both leave `calibrated: None` — is a hard failure:
            // the plan gate cannot see a stage-1 failure, so the run says
            // so loudly instead of silently substituting a different
            // reference. Any OTHER exclusion (measurement failure, or a
            // stage-3 selection filter — weight floor, maxFwhm, …) still
            // has a calibrated file, and the manual reference is forced
            // included regardless, per the same ruling — otherwise it
            // becomes the run's reference (`rc.reference_frame_id` below)
            // while stage 5's own `included`-filtered snapshot skips it
            // entirely, leaving it with no identity row, a `skipped` frame
            // row, and every later run stale forever
            // (`plan.rs::compute_register_stale`).
            let calibrated_missing = rc.measured[&group_key][idx].calibrated.is_none();
            if calibrated_missing {
                let reason = rc.measured[&group_key][idx]
                    .reason
                    .clone()
                    .unwrap_or_else(|| "excluded".to_string());
                return Err(RunError::Other(format!(
                    "the manual reference frame is excluded: {reason}"
                )));
            }

            let already_included = rc.measured[&group_key][idx].included;
            if !already_included {
                let reason = rc.measured[&group_key][idx]
                    .reason
                    .clone()
                    .unwrap_or_else(|| "excluded".to_string());
                let entries_mut = rc.measured.get_mut(&group_key).expect("checked above");
                entries_mut[idx].included = true;
                entries_mut[idx].reason = None;
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id,
                    note = %reason,
                    "manual reference kept despite selection"
                );
                rc.warnings.push(format!(
                    "manual reference frame {frame_id} kept despite selection: {reason}"
                ));
            }

            let entry = &rc.measured[&group_key][idx];
            let calibrated = entry
                .calibrated
                .clone()
                .expect("checked above: calibrated is Some");
            (
                frame_id,
                entry.frame.filename.clone(),
                entry.weight.as_ref().map(|w| w.normalized_mean),
                calibrated,
            )
        }
        ReferenceMode::Auto => {
            let mut best_group: Option<&IntegrationGroup> = None;
            let mut best_included = 0usize;
            for g in &rc.plan_groups {
                let included = rc
                    .measured
                    .get(&g.key)
                    .map(|v| v.iter().filter(|e| e.included).count())
                    .unwrap_or(0);
                if included == 0 {
                    continue;
                }
                let take = match best_group {
                    None => true,
                    Some(b) => {
                        included > best_included
                            || (included == best_included
                                && g.total_exposure_s > b.total_exposure_s)
                    }
                };
                if take {
                    best_group = Some(g);
                    best_included = included;
                }
            }
            let group = best_group
                .ok_or_else(|| RunError::Other("no group has any included frame".to_string()))?;
            let entries = rc
                .measured
                .get(&group.key)
                .expect("a group with an included frame has measured entries");
            let weights: Vec<FrameWeight> = entries
                .iter()
                .map(|e| {
                    e.weight.clone().unwrap_or(FrameWeight {
                        channels: Vec::new(),
                        normalized: Vec::new(),
                        mean: 0.0,
                        normalized_mean: 0.0,
                        missing: None,
                    })
                })
                .collect();
            let included: Vec<bool> = entries.iter().map(|e| e.included).collect();
            let star_counts: Vec<usize> = entries
                .iter()
                .map(|e| e.measurement.as_ref().map(|m| m.min_stars()).unwrap_or(0))
                .collect();
            let idx = best_by_weight(&weights, &included, &star_counts).ok_or_else(|| {
                RunError::Other("no included frame in the largest group".to_string())
            })?;
            let entry = &entries[idx];
            let calibrated = entry.calibrated.clone().ok_or_else(|| {
                RunError::Other("chosen reference frame has no calibrated file".to_string())
            })?;
            (
                entry.frame.frame_id,
                entry.frame.filename.clone(),
                entry.weight.as_ref().map(|w| w.normalized_mean),
                calibrated,
            )
        }
    };

    {
        let conn = db(&rc.ctx)?.conn();
        set_run_reference(
            &conn,
            rc.run_id,
            reference_frame_id,
            reference_mode_wire(cfg.reference.mode),
        )?;
    }

    rc.summary.reference = SummaryReference {
        frame_id: Some(reference_frame_id),
        filename: Some(filename),
        mode: cfg.reference.mode,
        weight: weight_val,
    };
    rc.reference_frame_id = Some(reference_frame_id);
    rc.reference_calibrated = Some(calibrated);

    rc.progress(
        Stage::Reference,
        None,
        1,
        1,
        0,
        0,
        Some(reference_frame_id),
        None,
    );

    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Reference,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

/// Stage 5 (register, spec §3, ruling 10): register every included frame of
/// every VIABLE group (≥ 3 included, per stage 3) onto the ONE global
/// reference (its stars computed once), reusing an existing
/// `registration_results` row when it is still fresh
/// ([`registration_row_is_fresh`], the SAME predicate the plan's own
/// `stale_stages` uses — ruling 10), fanning the rest out. Writes every
/// group's every frame's `stacking_run_frames` row at the end
/// ([`write_frame_rows`]), included or not.
fn stage_register(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
    let cfg = rc.config.clone();

    let reference_frame_id = rc
        .reference_frame_id
        .ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;
    let reference_calibrated = rc
        .reference_calibrated
        .clone()
        .ok_or_else(|| RunError::Other("reference frame has no calibrated file".to_string()))?;

    let ref_stars = {
        let pool_ref = &rc.ctx.image_pool;
        reference_stars(&reference_calibrated, &cfg.registration, Some(pool_ref))
            .map_err(|e| RunError::Other(format!("reference star detection failed: {e}")))?
    };
    rc.reference_width = ref_stars.width;
    rc.reference_height = ref_stars.height;

    let reference_group_frame = find_frame_in_groups(&rc.plan_groups, reference_frame_id)
        .ok_or_else(|| RunError::Other("reference frame not found in any group".to_string()))?;
    let reference_hash = {
        let conn = db(&rc.ctx)?.conn();
        rc.memo
            .calibration_hash_checked(&conn, &cfg, &reference_group_frame)?
    };

    let by_frame: HashMap<i64, RegistrationRecord> = {
        let conn = db(&rc.ctx)?.conn();
        get_registration_for_frame_set(&conn, rc.set_id)?
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect()
    };

    let groups = rc.plan_groups.clone();
    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Register);

    let mut viable_total = 0usize;
    for g in &groups {
        let included = rc
            .measured
            .get(&g.key)
            .map(|v| v.iter().filter(|e| e.included).count())
            .unwrap_or(0);
        if included >= 3 {
            viable_total += included;
        }
    }

    let mut current = 0usize;
    rc.progress(
        Stage::Register,
        None,
        current,
        viable_total,
        0,
        0,
        None,
        None,
    );

    for group in &groups {
        rc.check_cancel()?;

        let included_count = rc
            .measured
            .get(&group.key)
            .map(|v| v.iter().filter(|e| e.included).count())
            .unwrap_or(0);
        if included_count < 3 {
            continue;
        }

        let snapshot: Vec<(usize, GroupFrame, PathBuf, bool)> = rc
            .measured
            .get(&group.key)
            .map(|v| {
                v.iter()
                    .enumerate()
                    .filter(|(_, e)| e.included)
                    .filter_map(|(i, e)| {
                        e.calibrated.clone().map(|p| {
                            (
                                i,
                                e.frame.clone(),
                                p,
                                e.frame.frame_id == reference_frame_id,
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut to_register: Vec<(usize, GroupFrame, PathBuf, bool, String)> = Vec::new();
        for (idx, frame, path, is_reference) in snapshot {
            let frame_hash = {
                let conn = db(&rc.ctx)?.conn();
                rc.memo.calibration_hash_checked(&conn, &cfg, &frame)?
            };
            let expected_hash =
                registration_hash_for(&cfg, reference_frame_id, &reference_hash, &frame_hash);

            let mut reused: Option<RegisteredFrameOutcome> = None;
            if !force_fresh {
                if let Some(row) = by_frame.get(&frame.frame_id) {
                    if registration_row_is_fresh(row, reference_frame_id, &expected_hash) {
                        match PixelMap::from_json(row.transform_json.as_deref().unwrap_or_default())
                        {
                            Ok(map) => {
                                reused = Some(RegisteredFrameOutcome::Aligned {
                                    map,
                                    record: row.clone(),
                                    cached: true,
                                });
                            }
                            Err(e) => {
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    error = %e,
                                    "stored registration transform failed to parse; re-registering"
                                );
                            }
                        }
                    }
                }
            }

            if let Some(outcome) = reused {
                if let Some(entries) = rc.measured.get_mut(&group.key) {
                    entries[idx].registration = Some(outcome);
                }
                current += 1;
                rc.progress(
                    Stage::Register,
                    Some(group.key.clone()),
                    current,
                    viable_total,
                    0,
                    0,
                    Some(frame.frame_id),
                    None,
                );
            } else {
                to_register.push((idx, frame, path, is_reference, expected_hash));
            }
        }

        if to_register.is_empty() {
            continue;
        }

        // Registration warps every frame onto the ONE run-wide reference
        // geometry (already resolved above, at the top of this stage) —
        // the OUTPUT buffer size, and a more accurate admission bound than
        // any per-frame native geometry would be.
        let admission_n = admission(4 * rc.reference_width as u64 * rc.reference_height as u64 * 4);
        let meta: Vec<(usize, GroupFrame, String)> = to_register
            .iter()
            .map(|(idx, frame, _, _, hash)| (*idx, frame.clone(), hash.clone()))
            .collect();
        let items: Vec<(PathBuf, bool)> = to_register
            .into_iter()
            .map(|(_, _, path, is_reference, _)| (path, is_reference))
            .collect();

        let cancel_ref: &AtomicBool = &rc.cancel;
        let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
        let reg_cfg = &cfg.registration;
        let ref_stars_ref = &ref_stars;

        let results = fan_out(
            items,
            admission_n,
            cancel_ref,
            move |(path, is_reference)| {
                if is_reference {
                    Ok(identity_registration(ref_stars_ref))
                } else {
                    register_frame(ref_stars_ref, &path, reg_cfg, Some(pool_ref), cancel_ref)
                        .map_err(|e| format!("registration failed: {e}"))
                }
            },
        );

        rc.check_cancel()?;

        for (pos, res) in results.into_iter().enumerate() {
            let (idx, frame, hash) = &meta[pos];
            let idx = *idx;
            match res {
                None => return Err(RunError::Cancelled),
                Some(Err(msg)) => {
                    if cfg.selection.exclude_on_registration_failure {
                        rc.runtime_exclusions.push((frame.frame_id, msg.clone()));
                        if let Some(entries) = rc.measured.get_mut(&group.key) {
                            entries[idx].included = false;
                            entries[idx].reason = Some(msg.clone());
                            entries[idx].registration =
                                Some(RegisteredFrameOutcome::Failed(msg.clone()));
                        }
                        tracing::warn!(
                            run_id = rc.run_id,
                            frame_id = frame.frame_id,
                            reason = %msg,
                            "frame excluded"
                        );
                    } else {
                        return Err(RunError::Other(msg.clone()));
                    }
                }
                Some(Ok(reg)) => match &reg.outcome {
                    Ok(alignment) => {
                        tracing::debug!(
                            run_id = rc.run_id,
                            frame_id = frame.frame_id,
                            inliers = alignment.inliers,
                            rms_px = alignment.rms_px,
                            "frame registered"
                        );
                        for note in &alignment.warnings {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = frame.frame_id,
                                note = %note,
                                "registration warning"
                            );
                            rc.warnings
                                .push(format!("frame {}: {note}", frame.frame_id));
                        }
                        let now =
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let is_reference_row = frame.frame_id == reference_frame_id;
                        let rec = to_record(
                            rc.set_id,
                            frame.frame_id,
                            reference_frame_id,
                            is_reference_row,
                            &reg,
                            hash,
                            &now,
                        );
                        let map = alignment.map.clone();
                        {
                            let conn = db(&rc.ctx)?.conn();
                            upsert_registration(&conn, &rec)?;
                        }
                        if cfg.registration.write_registered_frames {
                            if let Err(e) =
                                write_registered_artifact(rc, group, frame, &map, &rec, &cfg)
                            {
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    error = ?e,
                                    "failed to write registered frame"
                                );
                            }
                        }
                        if let Some(entries) = rc.measured.get_mut(&group.key) {
                            entries[idx].registration = Some(RegisteredFrameOutcome::Aligned {
                                map,
                                record: rec,
                                cached: false,
                            });
                        }
                    }
                    Err(align_err) => {
                        let reason = format!("registration failed: {align_err}");
                        let now =
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let rec = to_record(
                            rc.set_id,
                            frame.frame_id,
                            reference_frame_id,
                            false,
                            &reg,
                            hash,
                            &now,
                        );
                        {
                            let conn = db(&rc.ctx)?.conn();
                            upsert_registration(&conn, &rec)?;
                        }
                        if cfg.selection.exclude_on_registration_failure {
                            rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
                            if let Some(entries) = rc.measured.get_mut(&group.key) {
                                entries[idx].included = false;
                                entries[idx].reason = Some(reason.clone());
                                entries[idx].registration =
                                    Some(RegisteredFrameOutcome::Failed(reason.clone()));
                            }
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = frame.frame_id,
                                reason = %reason,
                                "frame excluded"
                            );
                        } else {
                            return Err(RunError::Other(reason));
                        }
                    }
                },
            }
            current += 1;
            rc.progress(
                Stage::Register,
                Some(group.key.clone()),
                current,
                viable_total,
                0,
                0,
                Some(frame.frame_id),
                None,
            );
        }
    }

    write_frame_rows(rc)?;

    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Register,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

/// Optional debug/QC output (`cfg.registration.write_registered_frames`,
/// default off, not exercised by any required test): resample `calibrated`
/// into the reference geometry and write it under
/// `layout.registered_dir(group_key)`, plus a `registered` artifact row.
/// Runs on the run thread, sequentially, right after each frame's
/// registration DB write rather than inside the fan-out worker — simpler,
/// and this optional output is not on the pipeline's timing-critical path
/// (`write_registered_frames` defaults off).
fn write_registered_artifact(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    frame: &GroupFrame,
    map: &PixelMap,
    rec: &RegistrationRecord,
    cfg: &StackingConfig,
) -> anyhow::Result<()> {
    let group_key: &str = group.key.as_str();
    let (calibrated, planes) = rc
        .measured
        .get(group_key)
        .and_then(|v| v.iter().find(|e| e.frame.frame_id == frame.frame_id))
        .and_then(|e| e.calibrated.clone().map(|c| (c, e.planes)))
        .ok_or_else(|| {
            anyhow::anyhow!("no calibrated path recorded for frame {}", frame.frame_id)
        })?;

    let out_dir = rc.layout.registered_dir(group_key);
    std::fs::create_dir_all(&out_dir)?;
    // Fix round 1: route the registered name through the SAME
    // `calibrated_file_stem` stage 1 used, rather than recovering it by
    // trimming a leading "c_" off the calibrated file's own name — the two
    // stay consistent by construction, not by one parsing the other. The
    // debayer marker is likewise explicit (`planes == 3`, already known
    // from this same frame's own measured entry) rather than inferred by
    // checking whether the calibrated file's OWN name ends in "_d" — a
    // source light whose own filename happens to end in "_d" (e.g.
    // "vega_d.fits", mono) made that heuristic misfire.
    let stem = calibrated_file_stem(group, frame);
    let out = out_dir.join(registered_file_name(&stem, planes == 3));

    let reference_name = rc
        .reference_calibrated
        .as_deref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("reference")
        .to_string();
    let transform_json = map.to_json();
    let model = rec.model.clone().unwrap_or_default();

    let source = source_cards_from_file(&calibrated)?;
    let cards = build_registered_cards(
        &source,
        &RegisteredCards {
            reference_name: &reference_name,
            model: &model,
            transform_json: &transform_json,
            interpolation: cfg.registration.interpolation,
            clamping: cfg.registration.clamping_threshold,
            rms_px: rec.rms_residual_px,
            reference_roworder: None,
        },
    )?;
    write_registered_frame(
        &calibrated,
        map,
        rc.reference_width,
        rc.reference_height,
        cfg.registration.interpolation,
        cfg.registration.clamping_threshold,
        &cards,
        &out,
    )?;

    let (size, modified_at) = file_identity(&out)?;
    let conn = db(&rc.ctx)?.conn();
    upsert_artifact(
        &conn,
        &NewArtifact {
            frames_set_id: rc.set_id,
            frame_id: Some(frame.frame_id),
            group_key,
            kind: "registered",
            path: out.to_str(),
            config_hash: rec.config_hash.as_deref().unwrap_or_default(),
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: None,
        },
    )?;
    Ok(())
}

/// `r_<stem>[_d].fits` for the SAME collision-safe `stem`
/// [`calibrated_file_stem`] gave the calibrated file (fix round 1: the two
/// naming schemes must agree by construction). `debayered` is the caller's
/// own known fact (`MeasuredFrame::planes == 3`) — NOT inferred by
/// checking whether the calibrated file's own on-disk name ends in `_d`
/// (fix round 1's addendum: a SOURCE light whose own filename already ends
/// in `_d`, e.g. a mono `vega_d.fits`, made that heuristic misfire —
/// `c_vega_d.fits` "looks" debayered by name alone even though it is not).
/// Matches `register_probe.rs`'s own convention of keeping the marker on
/// the registered output too.
fn registered_file_name(stem: &str, debayered: bool) -> String {
    if debayered {
        format!("r_{stem}_d.fits")
    } else {
        format!("r_{stem}.fits")
    }
}

/// One `stacking_run_frames` row per frame of every group, included or not
/// — the frame's whole stage 3-5 outcome as `rc.measured` now holds it.
/// Called once, at the very end of stage 5 (decision 8).
fn write_frame_rows(rc: &mut RunContext) -> Result<(), RunError> {
    let conn = db(&rc.ctx)?.conn();
    for group in &rc.plan_groups {
        let Some(&group_id) = rc.group_ids.get(&group.key) else {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                "frame rows skipped: unknown group"
            );
            continue;
        };
        let Some(entries) = rc.measured.get(&group.key) else {
            continue;
        };
        for entry in entries {
            upsert_frame_row_for_entry(&conn, rc.run_id, group_id, entry)?;
        }
    }
    Ok(())
}

/// Builds and upserts one frame's `stacking_run_frames` row from its current
/// [`MeasuredFrame`] state — the row shape [`write_frame_rows`] writes for
/// EVERY frame at the end of stage 5, factored out (fix round 1, item 1) so
/// a LATER stage that flips a frame's `included`/`reason` in place — stage
/// 6's LN exclusion, via [`exclude_frame_and_persist`] — can re-persist the
/// SAME row instead of leaving stage 5's `included = 1` stale in the DB:
/// the frames table renders `stacking_run_frames.included` directly, so
/// without this a frame the LN pass dropped still showed as stacked even
/// though the master does not contain it. `rejected_fraction` is always
/// `None` here (matching stage 5's own original write) — the ONLY writer of
/// that column is `set_frame_rejected_fraction`, called later still, at
/// Output time, once integration has actually run; calling this function
/// before that point can never clobber a value that has not been written
/// yet in THIS run (a fresh run's row never had one).
fn upsert_frame_row_for_entry(
    conn: &rusqlite::Connection,
    run_id: i64,
    group_id: i64,
    entry: &MeasuredFrame,
) -> Result<(), RunError> {
    let weight = entry.weight.as_ref().map(|w| w.normalized_mean);
    let weight_channels_json = entry
        .weight
        .as_ref()
        .map(|w| serde_json::to_string(&w.normalized))
        .transpose()
        .map_err(|e| RunError::Other(format!("failed to serialize weight channels: {e}")))?;
    let metrics_json = entry
        .measurement
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| RunError::Other(format!("failed to serialize measurement: {e}")))?;

    let (reg_status, reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio, reg_flipped): (
        &str,
        Option<String>,
        Option<f64>,
        Option<i64>,
        Option<f64>,
        Option<bool>,
    ) = match &entry.registration {
        Some(RegisteredFrameOutcome::Aligned { record, .. }) => (
            record.status.as_str(),
            record.model.clone(),
            Some(record.rms_residual_px),
            Some(record.matched_stars),
            record.inlier_ratio,
            Some(record.flipped),
        ),
        Some(RegisteredFrameOutcome::Failed(_)) => ("failed", None, None, None, None, None),
        None => ("skipped", None, None, None, None, None),
    };

    upsert_frame_row(
        conn,
        &NewFrameRow {
            run_id,
            group_id,
            frame_id: entry.frame.frame_id,
            included: entry.included,
            exclusion_reason: entry.reason.as_deref(),
            weight,
            weight_channels_json: weight_channels_json.as_deref(),
            metrics_json: metrics_json.as_deref(),
            reg_status: Some(reg_status),
            reg_model: reg_model.as_deref(),
            reg_rms_px,
            reg_inliers,
            reg_inlier_ratio,
            reg_flipped,
            rejected_fraction: None,
        },
    )?;
    Ok(())
}

/// Flips one frame to excluded in `rc.measured` and immediately re-persists
/// its `stacking_run_frames` row (fix round 1, item 1) — see
/// [`upsert_frame_row_for_entry`]'s own doc for why this can't wait: stage 5
/// already wrote this frame's row as `included = 1` before stage 6 ever
/// runs. A no-op (never seen in practice) if the group/frame lookups miss.
fn exclude_frame_and_persist(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    frame_id: i64,
    reason: String,
) -> Result<(), RunError> {
    if let Some(entries) = rc.measured.get_mut(&group.key) {
        if let Some(e) = entries.iter_mut().find(|e| e.frame.frame_id == frame_id) {
            e.included = false;
            e.reason = Some(reason);
        }
    }
    let Some(&group_id) = rc.group_ids.get(&group.key) else {
        return Ok(());
    };
    let entry = rc
        .measured
        .get(&group.key)
        .and_then(|entries| entries.iter().find(|e| e.frame.frame_id == frame_id));
    if let Some(entry) = entry {
        let conn = db(&rc.ctx)?.conn();
        upsert_frame_row_for_entry(&conn, rc.run_id, group_id, entry)?;
    }
    Ok(())
}

// ── Stages 6/7/9 (Task 8): normalize, integrate, output ────────────────────

/// One process, one writer: `master_file_name`/`write_master_light`
/// (`resolve_collision`) are check-then-write, so two groups' writes — even
/// across two concurrent runs of the same process — must never race on the
/// same free name (carry-forward).
static OUTPUT_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// One included member of a group, snapshotted out of `rc.measured` before
/// the (possibly slow) integration call so nothing below needs a borrow of
/// `rc` to survive it — the fields [`crate::stacking::integrate::StackFrame`]
/// needs, plus `frame_id` (`StackFrame` itself has no identity field).
struct GroupMember {
    frame_id: i64,
    /// The catalog `files.filename` (fix round 1, item 3) — `ATH_STKF`
    /// names the SOURCE frame, not the calibrated artifact, and this needs
    /// no `c_`/`_d` stripping at all (unlike `calibrated`'s own name).
    filename: String,
    calibrated: PathBuf,
    map: PixelMap,
    measurement: FrameMeasurement,
    weight: FrameWeight,
    exposure_s: Option<f64>,
    date_obs: Option<String>,
    /// This frame's stage-5 `registration_results.config_hash` (spec §9.3) —
    /// `String::new()` for a member somehow reaching here without an
    /// `Aligned` outcome (never happens: the loop below only pushes members
    /// whose `registration` matched `Aligned`, see [`process_group_output`]).
    /// Stage 6's own [`normalization_hash_for`] keys a frame's `ln` artifact
    /// on this, so a re-registered frame's sidecar is recomputed too.
    registration_hash: String,
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
fn enum_serde_name<T: Serialize>(v: &T) -> Result<String, RunError> {
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
fn summary_frame_for(entry: &MeasuredFrame, rejected_fraction: Option<f64>) -> SummaryFrame {
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
    }
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

    rc.summary.groups.push(SummaryGroup {
        key: group.key.clone(),
        frame_count: group.frames.len(),
        included_count,
        master_path,
        rejection_low_path,
        rejection_high_path,
        stats,
        normalization_reference_frame_id,
        ln_reference_path,
        drizzle_path,
        weight_map_path,
        drizzle,
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
}

impl IntegrateTickState {
    fn new() -> Self {
        IntegrateTickState {
            last_emit: Instant::now() - Duration::from_millis(PROGRESS_THROTTLE_MS),
            last_percent: f64::NEG_INFINITY,
            last_plane: 0,
            last_bytes_done: 0,
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
    set_id: i64,
    group_key: &str,
    channels: usize,
    plane: usize,
    frac: f64,
    bytes_done: u64,
    bytes_total: u64,
    force: bool,
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
            stage: Stage::Integrate,
            group_key: Some(group_key.to_string()),
            current: plane,
            total: channels,
            percent,
            bytes_done,
            bytes_total,
            frame_id: None,
            message: None,
        },
    );
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
    set_id: i64,
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
    emit_event(
        emitter,
        STACKING_PROGRESS_EVENT,
        &StackingProgressEvent {
            run_id,
            set_id,
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
                Some(RegisteredFrameOutcome::Aligned { map, record, .. }) => {
                    (map.clone(), record.config_hash.clone().unwrap_or_default())
                }
                _ => continue,
            };
            let weight = e.weight.clone().unwrap_or(FrameWeight {
                channels: Vec::new(),
                normalized: Vec::new(),
                mean: 0.0,
                normalized_mean: 0.0,
                missing: None,
            });
            members.push(GroupMember {
                frame_id: e.frame.frame_id,
                filename: e.frame.filename.clone(),
                calibrated,
                map,
                measurement,
                weight,
                exposure_s: e.frame.exposure_s,
                date_obs: e.frame.date_obs.clone(),
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

    // Ruling 7: the global reference anchors normalization only when it is
    // itself a member of this group; otherwise the group's own best-weighted
    // included member does. Factored into a closure (not just a one-shot
    // match) because stage 6's LN pass, below, can narrow `members` further
    // (a `TooFewMatches` exclusion when LN drives OUTPUT normalization) —
    // the anchor is re-derived from whichever members survive that.
    let global_reference_frame_id = rc.reference_frame_id;
    let pick_reference_idx = |members: &[GroupMember]| -> Option<usize> {
        match members
            .iter()
            .position(|m| Some(m.frame_id) == global_reference_frame_id)
        {
            Some(idx) => Some(idx),
            None => {
                let weights: Vec<FrameWeight> = members.iter().map(|m| m.weight.clone()).collect();
                let included_mask = vec![true; members.len()];
                let star_counts: Vec<usize> =
                    members.iter().map(|m| m.measurement.min_stars()).collect();
                best_by_weight(&weights, &included_mask, &star_counts)
            }
        }
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
            })
            .collect()
    };

    let mut reference_idx = match pick_reference_idx(&members) {
        Some(idx) => idx,
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

    // Ruling 6: every group's master adopts the GLOBAL reference's geometry;
    // the group's OWN plane count (mono vs. debayered OSC) is whatever this
    // group's own measurements actually carry.
    let mut channels = members[reference_idx].measurement.channels.len();
    let width = rc.reference_width;
    let height = rc.reference_height;

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
    let mut ln_reference_path: Option<String> = None;
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
        match run_group_normalization(rc, group, &members, &input, measure_opts, io) {
            Ok(outcome) => {
                ln_reference_path = outcome.reference_path.clone();
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
                        let n = normalize_start.elapsed();
                        let (_, i, d, o) = zero();
                        return Ok((skip_group(rc, group, members.len())?, n, i, d, o));
                    }
                    reference_idx = match pick_reference_idx(&members) {
                        Some(idx) => idx,
                        None => {
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
                rc.warnings.push(format!(
                    "group {}: local normalization failed: {msg}",
                    group.key
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
    let normalize_dur = normalize_start.elapsed();

    // M2 Task 7 (fix round 1, item 3): `GroupInput.ln`, aligned with
    // `members`' (possibly LN-narrowed) order, built directly from the
    // already-verified grids `run_group_normalization` handed back — no
    // file I/O and no exclusion handling here, both moved to the source
    // (see `ln_sidecar_grids`'s own doc above). A member absent from the
    // map (kept with a warning while LN drives rejection only, after its
    // OWN `normalize_frame` call failed this run) gets `None`. `None`
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
    let mut rej_set: Option<RejBitmapSet> = None;
    let mut rej_set_failure: Option<String> = None;
    if rc.config.drizzle.enabled && rc.config.drizzle.use_rejection {
        let included_for_rej = included_after_min_weight(
            &stack_frames,
            reference_idx,
            group_integration.min_weight,
        );
        let stems: Vec<String> = included_for_rej
            .iter()
            .map(|&i| {
                stack_frames[i]
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("frame_{i}"))
            })
            .collect();
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
                    "drizzle skipped for this group"
                );
                rc.warnings.push(format!(
                    "group {}: drizzle skipped — {msg}",
                    group.key
                ));
                rej_set_failure = Some(msg);
            }
        }
    }

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
    let current_plane = std::sync::atomic::AtomicUsize::new(0);
    let tick_state: Mutex<IntegrateTickState> = Mutex::new(IntegrateTickState::new());
    let emitter = rc.emitter.clone();
    let run_id = rc.run_id;
    let set_id = rc.set_id;
    let group_key_for_progress = group.key.clone();

    let on_plane = |p: usize, _total: usize| {
        current_plane.store(p, Ordering::Relaxed);
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            set_id,
            &group_key_for_progress,
            channels,
            p,
            0.0,
            0,
            0,
            true,
        );
    };
    let on_band = |_band: usize, _bands: usize, bytes_done: u64, bytes_total: u64| {
        let plane = current_plane.load(Ordering::Relaxed);
        let frac = if bytes_total > 0 {
            (bytes_done as f64 / bytes_total as f64).min(1.0)
        } else {
            0.0
        };
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            set_id,
            &group_key_for_progress,
            channels,
            plane,
            frac,
            bytes_done,
            bytes_total,
            false,
        );
    };
    let on_combine = |_rows: usize, _rows_total: usize, bytes_done: u64, bytes_total: u64| {
        let plane = current_plane.load(Ordering::Relaxed);
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            set_id,
            &group_key_for_progress,
            channels,
            plane,
            1.0,
            bytes_done,
            bytes_total,
            false,
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
    let cancel: &AtomicBool = &rc.cancel;
    let pool: &rayon::ThreadPool = rc.ctx.image_pool.as_ref();
    let output = match integrate_group(&input, measure_opts, pool, cancel, &progress, io) {
        Ok(o) => o,
        Err(IntegrationError::Cancelled) => return Err(RunError::Cancelled),
        Err(e) => {
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
        set_id,
        &group_key_for_progress,
        channels,
        channels,
        0.0,
        0,
        0,
        true,
    );
    let integrate_dur = integrate_start.elapsed();

    let output_start = Instant::now();

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

    let cards = match build_master_light_cards(&MasterCardInputs {
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

    let name = master_file_name(
        &rc.set_name,
        group.filter.as_deref(),
        group.color_mode,
        group.binning,
        group.exposure_s,
        output.stats.included,
    );

    let written = {
        // Fix round 1, item 2: `unwrap_or_else(|e| e.into_inner())`, not
        // `.unwrap()` — this is a process-wide `static`, so one panic
        // inside a PRIOR `write_master_light` call (any group, any run)
        // would otherwise poison it for the rest of the process's life,
        // failing every later master write with no write ever attempted.
        // Safe to recover: the guarded section only claims a name
        // (`resolve_collision`) and writes through `write_fits_f32`, which
        // already writes to a sibling temp file and atomically renames it
        // into place (`fits_writer::writer::write_fits_f32`) — a panic mid-write
        // leaves an orphaned `*.fits.tmp.<pid>.<seq>` file, never a partial
        // file AT the resolved name itself, so a later `resolve_collision`
        // call still sees that name correctly free and reuses it; there is
        // no partial state under this lock for a later writer to misread.
        let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_master_light(&rc.output_dir, &name, &output, &cards)
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

        for (k, &idx) in output.included.iter().enumerate() {
            if let Some(frac) = output.stats.rejected_fraction_per_frame.get(k).copied() {
                let frame_id = members[idx].frame_id;
                set_frame_rejected_fraction(&conn, rc.run_id, frame_id, frac)?;
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
    let mut drizzle_stats: Option<DrizzleStats> = None;
    if rc.config.drizzle.enabled {
        if let Some(reason) = &rej_set_failure {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                error = %reason,
                "drizzle skipped for this group; the master is unaffected"
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

            let drizzle_outcome: Result<(WrittenDrizzle, DrizzleStats), DrizzleFailure> =
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
                    let output_pairs_by_frame: Vec<Vec<NormalizationPair>> = (0..output
                        .included
                        .len())
                        .map(|k| (0..channels).map(|p| output.output_pairs[p][k]).collect())
                        .collect();
                    let rej_paths: Vec<Option<PathBuf>> = (0..output.included.len())
                        .map(|k| rej_set.as_ref().map(|s| s.path(k).to_path_buf()))
                        .collect();
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
                        use_local_normalization: drizzle_cfg.use_local_normalization,
                        write_weight_map: drizzle_cfg.write_weight_map,
                        measure: measure_opts,
                        ram_total_bytes: None,
                    };

                    // Fix round 1, Important I1: real per-frame progress —
                    // `drizzle_group` calls `on_frame` once per (frame,
                    // plane) unit, strictly sequentially (see
                    // `DrizzleTickState`'s own doc), so a throttled
                    // `emit_drizzle_tick` reaches the SAME `stacking-progress`
                    // channel `rc.progress` uses, via cloned-out plain
                    // values (`emitter`/`run_id`/`set_id`/`group_key`) since
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
                    let drizzle_tick_set_id = rc.set_id;
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
                            drizzle_tick_set_id,
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
                        let drz = match drizzle_group(&drizzle_input, pool, cancel, &drizzle_progress)
                        {
                            Ok(o) => o,
                            Err(e) => break 'attempt Err(e.into()),
                        };
                        let scaled = match wcs.map(|w| scale_plate_solve(w, scale)).transpose() {
                            Ok(s) => s,
                            Err(e) => break 'attempt Err(DrizzleFailure::Other(e.to_string())),
                        };
                        let drz_cards = match build_drizzle_cards(
                            &cards,
                            scaled.as_ref(),
                            scale,
                            drop_shrink,
                            drizzle_cfg.kernel,
                        ) {
                            Ok(c) => c,
                            Err(e) => break 'attempt Err(DrizzleFailure::Other(e.to_string())),
                        };
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
                        match write_drizzled_master(&rc.output_dir, &master_stem, &drz, &drz_cards)
                        {
                            Ok(wd) => Ok((wd, drz.stats)),
                            Err(e) => Err(DrizzleFailure::Other(format!("{e:#}"))),
                        }
                    }
                };
            drizzle_dur = drizzle_start.elapsed();

            match drizzle_outcome {
                Ok((wd, stats)) => {
                    let drizzle_path_str = wd.drizzle.display().to_string();
                    let weight_map_path_str =
                        wd.weight_map.as_ref().map(|p| p.display().to_string());
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
                }
            }
        }
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
    );

    let output_dur = output_start.elapsed();
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

/// One group's stage-6 result: the reference's own artifact path (feeds
/// `SummaryGroup.ln_reference_path` — `None` means "LN did not run for this
/// group", full stop: disabled by the caller, OR EITHER shape of the
/// group-level ruling-R3-style fallback below — carry-over (a), Task 5's
/// re-review) and which frame ids the pass excluded (only ever non-empty
/// when LN drives OUTPUT normalization; ruling R2). `sidecar_grids` (M2 Task
/// 7, fix round 1 item 3) carries the ALREADY-PARSED, ALREADY-VERIFIED
/// `LnFrameGrids` for every member this call itself confirmed has a
/// READABLE `ln` artifact THIS run — a fresh cache hit whose file was
/// re-opened and parsed (not just trusted by hash/size), or a fan-out item
/// that normalized, wrote, and had its own new file read straight back —
/// `process_group_output` hands these to `GroupInput.ln` directly, no
/// second read. A member absent from this map (kept with a warning while
/// LN drives rejection only, after its OWN `normalize_frame` call failed)
/// has no verified grid and must fall back to `None` in `GroupInput.ln`.
/// Always empty when `reference_path` is `None` (LN never ran, nothing to
/// verify).
struct LnGroupOutcome {
    reference_path: Option<String>,
    excluded_frame_ids: Vec<i64>,
    sidecar_grids: HashMap<i64, LnFrameGrids>,
}

/// The scalar diagnostics [`crate::stacking::ln::LnFrameOutcome`] carries, persisted as the `ln`
/// artifact's `payload_json` (same convention as the `metrics` artifact
/// storing a serialized [`FrameMeasurement`]) so a cache hit can fill
/// `SummaryFrame.ln_scale` without re-reading the `.athln` sidecar's actual
/// grids — nothing in the summary needs those, only these three numbers.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct LnArtifactPayload {
    scale: f64,
    matches: usize,
    cells_rejected: usize,
}

/// Sets `ln_scale`/`cached_ln` on `group_key`'s `MeasuredFrame` entry for
/// `frame_id` (a no-op if either lookup misses, which never happens for a
/// frame [`run_group_normalization`] itself just fanned `normalize_frame`
/// out over or found a fresh `ln` artifact for).
fn set_ln_summary(
    rc: &mut RunContext,
    group_key: &str,
    frame_id: i64,
    ln_scale: Option<f64>,
    cached: bool,
) {
    if let Some(entries) = rc.measured.get_mut(group_key) {
        if let Some(e) = entries.iter_mut().find(|e| e.frame.frame_id == frame_id) {
            e.ln_scale = ln_scale;
            e.cached_ln = cached;
        }
    }
}

/// Stage 6 proper (spec §5.2, M2 Task 5): resolves/caches `group`'s LN
/// reference (best `referenceFrames` of `members` by weight, linear-fit
/// rejection, global normalization — [`build_ln_reference`]), models its
/// backgrounds once per channel, then fans [`normalize_frame`] out over
/// EVERY member (the measure stage's own [`admission`] sizing, working set
/// `channels × W × H × 4 × 2` bytes — one warped target plane plus its
/// background grid, twice over for the read-then-decode round trip),
/// skipping a member whose `ln` artifact is already fresh (path + size +
/// hash — same [`is_fresh`] rule every other per-frame artifact uses).
///
/// Ruling R3: fewer than 3 of `members` (after `referenceFrames` is applied)
/// falls back to global normalization for this WHOLE group with a warning —
/// `Ok(LnGroupOutcome { reference_path: None, excluded_frame_ids: vec![] })`,
/// never a hard failure. A per-member [`LnError`] (most commonly
/// [`LnError::TooFewMatches`]) excludes that member (`rc.runtime_exclusions`,
/// its `MeasuredFrame::included`/`reason`) when `cfg.normalization.local.enabled`
/// — LN is the output normalization the run actually wants — and is only
/// `warn!`ed otherwise (LN drives rejection normalization alone; the frame
/// keeps global normalization). `Err(RunError::Cancelled)` propagates from a
/// cancel noticed while resolving/building the reference; a fan-out item's
/// own cancellation surfaces as an ordinary per-item error and is caught by
/// the `rc.check_cancel()?` immediately after the fan-out returns, the same
/// pattern [`stage_measure`] uses.
fn run_group_normalization(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    members: &[GroupMember],
    input: &GroupInput<'_>,
    measure_opts: &MeasureOptions,
    io: IoPolicy,
) -> Result<LnGroupOutcome, RunError> {
    let cfg = rc.config.clone();
    let ln_cfg = cfg.normalization.local;
    let ln_drives_output = ln_cfg.enabled;

    // Best-weighted members first (stable sort — ties keep `members`' own
    // order), same ranking [`build_ln_reference`] performs internally: computed
    // here too because the normalize-stage hash needs the member LIST before
    // knowing whether the reference will be rebuilt or reused (a cache hit's
    // `LnReference::frames_used` always comes back empty, see its own doc).
    let mut ranked: Vec<usize> = (0..members.len()).collect();
    ranked.sort_by(|&a, &b| {
        members[b]
            .weight
            .normalized_mean
            .total_cmp(&members[a].weight.normalized_mean)
    });
    let n = (ln_cfg.reference_frames as usize).min(ranked.len());
    if n < 3 {
        // Fix round 1, item 10: this branch fires ONLY when the config's
        // own `referenceFrames < 3` (a group under 3 MEMBERS never reaches
        // this function — `process_group_output` skips it upstream), so the
        // message must name that, not repeat "fewer than 3 included frames"
        // (a different, already-handled condition).
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            reference_frames = ln_cfg.reference_frames,
            "local normalization off for this group: referenceFrames < 3"
        );
        rc.warnings.push(format!(
            "group {}: local normalization off — referenceFrames ({}) < 3; using global normalization",
            group.key, ln_cfg.reference_frames
        ));
        return Ok(LnGroupOutcome {
            reference_path: None,
            excluded_frame_ids: Vec::new(),
            sidecar_grids: HashMap::new(),
        });
    }
    let reference_members: Vec<usize> = ranked[..n].to_vec();
    let reference_member_ids: Vec<i64> = reference_members
        .iter()
        .map(|&i| members[i].frame_id)
        .collect();

    let mut member_reg_hashes: Vec<&str> = reference_members
        .iter()
        .map(|&i| members[i].registration_hash.as_str())
        .collect();
    member_reg_hashes.sort_unstable();
    let combined_registration_hash = member_reg_hashes.join(",");
    // Fix round 1, item 2: the reference's OWN hash has no further
    // reference to fold in — `""` (see `normalization_hash_for`'s own doc).
    let reference_hash =
        normalization_hash_for(&cfg, &combined_registration_hash, &reference_member_ids, "");

    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Normalize);
    let reference_path_buf = rc.layout.ln_reference_path(&group.key);
    std::fs::create_dir_all(rc.layout.ln_dir(&group.key)).map_err(|e| {
        RunError::Other(format!(
            "creating {}: {e}",
            rc.layout.ln_dir(&group.key).display()
        ))
    })?;

    let existing_reference_artifact = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(&conn, rc.set_id, &group.key, "ln_reference", None)?
    };
    let cached_reference_path: Option<PathBuf> = if force_fresh {
        None
    } else {
        existing_reference_artifact
            .as_ref()
            .filter(|row| is_fresh(row, &reference_hash))
            .and_then(|row| row.path.clone())
            .map(PathBuf::from)
    };

    // M6 (final fix wave): the path reported in the summary must be the
    // RESOLVED one — the artifact row's own stored path on a cache hit
    // (which can differ from `reference_path_buf` if the working folder
    // moved since that row was written, same reasoning Task 7's fix round
    // already applied to a per-frame sidecar's own path), the freshly
    // written `reference_path_buf` otherwise (`build_and_write_ln_reference`
    // always writes there). Defaults to `reference_path_buf` and is
    // overridden only by the cache-hit-success arm below.
    let mut resolved_reference_path: PathBuf = reference_path_buf.clone();
    let ln_reference: LnReference = match cached_reference_path {
        Some(path) => match read_reference(&path) {
            Ok(r) => {
                resolved_reference_path = path;
                r
            }
            Err(e) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %e,
                    "failed to read the cached LN reference; rebuilding"
                );
                build_and_write_ln_reference(
                    rc,
                    group,
                    input,
                    n,
                    io,
                    &reference_hash,
                    &reference_member_ids,
                )?
            }
        },
        None => build_and_write_ln_reference(
            rc,
            group,
            input,
            n,
            io,
            &reference_hash,
            &reference_member_ids,
        )?,
    };

    let ref_params = BackgroundParams {
        scale: ln_cfg.scale,
        ..DEFAULT_PARAMS
    };
    let ref_backgrounds: Vec<BackgroundGrid> = ln_reference
        .planes
        .iter()
        .map(|plane| background_grid(plane, ln_reference.width, ln_reference.height, &ref_params))
        .collect();

    // Fix round 1, item 3: `BackgroundGrid`'s own documented contract — a
    // fully invalid grid (no measurable cell anywhere) is a fallback the
    // caller "should refuse", never trust. A broken REFERENCE background
    // makes no frame's sidecar trustworthy, so this is a group-level
    // ruling-R3-shaped fallback (warn + global normalization), not a
    // per-frame failure. Carry-over (a), Task 5's re-review:
    // `reference_path` is `None` here, same as the `n < 3` fallback above —
    // `Some` means "LN ran"; the reference FILE was still written to disk at
    // `reference_path_buf` (harmless, just not reported as a usable one),
    // but nothing below normalized against it, so there is nothing to read
    // back.
    for (p, bg) in ref_backgrounds.iter().enumerate() {
        let total_cells = bg.gw * bg.gh;
        if total_cells > 0 && bg.invalid_cells == total_cells {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                channel = p,
                "local normalization off for this group: the LN reference's background model has no measurable cell"
            );
            rc.warnings.push(format!(
                "group {}: local normalization off — the LN reference's background model has no measurable cell; using global normalization",
                group.key
            ));
            return Ok(LnGroupOutcome {
                reference_path: None,
                excluded_frame_ids: Vec::new(),
                sidecar_grids: HashMap::new(),
            });
        }
    }

    // Fix round 1, item 6: computed ONCE per group, not once per frame.
    // I2 (final fix wave): also hoists the reference-side star detection +
    // PSF fit + match tree (`prepared`), so `normalize_frame`'s
    // `relative_scale_against` call never re-detects/re-fits the
    // reference plane on every frame either.
    let reference_for_detection =
        LnReferenceForDetection::build(&ln_reference, ln_cfg.psf_model, measure_opts.max_stars);

    let group_frames_by_id: HashMap<i64, &GroupFrame> =
        group.frames.iter().map(|f| (f.frame_id, f)).collect();

    let mut sidecar_paths: Vec<PathBuf> = Vec::with_capacity(members.len());
    let mut per_member_hash: Vec<String> = Vec::with_capacity(members.len());
    let mut needs_normalize: Vec<usize> = Vec::new();
    // M2 Task 7 (fix round 1, item 3): every member this function itself
    // VERIFIES has a READABLE `ln` artifact THIS run (a fresh cache hit
    // whose file was re-opened and parsed, or a fan-out item that
    // normalized, wrote, and had its own new file read straight back) —
    // becomes the returned `LnGroupOutcome.sidecar_grids`, already parsed,
    // so `process_group_output` never has to read a file itself. A member
    // that fails or is never attempted is simply absent.
    let mut sidecar_grids: HashMap<i64, LnFrameGrids> = HashMap::with_capacity(members.len());

    for (i, m) in members.iter().enumerate() {
        let stem = match group_frames_by_id.get(&m.frame_id).copied() {
            Some(gf) => calibrated_file_stem(group, gf),
            None => Path::new(&m.filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&m.filename)
                .to_string(),
        };
        let sidecar = rc.layout.ln_sidecar_path(&group.key, &stem);
        // Fix round 1, item 2: folds in the group's OWN reference hash —
        // re-registering or recalibrating ONE reference member rebuilds the
        // reference (a new `B_ref`, a new scale anchor), which must
        // invalidate every OTHER frame's sidecar too, not just that one
        // member's (the id LIST alone is unchanged by a member's own
        // re-registration, so it can't catch this on its own).
        let frame_hash = normalization_hash_for(
            &cfg,
            &m.registration_hash,
            &reference_member_ids,
            &reference_hash,
        );
        sidecar_paths.push(sidecar);
        per_member_hash.push(frame_hash.clone());

        let existing = {
            let conn = db(&rc.ctx)?.conn();
            crate::db::stacking::find_artifact(
                &conn,
                rc.set_id,
                &group.key,
                "ln",
                Some(m.frame_id),
            )?
        };
        let fresh = !force_fresh
            && existing
                .as_ref()
                .is_some_and(|row| is_fresh(row, &frame_hash));
        if fresh {
            let cached_payload: Option<LnArtifactPayload> = existing
                .as_ref()
                .and_then(|row| row.payload_json.as_deref())
                .and_then(|s| serde_json::from_str(s).ok());
            match cached_payload {
                Some(payload) => {
                    // Fix round 1, items 2 + 3: verify against the
                    // ARTIFACT ROW's OWN path — `is_fresh` already
                    // confirmed THAT path's size matches, not the
                    // recomputed `sidecar` above (the working folder may
                    // have moved since this artifact was written, leaving
                    // `sidecar` pointing at a location nothing has ever
                    // written to — reading it would wrongly find nothing
                    // even though the real, fresh file is still sitting at
                    // the row's own path). A read failure here means
                    // RE-NORMALIZE in this same run, never an "unreadable"
                    // exclusion for what a fresh write in this very run can
                    // fix.
                    let row_path = existing.as_ref().and_then(|row| row.path.as_deref());
                    match row_path.map(|p| LnFrameGrids::read(Path::new(p))) {
                        Some(Ok(grids)) => {
                            set_ln_summary(rc, &group.key, m.frame_id, Some(payload.scale), true);
                            sidecar_grids.insert(m.frame_id, grids);
                        }
                        Some(Err(e)) => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = m.frame_id,
                                error = %e,
                                "cached ln sidecar unreadable; re-normalizing"
                            );
                            needs_normalize.push(i);
                        }
                        None => {
                            // `is_fresh`'s own `.and_then` chain requires
                            // `row.path` to be `Some` before it can ever
                            // return `true` — unreachable in practice,
                            // handled the same as a read failure for
                            // safety.
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = m.frame_id,
                                "fresh ln artifact row has no stored path; re-normalizing"
                            );
                            needs_normalize.push(i);
                        }
                    }
                }
                None => {
                    // Fix round 1, item 9: a fresh-by-hash row whose
                    // payload doesn't parse is NOT a usable cache hit — it
                    // used to yield `cached_ln = true` with `ln_scale =
                    // None`, a summary that claims "reused" while reporting
                    // nothing reused. Re-normalize instead.
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = m.frame_id,
                        "ln artifact payload unreadable; treating as stale"
                    );
                    needs_normalize.push(i);
                }
            }
        } else {
            needs_normalize.push(i);
        }
    }

    let total = members.len();
    rc.progress(
        Stage::Normalize,
        Some(group.key.clone()),
        0,
        total,
        0,
        0,
        None,
        None,
    );

    let admission_n =
        admission(input.channels as u64 * input.width as u64 * input.height as u64 * 4 * 2);
    let interpolation = input.interpolation;
    let clamping = input.clamping;
    let cancel_ref: &AtomicBool = &rc.cancel;
    let stack_frames_ref: &[StackFrame] = input.frames;
    let ref_backgrounds_ref: &[BackgroundGrid] = &ref_backgrounds;
    let ln_reference_ref: &LnReference = &ln_reference;
    let reference_for_detection_ref: &LnReferenceForDetection = &reference_for_detection;
    let sidecar_paths_ref: &[PathBuf] = &sidecar_paths;

    let results = fan_out(
        needs_normalize.clone(),
        admission_n,
        cancel_ref,
        move |i: usize| {
            normalize_frame(
                ln_reference_ref,
                reference_for_detection_ref,
                ref_backgrounds_ref,
                &stack_frames_ref[i],
                &ln_cfg,
                measure_opts,
                interpolation,
                clamping,
                &sidecar_paths_ref[i],
                cancel_ref,
            )
            .map_err(|e| e.to_string())
        },
    );
    rc.check_cancel()?;

    let mut excluded_frame_ids: Vec<i64> = Vec::new();
    // Shared handling for a frame whose local normalization has genuinely
    // failed this run — `normalize_frame` itself failed, OR (fix round 1,
    // item 3) writing succeeded but reading the file straight back failed.
    // Excludes the frame (persisted, `rc.runtime_exclusions`) when LN
    // drives OUTPUT normalization; otherwise warns and leaves it on global
    // normalization (ruling R2). `rc`/`excluded_frame_ids` are explicit
    // parameters, not captures, so the closure can be called from either
    // match arm below without fighting the borrow checker over `rc`.
    let fail_ln_frame = |rc: &mut RunContext,
                         excluded_frame_ids: &mut Vec<i64>,
                         frame_id: i64,
                         msg: String|
     -> Result<(), RunError> {
        if ln_drives_output {
            rc.runtime_exclusions.push((frame_id, msg.clone()));
            excluded_frame_ids.push(frame_id);
            // Fix round 1, item 1: flip AND persist — `write_frame_rows`
            // already ran at the end of stage 5, so without this the
            // frame's `stacking_run_frames` row still says `included = 1`
            // and the frames table would render a frame the master does
            // not contain as stacked.
            //
            // Carry-over (b), Task 5's re-review: a DB error HERE happens
            // AFTER `exclude_frame_and_persist` already flipped
            // `rc.measured`'s in-memory state — the generic "Other -> warn,
            // continue with global normalization" treatment the caller
            // gives every other internal error would silently leave this
            // frame excluded in memory while the group proceeds as if the
            // exclusion never happened. `RunError::ExclusionPersistFailed`
            // forces `process_group_output` to fail the group outright
            // instead.
            if let Err(e) = exclude_frame_and_persist(rc, group, frame_id, msg.clone()) {
                let text = match e {
                    RunError::Cancelled => return Err(RunError::Cancelled),
                    RunError::Other(inner) => inner,
                    RunError::ExclusionPersistFailed(inner) => inner,
                };
                return Err(RunError::ExclusionPersistFailed(text));
            }
            tracing::warn!(
                run_id = rc.run_id,
                frame_id,
                reason = %msg,
                "frame excluded: local normalization"
            );
        } else {
            tracing::warn!(
                run_id = rc.run_id,
                frame_id,
                reason = %msg,
                "local normalization failed; frame keeps global normalization"
            );
            rc.warnings.push(format!(
                "frame {frame_id}: local normalization failed: {msg}"
            ));
        }
        Ok(())
    };

    for (pos, res) in results.into_iter().enumerate() {
        let member_idx = needs_normalize[pos];
        let frame_id = members[member_idx].frame_id;
        match res {
            None => return Err(RunError::Cancelled),
            Some(Ok(outcome)) => {
                let (size, modified_at) = file_identity(&outcome.sidecar)
                    .map_err(|e| RunError::Other(format!("{e:#}")))?;
                let payload = serde_json::to_string(&LnArtifactPayload {
                    scale: outcome.scale,
                    matches: outcome.matches,
                    cells_rejected: outcome.cells_rejected,
                })
                .map_err(|e| RunError::Other(format!("failed to serialize ln payload: {e}")))?;
                {
                    let conn = db(&rc.ctx)?.conn();
                    upsert_artifact(
                        &conn,
                        &NewArtifact {
                            frames_set_id: rc.set_id,
                            frame_id: Some(frame_id),
                            group_key: &group.key,
                            kind: "ln",
                            path: Some(&outcome.sidecar.to_string_lossy()),
                            config_hash: &per_member_hash[member_idx],
                            size: Some(size),
                            modified_at: Some(&modified_at),
                            payload_json: Some(&payload),
                        },
                    )?;
                }
                // Fix round 1, item 3: read the just-written sidecar back
                // HERE — a hand-off, not a second read: the parsed grids
                // become this frame's `LnGroupOutcome.sidecar_grids` entry
                // directly, so `process_group_output` never re-opens the
                // file. On the rare chance this immediate read-back fails
                // (the write itself already succeeded), the frame is
                // treated exactly like any other normalization failure —
                // excluded (LN drives output) or warned (rejection only) —
                // via the SAME `fail_ln_frame` path, its error text folded
                // into the reason (item 5: the reason must say why).
                match LnFrameGrids::read(&outcome.sidecar) {
                    Ok(grids) => {
                        tracing::debug!(
                            run_id = rc.run_id,
                            frame_id,
                            ln_scale = outcome.scale,
                            ln_matches = outcome.matches,
                            ln_cells_rejected = outcome.cells_rejected,
                            "ln frame normalized"
                        );
                        set_ln_summary(rc, &group.key, frame_id, Some(outcome.scale), false);
                        sidecar_grids.insert(frame_id, grids);
                    }
                    Err(e) => {
                        fail_ln_frame(
                            rc,
                            &mut excluded_frame_ids,
                            frame_id,
                            format!(
                                "local normalization: writing .athln sidecar: reading it back failed: {e:#}"
                            ),
                        )?;
                    }
                }
            }
            Some(Err(msg)) => {
                fail_ln_frame(rc, &mut excluded_frame_ids, frame_id, msg)?;
            }
        }
    }

    rc.progress(
        Stage::Normalize,
        Some(group.key.clone()),
        total,
        total,
        0,
        0,
        None,
        None,
    );

    Ok(LnGroupOutcome {
        reference_path: Some(resolved_reference_path.display().to_string()),
        excluded_frame_ids,
        sidecar_grids,
    })
}

/// The cache-miss half of [`run_group_normalization`]'s reference
/// resolution: builds the group's LN reference from `input` (best `n`
/// included members by weight, [`build_ln_reference`]), writes it to
/// `ln/<group>/reference.fits` and upserts the `ln_reference` artifact row.
/// `reference_hash` is the caller's already-computed config hash — this
/// function only persists it, never recomputes it.
fn build_and_write_ln_reference(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    input: &GroupInput<'_>,
    n: usize,
    io: IoPolicy,
    reference_hash: &str,
    reference_member_ids: &[i64],
) -> Result<LnReference, RunError> {
    let build_start = Instant::now();
    let included: Vec<usize> = (0..input.frames.len()).collect();
    let cancel: &AtomicBool = &rc.cancel;
    let pool: &rayon::ThreadPool = rc.ctx.image_pool.as_ref();
    let no_progress = |_: usize, _: usize| {};

    let built =
        build_ln_reference(input, &included, n, pool, cancel, io, &no_progress).map_err(|e| {
            if matches!(e, IntegrationError::Cancelled) {
                RunError::Cancelled
            } else {
                RunError::Other(format!("LN reference build failed: {e}"))
            }
        })?;

    let reference_path = rc.layout.ln_reference_path(&group.key);
    let cards = vec![
        Card::new("ATH_STKI", CardValue::Str(rc.run_id.to_string()))
            .map_err(|e| RunError::Other(format!("building LN reference cards: {e}")))?,
        Card::new("ATH_STKG", CardValue::Str(group.key.clone()))
            .map_err(|e| RunError::Other(format!("building LN reference cards: {e}")))?,
    ];
    write_reference(&built, &reference_path, &cards)
        .map_err(|e| RunError::Other(format!("writing LN reference: {e:#}")))?;

    let (size, modified_at) =
        file_identity(&reference_path).map_err(|e| RunError::Other(format!("{e:#}")))?;
    // Fix round 1, item 5: `build_plan` verifies every `ln` row's freshness
    // WITHOUT the stage-3 weight ranking that picked this member list — it
    // can only do that by trusting a STORED copy of exactly the two inputs
    // `normalization_hash_for` needs beyond `cfg` and each frame's own
    // registration hash. `reference_hash` here duplicates the artifact
    // row's own `config_hash` column (kept in the payload too so `build_plan`
    // needs only this one parse, never a second read of that column).
    let reference_payload = serde_json::to_string(&LnReferencePayload {
        reference_member_ids: reference_member_ids.to_vec(),
        reference_hash: reference_hash.to_string(),
    })
    .map_err(|e| RunError::Other(format!("failed to serialize ln_reference payload: {e}")))?;
    {
        let conn = db(&rc.ctx)?.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                frames_set_id: rc.set_id,
                frame_id: None,
                group_key: &group.key,
                kind: "ln_reference",
                path: Some(&reference_path.to_string_lossy()),
                config_hash: reference_hash,
                size: Some(size),
                modified_at: Some(&modified_at),
                payload_json: Some(&reference_payload),
            },
        )?;
    }

    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_reference_frames = built.frames_used.len(),
        duration_ms = build_start.elapsed().as_millis() as u64,
        "ln reference built"
    );

    Ok(built)
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
fn stage_output(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let measure_opts = cfg
        .measurement
        .measure_options(cfg.normalization.scale_estimator);

    let reference_frame_id = rc
        .reference_frame_id
        .ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;

    // Ruling 6: every group's master shares the GLOBAL reference's WCS —
    // fetched once, not per group.
    let wcs = {
        let conn = db(&rc.ctx)?.conn();
        get_plate_solve(&conn, reference_frame_id)?
    };
    if wcs.is_none() {
        tracing::warn!(
            run_id = rc.run_id,
            frame_id = reference_frame_id,
            "no plate solve on the reference frame; the master has no WCS"
        );
        rc.warnings
            .push("no plate solve on the reference frame; the master has no WCS".to_string());
    }

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
        }
    }

    let groups = rc.plan_groups.clone();
    let mut any_master = false;
    let mut normalize_total = Duration::ZERO;
    let mut integrate_total = Duration::ZERO;
    let mut drizzle_total = Duration::ZERO;
    let mut output_total = Duration::ZERO;
    // Same expression `process_group_output` uses per group to decide
    // whether its own LN pass ran at all — one run has one `normalization`
    // config, so this is the same answer for every group.
    let ln_active = cfg.normalization.local.enabled
        || cfg.normalization.rejection == RejectionNormalization::Local;

    for group in &groups {
        rc.check_cancel()?;
        let (outcome, n, i, d, o) =
            process_group_output(rc, group, &measure_opts, wcs.as_ref())?;
        normalize_total += n;
        integrate_total += i;
        drizzle_total += d;
        output_total += o;
        if matches!(outcome, GroupOutcome::Written) {
            any_master = true;
        }
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
                cleanup_work(&conn, rc.set_id, &rc.layout, CleanupWhat::Registered)?
            };
            tracing::info!(run_id = rc.run_id, freed_bytes = freed, "cleanup applied");
        }
        CleanupPolicy::DeleteIntermediates => {
            let freed = {
                let conn = db(&rc.ctx)?.conn();
                cleanup_work(&conn, rc.set_id, &rc.layout, CleanupWhat::Intermediates)?
            };
            tracing::info!(run_id = rc.run_id, freed_bytes = freed, "cleanup applied");
        }
    }

    Ok(())
}

/// Build a [`RunContext`] directly from fixture data, bypassing
/// [`start_stacking`]'s DB/plan machinery — the calibrate-stage tests below
/// (and Task 7's own stage 3-5 tests) call [`stage_calibrate`] (or their own
/// stage functions) straight off a context built this way. `run_id` is the
/// caller's to choose: pass a real `stacking_runs.id` (from
/// [`crate::db::stacking::insert_run`]) for a test that also asserts on the
/// DB row or the provenance file; a fixed dummy otherwise — the calibrate
/// stage's reuse logic never depends on it, since `stacking_artifacts` is
/// keyed by `(frames_set_id, group_key, kind, frame_id)`, never by `run_id`.
///
/// `group_ids` (Task 7's own addition to this signature): stages 3 and 5
/// write `stacking_run_groups`/`stacking_run_frames` rows, both FK'd to a
/// real `stacking_runs` row (this crate's pooled connections run with
/// `PRAGMA foreign_keys = ON`) — a calibrate-only test can still pass
/// `HashMap::new()` (calibrate never reads it), but a test that runs stage 3
/// or 5 needs one `stacking_run_groups` row per group, inserted against the
/// SAME real `run_id`, with THIS map built from the returned ids
/// (`run_stages_for_test`'s own `seed_run_and_groups` helper does exactly
/// that).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn test_context(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    run_id: i64,
    set_id: i64,
    set_name: &str,
    config: StackingConfig,
    plan_groups: Vec<IntegrationGroup>,
    layout: WorkingLayout,
    output_dir: PathBuf,
    group_ids: HashMap<String, i64>,
) -> RunContext {
    let hash = config_hash(&config);
    let summary = RunSummary {
        run_id,
        set_id,
        set_name: set_name.to_string(),
        app_version: "test".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        status: "planning".to_string(),
        config: config.clone(),
        config_hash: hash.clone(),
        reference: SummaryReference {
            frame_id: None,
            filename: None,
            mode: config.reference.mode,
            weight: None,
        },
        measurement: SummaryMeasurement {
            seed_source: "fast".to_string(),
            scale_estimator: config.normalization.scale_estimator,
        },
        groups: Vec::new(),
        masters_built: Vec::new(),
        stages: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };

    RunContext {
        ctx,
        emitter,
        run_id,
        set_id,
        set_name: set_name.to_string(),
        config,
        hash,
        plan_groups,
        masters_to_build: Vec::new(),
        excluded: Vec::new(),
        group_ids,
        layout,
        output_dir,
        cancel: Arc::new(AtomicBool::new(false)),
        app_version: "test".to_string(),
        warnings: Vec::new(),
        timings: Vec::new(),
        summary,
        last_emit: Instant::now(),
        rerun_from: None,
        memo: HashMemo::new(),
        hot_maps: HashMap::new(),
        cached_calibrated: HashMap::new(),
        runtime_exclusions: Vec::new(),
        measured: HashMap::new(),
        reference_frame_id: None,
        reference_calibrated: None,
        reference_width: 0,
        reference_height: 0,
        drizzle_attempted: 0,
        fail_after_stage: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullEmitter;
    use crate::stacking::rej::RejBitmap;
    use crate::stacking::test_fixtures::{self, LightSpec};

    fn light_spec<'a>(stem: &'a str, date_obs: &'a str) -> LightSpec<'a> {
        LightSpec {
            stem,
            instrume: "cam",
            filter: None,
            binning: 1,
            width: 64,
            height: 48,
            exptime: 60.0,
            date_obs,
            bayerpat: None,
            write_file: true,
        }
    }

    /// As [`light_spec`], but at the star-field fixtures' own canvas size
    /// ([`STAR_FIELD_WIDTH`]x[`STAR_FIELD_HEIGHT`], defined with the Task 7
    /// stage 3-5 tests below) — every frame in one of those tests' groups,
    /// including an extra low-weight/flat-field frame added alongside the
    /// star frames, must share this same size to land in the same
    /// `group_frames` group.
    fn star_light_spec<'a>(stem: &'a str, date_obs: &'a str) -> LightSpec<'a> {
        LightSpec {
            stem,
            instrume: "cam",
            filter: None,
            binning: 1,
            width: STAR_FIELD_WIDTH,
            height: STAR_FIELD_HEIGHT,
            exptime: 60.0,
            date_obs,
            bayerpat: None,
            write_file: true,
        }
    }

    const THREE_TIMES: [&str; 3] = [
        "2025-01-01T00:00:00",
        "2025-01-01T00:05:00",
        "2025-01-01T00:10:00",
    ];
    const SET_NAME: &str = "LDN 1272";

    /// Waits (30s cap, 20ms polls) for `run_id` to disappear from
    /// `ctx.active_stacks` — the thread's single exit path removes it last
    /// among the observable side effects these tests care about.
    fn wait_for_run(ctx: &ServiceContext, run_id: i64) {
        let start = Instant::now();
        loop {
            if !ctx.active_stacks.lock().unwrap().contains_key(&run_id) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "stacking run {run_id} did not finish within 30s"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    struct Recording(std::sync::Mutex<Vec<(String, serde_json::Value)>>);

    impl Recording {
        fn new() -> Self {
            Recording(std::sync::Mutex::new(Vec::new()))
        }
        fn events(&self, name: &str) -> Vec<serde_json::Value> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .collect()
        }
    }

    impl ProgressEmitter for Recording {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0
                .lock()
                .unwrap()
                .push((event_name.to_string(), payload));
        }
    }

    /// Seed a ready-to-run fixture (3 lights + linked master dark/flat +
    /// `stacking.working_dir`/`stacking.output_dir`) onto `ctx`'s OWN
    /// on-disk catalog (`db_path` — the SAME path `ctx`'s pooled `Database`
    /// was opened on), by opening a second, plain `Connection` to that file
    /// and driving it through the SAME `test_fixtures` helpers `plan.rs`/
    /// `groups.rs`'s tests use. A `ServiceContext`'s pooled `Database` can
    /// only see rows committed to an on-disk file — never a private
    /// in-memory `Connection` — so this is the one way to reuse the proven
    /// fixture builder for a `start_stacking`/`RunContext`-level test.
    fn seed_ready(
        db_path: &Path,
        set_name: &str,
    ) -> (
        test_fixtures::Fixture,
        Vec<i64>,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
        let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

        let mut light_ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&fixture, &light_spec(&format!("f{i}"), t));
            light_ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_OUTPUT_DIR,
            output.path().to_str().unwrap(),
        )
        .unwrap();

        (fixture, light_ids, working, output)
    }

    // ── start_stacking / cancel_stacking ────────────────────────────────

    #[test]
    fn start_refuses_blocked_plans_and_double_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

        // No lights at all yet: the plan is blocked ("frames" — fewer than
        // 3 included) — start refuses with Invalid.
        let empty_conn = rusqlite::Connection::open(&db_path).unwrap();
        let empty_fixture = test_fixtures::frame_set_with_conn(empty_conn, "Empty");
        let err = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            empty_fixture.set_id,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        drop(empty_fixture);

        // A ready fixture (masters + folders), on the SAME catalog. Fix
        // round 1, item 6: hold the queue's one slot so the first run
        // parks in `acquire` — start twice → the second is a Conflict
        // deterministically, with no race against the first run's own
        // completion (same technique as `cancel_before_admission_finishes_cancelled`).
        let (fixture, light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);
        let _ = &light_ids;

        let hold_flag = Arc::new(AtomicBool::new(false));
        let (hold_permit, _hold_job) = ctx
            .compute_queue
            .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
            .unwrap();

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("first start should succeed");

        let second = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(second, ApiError::Conflict(_)), "{second:?}");

        cancel_stacking(&ctx, started.run_id).unwrap();
        drop(hold_permit);

        wait_for_run(&ctx, started.run_id);
    }

    /// Plan 5b final fix wave, review finding B2: a failure AFTER the
    /// `active_stacks` handle is registered but BEFORE the run thread
    /// actually spawns must never wedge the frame set. Before
    /// `StartStackingGuard` existed, such a failure (here: the per-group
    /// `insert_group` loop, via the brief's own suggested fault — a
    /// `stacking_run_groups` write failing) returned via `?` straight past
    /// all cleanup: the handle stayed in `active_stacks` forever (a
    /// process-memory map `heal_interrupted_runs` cannot see past) and the
    /// `stacking_runs` row stayed `"planning"` forever, so every later
    /// `start_stacking` for the same set answered `Conflict` until restart.
    #[test]
    fn start_stacking_never_wedges_a_set_when_insert_group_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);

        // Fault injection: the per-group insert loop writes to
        // `stacking_run_groups`; `insert_run` (the row that must NOT be
        // left stuck) and the handle registration both land against
        // `stacking_runs`, an untouched table, so they still succeed.
        fixture
            .conn
            .execute("DROP TABLE stacking_run_groups", [])
            .unwrap();

        let err = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect_err("insert_group failing must surface as an error, not wedge the set");
        assert!(matches!(err, ApiError::Internal(_)), "{err:?}");

        assert!(
            ctx.active_stacks.lock().unwrap().is_empty(),
            "the guard must remove the handle on this early-return path"
        );

        let rows = crate::db::stacking::list_runs(&fixture.conn, fixture.set_id, 10).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].status, "failed", "must never stay stuck planning");
        assert!(rows[0].finished_at.is_some());
        assert!(rows[0].error.is_some());

        // Undo the fault and confirm the set is no longer wedged: a second
        // `start_stacking` is admitted, not `Conflict`.
        crate::db::init_db(&fixture.conn).unwrap();
        let second = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("a second start on the same set must be admitted after the failed setup");

        cancel_stacking(&ctx, second.run_id).unwrap();
        wait_for_run(&ctx, second.run_id);
    }

    #[test]
    fn cancel_before_admission_finishes_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);

        // Occupy the queue's one slot with a dummy Analysis permit from this
        // test thread, so the stacking run below is admitted only once it
        // is dropped.
        let hold_flag = Arc::new(AtomicBool::new(false));
        let (hold_permit, _hold_job) = ctx
            .compute_queue
            .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
            .unwrap();

        let recorder = Arc::new(Recording::new());
        let started = start_stacking(
            ctx.clone(),
            recorder.clone(),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("start should succeed even while the queue is held");

        // Still queued behind the held permit — must not be 'running' yet.
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_ne!(row.status, "running", "must not be running while queued");

        cancel_stacking(&ctx, started.run_id).unwrap();
        drop(hold_permit);

        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "cancelled");
        assert!(row.finished_at.is_some());

        let completes = recorder.events(STACKING_COMPLETE_EVENT);
        assert_eq!(completes.len(), 1, "{completes:?}");
        assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
        assert_eq!(completes[0]["success"].as_bool(), Some(false));

        // Never even reached "running" — the queue wait handles it before
        // `run_pipeline` gets that far.
        let running_events: Vec<_> = recorder
            .events(STACKING_PROGRESS_EVENT)
            .into_iter()
            .filter(|e| e["stage"] == "calibrate")
            .collect();
        assert!(
            running_events.is_empty(),
            "a queue-cancelled run must never reach stage 1: {running_events:?}"
        );
    }

    #[test]
    fn thread_emits_complete_exactly_once_even_on_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);

        let cfg = StackingConfig::default();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

        let run_id = insert_run(
            &fixture.conn,
            &NewRun {
                frames_set_id: fixture.set_id,
                config_json: "{}",
                config_hash: "test-hash",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: output.path().to_str().unwrap(),
            },
        )
        .unwrap();

        let recorder = Arc::new(Recording::new());
        let mut rc = test_context(
            ctx.clone(),
            recorder.clone(),
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output.path().to_path_buf(),
            HashMap::new(),
        );
        rc.fail_after_stage = Some(Stage::Calibrate);
        let cancel = rc.cancel.clone();

        {
            let mut active = ctx.active_stacks.lock().unwrap();
            active.insert(
                run_id,
                StackHandle {
                    cancel_flag: cancel,
                    frames_set_id: fixture.set_id,
                },
            );
        }

        let handle = std::thread::Builder::new()
            .name(format!("stacking-run-{run_id}"))
            .spawn(move || {
                run_thread(rc);
            })
            .unwrap();
        handle
            .join()
            .expect("the injected panic must be caught inside run_thread, never propagate");

        wait_for_run(&ctx, run_id);

        let completes = recorder.events(STACKING_COMPLETE_EVENT);
        assert_eq!(completes.len(), 1, "{completes:?}");
        assert_eq!(completes[0]["success"].as_bool(), Some(false));
        let error = completes[0]["error"].as_str().expect("error text present");
        assert!(error.contains("panicked"), "{error}");

        assert!(
            !ctx.active_stacks.lock().unwrap().contains_key(&run_id),
            "handle removed"
        );

        let row = crate::db::stacking::get_run(&fixture.conn, run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "failed");
        assert!(row.error.as_deref().unwrap_or("").contains("panicked"));
    }

    // ── stage_calibrate ──────────────────────────────────────────────────

    #[test]
    fn calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, light_ids, working, output) = seed_ready(&db_path, SET_NAME);

        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();

        let build = |run_id: i64, groups: Vec<IntegrationGroup>| {
            test_context(
                ctx.clone(),
                Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
                run_id,
                fixture.set_id,
                SET_NAME,
                cfg.clone(),
                groups,
                layout.clone(),
                output_dir.clone(),
                HashMap::new(),
            )
        };

        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");
        let group_key = plan_groups[0].key.clone();

        // First pass: 3 calibrated files, 3 artifact rows.
        let mut rc1 = build(1, plan_groups.clone());
        stage_calibrate(&mut rc1).unwrap();

        let calibrated_dir = layout.calibrated_dir(&group_key);
        let files_on_disk = std::fs::read_dir(&calibrated_dir).unwrap().count();
        assert_eq!(files_on_disk, 3);

        let artifacts_after_1 =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("calibrated"))
                .unwrap();
        assert_eq!(artifacts_after_1.len(), 3);

        let snapshot = |artifacts: &[crate::db::stacking::StackingArtifactRow]| -> HashMap<i64, (std::time::SystemTime, String)> {
            artifacts
                .iter()
                .map(|a| {
                    let path = a.path.clone().unwrap();
                    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
                    (a.frame_id.unwrap(), (mtime, a.config_hash.clone()))
                })
                .collect()
        };
        let before = snapshot(&artifacts_after_1);

        // Second pass: 0 files rewritten (mtimes unchanged), rows unchanged.
        let mut rc2 = build(2, plan_groups.clone());
        stage_calibrate(&mut rc2).unwrap();
        let artifacts_after_2 =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("calibrated"))
                .unwrap();
        assert_eq!(artifacts_after_2.len(), 3);
        let after_reuse = snapshot(&artifacts_after_2);
        assert_eq!(
            before, after_reuse,
            "a fresh artifact must not be rewritten"
        );

        // Touch one source file's size in `files` — only that frame regenerates.
        fixture
            .conn
            .execute(
                "UPDATE files SET size = size + 1000 WHERE id = \
                 (SELECT file_id FROM frames WHERE id = ?1)",
                rusqlite::params![light_ids[0]],
            )
            .unwrap();
        let plan_groups_touched =
            group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let mut rc3 = build(3, plan_groups_touched);
        stage_calibrate(&mut rc3).unwrap();
        let artifacts_after_3 =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("calibrated"))
                .unwrap();
        let after_touch = snapshot(&artifacts_after_3);
        assert_ne!(
            after_touch[&light_ids[0]], before[&light_ids[0]],
            "the touched frame must regenerate"
        );
        assert_eq!(
            after_touch[&light_ids[1]], before[&light_ids[1]],
            "an untouched frame must not regenerate"
        );
        assert_eq!(
            after_touch[&light_ids[2]], before[&light_ids[2]],
            "an untouched frame must not regenerate"
        );

        // rerun_from Calibrate: every artifact is stale, all three regenerate.
        let plan_groups_rerun = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let mut rc4 = build(4, plan_groups_rerun);
        rc4.rerun_from = Some(Stage::Calibrate);
        stage_calibrate(&mut rc4).unwrap();
        let artifacts_after_4 =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("calibrated"))
                .unwrap();
        let after_rerun = snapshot(&artifacts_after_4);
        for &id in &light_ids {
            assert_ne!(
                after_rerun[&id], after_touch[&id],
                "frame {id} must regenerate under rerun_from Calibrate"
            );
        }
    }

    /// Fix round 1, Critical: two frames of one group sharing a source
    /// basename (a capture counter restarted on another night) must not
    /// collide on one calibrated output — every colliding frame gets
    /// `_f<frame_id>` inserted before the extension, and a frame whose name
    /// is unique in the group keeps the plain `c_<stem>.fits` name.
    #[test]
    fn calibrate_outputs_disambiguate_same_basename_within_a_group() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

        let fixture_conn = rusqlite::Connection::open(&db_path).unwrap();
        let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

        // Two lights sharing the SAME source basename ("dup.fits") — a
        // capture counter that restarted on a different night, so the two
        // physical files live in DIFFERENT directories (`files.path` is
        // UNIQUE, so they can't share a path even though they share a
        // name). `add_light` gives the first one a real file; the second
        // is a manual copy into a sibling directory with the same
        // filename, inserted the same way `add_light`'s own `insert_light_row`
        // would (this test's file scope is `run.rs` only — the fixture
        // builder itself is untouched).
        let (dup_a, path_a) =
            test_fixtures::add_light(&fixture, &light_spec("dup", THREE_TIMES[0]));

        let night2_dir = fixture.dir.path().join("night2");
        std::fs::create_dir_all(&night2_dir).unwrap();
        let path_b = night2_dir.join("dup.fits");
        std::fs::copy(&path_a, &path_b).unwrap();
        let meta_b = std::fs::metadata(&path_b).unwrap();
        let modified_b =
            chrono::DateTime::<chrono::Utc>::from(meta_b.modified().unwrap()).to_rfc3339();
        fixture
            .conn
            .execute(
                "INSERT INTO files (path, filename, size, modified_at, format) \
                 VALUES (?1, 'dup.fits', ?2, ?3, 'FITS')",
                rusqlite::params![path_b.to_string_lossy(), meta_b.len() as i64, modified_b],
            )
            .unwrap();
        let file_id_b = fixture.conn.last_insert_rowid();
        fixture
            .conn
            .execute(
                "INSERT INTO frames (file_id, instrume, filter, xbinning, naxis1, naxis2, \
                 exptime, date_obs, imagetyp) VALUES (?1, 'cam', NULL, 1, 64, 48, 60.0, ?2, 'Light')",
                rusqlite::params![file_id_b, THREE_TIMES[1]],
            )
            .unwrap();
        let dup_b = fixture.conn.last_insert_rowid();
        fixture
            .conn
            .execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![fixture.session_id, dup_b],
            )
            .unwrap();

        let (uniq, _) = test_fixtures::add_light(&fixture, &light_spec("uniq", THREE_TIMES[2]));
        let light_ids = vec![dup_a, dup_b, uniq];
        test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();

        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();

        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");
        let group_key = plan_groups[0].key.clone();

        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            1,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout.clone(),
            output_dir,
            HashMap::new(),
        );
        stage_calibrate(&mut rc).unwrap();

        let artifacts =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("calibrated"))
                .unwrap();
        assert_eq!(artifacts.len(), 3, "{artifacts:?}");

        let path_of = |frame_id: i64| -> String {
            artifacts
                .iter()
                .find(|a| a.frame_id == Some(frame_id))
                .unwrap_or_else(|| panic!("no artifact row for frame {frame_id}"))
                .path
                .clone()
                .unwrap()
        };
        let path_a = path_of(dup_a);
        let path_b = path_of(dup_b);
        let path_uniq = path_of(uniq);

        assert_ne!(
            path_a, path_b,
            "colliding frames must get two DISTINCT calibrated files"
        );
        assert!(
            path_a.contains(&format!("_f{dup_a}")),
            "colliding frame {dup_a} must carry the disambiguating suffix: {path_a}"
        );
        assert!(
            path_b.contains(&format!("_f{dup_b}")),
            "colliding frame {dup_b} must carry the disambiguating suffix: {path_b}"
        );
        assert!(
            !path_uniq.contains("_f"),
            "a frame with a unique source name keeps the plain name: {path_uniq}"
        );

        let calibrated_dir = layout.calibrated_dir(&group_key);
        let files_on_disk = std::fs::read_dir(&calibrated_dir).unwrap().count();
        assert_eq!(
            files_on_disk, 3,
            "three distinct files on disk — neither collision overwrote the other"
        );
    }

    // ── stages 3-5 (Task 7): measure & select, reference, register ─────────

    /// The star-field fixtures' canvas — large enough that the detector's
    /// own auto-threshold blob radius (empirically several px at these
    /// amplitudes/sigma) never touches a neighbouring star's blob at
    /// [`BASE_STARS`]' spacing (a 64x48 canvas at this SAME layout, tried
    /// first, made adjacent stars' supra-threshold regions overlap and
    /// merge into one blob, collapsing detection to a single point).
    const STAR_FIELD_WIDTH: usize = 192;
    const STAR_FIELD_HEIGHT: usize = 144;

    /// Ten stars spread across the [`STAR_FIELD_WIDTH`]x[`STAR_FIELD_HEIGHT`]
    /// field, margin generous enough that every shift used below (max
    /// magnitude 3 px) keeps every star well inside the frame. The SAME
    /// list, shifted by a per-frame `(dx, dy)` integer offset, is what makes
    /// registration find real inlier matches (well above the required ≥ 8)
    /// across every star-field fixture below.
    const BASE_STARS: &[(f64, f64, f64)] = &[
        (30.0, 24.0, 9000.0),
        (66.0, 30.0, 7000.0),
        (102.0, 27.0, 8500.0),
        (138.0, 33.0, 6500.0),
        (36.0, 60.0, 7500.0),
        (78.0, 66.0, 9000.0),
        (120.0, 63.0, 6000.0),
        (150.0, 72.0, 8000.0),
        (54.0, 102.0, 7000.0),
        (114.0, 99.0, 8500.0),
    ];

    fn shifted_stars(dx: f64, dy: f64) -> Vec<(f64, f64, f64)> {
        BASE_STARS
            .iter()
            .map(|&(x, y, a)| (x + dx, y + dy, a))
            .collect()
    }

    fn date_obs_at(i: usize) -> String {
        format!("2025-01-01T00:{:02}:00", (i as u32) * 5)
    }

    /// Seed `shifts.len()` star-field lights (`f0`, `f1`, …), the SAME
    /// [`BASE_STARS`] field shifted by each entry of `shifts` (known integer
    /// offsets), with `noise_sigmas[i]` raw-ADU Gaussian noise added to
    /// frame `i` — plus `stacking.working_dir`/`output_dir` settings.
    /// Frames share `instrume`/`filter`/binning/size, so `group_frames`
    /// puts them all in ONE group. Does NOT link a master dark/flat —
    /// callers call [`test_fixtures::add_master_dark_and_flat`] themselves,
    /// once, over every frame id they end up needing (including any extra
    /// frame added after this call, e.g. a low-weight or flat-field one).
    fn seed_star_group(
        db_path: &Path,
        set_name: &str,
        shifts: &[(f64, f64)],
        noise_sigmas: &[f32],
    ) -> (
        test_fixtures::Fixture,
        Vec<i64>,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        assert_eq!(shifts.len(), noise_sigmas.len());
        let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
        let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

        let mut light_ids = Vec::new();
        for (i, (&(dx, dy), &sigma)) in shifts.iter().zip(noise_sigmas.iter()).enumerate() {
            let stars = shifted_stars(dx, dy);
            let date_obs = date_obs_at(i);
            let stem = format!("f{i}");
            let spec = star_light_spec(&stem, &date_obs);
            let (id, _path) = test_fixtures::add_light_with_field(
                &fixture,
                &spec,
                &stars,
                600.0,
                sigma,
                100 + i as u64,
            );
            light_ids.push(id);
        }

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_OUTPUT_DIR,
            output.path().to_str().unwrap(),
        )
        .unwrap();

        (fixture, light_ids, working, output)
    }

    /// A real `stacking_runs` row plus one `stacking_run_groups` row per
    /// `plan_groups` entry, all FK-linked to that same run — everything
    /// [`test_context`]'s `group_ids` and stages 3/5's own DB writes need
    /// that [`seed_ready`]'s calibrate-only fixture never had to provide.
    fn seed_run_and_groups(
        conn: &rusqlite::Connection,
        frames_set_id: i64,
        plan_groups: &[IntegrationGroup],
        working: &Path,
        output: &Path,
    ) -> (i64, HashMap<String, i64>) {
        let run_id = insert_run(
            conn,
            &NewRun {
                frames_set_id,
                config_json: "{}",
                config_hash: "test-hash",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.to_str().unwrap(),
                output_dir: output.to_str().unwrap(),
            },
        )
        .unwrap();

        let mut group_ids = HashMap::new();
        for g in plan_groups {
            let (anchor_width, anchor_height) = group_anchor_geometry(g);
            let group_id = insert_group(
                conn,
                &NewGroup {
                    run_id,
                    group_key: &g.key,
                    instrume: g.instrume.as_deref(),
                    color_mode: color_mode_wire(g.color_mode),
                    filter: g.filter.as_deref(),
                    binning: Some(g.binning),
                    width: anchor_width,
                    height: anchor_height,
                    exposure: g.exposure_s,
                    frame_count: g.frames.len() as i64,
                    included_count: g.frames.len() as i64,
                },
            )
            .unwrap();
            group_ids.insert(g.key.clone(), group_id);
        }
        (run_id, group_ids)
    }

    /// Run stages 1 through `through` (inclusive) on `rc` — the harness the
    /// tests below drive instead of calling each stage function by hand.
    fn run_stages_for_test(rc: &mut RunContext, through: Stage) -> Result<(), RunError> {
        stage_calibrate(rc)?;
        if through == Stage::Calibrate {
            return Ok(());
        }
        stage_measure(rc)?;
        if through == Stage::Measure {
            return Ok(());
        }
        stage_reference(rc)?;
        if through == Stage::Reference {
            return Ok(());
        }
        stage_register(rc)?;
        Ok(())
    }

    #[test]
    fn registered_file_name_does_not_misdetect_a_source_name_ending_in_d() {
        // Fix round 1 addendum: a mono source light named "vega_d" must not
        // be mistaken for a debayered output just because its own stem
        // happens to end in "_d" — `debayered` is now an explicit fact the
        // caller passes (`MeasuredFrame::planes == 3`), never inferred from
        // the calibrated file's own name.
        assert_eq!(registered_file_name("vega_d", false), "r_vega_d.fits");
        assert_eq!(registered_file_name("vega_d", true), "r_vega_d_d.fits");
    }

    #[test]
    fn measure_reuses_metrics_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");

        let build = |run_id: i64, group_ids: HashMap<String, i64>| {
            test_context(
                ctx.clone(),
                Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
                run_id,
                fixture.set_id,
                SET_NAME,
                cfg.clone(),
                plan_groups.clone(),
                layout.clone(),
                output_dir.clone(),
                group_ids,
            )
        };

        let (run_id1, group_ids1) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc1 = build(run_id1, group_ids1);
        run_stages_for_test(&mut rc1, Stage::Measure).unwrap();

        let metrics1 =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("metrics"))
                .unwrap();
        assert_eq!(metrics1.len(), 4, "{metrics1:?}");
        let before: HashMap<i64, String> = metrics1
            .iter()
            .map(|a| (a.frame_id.unwrap(), a.created_at.clone()))
            .collect();

        let (run_id2, group_ids2) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc2 = build(run_id2, group_ids2);
        run_stages_for_test(&mut rc2, Stage::Measure).unwrap();

        let metrics2 =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("metrics"))
                .unwrap();
        assert_eq!(metrics2.len(), 4, "{metrics2:?}");
        let after: HashMap<i64, String> = metrics2
            .iter()
            .map(|a| (a.frame_id.unwrap(), a.created_at.clone()))
            .collect();
        assert_eq!(
            before, after,
            "a fresh metrics artifact must not be recreated"
        );
    }

    #[test]
    fn selection_excludes_manual_and_low_weight() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);

        let noise_date = date_obs_at(4);
        let noise_spec = star_light_spec("noise", &noise_date);
        let (noise_id, _path) =
            test_fixtures::add_light_with_field(&fixture, &noise_spec, &[], 600.0, 30.0, 999);

        let mut all_ids = light_ids.clone();
        all_ids.push(noise_id);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &all_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );
        rc.excluded = vec![light_ids[1]];

        run_stages_for_test(&mut rc, Stage::Measure).unwrap();

        let group_key = rc.plan_groups[0].key.clone();
        let entries = rc.measured.get(&group_key).expect("group measured");
        let entry_of = |id: i64| entries.iter().find(|e| e.frame.frame_id == id).unwrap();

        assert!(
            !entry_of(light_ids[1]).included,
            "manually excluded frame must not be included"
        );
        assert_eq!(
            entry_of(light_ids[1]).reason.as_deref(),
            Some("excluded manually")
        );

        assert!(
            !entry_of(noise_id).included,
            "pure-noise frame must be excluded by weight"
        );
        let noise_reason = entry_of(noise_id).reason.clone().unwrap_or_default();
        assert!(noise_reason.contains("weight"), "{noise_reason}");

        for &id in &[light_ids[0], light_ids[2], light_ids[3]] {
            assert!(entry_of(id).included, "frame {id} should remain included");
        }
    }

    #[test]
    fn auto_reference_is_the_best_frame_of_the_largest_group() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        // f0 is the cleanest frame (noticeably, but not by a factor big
        // enough to push the others below the 5% weight floor) — it must
        // win the auto reference pick.
        let noise = [3.0f32, 5.0, 5.0, 5.0];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Reference).unwrap();

        assert_eq!(rc.reference_frame_id, Some(light_ids[0]));
        assert!(rc.reference_calibrated.is_some());

        let row = crate::db::stacking::get_run(&fixture.conn, run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.reference_frame_id, Some(light_ids[0]));
        assert_eq!(row.reference_mode, "auto");
    }

    #[test]
    fn registration_rows_are_written_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();

        let run_pass = |cfg: StackingConfig| -> RunContext {
            let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
            let (run_id, group_ids) = seed_run_and_groups(
                &fixture.conn,
                fixture.set_id,
                &plan_groups,
                working.path(),
                output.path(),
            );
            let mut rc = test_context(
                ctx.clone(),
                Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
                run_id,
                fixture.set_id,
                SET_NAME,
                cfg,
                plan_groups,
                layout.clone(),
                output_dir.clone(),
                group_ids,
            );
            run_stages_for_test(&mut rc, Stage::Register).unwrap();
            rc
        };

        let rc1 = run_pass(StackingConfig::default());
        let rows1 = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
        assert_eq!(rows1.len(), 4, "{rows1:?}");
        assert_eq!(rows1.iter().filter(|r| r.is_reference).count(), 1);
        let reference_id = rc1.reference_frame_id.unwrap();
        assert!(rows1
            .iter()
            .any(|r| r.frame_id == reference_id && r.is_reference));

        // `stacking_run_frames` rows: one per frame of the run's group, all
        // four included and aligned (one of them the reference).
        let frame_rows = crate::db::stacking::list_frame_rows(&fixture.conn, rc1.run_id).unwrap();
        assert_eq!(frame_rows.len(), 4, "{frame_rows:?}");
        assert!(frame_rows.iter().all(|r| r.included));
        assert!(frame_rows.iter().all(|r| r.metrics_json.is_some()));
        assert!(frame_rows
            .iter()
            .all(|r| matches!(r.reg_status.as_deref(), Some("aligned" | "reference"))));
        assert_eq!(
            frame_rows
                .iter()
                .filter(|r| r.reg_status.as_deref() == Some("reference"))
                .count(),
            1
        );
        let before_at: HashMap<i64, String> = rows1
            .iter()
            .map(|r| (r.frame_id, r.registered_at.clone()))
            .collect();
        let before_hash: HashMap<i64, Option<String>> = rows1
            .iter()
            .map(|r| (r.frame_id, r.config_hash.clone()))
            .collect();

        // Second pass, same config: every row is reused verbatim.
        let rc2 = run_pass(StackingConfig::default());
        assert_eq!(rc2.reference_frame_id, rc1.reference_frame_id);
        let rows2 = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
        let after_at: HashMap<i64, String> = rows2
            .iter()
            .map(|r| (r.frame_id, r.registered_at.clone()))
            .collect();
        assert_eq!(
            before_at, after_at,
            "a fresh registration row must not be rewritten"
        );

        // Every frame's OWN registration outcome on the second pass is a
        // reuse (`cached: true`) — not merely a DB row that happens to
        // look the same, but a run that provably never called
        // `register_frame`/`identity_registration` again.
        for entries in rc2.measured.values() {
            for entry in entries {
                match &entry.registration {
                    Some(RegisteredFrameOutcome::Aligned { cached, .. }) => {
                        assert!(
                            *cached,
                            "frame {} was re-registered on the second pass",
                            entry.frame.frame_id
                        );
                    }
                    Some(RegisteredFrameOutcome::Failed(msg)) => {
                        panic!("frame {} failed registration: {msg}", entry.frame.frame_id);
                    }
                    None => {
                        panic!("frame {} has no registration outcome", entry.frame.frame_id);
                    }
                }
            }
        }

        // Third pass, `max_stars` changed: the registration config hash
        // changes for every frame, so every row must re-register (asserted
        // via the hash, not `registered_at` — a fast, all-in-memory test
        // pass can legitimately land in the same millisecond twice).
        let mut cfg3 = StackingConfig::default();
        cfg3.registration.max_stars = 50;
        let _rc3 = run_pass(cfg3);
        let rows3 = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
        let after_hash: HashMap<i64, Option<String>> = rows3
            .iter()
            .map(|r| (r.frame_id, r.config_hash.clone()))
            .collect();
        for (id, hash) in &before_hash {
            assert_ne!(
                &after_hash[id], hash,
                "frame {id} must re-register after cfg.registration.max_stars changes"
            );
        }
    }

    #[test]
    fn registration_failure_excludes_by_default_and_fails_when_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0)];
        let noise = [4.0f32; 3];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);

        // A flat-field frame: constant background + noise, no stars — the
        // registration detector finds nothing on it, so RANSAC refuses it
        // with too few inliers.
        let flat_date = date_obs_at(3);
        let flat_spec = star_light_spec("flat", &flat_date);
        let (flat_id, _path) =
            test_fixtures::add_light_with_field(&fixture, &flat_spec, &[], 600.0, 4.0, 777);

        let mut all_ids = light_ids.clone();
        all_ids.push(flat_id);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &all_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();

        // `WeightMode::None` gives every frame the SAME weight, so the
        // starless flat frame is not excluded by stage 3's own weight
        // filter before ever reaching registration — isolating THIS test's
        // signal to the registration-failure path alone.
        let mut base_cfg = StackingConfig::default();
        base_cfg.measurement.weight_mode = WeightMode::None;

        let run_pass = |cfg: StackingConfig| -> (RunContext, Result<(), RunError>) {
            let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
            let (run_id, group_ids) = seed_run_and_groups(
                &fixture.conn,
                fixture.set_id,
                &plan_groups,
                working.path(),
                output.path(),
            );
            let mut rc = test_context(
                ctx.clone(),
                Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
                run_id,
                fixture.set_id,
                SET_NAME,
                cfg,
                plan_groups,
                layout.clone(),
                output_dir.clone(),
                group_ids,
            );
            let result = run_stages_for_test(&mut rc, Stage::Register);
            (rc, result)
        };

        // Pass A: default `exclude_on_registration_failure = true` — the
        // flat frame is excluded, the run otherwise succeeds.
        let (rc_a, result_a) = run_pass(base_cfg.clone());
        result_a.expect("run must succeed when the failing frame is simply excluded");
        let group_key = rc_a.plan_groups[0].key.clone();
        let entries = rc_a.measured.get(&group_key).unwrap();
        let flat_entry = entries
            .iter()
            .find(|e| e.frame.frame_id == flat_id)
            .unwrap();
        assert!(!flat_entry.included, "flat-field frame must be excluded");
        let reason = flat_entry.reason.clone().unwrap_or_default();
        assert!(reason.contains("registration failed"), "{reason}");
        for &id in &light_ids {
            assert!(
                entries
                    .iter()
                    .find(|e| e.frame.frame_id == id)
                    .unwrap()
                    .included,
                "frame {id} should remain included"
            );
        }

        // Pass B: `exclude_on_registration_failure = false` — the same
        // failure now fails the whole run.
        let mut cfg_b = base_cfg;
        cfg_b.selection.exclude_on_registration_failure = false;
        let (_rc_b, result_b) = run_pass(cfg_b);
        let err = result_b.expect_err("run must fail when registration failures are not excluded");
        match err {
            RunError::Other(msg) => assert!(msg.contains("registration failed"), "{msg}"),
            other => panic!("expected RunError::Other, got {other:?}"),
        }
    }

    #[test]
    fn manual_reference_excluded_by_selection_is_forced_included() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);

        // A 5th, pure-noise frame — selection excludes it by weight. It is
        // then chosen as the MANUAL reference: the run must keep it
        // included regardless (fix round 1, item 1).
        let noise_date = date_obs_at(4);
        let noise_spec = star_light_spec("noise", &noise_date);
        let (noise_id, _path) =
            test_fixtures::add_light_with_field(&fixture, &noise_spec, &[], 600.0, 30.0, 999);

        let mut all_ids = light_ids.clone();
        all_ids.push(noise_id);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &all_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        crate::registration::db::set_frame_set_reference(&fixture.conn, fixture.set_id, noise_id)
            .unwrap();

        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Register).unwrap();

        assert_eq!(rc.reference_frame_id, Some(noise_id));

        let rows = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
        let noise_row = rows
            .iter()
            .find(|r| r.frame_id == noise_id)
            .expect("the manual reference has a registration_results row");
        assert!(noise_row.is_reference);
        assert_eq!(noise_row.status, "reference");

        let frame_rows = crate::db::stacking::list_frame_rows(&fixture.conn, rc.run_id).unwrap();
        let noise_frame_row = frame_rows
            .iter()
            .find(|r| r.frame_id == noise_id)
            .expect("the manual reference has a stacking_run_frames row");
        assert!(noise_frame_row.included, "{noise_frame_row:?}");
        assert_eq!(noise_frame_row.reg_status.as_deref(), Some("reference"));

        assert_eq!(
            rc.warnings
                .iter()
                .filter(|w| w.contains("kept despite selection"))
                .count(),
            1,
            "{:?}",
            rc.warnings
        );
    }

    #[test]
    fn manual_reference_in_the_exclusion_list_fails_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        crate::registration::db::set_frame_set_reference(
            &fixture.conn,
            fixture.set_id,
            light_ids[0],
        )
        .unwrap();

        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );
        // The reference frame is ALSO in the run's own manual exclusion
        // list — the plan gate cannot see this (it only checks the raw
        // file is on disk), so the run itself must refuse loudly.
        rc.excluded = vec![light_ids[0]];

        let err = run_stages_for_test(&mut rc, Stage::Register)
            .expect_err("a manually-excluded reference frame must fail the run");
        match err {
            RunError::Other(msg) => {
                assert!(
                    msg.contains("the manual reference frame is excluded"),
                    "{msg}"
                );
            }
            other => panic!("expected RunError::Other, got {other:?}"),
        }
    }

    // ── stages 6/7/9 (Task 8): normalize, integrate, output ────────────────

    /// Every canonical stage in pipeline order, used to check the SET of
    /// stages a full run's progress events cover, in the order they first
    /// appear.
    const CANONICAL_STAGE_ORDER: [&str; 7] = [
        "calibrate",
        "measure",
        "reference",
        "register",
        "normalize",
        "integrate",
        "output",
    ];

    /// The distinct `stage` values of `events`, in first-appearance order —
    /// used to check a run's progress events cover the canonical stage
    /// sequence, and that `percent` is monotonic WITHIN each `(stage,
    /// groupKey)` pair's own consecutive run of events (fix round 1, item 7:
    /// NOT keyed on `stage` alone — `Normalize`/`Integrate`/`Output` each
    /// reset to 0% per group, so a multi-group run legitimately sees percent
    /// "go backwards" moving from one group's 100% to the next group's 0%
    /// within the same stage; a per-frame stage like `register` carries a
    /// `groupKey` too but its `current`/`total` are cumulative across every
    /// group, so keying on the pair is still correct there — it just never
    /// changes group_key mid-climb).
    fn stage_sequence_and_monotonic(events: &[serde_json::Value]) -> Vec<String> {
        let mut order: Vec<String> = Vec::new();
        let mut last_percent: HashMap<(String, String), f64> = HashMap::new();
        for e in events {
            let stage = e["stage"].as_str().unwrap_or("").to_string();
            if !order.contains(&stage) {
                order.push(stage.clone());
            }
            let group_key = e["groupKey"].as_str().unwrap_or("").to_string();
            let percent = e["percent"].as_f64().unwrap_or(0.0);
            let key = (stage.clone(), group_key.clone());
            if let Some(&prev) = last_percent.get(&key) {
                assert!(
                    percent + 1e-9 >= prev,
                    "percent went backwards for stage {stage} group {group_key}: {prev} -> {percent}"
                );
            }
            last_percent.insert(key, percent);
        }
        order
    }

    #[test]
    fn emit_integrate_ticks_never_race_percent_backwards() {
        // Fix round 1, item 1 + fix round 2: drives the REAL
        // `emit_integrate_tick` (not a reimplementation) from two genuine
        // `std::thread::scope` threads, both hitting the SAME
        // `IntegrateTickState` mutex, under FULLY DETERMINISTIC turn-taking
        // — kept alongside `emit_integrate_ticks_never_go_backwards_under_free_racing`
        // below because it proves something that one cannot: with no
        // legitimate reason for ANY tick to be stale (every value is
        // offered to the function in strict, correct order), the max-latch
        // must never drop a genuinely-valid tick — `events.len() == TOTAL`
        // below is exactly that "zero false positives" check. The free-race
        // test proves the opposite direction (stale ticks under genuine
        // racing are dropped, never delivered backwards) but, precisely
        // because it allows drops, cannot also prove the latch isn't
        // OVER-eager.
        //
        // Fix round 1's own history, preserved: a version of the free-race
        // test with no coordination beyond the mutex was flaky against the
        // fix-round-1-only code (~1 in 10 runs) — a value read outside the
        // lock can sit unsubmitted for an unbounded time regardless of lock
        // discipline inside this function, so a later, higher tick could
        // reach the recorder before an earlier, lower one. That is now
        // fully addressed by fix round 2's max-latch (the stale tick is
        // dropped, not delivered out of order) — see the free-race test
        // below, which fix round 1 could not have passed but fix round 2
        // does, reliably.
        //
        // This test's own determinism comes from a shared `turn` counter
        // that hands each value to the thread whose parity matches it,
        // advancing only AFTER that thread's own `emit_integrate_tick` call
        // has fully returned (a `compare_exchange`-before-call variant was
        // tried and failed the same way the free-race version does, for the
        // same reason — releasing the next turn before this call's own
        // critical section is done lets the two threads' calls run
        // concurrently again).
        struct ThreadPercentRecorder {
            events: Mutex<Vec<f64>>,
        }
        impl ProgressEmitter for ThreadPercentRecorder {
            fn emit_json(&self, _event_name: &str, payload: serde_json::Value) {
                self.events
                    .lock()
                    .unwrap()
                    .push(payload["percent"].as_f64().unwrap_or(f64::NAN));
            }
        }

        let tick_state: Mutex<IntegrateTickState> = Mutex::new(IntegrateTickState::new());
        let recorder = ThreadPercentRecorder {
            events: Mutex::new(Vec::new()),
        };
        let turn = std::sync::atomic::AtomicU64::new(0);
        const TOTAL: u64 = 400;
        const CHANNELS: usize = 1;

        let worker = |thread_id: u64| {
            loop {
                let v = turn.load(Ordering::SeqCst);
                if v >= TOTAL {
                    return;
                }
                if v % 2 != thread_id {
                    std::hint::spin_loop();
                    continue;
                }
                emit_integrate_tick(
                    &tick_state,
                    &recorder,
                    1,
                    1,
                    "g",
                    CHANNELS,
                    0,
                    v as f64 / TOTAL as f64,
                    v,
                    TOTAL,
                    true, // force: every value must reach the recorder, or
                          // a throttle-skipped tick could hide a real
                          // ordering violation.
                );
                turn.store(v + 1, Ordering::SeqCst);
            }
        };

        std::thread::scope(|scope| {
            scope.spawn(|| worker(0));
            scope.spawn(|| worker(1));
        });

        let events = recorder.events.into_inner().unwrap();
        assert_eq!(
            events.len(),
            TOTAL as usize,
            "the latch must never drop a genuinely valid, in-order tick: {events:?}"
        );
        let mut last = -1.0f64;
        for &percent in &events {
            assert!(
                percent + 1e-9 >= last,
                "percent went backwards: {last} -> {percent}"
            );
            last = percent;
        }
    }

    #[test]
    fn emit_integrate_ticks_never_go_backwards_under_free_racing() {
        // Fix round 2, brief's own test ask: TWO THREADS RACE FREELY — no
        // external turn-taking, genuine concurrent calls — each pulling its
        // next `(plane, frac)` from a shared, monotonically increasing
        // counter immediately before calling the REAL `emit_integrate_tick`.
        // This is the exact scenario that was flaky before fix round 2 (see
        // `emit_integrate_ticks_never_race_percent_backwards`'s own doc
        // comment) — a value fetched early by one thread could sit
        // unsubmitted while the other thread ran far ahead, and once
        // finally submitted, land AFTER a much higher percent already on
        // the wire. The max-latch fixes this not by reordering (impossible
        // without a queue) but by DROPPING the stale tick outright: the
        // recorded sequence can be a strict SUBSET of everything fetched,
        // but whatever subset does get through must never go backwards.
        // Run 20 times (fresh state each iteration) per the brief, each
        // iteration racing >= 200 values across the two threads.
        struct ThreadPercentRecorder {
            events: Mutex<Vec<f64>>,
        }
        impl ProgressEmitter for ThreadPercentRecorder {
            fn emit_json(&self, _event_name: &str, payload: serde_json::Value) {
                self.events
                    .lock()
                    .unwrap()
                    .push(payload["percent"].as_f64().unwrap_or(f64::NAN));
            }
        }

        const ITERATIONS: usize = 20;
        const TOTAL: u64 = 250;
        const CHANNELS: usize = 1;

        for iteration in 0..ITERATIONS {
            let tick_state: Mutex<IntegrateTickState> = Mutex::new(IntegrateTickState::new());
            let recorder = ThreadPercentRecorder {
                events: Mutex::new(Vec::new()),
            };
            let counter = std::sync::atomic::AtomicU64::new(0);

            let worker = || loop {
                let v = counter.fetch_add(1, Ordering::SeqCst);
                if v >= TOTAL {
                    break;
                }
                emit_integrate_tick(
                    &tick_state,
                    &recorder,
                    1,
                    1,
                    "g",
                    CHANNELS,
                    0,
                    v as f64 / TOTAL as f64,
                    v,
                    TOTAL,
                    true, // force: bypass the throttle so every fetched
                          // value reaches the latch — the latch, not the
                          // throttle, is what this test exercises.
                );
            };

            std::thread::scope(|scope| {
                scope.spawn(worker);
                scope.spawn(worker);
            });

            let events = recorder.events.into_inner().unwrap();
            assert!(
                !events.is_empty(),
                "iteration {iteration}: no ticks recorded at all"
            );
            let mut last = -1.0f64;
            for &percent in &events {
                assert!(
                    percent + 1e-9 >= last,
                    "iteration {iteration}: percent went backwards: {last} -> {percent}"
                );
                last = percent;
            }
        }
    }

    #[test]
    fn full_run_writes_a_master_with_provenance() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let recorder = Arc::new(Recording::new());
        let started = start_stacking(
            ctx.clone(),
            recorder.clone(),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("start should succeed");

        wait_for_run(&ctx, started.run_id);

        let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(run_row.status, "done", "{run_row:?}");

        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        assert_eq!(groups.len(), 1, "{groups:?}");
        let group = &groups[0];
        assert_eq!(group.status, "done", "{group:?}");
        let master_path = group.master_path.clone().expect("master path recorded");
        assert!(
            Path::new(&master_path).exists(),
            "master file missing: {master_path}"
        );

        let stats: GroupStats =
            serde_json::from_str(group.stats_json.as_deref().expect("stats_json set")).unwrap();
        assert_eq!(stats.frames, 4, "{stats:?}");

        let header = FitsHeader::from_path(Path::new(&master_path)).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Light"));
        assert_eq!(header.get_i32("NCOMBINE"), Some(4));
        assert_eq!(
            header.get_str("ATH_STKI").as_deref(),
            Some(started.run_id.to_string().as_str())
        );
        assert_eq!(
            header.get_str("ATH_STKG").as_deref(),
            Some(group.group_key.as_str())
        );
        // Fix round 1, item 3: `ATH_STKF` names the SOURCE frame (the
        // catalog `files.filename` stem, "f<i>" for `seed_star_group`'s
        // fixture lights) — never the calibrated artifact's own on-disk
        // name (which would need `c_`/`_d` stripping the source name never
        // does).
        let reference_frame_id = run_row
            .reference_frame_id
            .expect("reference frame recorded");
        let reference_index = light_ids
            .iter()
            .position(|&id| id == reference_frame_id)
            .expect("reference frame is one of the fixture's own lights");
        let expected_stem = format!("f{reference_index}");
        assert_eq!(
            header.get_str("ATH_STKF").as_deref(),
            Some(expected_stem.as_str()),
            "ATH_STKF must be the fixture light's own stem"
        );

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let run_json_path = layout.run_json(started.run_id);
        let run_json_text = std::fs::read_to_string(&run_json_path).unwrap();
        let _summary: RunSummary =
            serde_json::from_str(&run_json_text).expect("runs/run-<id>.json parses");
        let summary_json = run_row.summary_json.clone().expect("summary_json stored");
        assert_eq!(
            run_json_text, summary_json,
            "runs/run-<id>.json and the DB's summary_json must be the SAME document"
        );

        let events = recorder.events(STACKING_PROGRESS_EVENT);
        let order = stage_sequence_and_monotonic(&events);
        assert_eq!(order, CANONICAL_STAGE_ORDER, "{order:?}");

        let completes = recorder.events(STACKING_COMPLETE_EVENT);
        assert_eq!(completes.len(), 1, "{completes:?}");
        assert_eq!(completes[0]["success"].as_bool(), Some(true));
        let masters = completes[0]["masters"].as_array().unwrap();
        assert_eq!(masters.len(), 1, "{masters:?}");
        assert_eq!(
            masters[0]["groupKey"].as_str(),
            Some(group.group_key.as_str())
        );
    }

    // ── Stage 0.5: the run builds/rebuilds its own masters ────────────────

    /// Owner requirement 2026-09-09 ("the pipeline should build the
    /// calibration masters itself when they are missing"): the Task 6/7
    /// fixture's TWO master files deleted from disk — provenance rows and
    /// their real raw source sub-frames intact, via `add_master_dark_and_flat`'s
    /// real `register_master` registration (Task 8) — `start_stacking`
    /// rebuilds both inside stage 0.5, BEFORE calibrate, and the run still
    /// finishes `done`.
    #[test]
    fn deleted_masters_are_rebuilt_by_stage_masters() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, _working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        let (dark_set, flat_set) = test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let master_path = |set_id: i64| -> String {
            fixture
                .conn
                .query_row(
                    "SELECT fi.path FROM calibration_set_frames csf
                     JOIN frames fr ON fr.id = csf.frame_id
                     JOIN files fi ON fi.id = fr.file_id
                     WHERE csf.set_id = ?1",
                    [set_id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let dark_path = master_path(dark_set);
        let flat_path = master_path(flat_set);
        // Before: both master files exist (the fixture just wrote them).
        assert!(
            Path::new(&dark_path).exists(),
            "fixture sanity: dark master written"
        );
        assert!(
            Path::new(&flat_path).exists(),
            "fixture sanity: flat master written"
        );
        std::fs::remove_file(&dark_path).unwrap();
        std::fs::remove_file(&flat_path).unwrap();
        assert!(!Path::new(&dark_path).exists());
        assert!(!Path::new(&flat_path).exists());

        let recorder = Arc::new(Recording::new());
        let started = start_stacking(
            ctx.clone(),
            recorder.clone(),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("start should succeed");

        wait_for_run(&ctx, started.run_id);

        let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(run_row.status, "done", "{run_row:?}");

        // After: both masters rebuilt AT THEIR ORIGINAL LIBRARY PATHS.
        assert!(
            Path::new(&dark_path).exists(),
            "dark master must be rebuilt at its original path"
        );
        assert!(
            Path::new(&flat_path).exists(),
            "flat master must be rebuilt at its original path"
        );

        // Progress: stage `masters` reports 2/2 before `calibrate` ever ticks.
        let events = recorder.events(STACKING_PROGRESS_EVENT);
        let masters_indices: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| e["stage"].as_str() == Some("masters"))
            .map(|(i, _)| i)
            .collect();
        assert!(!masters_indices.is_empty(), "no masters progress events");
        let last_masters = &events[*masters_indices.last().unwrap()];
        assert_eq!(
            last_masters["current"].as_u64(),
            Some(2),
            "{last_masters:?}"
        );
        assert_eq!(last_masters["total"].as_u64(), Some(2), "{last_masters:?}");
        let first_calibrate_index = events
            .iter()
            .position(|e| e["stage"].as_str() == Some("calibrate"))
            .expect("a calibrate progress event exists");
        assert!(
            *masters_indices.last().unwrap() < first_calibrate_index,
            "masters progress must finish before calibrate starts: masters at {masters_indices:?}, calibrate first at {first_calibrate_index}"
        );

        // The summary lists two masters_built, one per rebuilt master.
        let summary: RunSummary = serde_json::from_str(
            run_row
                .summary_json
                .as_deref()
                .expect("summary_json stored"),
        )
        .unwrap();
        assert_eq!(
            summary.masters_built.len(),
            2,
            "{:?}",
            summary.masters_built
        );
        let rebuilt_master_set_ids: std::collections::HashSet<i64> = summary
            .masters_built
            .iter()
            .map(|m| m.master_set_id)
            .collect();
        assert_eq!(
            rebuilt_master_set_ids,
            std::collections::HashSet::from([dark_set, flat_set]),
            "{:?}",
            summary.masters_built
        );
        for m in &summary.masters_built {
            assert_eq!(m.kind, MasterWork::Rebuild, "{m:?}");
            assert!(Path::new(&m.path).exists(), "{m:?}");
        }
    }

    #[test]
    fn rerun_reuses_everything_and_only_integrates() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, _working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let started1 = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("first run should succeed");
        wait_for_run(&ctx, started1.run_id);
        let row1 = crate::db::stacking::get_run(&fixture.conn, started1.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row1.status, "done", "{row1:?}");
        let groups1 = crate::db::stacking::list_groups(&fixture.conn, started1.run_id).unwrap();
        let master_path1 = groups1[0].master_path.clone().expect("first master path");

        let started2 = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("second run should succeed");
        wait_for_run(&ctx, started2.run_id);
        let row2 = crate::db::stacking::get_run(&fixture.conn, started2.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row2.status, "done", "{row2:?}");

        let summary2: RunSummary =
            serde_json::from_str(row2.summary_json.as_deref().unwrap()).unwrap();
        let mut any_frame_checked = false;
        for group in &summary2.groups {
            for f in &group.frames {
                if f.included {
                    any_frame_checked = true;
                    assert!(f.cached_calibrated, "{f:?}");
                    assert!(f.cached_metrics, "{f:?}");
                    assert!(f.cached_registration, "{f:?}");
                }
            }
        }
        assert!(
            any_frame_checked,
            "no included frame in the second run's summary"
        );

        let groups2 = crate::db::stacking::list_groups(&fixture.conn, started2.run_id).unwrap();
        let master_path2 = groups2[0].master_path.clone().expect("second master path");
        assert_ne!(
            master_path1, master_path2,
            "a rerun must never overwrite the first master"
        );
        assert!(
            master_path2.ends_with("_2.fits"),
            "expected a collision-suffixed name: {master_path2}"
        );
        assert!(Path::new(&master_path1).exists());
        assert!(Path::new(&master_path2).exists());
    }

    #[test]
    fn cancel_mid_run_keeps_artifacts_writes_no_master() {
        /// Cancels every active stacking run for `frames_set_id` the moment
        /// the FIRST `measure`-stage progress event fires — no need to know
        /// the run id ahead of time (which `start_stacking` has not
        /// returned yet at the point the run thread's own first events can
        /// already be firing): a stacking run's cancel flag lives on its
        /// `StackHandle` in `ctx.active_stacks`, keyed by `frames_set_id`
        /// there too.
        struct CancelOnFirstMeasure {
            ctx: Arc<ServiceContext>,
            frames_set_id: i64,
            fired: std::sync::atomic::AtomicBool,
            recording: Recording,
        }
        impl ProgressEmitter for CancelOnFirstMeasure {
            fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
                self.recording.emit_json(event_name, payload.clone());
                if event_name == STACKING_PROGRESS_EVENT
                    && payload["stage"] == "measure"
                    && !self.fired.swap(true, Ordering::SeqCst)
                {
                    let active = self.ctx.active_stacks.lock().unwrap();
                    for h in active.values() {
                        if h.frames_set_id == self.frames_set_id {
                            h.cancel_flag.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let emitter = Arc::new(CancelOnFirstMeasure {
            ctx: ctx.clone(),
            frames_set_id: fixture.set_id,
            fired: std::sync::atomic::AtomicBool::new(false),
            recording: Recording::new(),
        });

        let started = start_stacking(
            ctx.clone(),
            emitter.clone(),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("start should succeed");

        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "cancelled", "{row:?}");

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        assert!(
            crate::api::sync::dir_size_bytes(&layout.calibrated_root()) > 0,
            "calibrated artifacts from stage 1 must survive a cancel"
        );

        let output_entries: Vec<_> = std::fs::read_dir(output.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            output_entries.is_empty(),
            "a cancelled run must write no master: {output_entries:?}"
        );

        let completes = emitter.recording.events(STACKING_COMPLETE_EVENT);
        assert_eq!(completes.len(), 1, "{completes:?}");
        assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
        assert_eq!(completes[0]["success"].as_bool(), Some(false));
        assert_eq!(completes[0]["masters"].as_array().map(|v| v.len()), Some(0));
    }

    #[test]
    fn delete_intermediates_cleans_the_working_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;
        // Fix round 1, item 8: without this, `registered_root()` never has
        // any content to begin with (`write_registered_frames` defaults
        // off), so the assertion that cleanup emptied it would pass
        // trivially even if `CleanupWhat::Intermediates` never touched that
        // subtree at all.
        cfg.registration.write_registered_frames = true;

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        assert_eq!(
            crate::api::sync::dir_size_bytes(&layout.calibrated_root()),
            0,
            "calibrated frames must be gone after DeleteIntermediates"
        );
        assert_eq!(
            crate::api::sync::dir_size_bytes(&layout.registered_root()),
            0,
            "registered frames must be gone after DeleteIntermediates"
        );

        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        let master_path = groups[0].master_path.clone().expect("master path");
        assert!(
            Path::new(&master_path).exists(),
            "cleanup must never touch the output folder"
        );
        let _ = output;
    }

    #[test]
    fn maps_written_when_requested() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, _working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.integration.write_rejection_maps = true;

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");

        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        let group = &groups[0];
        let low = group.rejection_low_path.clone().expect("_rejlow path set");
        let high = group
            .rejection_high_path
            .clone()
            .expect("_rejhigh path set");
        assert!(low.ends_with("_rejlow.fits"), "{low}");
        assert!(high.ends_with("_rejhigh.fits"), "{high}");
        assert!(Path::new(&low).exists(), "{low}");
        assert!(Path::new(&high).exists(), "{high}");
    }

    // ── Stage 6 (Task 5): local normalization ───────────────────────────

    /// Dense enough (24 stars, comfortably over `ln::scale::MIN_MATCHES`'s 20)
    /// for `relative_scale` to succeed — [`BASE_STARS`]' own 10-star field is
    /// too sparse for local normalization's own tests, which need a match
    /// count, not just a detection count. Same canvas
    /// ([`STAR_FIELD_WIDTH`]x[`STAR_FIELD_HEIGHT`]) and spacing philosophy as
    /// [`BASE_STARS`] (comfortably wider than the detector's own blob radius
    /// at this sigma/amplitude); jittered off a 6x4 grid (`SplitMix64`, same
    /// technique `ln::scale`'s own tests use) rather than a perfectly regular
    /// one — a plain grid's repeated distances/angles are exactly what makes
    /// registration's quad-based star matching ambiguous ("quad seed
    /// failed" with thousands of candidate quads, observed empirically on
    /// the first, unjittered version of this fixture).
    fn ln_base_stars() -> Vec<(f64, f64, f64)> {
        let mut rng = crate::geometry::ransac::SplitMix64(7);
        let mut stars = Vec::with_capacity(24);
        for row in 0..4 {
            for col in 0..6 {
                let x = 16.0 + col as f64 * 32.0 + (rng.next_f64() - 0.5) * 10.0;
                let y = 18.0 + row as f64 * 36.0 + (rng.next_f64() - 0.5) * 10.0;
                let amp = if (row + col) % 2 == 0 { 9000.0 } else { 6500.0 };
                stars.push((x, y, amp));
            }
        }
        stars
    }

    fn ln_shifted_stars(dx: f64, dy: f64) -> Vec<(f64, f64, f64)> {
        ln_base_stars()
            .iter()
            .map(|&(x, y, a)| (x + dx, y + dy, a))
            .collect()
    }

    /// Same shape as [`seed_star_group`], but [`ln_base_stars`]' denser field
    /// instead of [`BASE_STARS`] — Task 5's local-normalization tests need
    /// `relative_scale` to actually succeed (≥ `ln::scale::MIN_MATCHES`
    /// matched star pairs), not just a viable group.
    fn seed_ln_star_group(
        db_path: &Path,
        set_name: &str,
        shifts: &[(f64, f64)],
        noise_sigmas: &[f32],
    ) -> (
        test_fixtures::Fixture,
        Vec<i64>,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        assert_eq!(shifts.len(), noise_sigmas.len());
        let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
        let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

        let mut light_ids = Vec::new();
        for (i, (&(dx, dy), &sigma)) in shifts.iter().zip(noise_sigmas.iter()).enumerate() {
            let stars = ln_shifted_stars(dx, dy);
            let date_obs = date_obs_at(i);
            let stem = format!("f{i}");
            let spec = star_light_spec(&stem, &date_obs);
            let (id, _path) = test_fixtures::add_light_with_field(
                &fixture,
                &spec,
                &stars,
                600.0,
                sigma,
                100 + i as u64,
            );
            light_ids.push(id);
        }

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_OUTPUT_DIR,
            output.path().to_str().unwrap(),
        )
        .unwrap();

        (fixture, light_ids, working, output)
    }

    #[test]
    fn local_normalization_writes_one_sidecar_per_included_frame_and_a_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
        let group_key = plan_groups[0].key.clone();

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout.clone(),
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        stage_output(&mut rc).unwrap();

        let reference_path = layout.ln_reference_path(&group_key);
        assert!(reference_path.exists(), "LN reference must be written");

        let group_summary = rc
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        assert_eq!(
            group_summary.ln_reference_path.as_deref(),
            Some(reference_path.to_string_lossy().as_ref())
        );

        let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
        assert_eq!(included.len(), 4, "{:?}", group_summary.frames);

        for i in 0..4 {
            let sidecar = layout.ln_sidecar_path(&group_key, &format!("f{i}"));
            assert!(sidecar.exists(), "sidecar for f{i} must exist: {sidecar:?}");
        }

        for f in included {
            assert!(f.ln_scale.is_some(), "{f:?}");
            assert!(!f.cached_ln, "first run must not be cached: {f:?}");
        }

        let ln_artifacts =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("ln")).unwrap();
        assert_eq!(ln_artifacts.len(), 4, "{ln_artifacts:?}");
        let ln_ref_artifacts = crate::db::stacking::list_artifacts(
            &fixture.conn,
            fixture.set_id,
            Some("ln_reference"),
        )
        .unwrap();
        assert_eq!(ln_ref_artifacts.len(), 1, "{ln_ref_artifacts:?}");
    }

    #[test]
    fn local_normalization_sidecars_are_cached_on_the_second_run() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();

        let build = |run_id: i64, group_ids: HashMap<String, i64>| {
            test_context(
                ctx.clone(),
                Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
                run_id,
                fixture.set_id,
                SET_NAME,
                cfg.clone(),
                plan_groups.clone(),
                layout.clone(),
                output_dir.clone(),
                group_ids,
            )
        };

        let (run_id1, group_ids1) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc1 = build(run_id1, group_ids1);
        run_stages_for_test(&mut rc1, Stage::Register).unwrap();
        stage_output(&mut rc1).unwrap();

        let (run_id2, group_ids2) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc2 = build(run_id2, group_ids2);
        run_stages_for_test(&mut rc2, Stage::Register).unwrap();

        let start = Instant::now();
        stage_output(&mut rc2).unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "second run's stage_output (LN fully cached) took {elapsed:?}"
        );

        let group_summary = rc2
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
        assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
        for f in included {
            assert!(f.cached_ln, "{f:?}");
        }

        let ln_artifacts =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("ln")).unwrap();
        assert_eq!(ln_artifacts.len(), 4, "{ln_artifacts:?}");
        let ln_ref_artifacts = crate::db::stacking::list_artifacts(
            &fixture.conn,
            fixture.set_id,
            Some("ln_reference"),
        )
        .unwrap();
        assert_eq!(ln_ref_artifacts.len(), 1, "{ln_ref_artifacts:?}");
    }

    /// Fix round 1, item 2: the per-frame `ln` hash must fold in the
    /// group's own `reference_hash`, not just the reference MEMBER ID list —
    /// otherwise re-registering (or recalibrating) ONE reference member
    /// rebuilds the reference (a new `B_ref`, a new scale anchor) while
    /// every OTHER member's sidecar still reads as fresh against a
    /// reference that no longer matches it. A direct SQL flip of
    /// `registration_results.config_hash` would just get silently
    /// re-derived (and reverted) by the second run's own stage 5, so this
    /// forces a REAL registration change the established way (touch a
    /// source file's `size` in the catalog, same technique
    /// `calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones`
    /// uses): recalibration cascades into a genuinely different calibration
    /// hash for that one frame, which cascades into a genuinely different
    /// registration hash for it. `referenceFrames` defaults to 20 (clamped
    /// to this fixture's 4 members), so ALL FOUR are reference members —
    /// touching any one of them is guaranteed to touch the reference.
    ///
    /// Carry-over (d), Task 5's re-review: the touched member
    /// (`light_ids[0]`) must NOT also be the run's own REGISTRATION
    /// reference — `registration_hash_for` folds the registration
    /// reference's own identity into EVERY frame's registration hash
    /// (`stage_reference`/`stage_register`), so touching the registration
    /// reference itself would already invalidate every OTHER frame's plain
    /// registration hash regardless of this task's item-2 fix, confounding
    /// what the test is meant to isolate. Pinned via a MANUAL registration
    /// reference at `light_ids[1]` (any frame other than the touched one)
    /// so `light_ids[0]`'s own registration change cannot cascade into the
    /// other three frames' registration hashes on its own — only the LN
    /// reference's own hash (item 2's fix) can.
    #[test]
    fn local_normalization_sidecars_all_rebuild_when_a_reference_members_registration_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        crate::registration::db::set_frame_set_reference(
            &fixture.conn,
            fixture.set_id,
            light_ids[1],
        )
        .unwrap();

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;
        cfg.reference.mode = ReferenceMode::Manual;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();

        let (run_id1, group_ids1) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc1 = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id1,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups,
            layout.clone(),
            output_dir.clone(),
            group_ids1,
        );
        run_stages_for_test(&mut rc1, Stage::Register).unwrap();
        stage_output(&mut rc1).unwrap();

        // Touch light_ids[0]'s catalog `size` — forces recalibration (and,
        // downstream, re-registration) of that ONE frame on the next run,
        // without changing the actual pixels on disk at all.
        fixture
            .conn
            .execute(
                "UPDATE files SET size = size + 1000 WHERE id = \
                 (SELECT file_id FROM frames WHERE id = ?1)",
                rusqlite::params![light_ids[0]],
            )
            .unwrap();

        let plan_groups2 = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let (run_id2, group_ids2) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups2,
            working.path(),
            output.path(),
        );
        let mut rc2 = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id2,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups2,
            layout,
            output_dir,
            group_ids2,
        );
        run_stages_for_test(&mut rc2, Stage::Register).unwrap();
        stage_output(&mut rc2).unwrap();

        let group_summary = rc2
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
        assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
        for f in included {
            assert!(
                !f.cached_ln,
                "frame {} must rebuild its sidecar once a reference member's own \
                 registration changed: {f:?}",
                f.frame_id
            );
        }
    }

    /// Fix round 1, item 4: deterministic exclusion coverage (no starless
    /// frame needed) — pre-creating a DIRECTORY at one frame's own sidecar
    /// path makes [`crate::stacking::ln::LnFrameGrids::write`]'s tmp-file +
    /// atomic-rename fail outright (`rename` onto an existing directory is
    /// never valid), so `normalize_frame` returns an error for exactly that
    /// frame with no need to engineer a starless field. This exercises the
    /// FULL exclusion path: `members.retain`/`pick_reference_idx`
    /// re-derivation in `process_group_output`, the group still integrating
    /// and writing its master from the surviving 3, `SummaryFrame.included
    /// == false` with the reason, and (item 1) the SAME reason reaching the
    /// frame's `stacking_run_frames` row.
    #[test]
    fn local_normalization_excludes_a_frame_whose_sidecar_write_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();

        // f1's stem is plain "f1" (no basename collisions in this fixture —
        // `calibrated_file_stem` only appends `_f<id>` when two frames
        // share a source name).
        let blocked_sidecar = layout.ln_sidecar_path(&group_key, "f1");
        std::fs::create_dir_all(&blocked_sidecar).unwrap();

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        stage_output(&mut rc).unwrap();

        let blocked_frame_id = light_ids[1];
        let group_summary = rc
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        let blocked_summary = group_summary
            .frames
            .iter()
            .find(|f| f.frame_id == blocked_frame_id)
            .expect("f1's own summary entry exists");
        assert!(!blocked_summary.included, "{blocked_summary:?}");
        assert!(
            blocked_summary
                .exclusion_reason
                .as_deref()
                .unwrap_or_default()
                .contains("writing .athln sidecar"),
            "{blocked_summary:?}"
        );

        let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
        assert_eq!(included.len(), 3, "{:?}", group_summary.frames);
        assert!(group_summary.master_path.is_some(), "{group_summary:?}");
        assert!(Path::new(group_summary.master_path.as_ref().unwrap()).exists());

        // Item 1: the SAME exclusion reached the frame's own DB row, not
        // just the in-memory summary.
        let frame_rows = crate::db::stacking::list_frame_rows(&fixture.conn, rc.run_id).unwrap();
        let blocked_row = frame_rows
            .iter()
            .find(|r| r.frame_id == blocked_frame_id)
            .expect("f1's own stacking_run_frames row exists");
        assert!(!blocked_row.included, "{blocked_row:?}");
        assert!(
            blocked_row
                .exclusion_reason
                .as_deref()
                .unwrap_or_default()
                .contains("writing .athln sidecar"),
            "{blocked_row:?}"
        );
    }

    #[test]
    fn local_normalization_off_leaves_no_ln_files() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        // `StackingConfig::default()`'s `normalization.local.enabled` is
        // `false` — no override needed to exercise the disabled path.
        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout.clone(),
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        stage_output(&mut rc).unwrap();

        let ln_dir = layout.ln_dir(&group_key);
        assert!(
            !ln_dir.exists(),
            "no ln/ files must be written when local normalization is off"
        );

        let group_summary = rc
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        assert!(group_summary.ln_reference_path.is_none());
        for f in &group_summary.frames {
            assert!(f.ln_scale.is_none(), "{f:?}");
            assert!(!f.cached_ln, "{f:?}");
        }

        let ln_artifacts =
            crate::db::stacking::list_artifacts(&fixture.conn, fixture.set_id, Some("ln")).unwrap();
        assert!(ln_artifacts.is_empty(), "{ln_artifacts:?}");
        let ln_ref_artifacts = crate::db::stacking::list_artifacts(
            &fixture.conn,
            fixture.set_id,
            Some("ln_reference"),
        )
        .unwrap();
        assert!(ln_ref_artifacts.is_empty(), "{ln_ref_artifacts:?}");
    }

    /// M2 Task 7: `GroupInput.ln` — read back from the sidecars stage 6 just
    /// wrote/confirmed fresh — must actually reach `integrate_group`, not
    /// just exist as an unused field. `GroupStats.ln_frames` (Task 6) is the
    /// signal: it is computed from `input.ln` alone (`integrate.rs`), so if
    /// the wiring in `process_group_output` were broken (still passing
    /// `None`, or misaligned with `frames`' own order), it would read `0`
    /// even though every included frame has a real sidecar.
    #[test]
    fn local_normalization_grids_reach_integration_and_ln_frames_matches_included() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        stage_output(&mut rc).unwrap();

        let group_summary = rc
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        let stats = group_summary
            .stats
            .as_ref()
            .expect("group wrote a master and its stats");
        assert_eq!(stats.included, 4, "{stats:?}");
        assert_eq!(
            stats.ln_frames, stats.included,
            "every included frame must have carried its LN grid into integration: {stats:?}"
        );
    }

    /// M2 fix round 1, item 3: a `ln` artifact row a cache hit trusts
    /// (hash/size/mtime all still match) whose actual FILE content can no
    /// longer be read — corruption, a race, anything short of the size
    /// changing — must be RE-NORMALIZED in the same run, not excluded
    /// forever (the OLD behaviour this test used to pin: excluding left the
    /// stale artifact row and the corrupt file in place, so every LATER run
    /// would cache-hit the same bytes and re-exclude the same frame,
    /// permanently). One payload byte is flipped WITHOUT changing the
    /// file's length, so `is_fresh`'s live `std::fs::metadata` stat still
    /// agrees with the artifact row's stored size and the run treats this
    /// as a cache hit — `run_group_normalization`'s own read-back (moved
    /// earlier by item 3, no longer deferred to `process_group_output`) is
    /// the first point this second run actually looks at the bytes, finds
    /// them corrupt, and falls through to `needs_normalize` instead of
    /// excluding. (Contrast
    /// `local_normalization_excludes_a_frame_whose_sidecar_write_fails`,
    /// which exercises a genuine WRITE failure via a directory at the
    /// path — that path is unaffected by this fix, since a `needs_normalize`
    /// frame that then FAILS to normalize is still excluded/warned exactly
    /// as before.)
    #[test]
    fn local_normalization_re_normalizes_a_frame_whose_cached_sidecar_is_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();

        let build = |run_id: i64, group_ids: HashMap<String, i64>| {
            test_context(
                ctx.clone(),
                Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
                run_id,
                fixture.set_id,
                SET_NAME,
                cfg.clone(),
                plan_groups.clone(),
                layout.clone(),
                output_dir.clone(),
                group_ids,
            )
        };

        let (run_id1, group_ids1) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc1 = build(run_id1, group_ids1);
        run_stages_for_test(&mut rc1, Stage::Register).unwrap();
        stage_output(&mut rc1).unwrap();

        // Corrupt f1's sidecar payload in place, same total length: the
        // artifact row's stored size/hash are untouched, so the second
        // run's `is_fresh` check still calls this a cache hit.
        let touched_frame_id = light_ids[1];
        let sidecar = layout.ln_sidecar_path(&group_key, "f1");
        let mut bytes = std::fs::read(&sidecar).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&sidecar, &bytes).unwrap();

        let (run_id2, group_ids2) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc2 = build(run_id2, group_ids2);
        run_stages_for_test(&mut rc2, Stage::Register).unwrap();
        stage_output(&mut rc2).unwrap();

        let group_summary = rc2
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        let touched_summary = group_summary
            .frames
            .iter()
            .find(|f| f.frame_id == touched_frame_id)
            .expect("f1's own summary entry exists");
        assert!(
            touched_summary.included,
            "a corrupt cached sidecar must be re-normalized, not excluded: {touched_summary:?}"
        );
        assert!(
            !touched_summary.cached_ln,
            "f1 was re-normalized this run, not reused: {touched_summary:?}"
        );
        assert!(touched_summary.ln_scale.is_some(), "{touched_summary:?}");
        // The other three frames' own artifacts were never touched — still
        // genuine cache hits.
        for f in group_summary
            .frames
            .iter()
            .filter(|f| f.frame_id != touched_frame_id)
        {
            assert!(f.cached_ln, "{f:?}");
        }

        let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
        assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
        assert!(group_summary.master_path.is_some(), "{group_summary:?}");
        assert!(Path::new(group_summary.master_path.as_ref().unwrap()).exists());
        let stats = group_summary.stats.as_ref().unwrap();
        assert_eq!(stats.included, 4, "{stats:?}");
        assert_eq!(
            stats.ln_frames, 4,
            "every member — the re-normalized one included — has a grid: {stats:?}"
        );

        // The re-normalized sidecar file itself is fixed (a whole, readable
        // .athln, not the corrupt bytes) — the group's own artifact caching
        // for the NEXT run depends on this actually landing on disk.
        let repaired = crate::stacking::ln::LnFrameGrids::read(&sidecar);
        assert!(repaired.is_ok(), "{repaired:?}");

        let frame_rows = crate::db::stacking::list_frame_rows(&fixture.conn, rc2.run_id).unwrap();
        let touched_row = frame_rows
            .iter()
            .find(|r| r.frame_id == touched_frame_id)
            .expect("f1's own stacking_run_frames row exists");
        assert!(touched_row.included, "{touched_row:?}");
        assert!(touched_row.exclusion_reason.is_none(), "{touched_row:?}");
    }

    /// Final fix wave, A1 (ruling R5): with local normalization on,
    /// `RunSummary.stages` (here, `rc.timings` — `stage_output` pushes into
    /// it directly, same source the run's final summary copies) must carry
    /// a `Normalize` entry with a real, non-zero duration, positioned
    /// BEFORE `Integrate` (stage order) — the acceptance run measured LN as
    /// the single most expensive stage, so it cannot stay invisible.
    #[test]
    fn local_normalization_pushes_a_stage_timing_before_integrate() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        stage_output(&mut rc).unwrap();

        let normalize_idx = rc
            .timings
            .iter()
            .position(|t| t.stage == Stage::Normalize)
            .expect("a Normalize StageTiming must be pushed when LN is active");
        let integrate_idx = rc
            .timings
            .iter()
            .position(|t| t.stage == Stage::Integrate)
            .expect("Integrate StageTiming always pushed");
        assert!(
            normalize_idx < integrate_idx,
            "Normalize must precede Integrate in stage order: {:?}",
            rc.timings
        );
        assert!(
            rc.timings[normalize_idx].duration_ms > 0,
            "LN did real work (a reference build + a 4-frame fan-out); its timing must not be zero: {:?}",
            rc.timings[normalize_idx]
        );
    }

    /// Same run shape, LN off (the M1 shape): no `Normalize` entry at all —
    /// not a zero-duration one.
    #[test]
    fn local_normalization_off_pushes_no_stage_timing() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let cfg = StackingConfig::default();
        assert!(!cfg.normalization.local.enabled);
        assert_eq!(cfg.normalization.rejection, RejectionNormalization::ScaleZeroOffset);

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();

        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        );
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output_dir,
            group_ids,
        );

        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        stage_output(&mut rc).unwrap();

        assert!(
            !rc.timings.iter().any(|t| t.stage == Stage::Normalize),
            "LN disabled must push no Normalize entry at all: {:?}",
            rc.timings
        );
    }

    /// B1 (Critical C1), test (ii): the group-level LN fallback (ruling
    /// R3-shaped — here forced via trigger 1, an unwritable `ln/<group>`
    /// path so `run_group_normalization`'s own `create_dir_all` fails
    /// outright before any frame is attempted) must integrate with the SAME
    /// rejection pair `ScaleZeroOffset` would give it — never an
    /// un-normalized identity pair. Proven by bit-for-bit pixel equality
    /// against the identical fixture integrated with
    /// `rejection = scaleZeroOffset` and LN off entirely (the fallback
    /// leaves `GroupInput.ln` `None`, so `local_for_output`'s value cannot
    /// affect the written pixels either — see `integrate_planes`'s own doc).
    #[test]
    fn local_rejection_normalization_group_fallback_matches_global_scale_zero_offset() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let plan_groups_probe =
            group_frames(&fixture.conn, fixture.set_id, &StackingConfig::default().grouping)
                .unwrap();
        let group_key = plan_groups_probe[0].key.clone();

        // ── Run A: rejection = local, group-level fallback forced ──
        let mut cfg_fallback = StackingConfig::default();
        cfg_fallback.normalization.rejection = RejectionNormalization::Local;
        cfg_fallback.normalization.local.enabled = true;

        let layout_a = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        // Trigger 1: a plain FILE where `run_group_normalization` expects to
        // `create_dir_all` the group's `ln/` directory — the mkdir fails
        // outright, before any per-frame work, landing in the
        // `Err(RunError::Other(..))` arm ("continuing with global
        // normalization").
        let blocked_ln_dir = layout_a.ln_dir(&group_key);
        std::fs::create_dir_all(blocked_ln_dir.parent().unwrap()).unwrap();
        std::fs::write(&blocked_ln_dir, b"not a directory").unwrap();

        let plan_groups_a =
            group_frames(&fixture.conn, fixture.set_id, &cfg_fallback.grouping).unwrap();
        let (run_id_a, group_ids_a) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups_a,
            working.path(),
            output.path(),
        );
        let mut rc_a = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id_a,
            fixture.set_id,
            SET_NAME,
            cfg_fallback,
            plan_groups_a,
            layout_a,
            output.path().to_path_buf(),
            group_ids_a,
        );
        run_stages_for_test(&mut rc_a, Stage::Register).unwrap();
        stage_output(&mut rc_a).unwrap();

        let group_summary_a = rc_a
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        assert!(
            group_summary_a.ln_reference_path.is_none(),
            "the mkdir failure must fall back to global normalization, not run LN: {group_summary_a:?}"
        );
        assert!(
            rc_a.warnings
                .iter()
                .any(|w| w.contains("local normalization failed")),
            "the fallback must warn: {:?}",
            rc_a.warnings
        );
        let master_a = group_summary_a
            .master_path
            .clone()
            .expect("group A wrote a master despite the LN fallback");

        // ── Run B: rejection = scaleZeroOffset, LN off, separate output dir ──
        let mut cfg_global = StackingConfig::default();
        cfg_global.normalization.rejection = RejectionNormalization::ScaleZeroOffset;
        cfg_global.normalization.local.enabled = false;

        let working_b = tempfile::tempdir().unwrap();
        let output_b = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working_b.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_OUTPUT_DIR,
            output_b.path().to_str().unwrap(),
        )
        .unwrap();
        let layout_b = WorkingLayout::new(working_b.path(), &set_slug(SET_NAME));
        let plan_groups_b =
            group_frames(&fixture.conn, fixture.set_id, &cfg_global.grouping).unwrap();
        let (run_id_b, group_ids_b) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups_b,
            working_b.path(),
            output_b.path(),
        );
        let mut rc_b = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id_b,
            fixture.set_id,
            SET_NAME,
            cfg_global,
            plan_groups_b,
            layout_b,
            output_b.path().to_path_buf(),
            group_ids_b,
        );
        run_stages_for_test(&mut rc_b, Stage::Register).unwrap();
        stage_output(&mut rc_b).unwrap();

        let group_summary_b = rc_b
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        assert!(group_summary_b.ln_reference_path.is_none());
        let master_b = group_summary_b
            .master_path
            .clone()
            .expect("group B wrote a master");

        // ── Compare pixel data bit-for-bit (headers legitimately differ:
        // NORMALIZATION card, run id, timestamps). ──
        let reader_a = PlaneReader::open(Path::new(&master_a)).unwrap();
        let reader_b = PlaneReader::open(Path::new(&master_b)).unwrap();
        assert_eq!(reader_a.channels(), reader_b.channels());
        for p in 0..reader_a.channels() {
            let plane_a = reader_a.read_plane(p).unwrap();
            let plane_b = reader_b.read_plane(p).unwrap();
            assert_eq!(plane_a.len(), plane_b.len());
            for (i, (a, b)) in plane_a.iter().zip(plane_b.iter()).enumerate() {
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "plane {p} pixel {i}: fallback {a} vs global-normalization {b}"
                );
            }
        }
    }

    /// B1, test (iii) + A3: the rejection-only composite mode
    /// (`rejection = local`, `local.enabled = false`) with one frame whose
    /// own LN normalization fails (a directory pre-created at its sidecar
    /// path, same deterministic trick as
    /// `local_normalization_excludes_a_frame_whose_sidecar_write_fails`) —
    /// the frame stays INCLUDED (ruling R2: LN driving rejection only never
    /// excludes), `ln_frames < included`, the group still integrates and
    /// writes a master, and a `warn!`/`rc.warnings` entry is recorded. The
    /// SURVIVING grid-less frame's own rejection normalization must have
    /// used the global pair — proven the same way as the group-level test
    /// above, by bit-for-bit pixel equality against the identical fixture
    /// integrated with `rejection = scaleZeroOffset` (no LN at all).
    #[test]
    fn local_normalization_rejection_only_frame_failure_keeps_global_normalization() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, output) =
            seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg_rejection_only = StackingConfig::default();
        cfg_rejection_only.normalization.rejection = RejectionNormalization::Local;
        cfg_rejection_only.normalization.local.enabled = false;

        let layout_a = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        let plan_groups_a =
            group_frames(&fixture.conn, fixture.set_id, &cfg_rejection_only.grouping).unwrap();
        let group_key = plan_groups_a[0].key.clone();

        // f1's stem is plain "f1" — same deterministic write-failure trick
        // as the OUTPUT-normalization exclusion test above.
        let blocked_sidecar = layout_a.ln_sidecar_path(&group_key, "f1");
        std::fs::create_dir_all(&blocked_sidecar).unwrap();

        let (run_id_a, group_ids_a) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups_a,
            working.path(),
            output.path(),
        );
        let mut rc_a = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id_a,
            fixture.set_id,
            SET_NAME,
            cfg_rejection_only,
            plan_groups_a,
            layout_a,
            output_dir,
            group_ids_a,
        );
        run_stages_for_test(&mut rc_a, Stage::Register).unwrap();
        stage_output(&mut rc_a).unwrap();

        let blocked_frame_id = light_ids[1];
        let group_summary_a = rc_a
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        let stats_a = group_summary_a
            .stats
            .as_ref()
            .expect("group wrote a master and its stats");
        assert_eq!(stats_a.included, 4, "{stats_a:?}");
        assert!(
            stats_a.ln_frames < stats_a.included,
            "the blocked frame's own grid must be missing: {stats_a:?}"
        );
        let blocked_summary = group_summary_a
            .frames
            .iter()
            .find(|f| f.frame_id == blocked_frame_id)
            .expect("f1's own summary entry exists");
        assert!(
            blocked_summary.included,
            "rejection-only LN never excludes a frame it could not normalize: {blocked_summary:?}"
        );
        assert!(
            rc_a.warnings
                .iter()
                .any(|w| w.contains(&blocked_frame_id.to_string()) && w.contains("local normalization")),
            "the per-frame failure must warn: {:?}",
            rc_a.warnings
        );
        let master_a = group_summary_a
            .master_path
            .clone()
            .expect("group A wrote a master with the blocked frame kept");

        // ── Comparison run: rejection = scaleZeroOffset, LN off entirely ──
        let mut cfg_global = StackingConfig::default();
        cfg_global.normalization.rejection = RejectionNormalization::ScaleZeroOffset;
        cfg_global.normalization.local.enabled = false;

        let working_b = tempfile::tempdir().unwrap();
        let output_b = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working_b.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_OUTPUT_DIR,
            output_b.path().to_str().unwrap(),
        )
        .unwrap();
        let layout_b = WorkingLayout::new(working_b.path(), &set_slug(SET_NAME));
        let plan_groups_b =
            group_frames(&fixture.conn, fixture.set_id, &cfg_global.grouping).unwrap();
        let (run_id_b, group_ids_b) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups_b,
            working_b.path(),
            output_b.path(),
        );
        let mut rc_b = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id_b,
            fixture.set_id,
            SET_NAME,
            cfg_global,
            plan_groups_b,
            layout_b,
            output_b.path().to_path_buf(),
            group_ids_b,
        );
        run_stages_for_test(&mut rc_b, Stage::Register).unwrap();
        stage_output(&mut rc_b).unwrap();

        let group_summary_b = rc_b
            .summary
            .groups
            .iter()
            .find(|g| g.key == group_key)
            .expect("group summary pushed");
        let master_b = group_summary_b
            .master_path
            .clone()
            .expect("comparison group wrote a master");

        let reader_a = PlaneReader::open(Path::new(&master_a)).unwrap();
        let reader_b = PlaneReader::open(Path::new(&master_b)).unwrap();
        assert_eq!(reader_a.channels(), reader_b.channels());
        for p in 0..reader_a.channels() {
            let plane_a = reader_a.read_plane(p).unwrap();
            let plane_b = reader_b.read_plane(p).unwrap();
            assert_eq!(plane_a.len(), plane_b.len());
            for (i, (a, b)) in plane_a.iter().zip(plane_b.iter()).enumerate() {
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "plane {p} pixel {i}: rejection-only fallback {a} vs global-normalization {b}"
                );
            }
        }
    }

    // ── M3 Task 5: the Drizzle stage wired into the run ─────────────────

    /// Brief test (a): drizzle 2x on, `useRejection` on, `cleanup =
    /// deleteIntermediates` — the run succeeds, the group row's
    /// `drizzle_path` names an existing file of `out_w = 2*W`,
    /// `out_h = 2*H`, `summary.groups[0].drizzle.scale == 2`, `stages`
    /// contains `drizzle` between `integrate` and `output`, `rej/run-<id>`
    /// does NOT exist after the run, `stacking-complete`'s
    /// `masters[0].drizzlePath` is `Some`.
    #[test]
    fn drizzle_writes_a_scaled_master_and_removes_rej_after_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
        cfg.drizzle.use_rejection = true;
        cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;

        let recorder = Arc::new(Recording::new());
        let started = start_stacking(
            ctx.clone(),
            recorder.clone(),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");

        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        assert_eq!(groups.len(), 1, "{groups:?}");
        let group = &groups[0];
        let master_path = group.master_path.clone().expect("master path recorded");
        let drizzle_path = group.drizzle_path.clone().expect("drizzle path recorded");
        assert!(
            Path::new(&drizzle_path).exists(),
            "drizzled master missing: {drizzle_path}"
        );

        let master_reader = PlaneReader::open(Path::new(&master_path)).unwrap();
        let drizzle_reader = PlaneReader::open(Path::new(&drizzle_path)).unwrap();
        assert_eq!(drizzle_reader.width(), 2 * master_reader.width());
        assert_eq!(drizzle_reader.height(), 2 * master_reader.height());

        let summary: RunSummary =
            serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
        let group_summary = &summary.groups[0];
        assert_eq!(
            group_summary
                .drizzle
                .as_ref()
                .expect("drizzle stats recorded")
                .scale,
            2
        );

        // Fix round 1, Important I1: real per-frame progress — the Drizzle
        // stage's own `stacking-progress` events must reach a genuine
        // `current == total` completion, not just the forced starting
        // `0/total` tick (a no-op `on_frame` would leave the LAST recorded
        // `current` at 0, exactly what a live progress bar user sees as
        // "stuck at 0%").
        let drizzle_events: Vec<_> = recorder
            .events(STACKING_PROGRESS_EVENT)
            .into_iter()
            .filter(|e| e["stage"] == "drizzle")
            .collect();
        assert!(
            !drizzle_events.is_empty(),
            "expected at least one drizzle-stage progress event"
        );
        let expected_total = (group.included_count as usize) * master_reader.channels();
        let last = drizzle_events.last().expect("checked non-empty above");
        assert_eq!(
            last["total"].as_u64(),
            Some(expected_total as u64),
            "{drizzle_events:?}"
        );
        assert_eq!(
            last["current"].as_u64(),
            Some(expected_total as u64),
            "the last drizzle progress event must reach current == total: {drizzle_events:?}"
        );

        let integrate_idx = summary
            .stages
            .iter()
            .position(|t| t.stage == Stage::Integrate)
            .expect("Integrate stage timing");
        let drizzle_idx = summary
            .stages
            .iter()
            .position(|t| t.stage == Stage::Drizzle)
            .expect("Drizzle stage timing");
        let output_idx = summary
            .stages
            .iter()
            .position(|t| t.stage == Stage::Output)
            .expect("Output stage timing");
        assert!(
            integrate_idx < drizzle_idx && drizzle_idx < output_idx,
            "expected integrate < drizzle < output, got {:?}",
            summary.stages
        );

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        assert!(
            !layout.rej_run_dir(started.run_id).exists(),
            "rej/run-<id> must be removed after the run"
        );

        let completes = recorder.events(STACKING_COMPLETE_EVENT);
        assert_eq!(completes.len(), 1, "{completes:?}");
        let masters = completes[0]["masters"].as_array().unwrap();
        assert_eq!(masters.len(), 1, "{masters:?}");
        assert_eq!(
            masters[0]["drizzlePath"].as_str(),
            Some(drizzle_path.as_str())
        );
    }

    /// Brief test (b): same as above with `cleanup = keepAll` —
    /// `rej/run-<id>/<group>/` holds `included` `.rej` files of the right
    /// size (validated by reading each one back via `RejBitmap::read` at
    /// the master's own geometry — a short/foreign file would refuse).
    #[test]
    fn drizzle_keep_all_leaves_rejection_bitmaps_of_the_right_size() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
        cfg.drizzle.use_rejection = true;
        cfg.output.cleanup = CleanupPolicy::KeepAll;

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");

        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        let group = &groups[0];
        let included_count = group.included_count as usize;

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let rej_dir = layout.rej_dir(started.run_id, &group.group_key);
        assert!(
            rej_dir.exists(),
            "rej dir must survive keepAll: {rej_dir:?}"
        );

        let entries: Vec<_> = std::fs::read_dir(&rej_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), included_count, "{entries:?}");

        let master_path = group.master_path.clone().expect("master path");
        let master_reader = PlaneReader::open(Path::new(&master_path)).unwrap();
        for e in &entries {
            RejBitmap::read(
                &e.path(),
                master_reader.width(),
                master_reader.height(),
                master_reader.channels(),
            )
            .unwrap_or_else(|err| {
                panic!(
                    "{:?} is not a valid .rej at the master's geometry: {err}",
                    e.path()
                )
            });
        }
    }

    /// Brief test (c): drizzle on with `writeWeightMap` — `weight_map_path`
    /// exists on disk.
    #[test]
    fn drizzle_write_weight_map_produces_a_weight_map_file() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, _working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
        cfg.drizzle.write_weight_map = true;

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");

        let summary: RunSummary =
            serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
        let weight_map_path = summary.groups[0]
            .weight_map_path
            .clone()
            .expect("weight map path recorded");
        assert!(
            Path::new(&weight_map_path).exists(),
            "weight map missing: {weight_map_path}"
        );
    }

    /// Brief test (d): drizzle off adds no `drizzle` timing and no `rej/`;
    /// the master's bytes are BIT-IDENTICAL to a run of the SAME fixture
    /// with drizzle 2x on (the drizzle stage must never touch the master —
    /// the M1/M2 pins stay green with drizzle wired in).
    #[test]
    fn drizzle_off_leaves_the_master_byte_identical_to_drizzle_on() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        // Run 1: drizzle off (the M1/M2 default).
        let started_off = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("first run should succeed");
        wait_for_run(&ctx, started_off.run_id);
        let row_off = crate::db::stacking::get_run(&fixture.conn, started_off.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row_off.status, "done", "{row_off:?}");
        let summary_off: RunSummary =
            serde_json::from_str(row_off.summary_json.as_deref().unwrap()).unwrap();
        assert!(
            !summary_off.stages.iter().any(|t| t.stage == Stage::Drizzle),
            "{:?}",
            summary_off.stages
        );
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        assert!(
            !layout.rej_run_dir(started_off.run_id).exists(),
            "no rej/ when drizzle is off"
        );
        let groups_off =
            crate::db::stacking::list_groups(&fixture.conn, started_off.run_id).unwrap();
        let master_off = groups_off[0].master_path.clone().expect("first master");

        // Run 2: SAME fixture, drizzle 2x on — the master must not change.
        let mut cfg_on = StackingConfig::default();
        cfg_on.drizzle.enabled = true;
        cfg_on.drizzle.scale = 2;

        let started_on = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg_on),
            None,
        )
        .expect("second run should succeed");
        wait_for_run(&ctx, started_on.run_id);
        let row_on = crate::db::stacking::get_run(&fixture.conn, started_on.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row_on.status, "done", "{row_on:?}");
        let groups_on =
            crate::db::stacking::list_groups(&fixture.conn, started_on.run_id).unwrap();
        let master_on = groups_on[0].master_path.clone().expect("second master");
        assert_ne!(
            master_off, master_on,
            "a rerun must never overwrite the first master"
        );

        let reader_off = PlaneReader::open(Path::new(&master_off)).unwrap();
        let reader_on = PlaneReader::open(Path::new(&master_on)).unwrap();
        assert_eq!(reader_off.channels(), reader_on.channels());
        for p in 0..reader_off.channels() {
            let plane_off = reader_off.read_plane(p).unwrap();
            let plane_on = reader_on.read_plane(p).unwrap();
            assert_eq!(plane_off.len(), plane_on.len());
            for (i, (a, b)) in plane_off.iter().zip(plane_on.iter()).enumerate() {
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "plane {p} pixel {i}: drizzle off {a} vs drizzle on {b} \u{2014} the drizzle stage must never touch the master"
                );
            }
        }
    }

    /// Brief test (e): a cancel raised during the drizzle stage (via a
    /// `stacking-progress` listener flipping the run's cancel flag on the
    /// first `stage == "drizzle"` event, same mechanism
    /// `cancel_mid_run_keeps_artifacts_writes_no_master` uses for
    /// `"measure"`) — the run finishes `cancelled`, `rej/` is still removed
    /// (cleanup != keepAll), the master is present, and `drizzle_path`
    /// stays `NULL` (ruling R-M3-7: a drizzle failure/cancel never touches
    /// an already-written master).
    #[test]
    fn cancel_during_drizzle_keeps_the_master_removes_rej_leaves_drizzle_path_null() {
        struct CancelOnDrizzle {
            ctx: Arc<ServiceContext>,
            frames_set_id: i64,
            fired: std::sync::atomic::AtomicBool,
            recording: Recording,
        }
        impl ProgressEmitter for CancelOnDrizzle {
            fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
                self.recording.emit_json(event_name, payload.clone());
                if event_name == STACKING_PROGRESS_EVENT
                    && payload["stage"] == "drizzle"
                    && !self.fired.swap(true, Ordering::SeqCst)
                {
                    let active = self.ctx.active_stacks.lock().unwrap();
                    for h in active.values() {
                        if h.frames_set_id == self.frames_set_id {
                            h.cancel_flag.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
        cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;

        let emitter = Arc::new(CancelOnDrizzle {
            ctx: ctx.clone(),
            frames_set_id: fixture.set_id,
            fired: std::sync::atomic::AtomicBool::new(false),
            recording: Recording::new(),
        });

        let started = start_stacking(
            ctx.clone(),
            emitter.clone(),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "cancelled", "{row:?}");

        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        let group = &groups[0];
        let master_path = group
            .master_path
            .clone()
            .expect("master must survive the cancel");
        assert!(Path::new(&master_path).exists(), "{master_path}");
        assert!(group.drizzle_path.is_none(), "{group:?}");

        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        assert!(
            !layout.rej_run_dir(started.run_id).exists(),
            "rej/run-<id> must be removed even on cancel"
        );

        let completes = emitter.recording.events(STACKING_COMPLETE_EVENT);
        assert_eq!(completes.len(), 1, "{completes:?}");
        assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
    }


    /// Fix round 2, Important I3 (re-review): the round-1 version of this
    /// test dropped the fixture's LAST frame (index 3 of 4), which leaves
    /// `output.included == [0, 1, 2]` — `k == idx` for every survivor, so a
    /// hypothetical bug that swapped `k` (position in `output.included`)
    /// for `idx` (position in `stack_frames`/`members`) anywhere in the
    /// drizzle fan-out would have been COMPLETELY INVISIBLE (`stack_frames[k]`
    /// and `stack_frames[idx]` are the same array access for every kept
    /// frame). Reworked: 5 frames, DROP THE SECOND (index 1 of 5) — engine
    /// order for the 4 survivors is `output.included == [0, 2, 3, 4]`, so
    /// `k=1→idx=2`, `k=2→idx=3`, `k=3→idx=4` for every survivor past the
    /// first (only `k=0→idx=0` still coincides, unavoidably, since the
    /// FIRST kept frame is always its own position).
    ///
    /// Each kept frame gets a DISTINCT, exactly-known weight
    /// (`measurement.weight_mode = Exposure`, so `weight == exptime`
    /// directly — no noise/PSF-derived formula to fight for a precise
    /// target) and a DISTINCT background level; `normalization.output =
    /// None` makes every frame's output pair the identity (no scale/offset
    /// warps the deposited value); `integration.rejection = None` disables
    /// algorithm rejection outright, so nothing in the flat, noise-free
    /// (well, near-zero-noise) background region this test samples is ever
    /// flagged and skipped. Under those three settings, ruling R-M3-2's
    /// `I = Σ a·w·N(d)`, `W = Σ a·w`, output `I/W` reduces EXACTLY to the
    /// plain weighted mean `Σ w_i·v_i / Σ w_i` of the kept frames' own
    /// calibrated levels at any interior, star-free output pixel — the `a`
    /// (area) factor is identical across every frame at a given output
    /// pixel (all frames share the SAME zero-shift geometry) and cancels.
    /// The test measures each kept frame's own calibrated level directly
    /// (reading the SAME file the drizzle stage itself reads, at the SAME
    /// star-free coordinate) rather than assuming a calibration formula, so
    /// the predicted level is independent of the calibration math entirely.
    ///
    /// See the task report for exactly which survivor pairs a `k`-instead-
    /// of-`idx` bug would swap here, and by how much the predicted level
    /// would move.
    #[test]
    fn drizzle_over_a_min_weight_drop_matches_the_kept_frames_exact_weighted_mean() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

        let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
        let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

        // Index 1 (of 0..5) is the one that will be dropped by the
        // min-weight gate — a middle member, not the tail, so every
        // survivor past the first has k != idx (see the doc comment
        // above). Zero relative shift for every frame: this test cares
        // about weight/level alignment, not registration robustness (which
        // every OTHER drizzle test already exercises with real shifts), and
        // a flat background region's value is unaffected by interpolation
        // under an identity map either way.
        let exptimes = [60.0f64, 0.001, 30.0, 15.0, 7.5];
        let backgrounds = [600.0f32, 9999.0, 900.0, 1200.0, 1500.0];
        let noise = [2.0f32, 2.0, 2.0, 2.0, 2.0];
        let mut light_ids = Vec::new();
        for i in 0..5 {
            let stem = format!("f{i}");
            let date_obs = date_obs_at(i);
            let spec = LightSpec {
                stem: &stem,
                instrume: "cam",
                filter: None,
                binning: 1,
                width: STAR_FIELD_WIDTH,
                height: STAR_FIELD_HEIGHT,
                exptime: exptimes[i],
                date_obs: &date_obs,
                bayerpat: None,
                write_file: true,
            };
            let stars = shifted_stars(0.0, 0.0);
            let (id, _path) = test_fixtures::add_light_with_field(
                &fixture,
                &spec,
                &stars,
                backgrounds[i],
                noise[i],
                100 + i as u64,
            );
            light_ids.push(id);
        }

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            crate::settings::keys::STACKING_OUTPUT_DIR,
            output.path().to_str().unwrap(),
        )
        .unwrap();

        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );

        let mut cfg = StackingConfig::default();
        // Grouping clusters by exposure too (spec §2) — its anchor-relative
        // rule (`cluster_indices`, `groups.rs`) compares every sorted
        // exposure against the CLUSTER'S FIRST value, so the 0.001s..60s
        // spread here needs a tolerance past the full span, or the 5
        // frames would fragment into several undersized groups before this
        // test's own weight gate ever runs.
        cfg.grouping.exposure_tolerance_sec = 100.0;
        // Exact, controllable weights: weight == exptime, nothing derived
        // from noise/PSF fits (which fix round 1 found hard to hit a
        // precise target with).
        cfg.measurement.weight_mode = WeightMode::Exposure;
        // Stage 3 must not exclude frame 1 on weight — it has to survive
        // selection and reach `integrate_group`'s OWN gate.
        cfg.selection.min_weight_fraction = 0.0;
        // Identity output pairs for every frame: the deposited value is
        // each frame's own calibrated level, unscaled/unshifted.
        cfg.normalization.output = crate::integration::stats::OutputNormalization::None;
        // No algorithm rejection: nothing in the flat background region
        // this test samples is ever flagged, whatever a real sigma-clip
        // might make of four frames with deliberately different raw
        // background levels.
        cfg.integration.rejection = crate::stacking::integrate::RejectionChoice::None;
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
        cfg.drizzle.use_rejection = true;
        cfg.drizzle.use_weights = true;
        cfg.output.cleanup = CleanupPolicy::KeepAll;

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");

        let summary: RunSummary =
            serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
        assert_eq!(summary.groups.len(), 1, "{:?}", summary.groups);
        let group_summary = &summary.groups[0];
        let stats = group_summary
            .stats
            .as_ref()
            .expect("group stats recorded");
        assert_eq!(stats.frames, 5, "all 5 frames must reach integration: {stats:?}");
        assert_eq!(
            stats.dropped_below_min_weight, 1,
            "expected exactly one frame dropped below the weight floor: {stats:?}"
        );
        assert_eq!(stats.included, 4, "{stats:?}");

        let drizzle_stats = group_summary
            .drizzle
            .as_ref()
            .expect("drizzle stats recorded");
        assert_eq!(
            drizzle_stats.frames, stats.included,
            "drizzle.frames must equal the group's own included count: {drizzle_stats:?}"
        );

        // The 4 KEPT frames' own calibrated levels, measured directly from
        // the SAME files the drizzle stage reads (`SummaryFrame.calibrated_path`),
        // at a coordinate far from every star (`BASE_STARS` spans roughly
        // x in [30, 150], y in [24, 102] on this 192x144 canvas — (165..185,
        // 110..130) is comfortably outside any star's PSF wing at sigma
        // 1.6px) and averaged over a 20x20 patch there to wash out the
        // (small, near-zero) per-pixel noise.
        let region_mean = |plane: &[f32], width: usize, x0: usize, y0: usize, w: usize, h: usize| -> f64 {
            let mut sum = 0.0f64;
            let mut count = 0usize;
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    sum += plane[y * width + x] as f64;
                    count += 1;
                }
            }
            sum / count as f64
        };

        let kept_indices = [0usize, 2, 3, 4];
        let kept_weights = [1.0f64, 0.5, 0.25, 0.125]; // exptime / max(exptime) = 60/60, 30/60, 15/60, 7.5/60
        let mut kept_levels = Vec::with_capacity(4);
        let mut kept_stems = Vec::with_capacity(4);
        for &i in &kept_indices {
            let frame_id = light_ids[i];
            let sf = group_summary
                .frames
                .iter()
                .find(|f| f.frame_id == frame_id)
                .unwrap_or_else(|| panic!("frame {i} (id {frame_id}) not in summary"));
            let calibrated_path = sf
                .calibrated_path
                .as_ref()
                .unwrap_or_else(|| panic!("frame {i} has no calibrated_path: {sf:?}"));
            let reader = PlaneReader::open(Path::new(calibrated_path)).unwrap();
            let plane = reader.read_plane(0).unwrap();
            let level = region_mean(&plane, reader.width(), 165, 110, 20, 20);
            kept_levels.push(level);
            kept_stems.push(
                Path::new(calibrated_path)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }

        let sum_w: f64 = kept_weights.iter().sum();
        let sum_wv: f64 = kept_weights
            .iter()
            .zip(kept_levels.iter())
            .map(|(&w, &v)| w * v)
            .sum();
        let expected_level = sum_wv / sum_w;

        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        let group = &groups[0];
        let drizzle_path = group
            .drizzle_path
            .clone()
            .expect("drizzle path recorded");
        let drizzle_reader = PlaneReader::open(Path::new(&drizzle_path)).unwrap();
        let drizzle_plane = drizzle_reader.read_plane(0).unwrap();
        // Scale-2 output: the same (165..185, 110..130) source patch maps
        // to (330..370, 220..260) in output pixels.
        let drizzled_level = region_mean(&drizzle_plane, drizzle_reader.width(), 330, 220, 40, 40);

        assert!(
            (drizzled_level - expected_level).abs() < 1e-4,
            "drizzled interior level does not match the kept frames' exact weighted mean: \
             expected={expected_level} (levels={kept_levels:?}, weights={kept_weights:?}), \
             drizzled={drizzled_level}, diff={}",
            (drizzled_level - expected_level).abs()
        );

        // The .rej files: exactly the 4 KEPT frames' own calibrated stems,
        // no more, no less — in particular never the dropped frame's.
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let rej_dir = layout.rej_dir(started.run_id, &group.group_key);
        let mut actual_stems: Vec<String> = std::fs::read_dir(&rej_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| {
                Path::new(&e.file_name())
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        actual_stems.sort();
        let mut expected_stems = kept_stems.clone();
        expected_stems.sort();
        assert_eq!(
            actual_stems, expected_stems,
            "rej/ must hold exactly the kept frames' bitmaps, named by their own calibrated stems"
        );
    }
}
