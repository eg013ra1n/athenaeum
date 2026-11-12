//! Archive feature Tauri commands.

use super::AppState;
use athenaeum_core::api::archive as archive_api;
use athenaeum_core::archive::models::*;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_archive_roots(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    archive_api::list_archive_roots(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn add_archive_root(
    state: State<'_, AppState>,
    path: String,
    label: Option<String>,
) -> Result<i64, String> {
    archive_api::add_archive_root(&state.ctx, path, label).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_archive_root(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    archive_api::delete_archive_root(&state.ctx, id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_default_archive_root(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    archive_api::set_default_archive_root(&state.ctx, id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_archive_settings(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    archive_api::get_archive_settings(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_archive_root_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    archive_api::set_archive_root_path(&state.ctx, &path).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_archive_compression(
    state: State<'_, AppState>,
    compression: String,
) -> Result<(), String> {
    archive_api::set_archive_compression(&state.ctx, &compression).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn plan_archive_operation(
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    archive_root_path: Option<String>,
) -> Result<ArchivePlan, String> {
    archive_api::plan_archive_operation(
        &state.ctx,
        frames_set_id,
        &dispositions,
        compression,
        archive_root_path.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict_resolution: ConflictResolution,
    archive_root_path: Option<String>,
) -> Result<i64, String> {
    archive_api::start_archive_operation(
        state.ctx.clone(),
        Arc::new(crate::tauri_events::TauriProgressEmitter(app)),
        frames_set_id,
        &dispositions,
        compression,
        conflict_resolution,
        archive_root_path.as_deref(),
        false,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_archive_operation(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    archive_api::cancel_archive_operation(&state.ctx, operation_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_unfinished_archive_operations(
    state: State<'_, AppState>,
) -> Result<Vec<ArchiveOperationSummary>, String> {
    archive_api::list_unfinished_archive_operations(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn resume_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    archive_api::resume_archive_operation(
        state.ctx.clone(),
        Arc::new(crate::tauri_events::TauriProgressEmitter(app)),
        operation_id,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_restore_suggestions(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<serde_json::Value, String> {
    archive_api::get_restore_suggestions(&state.ctx, operation_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn rollback_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    archive_api::rollback_archive_operation(
        state.ctx.clone(),
        Arc::new(crate::tauri_events::TauriProgressEmitter(app)),
        operation_id,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_archive_zips(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<Vec<serde_json::Value>, String> {
    archive_api::list_archive_zips(&state.ctx, operation_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_archived_frame_sets(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    archive_api::list_archived_frame_sets(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_restore_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
    target_root_path: String,
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
) -> Result<(), String> {
    let emitter = Arc::new(crate::tauri_events::TauriProgressEmitter(app));
    archive_api::start_restore_operation(
        state.ctx.clone(),
        emitter,
        operation_id,
        target_root_path,
        overwrite_existing,
        keep_zip_after_restore,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_archive(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    archive_api::delete_archive(&state.ctx, operation_id)
        .map_err(|e| e.to_string())
}
