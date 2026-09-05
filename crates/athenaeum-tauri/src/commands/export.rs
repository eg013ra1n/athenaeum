//! Export-related Tauri commands
//!
//! Commands for exporting frame sets to PixInsight WBPP folder structure.

use crate::commands::{AppState, ExportHandle};
use crate::export::{
    apply_export_mode, collect_export_data, collect_export_summary,
    frame_set_queries::{self, ExportableFrameSet},
    models::{
        CalibratedLightOptions, CalibrationRoute, ExportCompleteEvent, ExportData, ExportMode,
        ExportProgressEvent, ExportResult, ExportSummary, WbppExportConfig,
    },
    resolve_export_mode,
};
use athenaeum_core::api::export::{run_export_organize, ExportRunOutcome};
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
/// than relying on a best-effort config sync.
///
/// `flat_norm` / `flat_norm_mode` / `params` / `hot_pixel` / `debayer` are the
/// calibrated-lights GENERATION options: in that mode this command calibrates
/// every light from its linked masters as it places it. Each stays optional so
/// a caller with no opinion keeps working — an omitted one takes
/// [`CalibratedLightOptions::default`]'s value for that field; every other mode
/// ignores them.
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
    hot_pixel: Option<bool>,
    debayer: Option<bool>,
) -> Result<ExportResult, String> {
    // Create cancel flag and register export
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.insert(
            frame_set_id,
            ExportHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
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

    // Collect export data and config. A DB failure here must still route
    // through `finish_export` — the export was just registered above, so a
    // bare `?` would strand the `active_exports` entry and skip
    // `export-complete` (the same defect M1 named at the resolve block below,
    // fixed here too since it is the identical shape).
    let collected: Result<(ExportData, WbppExportConfig), String> = (|| {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        let data = collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?;
        let cfg = load_wbpp_config(&conn).unwrap_or_default();
        Ok((data, cfg))
    })();

    let output_path = PathBuf::from(&output_dir);

    let make_fail = |error: String| ExportResult {
        success: false,
        output_dir: output_dir.clone(),
        files_organized: 0,
        scripts_generated: Vec::new(),
        warnings: Vec::new(),
        error: Some(error),
    };

    let result = match collected {
        Err(e) => {
            tracing::error!(frame_set_id, error = %e, "export blocked before collecting data");
            make_fail(e)
        }
        Ok((mut export_data, config)) => {
            // Explicit per-invocation override wins over the persisted config's mode.
            let mode = resolve_export_mode(export_mode, &config);
            // Read by the calibrated-lights mode only; the transform ignores it
            // in the others (its debayer flag decides the output NAMES, so it
            // must be the same value the pixel phase later resolves against).
            // An absent (or `null`) option takes its value from the type that
            // owns the defaults — `resolve` is the one place, shared with the
            // Axum mirror and the summary preview.
            let gen_opts =
                CalibratedLightOptions::resolve(flat_norm, flat_norm_mode, params, hot_pixel, debayer);

            // Strict gate (spec §12.2) + mode transform. `prepare` returns the
            // per-set omission warnings to fold into the final result, or an
            // Err message that aborts the export before any file is written.
            let prepare = |export_data: &mut ExportData| -> Result<Vec<String>, String> {
                let readiness = api_get_export_readiness(&state.ctx, frame_set_id)
                    .map_err(|e| e.to_string())?;
                if let Err(msg) = athenaeum_core::api::lights::check_mode_ready(&readiness, mode) {
                    tracing::warn!(frame_set_id, ?mode, error = %msg, "export refused: mode not ready");
                    return Err(msg);
                }
                let db = state.ctx.db.get().ok_or("Database not initialized")?;
                let conn = db.conn();
                apply_export_mode(&conn, export_data, mode, Some(&gen_opts))
                    .map_err(|e| e.to_string())
            };

            let cancelled = cancel_flag.load(Ordering::Relaxed);
            if cancelled {
                make_fail("Export cancelled".to_string())
            } else {
                match prepare(&mut export_data) {
                    Err(e) => {
                        // Mirrors the web route: the refusal returns
                        // Ok(ExportResult{ success: false }), so without this
                        // the only log record of a readiness/DB failure would
                        // be the gate's own warn! — and a DB failure emits
                        // none at all.
                        tracing::error!(frame_set_id, error = %e, "export blocked before organizing");
                        make_fail(e)
                    }
                    Ok(mode_warnings) => {
                        // I3: admission (a condvar park) + plan resolution +
                        // the pixel batch must not run on the async runtime
                        // worker — a multi-minute calibration run would block
                        // it for the whole export. `run_export_organize` is
                        // the ONE body both hosts drive from spawn_blocking;
                        // it also owns the I2 ordering (permit acquired
                        // before the per-frame resolve loop, catalog
                        // connection scoped and dropped before any pixel
                        // work — see its doc comment).
                        let ctx = state.ctx.clone();
                        let app_handle_bg = app_handle.clone();
                        let cancel_flag_bg = cancel_flag.clone();
                        let output_path_bg = output_path.clone();
                        let config_bg = config.clone();
                        let gen_opts_bg = gen_opts.clone();

                        let joined = tokio::task::spawn_blocking(move || {
                            let export_emitter =
                                crate::tauri_events::TauriProgressEmitter(app_handle_bg);
                            run_export_organize(
                                &ctx,
                                &output_path_bg,
                                &export_data,
                                use_symlinks,
                                &config_bg,
                                Some(
                                    &export_emitter as &dyn athenaeum_core::events::ProgressEmitter,
                                ),
                                frame_set_id,
                                &cancel_flag_bg,
                                mode,
                                &gen_opts_bg,
                            )
                        })
                        .await;

                        match joined {
                            Err(join_err) => {
                                tracing::error!(frame_set_id, error = %join_err, "export task panicked");
                                make_fail(format!("Export task panicked: {}", join_err))
                            }
                            Ok(Err(api_err)) => {
                                make_fail(format!("Failed to organize files: {}", api_err))
                            }
                            // Cancelled while queued for a compute slot —
                            // `run_export_organize` never touched the disk.
                            Ok(Ok(ExportRunOutcome::Cancelled)) => ExportResult {
                                success: false,
                                output_dir: output_dir.clone(),
                                files_organized: 0,
                                scripts_generated: Vec::new(),
                                warnings: mode_warnings,
                                error: Some("Export cancelled".to_string()),
                            },
                            Ok(Ok(ExportRunOutcome::Organized(org_result))) => {
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
                        }
                    }
                }
            }
        }
    };

    finish_export(&state, &app_handle, frame_set_id, result)
}

/// Unregister the export and emit its completion event. `export_to_wbpp`
/// folds every outcome — a collection/gate failure, a queued-then-cancelled
/// compute-slot wait, a spawn-blocking join error, or a normal finish — into
/// one `result` and calls this exactly once, so none of those paths can
/// strand the `active_exports` entry or skip `export-complete`.
fn finish_export(
    state: &State<'_, AppState>,
    app_handle: &tauri::AppHandle,
    frame_set_id: i64,
    result: ExportResult,
) -> Result<ExportResult, String> {
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.remove(&frame_set_id);
    }
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
pub async fn cancel_export(frame_set_id: i64, state: State<'_, AppState>) -> Result<(), String> {
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
///
/// Drawn for the mode the tab has selected — the tree, file total and size
/// estimate describe THAT export. `export_mode` `None` falls back to the
/// persisted config like `export_to_wbpp`; the five generation options are
/// read by `calibratedLights` only (the debayer flag decides the `c_*` names
/// in the tree) and default the same way as the export's.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_export_summary(
    state: State<'_, AppState>,
    frame_set_id: i64,
    export_mode: Option<ExportMode>,
    flat_norm: Option<bool>,
    flat_norm_mode: Option<FlatNormMode>,
    params: Option<LightCalParams>,
    hot_pixel: Option<bool>,
    debayer: Option<bool>,
) -> Result<ExportSummary, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let config = load_wbpp_config(&conn).unwrap_or_default();
    let mode = resolve_export_mode(export_mode, &config);
    let gen_opts =
        CalibratedLightOptions::resolve(flat_norm, flat_norm_mode, params, hot_pixel, debayer);

    collect_export_summary(&conn, frame_set_id, &config, mode, Some(&gen_opts))
        .map_err(|e| e.to_string())
}
