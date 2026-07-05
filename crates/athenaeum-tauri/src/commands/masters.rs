// Master-build commands — orchestrates the compute-queue-backed master
// build (Task 12) plus dependency-ordered batch builds and in-place rebuild
// (Task 13).
//
// Thin wrappers only: extraction + handler call + error mapping. Business
// logic lives in `athenaeum_core::api::masters`.

use std::sync::Arc;

use tauri::State;

use athenaeum_core::api::masters as api;

use crate::tauri_events::TauriProgressEmitter;

use super::AppState;

pub use athenaeum_core::api::masters::{
    BatchBuildReport, MasterBuildPreview, MasterProvenanceInfo, MasterRecipe,
};

/// Preview a master build: validation + recipe/precal selection + target
/// path. Pure DB work (no pixel I/O — precal pixels only ever load inside
/// the build thread via `load_precal_pixels`), but still run under
/// `spawn_blocking` so the queries stay off the async executor (matches the
/// `analyze_frame_set` wrapper precedent).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn preview_master_build(
    state: State<'_, AppState>,
    set_id: i64,
    recipe: MasterRecipe,
) -> Result<MasterBuildPreview, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::preview_master_build(&ctx, set_id, &recipe))
        .await
        .map_err(|e| format!("Preview task panicked: {}", e))?
        .map_err(|e| e.to_string())
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

/// Enqueue builds for many sets, dependency-ordered (Bias/DarkFlat -> Dark ->
/// Flat). Sets already superseded / too small / themselves masters / unknown
/// are skipped with a per-set reason instead of failing the whole batch.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_master_builds_batch(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    set_ids: Vec<i64>,
    recipe: MasterRecipe,
) -> Result<BatchBuildReport, String> {
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    api::start_master_builds_batch(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        set_ids,
        recipe,
    )
    .map_err(|e| e.to_string())
}

/// Re-integrate an existing built master in place (same path), refreshing
/// its provenance instead of registering a new master set.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn rebuild_master(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    master_set_id: i64,
) -> Result<(), String> {
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    api::rebuild_master(
        state.ctx.clone(),
        emitter,
        env!("CARGO_PKG_VERSION").to_string(),
        master_set_id,
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

/// Archive a SUPERSEDED calibration set's original member frames into a ZIP
/// (Task 14). Thin wrapper: plan+commit happen synchronously inside
/// `api::masters::archive_originals`, which returns as soon as the operation
/// is queued on the shared disk worker. Progress/completion arrive via the
/// existing `archive-progress` / `archive-finished` events, same as the
/// frame-set archive flow.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn archive_calibration_originals(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    calibration_set_id: i64,
) -> Result<i64, String> {
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    api::archive_originals(state.ctx.clone(), emitter, calibration_set_id).map_err(|e| e.to_string())
}

/// Restore a SUPERSEDED calibration set's archived originals from their zip
/// (mirror of `archive_calibration_originals`, reverse direction). Thin
/// wrapper: op-id resolution + zip-exists validation happen synchronously
/// inside `api::masters::restore_originals` (returns an actionable error
/// string if the zip is missing), which then enqueues the restore on the
/// shared disk worker exactly like `archive_originals` does. Progress/
/// completion arrive via the existing `archive-progress` / `archive-finished`
/// events (`kind: "restore"`), same as the frame-set restore flow.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn restore_calibration_originals(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    calibration_set_id: i64,
) -> Result<i64, String> {
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    api::restore_originals(state.ctx.clone(), emitter, calibration_set_id).map_err(|e| e.to_string())
}
