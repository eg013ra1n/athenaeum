use std::collections::HashMap;
use tauri::State;

use crate::commands::AppState;

pub use athenaeum_core::api::missing_files::MissingFileRecord;

/// Sync missing files to the database after a rescan
/// This updates the missing_files table with the current state
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn sync_missing_files(
    root_id: i64,
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    athenaeum_core::api::missing_files::sync_missing_files(&state.ctx, root_id, &file_ids)
        .map_err(|e| e.to_string())
}

/// Get missing files for a specific scan root with full details
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_missing_files(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<MissingFileRecord>, String> {
    athenaeum_core::api::missing_files::get_missing_files(&state.ctx, root_id)
        .map_err(|e| e.to_string())
}

/// Get missing files counts for all scan roots (for indicators)
/// Returns a map of scan_root_id -> count (only 'missing' status, not 'ignored')
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_missing_files_counts(
    state: State<'_, AppState>,
) -> Result<HashMap<i64, i64>, String> {
    athenaeum_core::api::scan_roots::get_missing_files_counts(&state.ctx)
        .map_err(|e| e.to_string())
}

/// Recheck missing files for a scan root and update their status
/// Returns the list of files that are still missing
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn recheck_missing_files(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<MissingFileRecord>, String> {
    athenaeum_core::api::missing_files::recheck_missing_files(&state.ctx, root_id)
        .map_err(|e| e.to_string())
}

/// Mark a missing file as ignored
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn ignore_missing_file(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    athenaeum_core::api::missing_files::ignore_missing_file(&state.ctx, file_id)
        .map_err(|e| e.to_string())
}

/// Remove ignored status from a missing file
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn unignore_missing_file(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    athenaeum_core::api::missing_files::unignore_missing_file(&state.ctx, file_id)
        .map_err(|e| e.to_string())
}

/// Delete files from the database entirely (removes from files, frames, missing_files tables)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_missing_files(
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    athenaeum_core::api::missing_files::delete_missing_files(&state.ctx, &file_ids)
        .map_err(|e| e.to_string())
}

/// Relocate a missing file to a new path
/// Updates the file path in the database and removes from missing_files
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn relocate_missing_file(
    file_id: i64,
    new_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    athenaeum_core::api::missing_files::relocate_missing_file(
        &state.ctx,
        file_id,
        new_path,
        &athenaeum_core::api::PathPolicy::AllowAll,
    )
    .map_err(|e| e.to_string())
}
