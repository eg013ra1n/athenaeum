// Export route handlers for the Axum web API.
//
// Mirrors the Tauri export commands.  Progress events are broadcast via SSE
// instead of Tauri's emit mechanism.

use athenaeum_core::api::export::{run_export_organize, ExportRunOutcome};
use athenaeum_core::api::lights::get_export_readiness as api_get_export_readiness;
use athenaeum_core::api::lights::{ExportReadiness, FlatNormMode, LightCalParams};
use athenaeum_core::events::{emit_event, ProgressEmitter};
use athenaeum_core::export::frame_set_queries::{self, ExportableFrameSet};
use athenaeum_core::export::models::{
    CalibratedLightOptions, CalibrationRoute, ExportCompleteEvent, ExportData, ExportMode,
    ExportProgressEvent, ExportResult, ExportSummary, WbppExportConfig,
};
use athenaeum_core::export::{
    apply_export_mode, collect_export_data, collect_export_summary, resolve_export_mode,
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
    /// Generation options for the `calibratedLights` mode: in that mode the
    /// export calibrates every light from its linked masters as it places it.
    /// Each field defaults to the recommended behavior so a caller with no
    /// opinion keeps working; every other mode ignores them.
    #[serde(default = "default_true")]
    pub flat_norm: bool,
    #[serde(default)]
    pub flat_norm_mode: FlatNormMode,
    #[serde(default)]
    pub params: LightCalParams,
    /// Replace the master dark's hot pixels with a neighbourhood median.
    #[serde(default = "default_true")]
    pub hot_pixel: bool,
    /// Debayer CFA lights to full-resolution planar RGB.
    #[serde(default = "default_true")]
    pub debayer: bool,
}

