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
    NamedPreset, StackingPaths, StackingPresets, StackingRunDetail, StackingRunSummary,
    StackingSetConfig,
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
/// `TauriProgressEmitter`. Fix round 1, item 1: passes the SAME `POLICY`
/// `get_stacking_plan`/`set_stacking_paths` use (desktop's `AllowAll`) — a
/// config-supplied `paths.workingDir`/`paths.outputDir` override is a
/// caller-controlled path, so plan and start must agree on what the host
/// allows. Final fix wave, item 2: `api::start_stacking` runs the full
/// `build_plan` (per-frame hashing, master-flat reads on cardless flats) plus
/// one `insert_group` per group before it ever spawns the run thread — under
/// `spawn_blocking` (same reasoning as `get_stacking_plan`) so that work
/// stays off the async executor rather than blocking every other in-flight
/// command on this Tokio worker thread.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_stacking(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    set_id: i64,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, String> {
    let ctx = state.ctx.clone();
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    tokio::task::spawn_blocking(move || {
        api::start_stacking(
            ctx,
            emitter,
            &POLICY,
            env!("CARGO_PKG_VERSION").to_string(),
            set_id,
            config,
            rerun_from,
        )
    })
    .await
    .map_err(|e| format!("Start task panicked: {e}"))?
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

/// The three built-in stacking presets (Default / Fast preview / Maximum
/// quality) the Stacking tab's preset selector compares the current config
/// against. Pure — no `ServiceContext`, no DB.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_stacking_presets() -> Result<StackingPresets, String> {
    Ok(api::get_stacking_presets())
}

/// The user's OWN saved presets (M4d Task 4, ruling R-M4d-6), sorted by
/// name case-insensitively. Separate from the three built-ins above: those
/// are code, these live in the `stacking.presets` settings row.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_stacking_presets(state: State<'_, AppState>) -> Result<Vec<NamedPreset>, String> {
    api::list_stacking_presets(&state.ctx).map_err(|e| e.to_string())
}

/// Save (or replace, by case-insensitive name) one user preset. Returns the
/// full list afterwards, so the caller never needs a follow-up list call.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn save_stacking_preset(
    state: State<'_, AppState>,
    name: String,
    config: StackingConfig,
) -> Result<Vec<NamedPreset>, String> {
    api::save_stacking_preset(&state.ctx, name, config).map_err(|e| e.to_string())
}

/// Delete one user preset by name. Returns the full list afterwards.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_stacking_preset(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<NamedPreset>, String> {
    api::delete_stacking_preset(&state.ctx, name).map_err(|e| e.to_string())
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

/// JPEG bytes for one master light a run wrote (M4d Task 3, ruling
/// R-M4d-5). Returns [`tauri::ipc::Response`] rather than a bare `Vec<u8>`
/// for the same reason `read_fits_image` does: the IPC layer would otherwise
/// serialize the bytes as a JSON number array, several times their size.
///
/// M4d final review, Important #1: `Resolution::Full` is out of this
/// command's reach (`api::MAX_MASTER_PREVIEW_MAX_PX`), but a preview still
/// reads the master's FULL float buffer before downscaling — hundreds of MB
/// for a drizzled master — so this takes `image_semaphore` the same way
/// `commands_rustafits::read_fits_image_bytes` does, around the render only:
/// the cache-only check (`cached_master_light_preview`) runs first with no
/// permit, so a hit never waits behind an unrelated full-resolution render.
///
/// `level = "debug"`: the Results panel fetches one of these per group (and
/// a second per drizzled master), so an `info` span here would flood a run's
/// log with per-thumbnail boundary events.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_master_light_preview(
    state: State<'_, AppState>,
    run_id: i64,
    group_key: String,
    kind: api::MasterLightKind,
    max_px: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    let ctx = state.ctx.clone();
    let max_px = max_px.unwrap_or(api::DEFAULT_MASTER_PREVIEW_MAX_PX);

    let cached = {
        let ctx = ctx.clone();
        let group_key = group_key.clone();
        tokio::task::spawn_blocking(move || {
            api::cached_master_light_preview(&ctx, run_id, &group_key, kind, max_px)
        })
        .await
        .map_err(|e| format!("Master-preview cache task panicked: {e}"))?
        .map_err(|e| e.to_string())?
    };
    if let Some(bytes) = cached {
        return Ok(tauri::ipc::Response::new(bytes));
    }

    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem.acquire().await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        api::render_master_light_preview(&ctx, run_id, &group_key, kind, max_px)
    })
    .await
    .map_err(|e| format!("Master-preview task panicked: {e}"))?
    .map(tauri::ipc::Response::new)
    .map_err(|e| e.to_string())
}
