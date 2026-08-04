// Export route handlers for the Axum web API.
//
// Mirrors the Tauri export commands.  Progress events are broadcast via SSE
// instead of Tauri's emit mechanism.

use athenaeum_core::api::lights::get_export_readiness as api_get_export_readiness;
use athenaeum_core::api::lights::{ExportReadiness, FlatNormMode, LightCalParams};
use athenaeum_core::events::{emit_event, ProgressEmitter};
use athenaeum_core::export::frame_set_queries::{self, ExportableFrameSet};
use athenaeum_core::export::{
    apply_export_mode, collect_export_data, collect_export_summary, organize_files_wbpp,
    resolve_export_mode,
};
use athenaeum_core::export::models::{
    CalibrationRoute, ExportCompleteEvent, ExportData, ExportMode, ExportProgressEvent,
    ExportResult, ExportSummary, WbppExportConfig,
};
use athenaeum_core::services::ExportHandle;
use axum::{extract::State, http::StatusCode, Json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

// ── Helpers ───────────────────────────────────────────────────────────────────

// The raw stderr prints formerly here duplicated the `#[tracing::instrument(err(Debug))]`
// attribute on every caller below, which already logs each returned Err at
// the command boundary — see the T7 sweep report.
fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database not initialized".to_string(),
    )
}

const WBPP_CONFIG_KEY: &str = "export.wbpp_config";

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

// ── Request arg structs ───────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWbppExportConfigArgs {
    pub config: WbppExportConfig,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frame_set_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportToWbppArgs {
    pub frame_set_id: i64,
    pub output_dir: String,
    pub use_symlinks: bool,
    /// Explicit per-invocation export mode. `Some` overrides the persisted
    /// [`WbppExportConfig`]'s mode; `None` falls back to it (mirrors the Tauri
    /// command). Optional so pre-mode-UI callers keep working.
    #[serde(default)]
    pub export_mode: Option<ExportMode>,
    /// Calibration preferences used only by the `calibratedLights` strict gate
    /// (spec §12.2); optional so the pre-mode-UI frontend keeps working
    /// (default: normalize ON).
    #[serde(default = "default_flat_norm")]
    pub flat_norm: bool,
    #[serde(default)]
    pub flat_norm_mode: FlatNormMode,
    #[serde(default)]
    pub params: LightCalParams,
}

fn default_flat_norm() -> bool {
    true
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetExportReadinessArgs {
    pub set_id: i64,
    pub mode: ExportMode,
    #[serde(default = "default_flat_norm")]
    pub flat_norm: bool,
    #[serde(default)]
    pub flat_norm_mode: FlatNormMode,
    #[serde(default)]
    pub params: LightCalParams,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelExportArgs {
    pub frame_set_id: i64,
}

// ── Config handlers ───────────────────────────────────────────────────────────

/// Get WBPP export configuration (returns default if not set).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_wbpp_export_config(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Result<Json<WbppExportConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let config = load_wbpp_config(&conn).map_err(db_err)?;
    Ok(Json(config))
}

/// Save WBPP export configuration to the database.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_wbpp_export_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetWbppExportConfigArgs>,
) -> Result<Json<WbppExportConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let json = serde_json::to_string(&args.config).map_err(db_err)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![WBPP_CONFIG_KEY, json],
    )
    .map_err(db_err)?;

    Ok(Json(args.config))
}

/// Reset WBPP export configuration to defaults.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn reset_wbpp_export_config(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Result<Json<WbppExportConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![WBPP_CONFIG_KEY],
    )
    .map_err(db_err)?;

    Ok(Json(WbppExportConfig::default()))
}

// ── Data collection handlers ──────────────────────────────────────────────────

/// Get export preview data for a frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_export_preview(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<ExportData>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let data = collect_export_data(&conn, args.frame_set_id).map_err(db_err)?;
    Ok(Json(data))
}

/// Get a list of available frame sets for export.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_exportable_frame_sets(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Result<Json<Vec<ExportableFrameSet>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let frame_sets = frame_set_queries::get_exportable_frame_sets(&conn).map_err(db_err)?;
    Ok(Json(frame_sets))
}

