// Master-build commands — orchestrates the compute-queue-backed master
// build (Task 12).
//
// Thin wrappers only: extraction + handler call + error mapping. Business
// logic lives in `athenaeum_core::api::masters`.

use std::sync::Arc;

use tauri::State;

use athenaeum_core::api::masters as api;

use crate::tauri_events::TauriProgressEmitter;

use super::AppState;

pub use athenaeum_core::api::masters::{MasterBuildPreview, MasterProvenanceInfo, MasterRecipe};

/// Preview a master build: validation + recipe/precal resolution + target
/// path, no thread spawned.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn preview_master_build(
    state: State<'_, AppState>,
    set_id: i64,
    recipe: MasterRecipe,
) -> Result<MasterBuildPreview, String> {
    api::preview_master_build(&state.ctx, set_id, &recipe).map_err(|e| e.to_string())
}

/// Start a master build. Validates, registers the cancel handle, and spawns
/// a dedicated named thread that does the actual queue-admission +
/// integration + write + register; returns as soon as the thread is
/// spawned. Progress/completion arrive via `master-build-progress` /
/// `master-build-complete` events emitted through `TauriProgressEmitter`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_master_build(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    set_id: i64,
    recipe: MasterRecipe,
) -> Result<(), String> {
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    api::start_master_build(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        set_id,
        recipe,
    )
    .map_err(|e| e.to_string())
}

/// Cancel an active master build (queued or running).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_master_build(state: State<'_, AppState>, set_id: i64) -> Result<(), String> {
    api::cancel_master_build(&state.ctx, set_id).map_err(|e| e.to_string())
}

/// Provenance + rebuildability info for a master calibration set.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_master_provenance(
    state: State<'_, AppState>,
    master_set_id: i64,
) -> Result<Option<MasterProvenanceInfo>, String> {
    api::get_master_provenance(&state.ctx, master_set_id).map_err(|e| e.to_string())
}
