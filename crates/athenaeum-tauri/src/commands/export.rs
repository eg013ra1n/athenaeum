//! Export-related Tauri commands
//!
//! Commands for exporting frame sets to WBPP folder structure.

use crate::commands::AppState;
use crate::export::{
    frame_set_queries::ExportableFrameSet,
    models::{
        CalibrationRoute, ExportData, ExportMode, ExportResult, ExportSummary, WbppExportConfig,
    },
};
use athenaeum_core::api::lights::get_export_readiness as api_get_export_readiness;
use athenaeum_core::api::lights::{ExportReadiness, FlatNormMode, LightCalParams};
use std::sync::Arc;
use tauri::State;

// ============================================================================
// WBPP Export Config Commands
// ============================================================================

/// Get WBPP export configuration (returns default if not set)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_wbpp_export_config(
    state: State<'_, AppState>,
) -> Result<WbppExportConfig, String> {
    athenaeum_core::api::export::get_wbpp_export_config(&state.ctx).map_err(|e| e.to_string())
}

/// Save WBPP export configuration to database
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_wbpp_export_config(
    state: State<'_, AppState>,
    config: WbppExportConfig,
) -> Result<WbppExportConfig, String> {
    athenaeum_core::api::export::set_wbpp_export_config(&state.ctx, config)
        .map_err(|e| e.to_string())
}

/// Reset WBPP export configuration to defaults
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reset_wbpp_export_config(
    state: State<'_, AppState>,
) -> Result<WbppExportConfig, String> {
    athenaeum_core::api::export::reset_wbpp_export_config(&state.ctx).map_err(|e| e.to_string())
}

// ============================================================================
// Export Preview & Data Commands
// ============================================================================

/// Get export preview data for a frame set
///
/// Collects all light frames and their calibrations for preview before export.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_export_preview(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<ExportData, String> {
    athenaeum_core::api::export::get_export_preview(&state.ctx, frame_set_id).map_err(|e| e.to_string())
}

/// Get a list of available frame sets for export
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_exportable_frame_sets(
    state: State<'_, AppState>,
) -> Result<Vec<ExportableFrameSet>, String> {
    athenaeum_core::api::export::get_exportable_frame_sets(&state.ctx).map_err(|e| e.to_string())
}

/// Get calibration route for UI display
///
/// Returns a structured view of the export groups and their calibration trees,
/// suitable for displaying in the UI.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calibration_route(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<CalibrationRoute, String> {
    athenaeum_core::api::export::get_calibration_route(&state.ctx, frame_set_id).map_err(|e| e.to_string())
}

/// Export-readiness tallies for the WBPP export dialog's mode selector
/// (spec §12.2, v2 §4). Read-only; run under `spawn_blocking` so the catalog
/// queries stay off the async executor. Takes no calibration preferences —
/// readiness is about the inputs (masters built, lights linked), which no
/// dialog toggle can change.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_export_readiness(
    state: State<'_, AppState>,
    set_id: i64,
) -> Result<ExportReadiness, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api_get_export_readiness(&ctx, set_id))
        .await
        .map_err(|e| format!("Export readiness task panicked: {}", e))?
        .map_err(|e| e.to_string())
}

/// Export a frame set to WBPP folder structure
///
/// Creates a folder structure optimized for the ERP's Weighted Batch Preprocessing (WBPP):
/// ```text
/// output/
/// └── camera_{name}/
///     ├── darks/
///     │   └── (bias, dark, darkflat files)
///     └── flats_{filter}/
///         └── lights/
///             └── (light frames)
/// ```
///
/// `export_mode` selects what the lights + calibration side put on disk
/// (spec §12.2). It is optional: `Some` is an explicit per-invocation override
/// (what the mode selector sends), `None` falls back to the persisted
/// [`WbppExportConfig`]'s mode — the frontend loads that config asynchronously
/// and could present it as `null`, so the mode is now passed explicitly rather
/// than relying on a best-effort config sync.
///
/// `flat_norm` / `flat_norm_mode` / `params` / `hot_pixel` / `debayer` /
/// `format` are the calibrated-lights GENERATION options: in that mode this
/// command calibrates every light from its linked masters as it places it.
/// Each stays optional so a caller with no opinion keeps working — an
/// omitted one takes [`CalibratedLightOptions::default`]'s value for that
/// field; every other mode ignores them.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
#[allow(clippy::too_many_arguments)]
pub async fn export_to_wbpp(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    frame_set_id: i64,
    output_dir: String,
    use_symlinks: bool,
    export_mode: Option<ExportMode>,
    flat_norm: Option<bool>,
    flat_norm_mode: Option<FlatNormMode>,
    params: Option<LightCalParams>,
    hot_pixel: Option<bool>,
    debayer: Option<bool>,
    format: Option<athenaeum_core::fits_writer::OutputFormat>,
) -> Result<ExportResult, String> {
    let emitter = Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle));
    Ok(athenaeum_core::api::export::export_to_wbpp(
        state.ctx.clone(),
        emitter,
        frame_set_id,
        output_dir,
        use_symlinks,
        export_mode,
        flat_norm,
        flat_norm_mode,
        params,
        hot_pixel,
        debayer,
        format,
        None,
    )
    .await)
}

/// Cancel an active export
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_export(frame_set_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    athenaeum_core::api::export::cancel_export(&state.ctx, frame_set_id).map_err(|e| e.to_string())
}

/// Get enhanced export summary for the new UI
///
/// Returns comprehensive export data with:
/// - Equipment info (cameras, telescopes, date range)
/// - Filter groups with exposure breakdown
/// - Calibration details with match quality
/// - Folder structure preview
/// - Detailed warnings with context
///
/// Drawn for the mode the tab has selected — the tree, file total and size
/// estimate describe THAT export. `export_mode` `None` falls back to the
/// persisted config like `export_to_wbpp`; the six generation options are
/// read by `calibratedLights` only (the debayer flag decides the `c_*` names
/// in the tree) and default the same way as the export's.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
#[allow(clippy::too_many_arguments)]
pub async fn get_export_summary(
    state: State<'_, AppState>,
    frame_set_id: i64,
    export_mode: Option<ExportMode>,
    flat_norm: Option<bool>,
    flat_norm_mode: Option<FlatNormMode>,
    params: Option<LightCalParams>,
    hot_pixel: Option<bool>,
    debayer: Option<bool>,
    format: Option<athenaeum_core::fits_writer::OutputFormat>,
) -> Result<ExportSummary, String> {
    athenaeum_core::api::export::get_export_summary(
        &state.ctx,
        frame_set_id,
        export_mode,
        flat_norm,
        flat_norm_mode,
        params,
        hot_pixel,
        debayer,
        format,
    )
    .map_err(|e| e.to_string())
}
