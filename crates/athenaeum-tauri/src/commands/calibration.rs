// Calibration commands - calibration frame matching and library management
//
// Thin wrappers only: extraction + handler call + error mapping. Business
// logic lives in `athenaeum_core::api::calibration` (Task 11 conversion —
// see `.superpowers/sdd/p1-task-11-report.md`).

use crate::models::*;
use tauri::State;

use athenaeum_core::api::calibration as api;

use super::AppState;

// ========== Equipment & Dark Library Commands ==========

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_equipment_cameras(state: State<'_, AppState>) -> Result<Vec<CameraStats>, String> {
    api::get_equipment_cameras(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_dark_library(state: State<'_, AppState>, instrume: String) -> Result<Vec<CalibrationSetDetail>, String> {
    api::get_dark_library(&state.ctx, instrume).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn has_dark_library(state: State<'_, AppState>, instrume: String) -> Result<bool, String> {
    api::has_dark_library(&state.ctx, instrume).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_master_dark_library(state: State<'_, AppState>, instrume: String) -> Result<Vec<CalibrationSetDetail>, String> {
    api::get_master_dark_library(&state.ctx, instrume).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn has_master_dark_library(state: State<'_, AppState>, instrume: String) -> Result<bool, String> {
    api::has_master_dark_library(&state.ctx, instrume).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_master_flat_library(state: State<'_, AppState>, instrume: String) -> Result<Vec<CalibrationSetDetail>, String> {
    api::get_master_flat_library(&state.ctx, instrume).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn has_master_flat_library(state: State<'_, AppState>, instrume: String) -> Result<bool, String> {
    api::has_master_flat_library(&state.ctx, instrume).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_set_frames(
    state: State<'_, AppState>,
    set_id: i64,
) -> Result<Vec<crate::models::FileWithFrame>, String> {
    api::get_calibration_set_frames(&state.ctx, set_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_set_consumers(
    state: State<'_, AppState>,
    set_id: i64,
) -> Result<Vec<crate::models::CalibrationSetConsumer>, String> {
    api::get_calibration_set_consumers(&state.ctx, set_id).map_err(|e| e.to_string())
}

// ===== CALIBRATION FINDER COMMANDS =====

/// Find and link calibration for all light frames in a frame set
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn find_calibration_for_frame_set(
    frame_set_id: i64,
    flat_date_warning_days: Option<i64>,
    dark_date_warning_days: Option<i64>,
    flat_pattern: Option<String>,
    manual_flat_selections: Option<std::collections::HashMap<String, i64>>,
    state: State<'_, AppState>,
) -> Result<crate::calibration::processor::ProcessingStats, String> {
    api::find_calibration_for_frame_set(
        &state.ctx,
        frame_set_id,
        flat_date_warning_days,
        dark_date_warning_days,
        flat_pattern,
        manual_flat_selections,
    )
    .map_err(|e| e.to_string())
}

/// Get calibration hierarchy organized by Date → Camera → Filter for a frame set
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_hierarchy_for_frame_set(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<crate::models::CalibrationHierarchyView, String> {
    api::get_calibration_hierarchy_for_frame_set(&state.ctx, frame_set_id).map_err(|e| e.to_string())
}

// ========== Calibration Matching Config Commands ==========

/// Get the current calibration matching configuration
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_matching_config(
    state: State<'_, AppState>,
) -> Result<crate::calibration::CalibrationMatchingConfig, String> {
    api::get_calibration_matching_config(&state.ctx).map_err(|e| e.to_string())
}

/// Set the calibration matching configuration
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_calibration_matching_config(
    config: crate::calibration::CalibrationMatchingConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    api::set_calibration_matching_config(&state.ctx, config).map_err(|e| e.to_string())
}

/// Reset calibration matching configuration to defaults
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reset_calibration_matching_config(
    state: State<'_, AppState>,
) -> Result<crate::calibration::CalibrationMatchingConfig, String> {
    api::reset_calibration_matching_config(&state.ctx).map_err(|e| e.to_string())
}

// ========== Manual Calibration Selection Commands ==========

/// Get average parameters of light frames for manual selection display
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_light_frame_parameters(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<LightFrameParameters, String> {
    api::get_light_frame_parameters(&state.ctx, frame_ids).map_err(|e| e.to_string())
}

/// Get calibration sets with match scores for manual selection.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_sets_for_manual_selection(
    frame_ids: Vec<i64>,
    calibration_type: String, // "flat", "dark", "bias"
    show_all: bool,
    state: State<'_, AppState>,
) -> Result<Vec<CalibrationSetWithScore>, String> {
    api::get_calibration_sets_for_manual_selection(&state.ctx, frame_ids, calibration_type, show_all)
        .map_err(|e| e.to_string())
}

/// Manually assign a calibration set to light frames
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn manual_assign_calibration(
    frame_ids: Vec<i64>,
    calibration_set_id: i64,
    calibration_type: String, // "Flat", "Dark", "Bias"
    state: State<'_, AppState>,
) -> Result<usize, String> {
    api::manual_assign_calibration(&state.ctx, frame_ids, calibration_set_id, calibration_type).map_err(|e| e.to_string())
}

/// Remove manual calibration override and allow auto-find to reassign
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn clear_manual_calibration_override(
    frame_ids: Vec<i64>,
    calibration_type: Option<String>, // None = clear all types
    state: State<'_, AppState>,
) -> Result<usize, String> {
    api::clear_manual_calibration_override(&state.ctx, frame_ids, calibration_type).map_err(|e| e.to_string())
}

// ========== Refresh Calibration Library Command ==========

/// Refresh calibration library for a specific camera
///
/// This command preserves calibration set IDs by:
/// 1. Clearing frame memberships (but keeping the sets)
/// 2. Queries all calibration frame IDs for the camera (Flat, Dark, Bias, DarkFlat)
/// 3. Reclusters and assigns frames to sets - sets are matched by params + date overlap
/// 4. Deletes orphaned sets (sets with 0 frames after reclustering)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn refresh_calibration_library_for_camera(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<crate::calibration::scan_integration::CalibrationScanResult, String> {
    api::refresh_calibration_library_for_camera(&state.ctx, instrume).map_err(|e| e.to_string())
}

// ========== Sub-Calibration Selection Commands ==========

/// Get parameters of a calibration set for sub-calibration selection display
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_set_parameters(
    set_id: i64,
    state: State<'_, AppState>,
) -> Result<CalibrationSetParameters, String> {
    api::get_calibration_set_parameters(&state.ctx, set_id).map_err(|e| e.to_string())
}

/// Get sub-calibration candidates for a Flat or Dark calibration set.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_subcalibration_sets_for_manual_selection(
    set_id: i64,
    calibration_type: String, // "dark", "darkflat", "bias"
    show_all: bool,
    state: State<'_, AppState>,
) -> Result<Vec<CalibrationSetWithScore>, String> {
    api::get_subcalibration_sets_for_manual_selection(&state.ctx, set_id, calibration_type, show_all)
        .map_err(|e| e.to_string())
}

/// Manually assign a sub-calibration set to a calibration set
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn manual_assign_subcalibration(
    source_set_id: i64,
    calibration_set_id: i64,
    calibration_type: String, // "Dark", "DarkFlat", "Bias"
    state: State<'_, AppState>,
) -> Result<(), String> {
    api::manual_assign_subcalibration(&state.ctx, source_set_id, calibration_set_id, calibration_type)
        .map_err(|e| e.to_string())
}

/// Clear sub-calibration override for a calibration set
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn clear_subcalibration_override(
    source_set_id: i64,
    calibration_type: Option<String>, // None = clear all sub-calibrations
    state: State<'_, AppState>,
) -> Result<usize, String> {
    api::clear_subcalibration_override(&state.ctx, source_set_id, calibration_type).map_err(|e| e.to_string())
}

// ========== Custom Metadata Editing Commands ==========

/// Bulk update calibration set metadata
/// Saves original values to calibration_set_originals table before updating
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn bulk_update_calibration_metadata(
    set_ids: Vec<i64>,
    edits: crate::models::CalibrationMetadataEdits,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    api::bulk_update_calibration_metadata(&state.ctx, set_ids, edits).map_err(|e| e.to_string())
}

/// Bulk restore calibration set metadata from originals
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn bulk_restore_calibration_metadata(set_ids: Vec<i64>, state: State<'_, AppState>) -> Result<usize, String> {
    api::bulk_restore_calibration_metadata(&state.ctx, set_ids).map_err(|e| e.to_string())
}

/// Get all set IDs that have custom metadata edits for a given camera
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_custom_metadata_set_ids(instrume: String, state: State<'_, AppState>) -> Result<Vec<i64>, String> {
    api::get_custom_metadata_set_ids(&state.ctx, instrume).map_err(|e| e.to_string())
}