fn default_true() -> bool {
    true
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetExportReadinessArgs {
    pub set_id: i64,
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
    let summary = collect_export_summary(&conn, args.frame_set_id, &config).map_err(db_err)?;
    Ok(Json(summary))
}

// ── Active export handlers ────────────────────────────────────────────────────

/// Export-readiness tallies for the WBPP export dialog's mode selector
/// (spec §12.2, v2 §6). Read-only; run under `spawn_blocking` so the catalog
/// queries stay off the async executor (matches the readiness precedent).
/// Takes no calibration preferences — readiness is about the inputs (masters
/// built, lights linked), which no dialog toggle can change.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_export_readiness(
    State(state): State<WebAppState>,
    Json(args): Json<GetExportReadinessArgs>,
) -> Result<Json<ExportReadiness>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || get_export_readiness_core(&ctx, args))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Export readiness task panicked: {}", e),
            )
        })?
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
    api_get_export_readiness(ctx, args.set_id)
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

    // Collect export data and config — release the DB lock immediately after.
    // A DB failure here must still route through `finish_export` (the export
    // was just registered above), so it is folded into `result` below instead
    // of a bare `?` — the same fix M1 named at the resolve block further down,
    // applied here too since it is the identical shape.
    let collected: Result<(ExportData, WbppExportConfig), String> = (|| {
        let db = state
            .ctx
            .db
            .get()
            .ok_or_else(|| "Database not initialized".to_string())?;
        let conn = db.conn();

        let data = collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?;
        let cfg = load_wbpp_config(&conn).unwrap_or_default();
        Ok((data, cfg))
    })(); // DB lock released here

    let output_path = PathBuf::from(&args.output_dir);

    let make_fail = |error: String| ExportResult {
        success: false,
        output_dir: args.output_dir.clone(),
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
            let mode = resolve_export_mode(args.export_mode, &config);
            // Read by the calibrated-lights mode only; the transform ignores it
            // in the others (its debayer flag decides the output NAMES, so it
            // must be the same value the pixel phase later resolves against).
            let gen_opts = CalibratedLightOptions {
                flat_norm: args.flat_norm,
                flat_norm_mode: args.flat_norm_mode,
                params: args.params,
                hot_pixel_correction: args.hot_pixel,
                debayer_osc: args.debayer,
            };

            // Validate output path is within the configured export directory.
            // A rejection here must still finish the export (same reasoning
            // as the DB failure above) rather than short-circuit past it.
            let path_ok = match state.export_dir {
                Some(ref export_dir) if !output_path.starts_with(export_dir) => Err(format!(
                    "Export path must be within {}",
                    export_dir.display()
                )),
                _ => Ok(()),
            };

            // Strict gate (spec §12.2) + mode transform. Returns per-set
            // omission warnings to fold in, or an Err message that aborts
            // before any file write.
            let prepare = |export_data: &mut ExportData| -> Result<Vec<String>, String> {
                let readiness = api_get_export_readiness(&state.ctx, frame_set_id)
                    .map_err(|e| e.to_string())?;
                if let Err(msg) = athenaeum_core::api::lights::check_mode_ready(&readiness, mode) {
                    tracing::warn!(frame_set_id, ?mode, error = %msg, "export refused: mode not ready");
                    return Err(msg);
                }
                let db = state
                    .ctx
                    .db
                    .get()
                    .ok_or_else(|| "Database not initialized".to_string())?;
                let conn = db.conn();
                apply_export_mode(&conn, export_data, mode, Some(&gen_opts))
                    .map_err(|e| e.to_string())
            };

            let was_cancelled = cancel_flag.load(Ordering::Relaxed);
            if was_cancelled {
                make_fail("Export cancelled".to_string())
            } else if let Err(e) = path_ok {
                make_fail(e)
            } else {
                match prepare(&mut export_data) {
                    Err(e) => {
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
                        let event_tx_bg = state.event_tx.clone();
                        let cancel_flag_bg = cancel_flag.clone();
                        let output_path_bg = output_path.clone();
                        let config_bg = config.clone();
                        let gen_opts_bg = gen_opts.clone();
                        let use_symlinks = args.use_symlinks;

                        let joined = tokio::task::spawn_blocking(move || {
                            let bg_emitter = SseProgressEmitter::new(event_tx_bg);
                            run_export_organize(
                                &ctx,
                                &output_path_bg,
                                &export_data,
                                use_symlinks,
                                &config_bg,
                                Some(&bg_emitter as &dyn ProgressEmitter),
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
                                output_dir: args.output_dir.clone(),
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
                        }
                    }
                }
            }
        }
    };

    finish_export(&state, &emitter, frame_set_id, result.clone());
    Ok(Json(result))
}

/// Unregister the export and broadcast its completion event. Extracted so the
/// compute-queue cancellation can leave `export_to_wbpp` early without
/// stranding the `active_exports` entry or skipping `export-complete` — a
/// cancelled export must look exactly like any other finished one. Mirrors the
/// Tauri command's `finish_export`.
fn finish_export(
    state: &WebAppState,
    emitter: &SseProgressEmitter,
    frame_set_id: i64,
    result: ExportResult,
) {
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.remove(&frame_set_id);
    }
    emit_event(
        emitter,
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
}

