//! Export-related Tauri commands
//!
//! Commands for exporting frame sets to PixInsight WBPP folder structure.

use crate::commands::{AppState, ExportHandle};
use crate::export::{
    apply_export_mode, collect_export_data, collect_export_summary, organize_files_wbpp,
    resolve_export_mode,
    frame_set_queries::{self, ExportableFrameSet},
    models::{
        CalibrationRoute, ExportCompleteEvent, ExportData, ExportMode, ExportProgressEvent,
        ExportResult, ExportSummary, WbppExportConfig,
    },
};
use athenaeum_core::api::lights::get_export_readiness as api_get_export_readiness;
use athenaeum_core::api::lights::{ExportReadiness, FlatNormMode, LightCalParams};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, State};

const WBPP_CONFIG_KEY: &str = "export.wbpp_config";

// ============================================================================
// WBPP Export Config Commands
// ============================================================================

/// Get WBPP export configuration (returns default if not set)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_wbpp_export_config(
    state: State<'_, AppState>,
) -> Result<WbppExportConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    load_wbpp_config(&conn)
}

/// Save WBPP export configuration to database
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_wbpp_export_config(
    state: State<'_, AppState>,
    config: WbppExportConfig,
) -> Result<WbppExportConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![WBPP_CONFIG_KEY, json],
    )
    .map_err(|e| e.to_string())?;

    Ok(config)
}

/// Reset WBPP export configuration to defaults
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reset_wbpp_export_config(
    state: State<'_, AppState>,
) -> Result<WbppExportConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![WBPP_CONFIG_KEY],
    )
    .map_err(|e| e.to_string())?;

    Ok(WbppExportConfig::default())
}

/// Load WBPP config from DB or return default
fn load_wbpp_config(conn: &rusqlite::Connection) -> Result<WbppExportConfig, String> {
    let result: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![WBPP_CONFIG_KEY],
            |row| row.get(0),
        )
        .ok();

    match result {
        Some(json) => serde_json::from_str(&json).map_err(|e| {
            tracing::warn!(error = %e, "failed to parse WBPP config");
            e.to_string()
        }),
        None => Ok(WbppExportConfig::default()),
    }
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
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())
}

/// Get a list of available frame sets for export
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_exportable_frame_sets(
    state: State<'_, AppState>,
) -> Result<Vec<ExportableFrameSet>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    frame_set_queries::get_exportable_frame_sets(&conn).map_err(|e| e.to_string())
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
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    frame_set_queries::get_calibration_route(&conn, frame_set_id).map_err(|e| e.to_string())
}

/// Export-readiness tallies for the WBPP export dialog's mode selector
/// (spec §12.2). Read-only; run under `spawn_blocking` so the derived-status
/// queries stay off the async executor. `params` is optional so the pre-mode-UI
/// frontend keeps compiling — an omitted arg defaults to
/// [`LightCalParams::default`].
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_export_readiness(
    state: State<'_, AppState>,
    set_id: i64,
    mode: ExportMode,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: Option<LightCalParams>,
) -> Result<ExportReadiness, String> {
    let params = params.unwrap_or_default();
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || {
        api_get_export_readiness(&ctx, set_id, mode, flat_norm, flat_norm_mode, params)
    })
    .await
    .map_err(|e| format!("Export readiness task panicked: {}", e))?
    .map_err(|e| e.to_string())
}