/// Get calibration route for UI display.
///
/// Builds a structured view of the export groups and their calibration trees,
/// suitable for displaying in the UI.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_route(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<CalibrationRoute>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let route =
        frame_set_queries::get_calibration_route(&conn, args.frame_set_id).map_err(db_err)?;
    Ok(Json(route))
}

/// Get enhanced export summary for the new UI.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_export_summary(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<ExportSummary>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let config = load_wbpp_config(&conn).unwrap_or_default();
    let summary =
        collect_export_summary(&conn, args.frame_set_id, &config).map_err(db_err)?;
    Ok(Json(summary))
}

// ── Active export handlers ────────────────────────────────────────────────────

/// Export-readiness tallies for the WBPP export dialog's mode selector
/// (spec §12.2). Read-only; run under `spawn_blocking` so the derived-status
/// queries stay off the async executor (matches the readiness precedent).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_export_readiness(
    State(state): State<WebAppState>,
    Json(args): Json<GetExportReadinessArgs>,
) -> Result<Json<ExportReadiness>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        get_export_readiness_core(&ctx, args)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Export readiness task panicked: {}", e)))?
    .map_err(api_err)?;

    Ok(Json(result))
}

/// Extracted so the `spawn_blocking` body stays a plain synchronous call — the
/// `get_export_readiness` name in `api::lights` is shadowed by this module's
/// handler, so route through the core function explicitly.
fn get_export_readiness_core(
    ctx: &athenaeum_core::services::ServiceContext,
    args: GetExportReadinessArgs,
) -> Result<ExportReadiness, athenaeum_core::api::ApiError> {
    api_get_export_readiness(ctx, args.set_id, args.mode, args.flat_norm, args.flat_norm_mode, args.params)
}

/// Export a frame set to PixInsight WBPP folder structure.
///
/// Emits SSE progress events via the broadcast channel while the file
/// organizer runs.  The DB lock is released before the (potentially long)
/// file-copy phase so other handlers can still serve requests.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn export_to_wbpp(
    State(state): State<WebAppState>,
    Json(args): Json<ExportToWbppArgs>,
) -> Result<Json<ExportResult>, (StatusCode, String)> {
    let frame_set_id = args.frame_set_id;

    // Create cancel flag and register this export
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

    // Emit "collecting" phase progress
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    emit_event(
        &emitter,
        "export-progress",
        &ExportProgressEvent {
            frame_set_id,
            current: 0,
            total: 0,
            percent: 0.0,
            current_file: None,
            phase: "collecting".to_string(),
        },
    );

    // Collect export data and config — release the DB lock immediately after
    let (mut export_data, config) = {
        let db = state.ctx.db.get().ok_or_else(no_db)?;
        let conn = db.conn();

        let data = collect_export_data(&conn, frame_set_id).map_err(db_err)?;
        let cfg = load_wbpp_config(&conn).unwrap_or_default();
        (data, cfg)
    }; // DB lock released here
    // Explicit per-invocation override wins over the persisted config's mode.
    let mode = resolve_export_mode(args.export_mode, &config);

    let output_path = PathBuf::from(&args.output_dir);

    let make_fail = |error: String| ExportResult {
        success: false,
        output_dir: args.output_dir.clone(),
        files_organized: 0,
        scripts_generated: Vec::new(),
        warnings: Vec::new(),
        error: Some(error),
    };

    // Validate output path is within the configured export directory
    if let Some(ref export_dir) = state.export_dir {
        if !output_path.starts_with(export_dir) {
            return Ok(Json(make_fail(format!(
                "Export path must be within {}",
                export_dir.display()
            ))));
        }
    }

    // Strict gate (spec §12.2) + mode transform. Returns per-set omission
    // warnings to fold in, or an Err message that aborts before any file write.
    let prepare = |export_data: &mut ExportData| -> Result<Vec<String>, String> {
        if mode == ExportMode::CalibratedLights {
            let readiness = api_get_export_readiness(
                &state.ctx,
                frame_set_id,
                mode,
                args.flat_norm,
                args.flat_norm_mode,
                args.params.clone(),
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
        let db = state.ctx.db.get().ok_or_else(|| "Database not initialized".to_string())?;
        let conn = db.conn();
        apply_export_mode(&conn, export_data, mode).map_err(|e| e.to_string())
    };

    // Run the file organizer (no DB lock held during file operations)
    let was_cancelled = cancel_flag.load(Ordering::Relaxed);
    let result = if was_cancelled {
        make_fail("Export cancelled".to_string())
    } else {
        match prepare(&mut export_data) {
            Err(e) => {
                tracing::error!(frame_set_id, error = %e, "export blocked before organizing");
                make_fail(e)
            }
            Ok(mode_warnings) => match organize_files_wbpp(
                &output_path,
                &export_data,
                args.use_symlinks,
                &config,
                Some(&emitter as &dyn ProgressEmitter),
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
                            output_dir: args.output_dir.clone(),
                            files_organized: org_result.files_organized,
                            scripts_generated: Vec::new(),
                            warnings,
                            error: Some("Export cancelled".to_string()),
                        }
                    } else {
                        ExportResult {
                            success: true,
                            output_dir: args.output_dir.clone(),
                            files_organized: org_result.files_organized,
                            scripts_generated: Vec::new(),
                            warnings,
                            error: None,
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("Failed to organize files: {}", e);
                    tracing::error!(frame_set_id, error = %e, "export failed");
                    make_fail(msg)
                }
            },
        }
    };

    // Unregister export
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.remove(&frame_set_id);
    }

    // Emit completion event
    emit_event(
        &emitter,
        "export-complete",
        &ExportCompleteEvent {
            frame_set_id,
            success: result.success,
            files_organized: result.files_organized,
            warnings: result.warnings.clone(),
            error: result.error.clone(),
            output_dir: result.output_dir.clone(),
        },
    );

    Ok(Json(result))
}

