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
    finish_run, get_run, insert_group, insert_run, set_run_reference, set_run_status, update_group,
    upsert_artifact, upsert_frame_row, GroupUpdate, NewArtifact, NewFrameRow, NewGroup, NewRun,
};
use crate::events::{emit_event, ProgressEmitter};
use crate::export::{execute_generation, resolve_generation_cached};
use crate::fits_parser::FitsHeader;
use crate::geometry::PixelMap;
use crate::integration::band_budget::total_ram_bytes;
use crate::integration::plane_reader::PlaneReader;
use crate::integration::IntegrationError;
use crate::registration::db::{
    get_frame_set_reference, get_registration_for_frame_set, upsert_registration,
    RegistrationRecord,
};
use crate::services::compute_queue::ComputeJobKind;
use crate::services::{ServiceContext, StackHandle};
#[cfg(test)]
use crate::stacking::config::config_hash;
use crate::stacking::config::{ReferenceMode, StackingConfig};
use crate::stacking::groups::{group_frames, set_slug, ColorMode, GroupFrame, IntegrationGroup};
use crate::stacking::measure::{measure_frame, FrameMeasurement};
use crate::stacking::paths::WorkingLayout;
use crate::stacking::plan::{
    build_plan, is_fresh, measurement_hash_for, registration_hash_for, registration_row_is_fresh,
    HashMemo, Stage,
};
use crate::stacking::provenance::{RunSummary, SummaryMeasurement, SummaryReference};
use crate::stacking::register::frame::{
    identity_registration, reference_stars, register_frame, to_record,
};
use crate::stacking::register::writer::{
    build_registered_cards, source_cards_from_file, write_registered_frame, RegisteredCards,
};
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
    /// Manually-excluded frame ids (spec §9.1's `stacking_set_config.excluded_frame_ids`,
    /// already resolved by the plan gate) — a frame in here is skipped by
    /// every stage, never calibrated/measured/registered/integrated.
    pub(crate) excluded: Vec<i64>,
    /// `group_key -> stacking_run_groups.id`, populated by [`start_stacking`]
    /// right after [`insert_run`] so every later stage (Tasks 7-8:
    /// `upsert_frame_row`'s `group_id`, `update_group`) has the row id
    /// without a second DB round-trip. Not read by Task 6's own stage.
    #[allow(dead_code)]
    pub(crate) group_ids: HashMap<String, i64>,
    pub(crate) layout: WorkingLayout,
    /// The run's output folder (masters land here — Task 8's Output stage).
    /// Not read by Task 6's own stage (calibrate writes only into `layout`).
    #[allow(dead_code)]
    pub(crate) output_dir: PathBuf,
    pub(crate) cancel: Arc<AtomicBool>,
    /// Not read again by Task 6 (only used to seed `summary.app_version` at
    /// construction) — Task 8's master-card headers (`MasterCardInputs`)
    /// need it again.
    #[allow(dead_code)]
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