/// Returns the configured export directory path, or null if not set.
#[tracing::instrument(skip_all)]
pub async fn get_export_dir(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Json<Option<String>> {
    Json(
        state
            .export_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
    )
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
    use crate::events::SseEvent;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
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
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
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
        assert_eq!(
            resp.keyword_order,
            vec!["FLAT".to_string(), "CAMERA".to_string()]
        );
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

/// Fix-round I2/I3/M1 coverage: the full `export_to_wbpp` handler, cancelled
/// while parked on the compute queue. `api::export::tests` already pins the
/// core ordering (`run_export_organize` returns `Cancelled` without touching
/// the DB or disk); this module pins the HOST wiring on top of it — the
/// route must still turn that outcome into the cancelled `ExportResult`,
/// clear `active_exports`, and broadcast `export-complete`, exactly like
/// every other exit.
#[cfg(test)]
mod export_cancel_while_queued_tests {
    use super::*;
    use crate::events::SseEvent;
    use athenaeum_core::api::lights::{FlatNormMode, LightCalParams};
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::compute_queue::{ComputeJobKind, ComputeQueue};
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

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
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
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

    /// One light, linked straight to a registered master dark — enough for
    /// `check_mode_ready(CalibratedLights)` (no unlinked lights, no raw set
    /// without a master). Readiness and the mode transform are DB-only (no
    /// filesystem probe), so no real FITS bytes are needed to reach the
    /// blocking phase this test wants to cancel before it starts.
    fn seed_ready_calibrated_export(conn: &Connection) {
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'Set')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            [],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            rusqlite::params![night_id],
        )
        .unwrap();
        let session_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (10, '/test/light_10.fits', 'light_10.fits', 0, '2026-07-05T00:00:00Z', 'FITS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume) VALUES (10, 10, 'Light', 'TestCam')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, 10)",
            rusqlite::params![session_id],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (100, 'MasterDark', '2026-07-05', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (100, '/lib/master_100.fits', 'master_100.fits', 0, '2026-07-05T00:00:00Z', 'FITS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (100, 100, 'MasterDark', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10, 'frame', 100, 'Dark', '2026-07-05T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn export_cancelled_while_queued_still_finishes_and_notifies() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        {
            let conn = db.conn();
            seed_ready_calibrated_export(&conn);
        }
        let state = test_state(db);

        // Occupy the queue's only slot (default max_concurrent = 1) so the
        // export's own `run_export_organize` admission parks instead of
        // running straight through.
        let (_held, _job_id) = state
            .ctx
            .compute_queue
            .acquire(
                ComputeJobKind::MasterBuild,
                "held",
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();

        let mut events = state.event_tx.subscribe();

        let state_bg = state.clone();
        let handle = tokio::spawn(async move {
            export_to_wbpp(
                State(state_bg),
                Json(ExportToWbppArgs {
                    frame_set_id: 1,
                    output_dir: tmp_export_dir(),
                    use_symlinks: false,
                    export_mode: Some(ExportMode::CalibratedLights),
                    flat_norm: true,
                    flat_norm_mode: FlatNormMode::default(),
                    params: LightCalParams::default(),
                    hot_pixel: true,
                    debayer: true,
                }),
            )
            .await
        });

        // The export registers itself in `active_exports` before it ever
        // touches the compute queue — poll for that, then cancel.
        for _ in 0..200 {
            if state.ctx.active_exports.lock().unwrap().contains_key(&1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            state.ctx.active_exports.lock().unwrap().contains_key(&1),
            "export never registered — the test raced ahead of the handler"
        );

        let _ = cancel_export(
            State(state.clone()),
            Json(CancelExportArgs { frame_set_id: 1 }),
        )
        .await
        .expect("an active export must be cancellable");

        let result = handle
            .await
            .expect("export task must not panic")
            .expect("export_to_wbpp must not return an HTTP error for a queued cancel")
            .0;

        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("Export cancelled"));
        assert_eq!(result.files_organized, 0);

        // M1/I3: `finish_export` ran — the entry is gone and `export-complete`
        // was broadcast, exactly like a normal finish.
        assert!(
            !state.ctx.active_exports.lock().unwrap().contains_key(&1),
            "active_exports must be cleared even for a queued cancel"
        );
        let mut saw_complete = false;
        while let Ok(ev) = events.try_recv() {
            if ev.event_name == "export-complete" {
                saw_complete = true;
                let payload: ExportCompleteEvent = serde_json::from_value(ev.data).unwrap();
                assert!(!payload.success);
                assert_eq!(payload.error.as_deref(), Some("Export cancelled"));
            }
        }
        assert!(saw_complete, "expected an export-complete SSE event");
    }

    fn tmp_export_dir() -> String {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        std::mem::forget(dir); // outlives the handler call; OS cleans up the tmp tree
        path
    }
}