/// Export a frame set to PixInsight WBPP folder structure
///
/// Creates a folder structure optimized for PixInsight's Weighted Batch Preprocessing (WBPP):
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
/// than relying on a best-effort config sync. `flat_norm` / `flat_norm_mode` /
/// `params` are the caller's calibration preferences, used only by the
/// `calibratedLights` strict gate; they are optional so the pre-mode-UI frontend
/// keeps working (defaults: normalize ON, central-third, default advanced params).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
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
) -> Result<ExportResult, String> {
    // Create cancel flag and register export
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.insert(frame_set_id, ExportHandle {
            cancel_flag: cancel_flag.clone(),
        });
    }

    // Emit collecting phase
    let _ = app_handle.emit(
        "export-progress",
        ExportProgressEvent {
            frame_set_id,
            current: 0,
            total: 0,
            percent: 0.0,
            current_file: None,
            phase: "collecting".to_string(),
        },
    );

    // Collect export data and config
    let (mut export_data, config) = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        let data = collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?;
        let cfg = load_wbpp_config(&conn).unwrap_or_default();
        (data, cfg)
    };
    // Explicit per-invocation override wins over the persisted config's mode.
    let mode = resolve_export_mode(export_mode, &config);

    let output_path = PathBuf::from(&output_dir);

    let make_fail = |error: String| ExportResult {
        success: false,
        output_dir: output_dir.clone(),
        files_organized: 0,
        scripts_generated: Vec::new(),
        warnings: Vec::new(),
        error: Some(error),
    };

    // Strict gate (spec §12.2) + mode transform. `prepare` returns the
    // per-set omission warnings to fold into the final result, or an Err
    // message that aborts the export before any file is written.
    let prepare = |export_data: &mut ExportData| -> Result<Vec<String>, String> {
        if mode == ExportMode::CalibratedLights {
            let readiness = api_get_export_readiness(
                &state.ctx,
                frame_set_id,
                mode,
                flat_norm.unwrap_or(true),
                flat_norm_mode.unwrap_or(FlatNormMode::CentralThird),
                params.clone().unwrap_or_default(),
            )
            .map_err(|e| e.to_string())?;
            let not_ready = readiness.missing + readiness.stale;
            if not_ready > 0 {
                return Err(format!(
                    "{} of {} lights lack a fresh calibrated output — run Calibrate Lights first",
                    not_ready, readiness.total
                ));
            }
        }
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        apply_export_mode(&conn, export_data, mode).map_err(|e| e.to_string())
    };

    // Organize files into WBPP structure (with progress events + cancel support)
    let cancelled = cancel_flag.load(Ordering::Relaxed);
    let result = if cancelled {
        make_fail("Export cancelled".to_string())
    } else {
        match prepare(&mut export_data) {
            Err(e) => make_fail(e),
            Ok(mode_warnings) => {
                let export_emitter = crate::tauri_events::TauriProgressEmitter(app_handle.clone());
                match organize_files_wbpp(
                    &output_path,
                    &export_data,
                    use_symlinks,
                    &config,
                    Some(&export_emitter as &dyn athenaeum_core::events::ProgressEmitter),
                    frame_set_id,
                    &cancel_flag,
                ) {
                    Ok(org_result) => {
                        let was_cancelled = cancel_flag.load(Ordering::Relaxed);
                        let mut warnings = org_result.warnings;
                        warnings.extend(mode_warnings);
                        if was_cancelled {
                            ExportResult {
                                success: false,
                                output_dir: output_dir.clone(),
                                files_organized: org_result.files_organized,
                                scripts_generated: Vec::new(),
                                warnings,
                                error: Some("Export cancelled".to_string()),
                            }
                        } else {
                            ExportResult {
                                success: true,
                                output_dir: output_dir.clone(),
                                files_organized: org_result.files_organized,
                                scripts_generated: Vec::new(),
                                warnings,
                                error: None,
                            }
                        }
                    }
                    Err(e) => make_fail(format!("Failed to organize files: {}", e)),
                }
            }
        }
    };

    // Unregister export
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.remove(&frame_set_id);
    }

    // Emit completion event
    let _ = app_handle.emit(
        "export-complete",
        ExportCompleteEvent {
            frame_set_id,
            success: result.success,
            files_organized: result.files_organized,
            warnings: result.warnings.clone(),
            error: result.error.clone(),
            output_dir: result.output_dir.clone(),
        },
    );

    Ok(result)
}

/// Cancel an active export
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_export(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let exports = state.ctx.active_exports.lock().unwrap();
    if let Some(handle) = exports.get(&frame_set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err("No active export for this frame set".to_string())
    }
}

/// Get enhanced export summary for the new UI
///
/// Returns comprehensive export data with:
/// - Equipment info (cameras, telescopes, date range)
/// - Filter groups with exposure breakdown
/// - Calibration details with match quality
/// - Folder structure preview
/// - Detailed warnings with context
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_export_summary(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<ExportSummary, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let config = load_wbpp_config(&conn).unwrap_or_default();

    collect_export_summary(&conn, frame_set_id, &config).map_err(|e| e.to_string())
}
