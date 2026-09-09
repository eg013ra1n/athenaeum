// Stacking-pipeline commands (M1 Plan 5a Task 9) — thin wrappers only.
// Business logic lives in `athenaeum_core::api::stacking`.

use std::sync::Arc;

use tauri::State;

use athenaeum_core::api::stacking as api;
use athenaeum_core::api::PathPolicy;
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::paths::{CleanupWhat, WorkUsage};
use athenaeum_core::stacking::plan::{StackingPlan, Stage};
use athenaeum_core::stacking::run::StartedStacking;

use crate::tauri_events::TauriProgressEmitter;

use super::AppState;

pub use athenaeum_core::api::stacking::{
    StackingPaths, StackingRunDetail, StackingRunSummary, StackingSetConfig,
};

/// Desktop has no path sandbox — every stacking folder handler uses
/// `PathPolicy::AllowAll`, same as `set_transfer_paths`.
const POLICY: PathPolicy = PathPolicy::AllowAll;

/// What a stacking run would do for `set_id` right now: groups, gate
/// blockers, stale-stage report, folder/space state. Pure DB reads plus
/// cheap filesystem probes, but run under `spawn_blocking` (same reasoning
/// as `preview_master_build`) so the work stays off the async executor.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_plan(
    state: State<'_, AppState>,
    set_id: i64,
    config: Option<StackingConfig>,
) -> Result<StackingPlan, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_stacking_plan(&ctx, &POLICY, set_id, config))
        .await
        .map_err(|e| format!("Plan task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

/// Start a stacking run. Returns as soon as the run thread is spawned;
/// `stacking-progress` / `stacking-complete` events arrive through
/// `TauriProgressEmitter`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_stacking(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    set_id: i64,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, String> {
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    api::start_stacking(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        set_id,
        config,
        rerun_from,
    )
    .map_err(|e| e.to_string())
}

/// Cancel an active stacking run (queued-in-compute-queue or running).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_stacking(state: State<'_, AppState>, run_id: i64) -> Result<(), String> {
    api::cancel_stacking(&state.ctx, run_id).map_err(|e| e.to_string())
}

/// A frame set's run history, newest first.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_runs(
    state: State<'_, AppState>,
    set_id: i64,
    limit: Option<usize>,
) -> Result<Vec<StackingRunSummary>, String> {
    api::get_stacking_runs(&state.ctx, set_id, limit).map_err(|e| e.to_string())
}

/// One run's full detail (run + groups + frames + parsed provenance).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_run(
    state: State<'_, AppState>,
    run_id: i64,
) -> Result<StackingRunDetail, String> {
    api::get_stacking_run(&state.ctx, run_id).map_err(|e| e.to_string())
}

/// A frame set's resolved stacking config (its own override, else the
/// global default, else the built-in default) plus its manual exclusions.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_config(
    state: State<'_, AppState>,
    set_id: i64,
) -> Result<StackingSetConfig, String> {
    api::get_stacking_config(&state.ctx, set_id).map_err(|e| e.to_string())
}

/// Persist a frame set's stacking config + manual frame exclusions.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_stacking_config(
    state: State<'_, AppState>,
    set_id: i64,
    config: StackingConfig,
    excluded_frame_ids: Vec<i64>,
) -> Result<(), String> {
    api::set_stacking_config(&state.ctx, set_id, config, excluded_frame_ids)
        .map_err(|e| e.to_string())
}

/// The global stacking defaults.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_defaults(state: State<'_, AppState>) -> Result<StackingConfig, String> {
    api::get_stacking_defaults(&state.ctx).map_err(|e| e.to_string())
}

/// Persist the global stacking defaults.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_stacking_defaults(
    state: State<'_, AppState>,
    config: StackingConfig,
) -> Result<(), String> {
    api::set_stacking_defaults(&state.ctx, config).map_err(|e| e.to_string())
}

/// Reset the global stacking defaults to the built-in default.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reset_stacking_defaults(state: State<'_, AppState>) -> Result<StackingConfig, String> {
    api::reset_stacking_defaults(&state.ctx).map_err(|e| e.to_string())
}

/// The two stacking folders as configured/effective right now.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_paths(state: State<'_, AppState>) -> Result<StackingPaths, String> {
    api::get_stacking_paths(&state.ctx).map_err(|e| e.to_string())
}

/// Persist the two stacking folders (`null` = reset that one).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_stacking_paths(
    state: State<'_, AppState>,
    working: Option<String>,
    output: Option<String>,
) -> Result<StackingPaths, String> {
    api::set_stacking_paths(&state.ctx, &POLICY, working, output).map_err(|e| e.to_string())
}

/// A frame set's working-layout byte usage.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_work_usage(
    state: State<'_, AppState>,
    set_id: i64,
) -> Result<WorkUsage, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_stacking_work_usage(&ctx, set_id))
        .await
        .map_err(|e| format!("Work-usage task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

/// Remove part or all of a frame set's working-layout subtrees. Refuses
/// while a run is active for the set.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cleanup_stacking_work(
    state: State<'_, AppState>,
    set_id: i64,
    what: CleanupWhat,
) -> Result<u64, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::cleanup_stacking_work(&ctx, set_id, what))
        .await
        .map_err(|e| format!("Cleanup task panicked: {e}"))?
        .map_err(|e| e.to_string())
}
