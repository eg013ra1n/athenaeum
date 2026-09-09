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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::api::{db, ApiError, PathPolicy};
use crate::calibration_library::cosmetic::HotPixelMapOutcome;
use crate::db::stacking::{
    finish_run, get_run, insert_group, insert_run, set_run_status, upsert_artifact, NewArtifact,
    NewGroup, NewRun,
};
use crate::events::{emit_event, ProgressEmitter};
use crate::export::{execute_generation, resolve_generation_cached};
use crate::integration::IntegrationError;
use crate::services::compute_queue::ComputeJobKind;
use crate::services::{ServiceContext, StackHandle};
#[cfg(test)]
use crate::stacking::config::config_hash;
use crate::stacking::config::{ReferenceMode, StackingConfig};
use crate::stacking::groups::{group_frames, set_slug, ColorMode, GroupFrame, IntegrationGroup};
use crate::stacking::paths::WorkingLayout;
use crate::stacking::plan::{build_plan, is_fresh, HashMemo, Stage};
use crate::stacking::provenance::{RunSummary, SummaryMeasurement, SummaryReference};

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
    /// "calibration failed: …")` here; Tasks 7-8 push their own stages'
    /// failures the same way and read the union back to build
    /// `stacking_run_frames` rows / `SummaryFrame.exclusion_reason`.
    #[allow(dead_code)]
    pub(crate) runtime_exclusions: Vec<(i64, String)>,
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

    // Tasks 7-8: stage_measure, stage_reference, stage_register,
    // stage_normalize/integrate, stage_output, cleanup.

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

/// Build a [`RunContext`] directly from fixture data, bypassing
/// [`start_stacking`]'s DB/plan machinery — the calibrate-stage tests below
/// (and Tasks 7-8's own) call [`stage_calibrate`] (or their own stage
/// functions) straight off a context built this way. `run_id` is the
/// caller's to choose: pass a real `stacking_runs.id` (from
/// [`crate::db::stacking::insert_run`]) for a test that also asserts on the
/// DB row or the provenance file; a fixed dummy otherwise — the calibrate
/// stage's reuse logic never depends on it, since `stacking_artifacts` is
/// keyed by `(frames_set_id, group_key, kind, frame_id)`, never by `run_id`.
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
        group_ids: HashMap::new(),
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
}
