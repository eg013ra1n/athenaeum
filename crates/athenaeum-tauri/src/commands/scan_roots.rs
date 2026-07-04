// Scan root commands - directory scanning and monitoring
//
// Thin wrappers only: extraction + policy construction + handler call + error
// mapping. Business logic lives in `athenaeum_core::api::scan_roots` (Task 9
// conversion — see `.superpowers/sdd/p1-task-9-report.md`).

use crate::models::*;
use crate::tauri_events::TauriProgressEmitter;
use tauri::State;

use athenaeum_core::api::scan_roots as api;
use athenaeum_core::api::PathPolicy;

use super::AppState;

pub use athenaeum_core::api::scan_roots::{RescanResultDto, ScanResultDto};

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn add_scan_root(
    path: String,
    kind: Option<String>,
    state: State<'_, AppState>,
) -> Result<ScanRoot, String> {
    api::add_scan_root(&state.ctx, path, &PathPolicy::AllowAll, kind).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_scan_roots(state: State<'_, AppState>) -> Result<Vec<ScanRoot>, String> {
    api::get_scan_roots(&state.ctx).map_err(|e| e.to_string())
}

/// The (single) calibration library root, if configured — used by the
/// Settings UI's "Calibration Library" section.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_library_root(state: State<'_, AppState>) -> Result<Option<ScanRoot>, String> {
    api::get_calibration_library_root(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_scan_root(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    api::delete_scan_root(&state.ctx, id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_scan(root_id: i64, state: State<'_, AppState>) -> Result<ScanResultDto, String> {
    api::start_scan(&state.ctx, root_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn rescan_all_for_content_hash(state: State<'_, AppState>) -> Result<RescanResultDto, String> {
    api::rescan_all_for_content_hash(&state.ctx).map_err(|e| e.to_string())
}

/// Relink files from old scan root to new location
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn relink_scan_root(
    root_id: i64,
    new_path: String,
    state: State<'_, AppState>,
) -> Result<RelinkResult, String> {
    api::relink_scan_root(&state.ctx, root_id, new_path, &PathPolicy::AllowAll)
        .map_err(|e| e.to_string())
}

/// Check availability of all scan roots
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn check_all_scan_roots_availability(
    state: State<'_, AppState>,
) -> Result<Vec<(i64, bool)>, String> {
    api::check_all_scan_roots_availability(&state.ctx).map_err(|e| e.to_string())
}

/// Check for missing files within a scan root
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn check_missing_files_in_scan_root(
    root_id: i64,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::OrphanedFile>, String> {
    let emitter = TauriProgressEmitter(app_handle.clone());
    api::check_missing_files_in_scan_root(&state.ctx, root_id, &emitter).map_err(|e| e.to_string())
}

/// Toggle unique_camera flag (flag-only, cascade happens on re-scan)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_scan_root_unique_camera_flag(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    api::set_scan_root_unique_camera_flag(&state.ctx, id, enabled).map_err(|e| e.to_string())
}

/// Toggle the background-monitoring flag for a scan root. The monitor service
/// polls only roots with this flag set. Persists immediately; the next monitor
/// tick respects the new value.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_scan_root_monitor_enabled(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    api::set_scan_root_monitor_enabled(&state.ctx, id, enabled, &state.monitor)
        .map_err(|e| e.to_string())
}

/// Start a scan with progress events - runs synchronously but emits progress events
/// The frontend should call this and listen to scan-progress/scan-complete events
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_scan_with_progress(
    root_id: i64,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ScanResultDto, String> {
    let scan_emitter = TauriProgressEmitter(app_handle.clone());
    api::start_scan_with_progress(&state.ctx, root_id, &scan_emitter).map_err(|e| e.to_string())
}

/// Cancel an active scan
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_scan(root_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    api::cancel_scan(&state.ctx, root_id).map_err(|e| e.to_string())
}