/// Returns the configured export directory path, or null if not set.
#[tracing::instrument(skip_all)]
pub async fn get_export_dir(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Json<Option<String>> {
    Json(state.export_dir.as_ref().map(|p| p.to_string_lossy().to_string()))
}

/// Cancel an active export operation.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_export(
    State(state): State<WebAppState>,
    Json(args): Json<CancelExportArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let exports = state.ctx.active_exports.lock().unwrap();
    if let Some(handle) = exports.get(&args.frame_set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(Json(()))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("No active export for frame set {}", args.frame_set_id),
        ))
    }
}

#[cfg(test)]
mod wbpp_export_config_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// these tests exercise actual `settings` table reads/writes for the
    /// WBPP export config. Mirrors `settings::logging_config_tests::test_state`.
    fn test_state(db: Database) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_light_cal: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths: Vec::new(),
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
            sync: std::sync::Arc::new(athenaeum_core::sync::SyncRuntime::new()),
            sync_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
            collab_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
        }
    }

    /// Regression guard for the real frontend payload: `api.invoke` sends
    /// `{ "config": { ... } }`, per the Tauri named-arg convention — not a
    /// bare `WbppExportConfig`. `set_wbpp_export_config` already took the
    /// `SetWbppExportConfigArgs` wrapper before this fix pass (it was not
    /// part of the broken quartet), but had no test coverage — this pins
    /// the wrapped round-trip. See `.superpowers/sdd/task-10-report.md`
    /// (Web wrapper rider).
    #[tokio::test]
    async fn set_wbpp_export_config_then_get_reflects_change() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let cfg = WbppExportConfig {
            keyword_order: vec!["FLAT".to_string(), "CAMERA".to_string()],
            ..WbppExportConfig::default()
        };

        let _ = set_wbpp_export_config(
            State(state.clone()),
            Json(SetWbppExportConfigArgs { config: cfg }),
        )
        .await
        .expect("valid config must be accepted");

        let resp = get_wbpp_export_config(State(state), Json(serde_json::json!({})))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.keyword_order, vec!["FLAT".to_string(), "CAMERA".to_string()]);
    }

    /// Pins the existing (already-correct) contract: deserializing a bare
    /// `WbppExportConfig` JSON body must fail hard (serde error), not
    /// silently succeed with the wrong shape.
    #[test]
    fn bare_wbpp_export_config_body_fails_to_deserialize_into_wrapped_args() {
        let bare = serde_json::to_value(WbppExportConfig::default()).unwrap();

        let result: Result<SetWbppExportConfigArgs, _> = serde_json::from_value(bare);
        assert!(
            result.is_err(),
            "bare WbppExportConfig body must NOT deserialize into SetWbppExportConfigArgs — \
             this is what closes the silent-mismatch hole (axum returns 422/400 for this shape)"
        );
    }
}