/// Start a stacking run for `frames_set_id`: build the plan, refuse on the
/// first blocker or an already-active run, insert the run + its group rows,
/// register a cancel handle, and spawn the dedicated thread. Returns as soon
/// as the thread is spawned — the queue permit is acquired INSIDE the thread
/// (ruling 2).
pub fn start_stacking(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    frames_set_id: i64,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, ApiError> {
    let (run_id, rc) = {
        let db_handle = db(&ctx)?;
        let conn = db_handle.conn();

        let plan = build_plan(
            &conn,
            &ctx.settings,
            &PathPolicy::AllowAll,
            frames_set_id,
            config,
        )?;

        if let Some(blocker) = plan.blockers.first() {
            return Err(ApiError::Invalid(blocker.message.clone()));
        }
        // `build_plan` already reads this (spec §9.4 gate) — reuse it rather
        // than a second `active_run_for_set` query. Benign TOCTOU: two
        // concurrent `start_stacking` calls for the SAME set, racing between
        // this read and the `insert_run` below, could both pass — accepted,
        // same tradeoff `calibration_library::check_library_root_uniqueness`
        // documents for the analogous "designate library root" race.
        if let Some(active_id) = plan.active_run_id {
            return Err(ApiError::Conflict(format!(
                "a stacking run (id {active_id}) is already active for frame set {frames_set_id}"
            )));
        }

        let working_dir = plan.working_dir.clone().ok_or_else(|| {
            ApiError::Internal(
                "stacking plan reported no blockers but no working folder".to_string(),
            )
        })?;
        let output_dir_str = plan.output_dir.clone().ok_or_else(|| {
            ApiError::Internal(
                "stacking plan reported no blockers but no output folder".to_string(),
            )
        })?;

        let plan_groups = group_frames(&conn, frames_set_id, &plan.config.grouping)?;

        let config_json = serde_json::to_string(&plan.config)
            .map_err(|e| ApiError::Internal(format!("failed to serialize stacking config: {e}")))?;

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

        let started_at = get_run(&conn, run_id)?
            .map(|r| r.started_at)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

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
            let group_id = insert_group(
                &conn,
                &NewGroup {
                    run_id,
                    group_key: &g.key,
                    instrume: g.instrume.as_deref(),
                    color_mode: color_mode_wire(g.color_mode),
                    filter: g.filter.as_deref(),
                    binning: Some(g.binning),
                    width: Some(g.width),
                    height: Some(g.height),
                    exposure: g.exposure_s,
                    frame_count: g.frames.len() as i64,
                    included_count: included_count as i64,
                },
            )?;
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
            stages: Vec::new(),
            warnings: Vec::new(),
            error: None,
        };

        let cancel = Arc::new(AtomicBool::new(false));

        let rc = RunContext {
            ctx: ctx.clone(),
            emitter: emitter.clone(),
            run_id,
            set_id: frames_set_id,
            set_name: plan.set_name,
            config: plan.config,
            hash: plan.config_hash,
            plan_groups,
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
            runtime_exclusions: Vec::new(),
            measured: HashMap::new(),
            reference_frame_id: None,
            reference_calibrated: None,
            reference_width: 0,
            reference_height: 0,
            #[cfg(test)]
            fail_after_stage: None,
        };

        (run_id, rc)
    };

    {
        let mut active = ctx.active_stacks.lock().unwrap();
        active.insert(
            run_id,
            StackHandle {
                cancel_flag: rc.cancel.clone(),
                frames_set_id,
            },
        );
    }

    let spawn_result = std::thread::Builder::new()
        .name(format!("stacking-run-{run_id}"))
        .spawn(move || {
            run_thread(rc);
        });

    if let Err(e) = spawn_result {
        // The thread never started, so nothing will ever remove this handle,
        // finish the run row, or emit stacking-complete — clean up right
        // here instead (mirrors `start_master_build`'s spawn-failure path).
        ctx.active_stacks.lock().unwrap().remove(&run_id);
        if let Ok(db_handle) = db(&ctx) {
            let conn = db_handle.conn();
            if let Err(e2) = finish_run(
                &conn,
                run_id,
                "failed",
                None,
                Some(&format!("failed to spawn stacking thread: {e}")),
            ) {
                tracing::warn!(run_id, error = %e2, "failed to mark run failed after spawn failure");
            }
        }
        return Err(ApiError::Internal(format!(
            "failed to spawn stacking thread: {e}"
        )));
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
/// path for the whole run: handle removal, `finish_run`, the provenance
/// snapshot, the terminal log lines, and `stacking-complete` ALWAYS happen
/// here, exactly once, regardless of how [`run_pipeline`] ended (including a
/// panic inside it).
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

    rc.ctx.active_stacks.lock().unwrap().remove(&run_id);

    let (status, success, cancelled, error): (&str, bool, bool, Option<String>) = match &result {
        Ok(()) => ("done", true, false, None),
        Err(RunError::Cancelled) => {
            tracing::info!(run_id, "stacking run cancelled");
            ("cancelled", false, true, None)
        }
        Err(RunError::Other(msg)) => {
            tracing::error!(run_id, error = %msg, "stacking run failed");
            ("failed", false, false, Some(msg.clone()))
        }
    };

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

    // Tasks 7-8 populate this from every group that actually wrote a master
    // (ruling 15) — stage 1 alone never does.
    let masters: Vec<StackingMasterRef> = Vec::new();

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

    // Task 8: stage_normalize/integrate, stage_output, cleanup.

    Ok(())
}

/// Calibrate-only byte footprint for progress's `bytes_total` (mirrors
/// [`crate::stacking::paths::estimate_bytes`]'s calibrated-frame term, but
/// scoped to just this stage rather than the whole run's estimate).
fn calibrate_bytes_total(groups: &[IntegrationGroup], excluded: &HashSet<i64>) -> u64 {
    let mut total = 0u64;
    for g in groups {
        let planes: u64 = if g.color_mode == ColorMode::Osc { 3 } else { 1 };
        let w = g.width.max(0) as u64;
        let h = g.height.max(0) as u64;
        let included = g
            .frames
            .iter()
            .filter(|f| !excluded.contains(&f.frame_id))
            .count() as u64;
        total += planes * w * h * 4 * included;
    }
    total
}

/// One frame's stage-1 outcome (private to [`stage_calibrate`]).
enum CalibrateOutcome {
    /// An existing `calibrated` artifact was fresh; nothing was written.
    Reused,
    /// A fresh `calibrated` file was written; `bytes` is its size.
    Generated { bytes: u64 },
    /// Calibration failed for this frame; `reason` is
    /// [`RunContext::runtime_exclusions`]'s text.
    Excluded { reason: String },
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
    group_key: &str,
    frame: &GroupFrame,
    scratch: &Path,
) -> Result<CalibrateOutcome, RunError> {
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
                return Ok(CalibrateOutcome::Reused);
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

    let out = rc
        .layout
        .calibrated_dir(group_key)
        .join(spec.output_filename(&frame.filename));

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

            match calibrate_one_frame(rc, &cfg, &group.key, frame, &scratch)? {
                CalibrateOutcome::Reused => {}
                CalibrateOutcome::Generated { bytes } => {
                    bytes_done += bytes;
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

/// Fan `items` out across `admission` worker threads pulling from one
/// shared FIFO queue (`std::thread::scope`), `cancel` checked before each
/// item is pulled — never mid-item, since only the item's own function (a
/// caller that needs per-item cancellation captures its OWN `&AtomicBool`
/// into `f`) can decide that. Results land at their item's ORIGINAL index
/// in `items`; an index whose item was never started because `cancel`
/// fired first is `None` — callers check `cancel` once, right after this
/// returns, rather than inspecting every entry for that case.
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
        for _ in 0..admission.max(1) {
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
                });
                continue;
            };

            let planes = PlaneReader::open(&calibrated_path)
                .map(|r| r.channels())
                .unwrap_or(0);
            if planes == 0 {
                let reason = "calibrated frame has no planes".to_string();
                rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
                entries.push(MeasuredFrame {
                    frame: frame.clone(),
                    calibrated: Some(calibrated_path),
                    planes: 0,
                    measurement: None,
                    weight: None,
                    included: false,
                    reason: Some(reason),
                    registration: None,
                });
                continue;
            }
            max_planes = max_planes.max(planes);

            entries.push(MeasuredFrame {
                frame: frame.clone(),
                calibrated: Some(calibrated_path.clone()),
                planes,
                measurement: None,
                weight: None,
                included: false,
                reason: None,
                registration: None,
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
            let admission_n = admission(
                8 * max_planes as u64 * group.width.max(0) as u64 * group.height.max(0) as u64 * 4,
            );
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
                    included = included_count,
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
            let entry = rc
                .measured
                .values()
                .flat_map(|v| v.iter())
                .find(|e| e.frame.frame_id == frame_id)
                .ok_or_else(|| {
                    RunError::Other("reference frame is not part of any group".to_string())
                })?;
            let calibrated = entry.calibrated.clone().ok_or_else(|| {
                RunError::Other("reference frame was excluded before measurement".to_string())
            })?;
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

        let admission_n = admission(4 * group.width.max(0) as u64 * group.height.max(0) as u64 * 4);
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
                                write_registered_artifact(rc, &group.key, frame, &map, &rec, &cfg)
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
    group_key: &str,
    frame: &GroupFrame,
    map: &PixelMap,
    rec: &RegistrationRecord,
    cfg: &StackingConfig,
) -> anyhow::Result<()> {
    let calibrated = rc
        .measured
        .get(group_key)
        .and_then(|v| v.iter().find(|e| e.frame.frame_id == frame.frame_id))
        .and_then(|e| e.calibrated.clone())
        .ok_or_else(|| {
            anyhow::anyhow!("no calibrated path recorded for frame {}", frame.frame_id)
        })?;

    let out_dir = rc.layout.registered_dir(group_key);
    std::fs::create_dir_all(&out_dir)?;
    let out = out_dir.join(registered_file_name(&calibrated));

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

/// `r_<stem>.fits` for a calibrated file's own stem, matching
/// `register_probe.rs`'s own convention (trim a leading `c_`).
fn registered_file_name(calibrated: &Path) -> String {
    let stem = calibrated
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("frame");
    format!("r_{}.fits", stem.trim_start_matches("c_"))
}

/// One `stacking_run_frames` row per frame of every group, included or not
/// — the frame's whole stage 3-5 outcome as `rc.measured` now holds it.
/// Called once, at the very end of stage 5 (decision 8).
fn write_frame_rows(rc: &mut RunContext) -> Result<(), RunError> {
    let conn = db(&rc.ctx)?.conn();
    for group in &rc.plan_groups {
        let Some(&group_id) = rc.group_ids.get(&group.key) else {
            continue;
        };
        let Some(entries) = rc.measured.get(&group.key) else {
            continue;
        };
        for entry in entries {
            let weight = entry.weight.as_ref().map(|w| w.normalized_mean);
            let weight_channels_json = entry
                .weight
                .as_ref()
                .map(|w| serde_json::to_string(&w.normalized))
                .transpose()
                .map_err(|e| {
                    RunError::Other(format!("failed to serialize weight channels: {e}"))
                })?;
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
                &conn,
                &NewFrameRow {
                    run_id: rc.run_id,
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
        runtime_exclusions: Vec::new(),
        measured: HashMap::new(),
        reference_frame_id: None,
        reference_calibrated: None,
        reference_width: 0,
        reference_height: 0,
        fail_after_stage: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullEmitter;
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
            "test".to_string(),
            empty_fixture.set_id,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        drop(empty_fixture);

        // A ready fixture (masters + folders), on the SAME catalog: start
        // twice — the second is a Conflict.
        let (fixture, light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);
        let _ = &light_ids;

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("first start should succeed");

        let second = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(second, ApiError::Conflict(_)), "{second:?}");

        wait_for_run(&ctx, started.run_id);
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
            let group_id = insert_group(
                conn,
                &NewGroup {
                    run_id,
                    group_key: &g.key,
                    instrume: g.instrume.as_deref(),
                    color_mode: color_mode_wire(g.color_mode),
                    filter: g.filter.as_deref(),
                    binning: Some(g.binning),
                    width: Some(g.width),
                    height: Some(g.height),
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
}
