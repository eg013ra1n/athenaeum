// Archive feature route handlers for the Axum web API.
//
// Mirrors the Tauri archive commands. Progress events are broadcast via SSE
// instead of Tauri's emit mechanism.

use crate::events::SseProgressEmitter;
use crate::WebAppState;
use athenaeum_core::archive::{db as adb, executor, planner, resume, restore, rollback, models::*};
use athenaeum_core::services::operation_queue::{OperationKind, QueuedJob};
use athenaeum_core::services::ArchiveHandle;
use athenaeum_core::settings::keys;
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// ── Request structs ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationIdRequest {
    pub operation_id: i64,
}

#[derive(Deserialize)]
pub struct SetRootRequest {
    pub path: String,
}

#[derive(Deserialize)]
pub struct SetCompressionRequest {
    pub compression: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    pub frames_set_id: i64,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
    #[serde(default)]
    pub archive_root_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub frames_set_id: i64,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
    pub conflict_resolution: ConflictResolution,
    #[serde(default)]
    pub archive_root_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddArchiveRootRequest {
    pub path: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveRootIdRequest {
    pub id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRestoreRequest {
    pub operation_id: i64,
    pub target_root_path: String,
    pub overwrite_existing: bool,
    pub keep_zip_after_restore: bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Thin `Result<_, String>` shim over the shared core helper
/// (`athenaeum_core::archive::migrate_legacy_archive_root`, moved there in
/// Task 14 so `api::masters::archive_originals` can share it too) so the
/// existing `?`-into-`String` call sites below don't change.
fn migrate_legacy_archive_root(
    conn: &rusqlite::Connection,
    settings: &athenaeum_core::settings::SettingsManager,
) -> Result<(), String> {
    athenaeum_core::archive::migrate_legacy_archive_root(conn, settings).map_err(|e| format!("{:#}", e))
}

/// Thin `Result<_, String>` shim over the shared core helper
/// (`athenaeum_core::archive::resolve_archive_root`).
fn resolve_archive_root(
    conn: &rusqlite::Connection,
    settings: &athenaeum_core::settings::SettingsManager,
    requested: Option<&str>,
) -> Result<String, String> {
    athenaeum_core::archive::resolve_archive_root(conn, settings, requested).map_err(|e| format!("{:#}", e))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_archive_settings(
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let root = state
        .ctx
        .settings
        .get_archive_root_path(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let compression = state
        .ctx
        .settings
        .get_archive_compression(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "rootPath": root, "compression": compression })))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_archive_root_path(
    State(state): State<WebAppState>,
    Json(req): Json<SetRootRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    state
        .ctx
        .settings
        .persist_setting(&db.conn(), keys::ARCHIVE_ROOT_PATH, &req.path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_archive_compression(
    State(state): State<WebAppState>,
    Json(req): Json<SetCompressionRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if !matches!(req.compression.as_str(), "store" | "deflate") {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("invalid compression value: {}", req.compression),
        ));
    }
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    state
        .ctx
        .settings
        .persist_setting(&db.conn(), keys::ARCHIVE_COMPRESSION, &req.compression)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn plan_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<PlanRequest>,
) -> Result<Json<ArchivePlan>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let root = resolve_archive_root(&conn, &state.ctx.settings, req.archive_root_path.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    planner::build_plan(
        &conn,
        req.frames_set_id,
        Path::new(&root),
        &req.dispositions,
        req.compression,
    )
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e)))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<StartRequest>,
) -> Result<Json<i64>, (StatusCode, String)> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                "another archive operation is already in progress".into(),
            ));
        }
    }
    let ctx = state.ctx.clone();
    let db = ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let root = resolve_archive_root(&conn, &ctx.settings, req.archive_root_path.as_deref())
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let plan = planner::build_plan(
        &conn,
        req.frames_set_id,
        Path::new(&root),
        &req.dispositions,
        req.compression,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e)))?;
    let op_id = planner::commit_plan(&conn, &plan, req.conflict_resolution)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e)))?;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(
        op_id,
        ArchiveHandle {
            operation_id: op_id,
            cancel_flag: cancel_flag.clone(),
        },
    );

    let event_tx = state.event_tx.clone();
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let emitter = SseProgressEmitter::new(event_tx);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            match executor::run_operation(&conn, op_id, &cancel_flag, &emitter) {
                Ok(()) => {
                    tracing::info!(operation_id = op_id, "archive operation completed");
                }
                Err(e) => {
                    if executor::was_cancelled(&e) {
                        let _ = adb::update_operation_status(
                            &conn,
                            op_id,
                            ArchiveStatus::Cancelled,
                            None,
                        );
                    } else {
                        tracing::error!(operation_id = op_id, error = ?e, "archive operation failed");
                        let msg = format!("{:#}", e);
                        let _ = adb::update_operation_status(
                            &conn,
                            op_id,
                            ArchiveStatus::Failed,
                            Some(&msg),
                        );
                    }
                    if let Err(rb_err) = rollback::rollback_operation(&conn, op_id, &emitter) {
                        tracing::error!(operation_id = op_id, error = ?rb_err, "rollback after failed archive operation also failed, operation may be left in an inconsistent state");
                    }
                }
            }
            ctx_for_worker
                .active_archives
                .lock()
                .unwrap()
                .remove(&op_id);
        }),
    });

    Ok(Json(op_id))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let map = state.ctx.active_archives.lock().unwrap();
    if let Some(handle) = map.get(&req.operation_id) {
        handle
            .cancel_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(StatusCode::OK)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("no active operation {}", req.operation_id),
        ))
    }
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_restore_suggestions(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();

    let suggested: Option<String> = conn
        .query_row(
            "SELECT source_path, target_path_in_zip
             FROM archive_operation_files
             WHERE operation_id = ?1
             LIMIT 1",
            [req.operation_id],
            |row| {
                let source_path: String = row.get(0)?;
                let path_in_zip: String = row.get(1)?;
                Ok(
                    athenaeum_core::archive::path_layout::original_parent_for_restore(
                        &source_path,
                        &path_in_zip,
                    ),
                )
            },
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut stmt = conn
        .prepare("SELECT path FROM scan_roots ORDER BY path")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let scan_roots: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let suggested_exists = suggested
        .as_ref()
        .map(|p| std::path::Path::new(p).is_dir())
        .unwrap_or(false);

    Ok(Json(serde_json::json!({
        "suggested_original_path": suggested,
        "suggested_original_exists": suggested_exists,
        "scan_roots": scan_roots,
    })))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_unfinished_archive_operations(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<ArchiveOperationSummary>>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    resume::find_unfinished_operations(&db.conn())
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn resume_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(
        req.operation_id,
        ArchiveHandle {
            operation_id: req.operation_id,
            cancel_flag: cancel_flag.clone(),
        },
    );
    let event_tx = state.event_tx.clone();
    let op_id = req.operation_id;
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let emitter = SseProgressEmitter::new(event_tx);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            if let Err(e) = resume::resume_operation(&conn, op_id, &cancel_flag, &emitter) {
                tracing::error!(operation_id = op_id, error = ?e, "archive resume failed");
                let msg = format!("{:#}", e);
                let _ = adb::update_operation_status(&conn, op_id, ArchiveStatus::Failed, Some(&msg));
                let _ = rollback::rollback_operation(&conn, op_id, &emitter);
            }
            ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
        }),
    });
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn rollback_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let event_tx = state.event_tx.clone();
    let op_id = req.operation_id;
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let emitter = SseProgressEmitter::new(event_tx);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            if let Err(e) = rollback::rollback_operation_standalone(&conn, op_id, &emitter) {
                tracing::error!(operation_id = op_id, error = ?e, "archive rollback failed");
            }
        }),
    });
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_archive_zips(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT target_zip_path, frame_role
             FROM archive_operation_files
             WHERE operation_id = ?1
             ORDER BY
                CASE frame_role
                    WHEN 'light' THEN 0
                    WHEN 'flat'  THEN 1
                    WHEN 'darkflat' THEN 2
                    WHEN 'dark'  THEN 3
                    WHEN 'bias'  THEN 4
                    ELSE 5
                END,
                target_zip_path",
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows = stmt
        .query_map([req.operation_id], |row| {
            let path: String = row.get(0)?;
            let frame_role: String = row.get(1)?;
            let p = std::path::Path::new(&path);
            let filename = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let exists = p.is_file();
            let size_bytes = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
            Ok(serde_json::json!({
                "path": path,
                "filename": filename,
                "frame_role": frame_role,
                "exists": exists,
                "size_bytes": size_bytes,
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_archived_frame_sets(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT fs.id, fs.name, fs.archived_at, fs.archive_operation_id,
                    op.archive_root_path, op.started_at,
                    (SELECT COUNT(*) FROM archive_operation_files
                     WHERE operation_id = op.id AND frame_role = 'light') AS lights,
                    (SELECT COUNT(*) FROM archive_operation_files
                     WHERE operation_id = op.id AND frame_role = 'flat') AS flats,
                    (SELECT COUNT(*) FROM archive_operation_files
                     WHERE operation_id = op.id AND frame_role = 'dark') AS darks,
                    (SELECT COUNT(*) FROM archive_operation_files
                     WHERE operation_id = op.id AND frame_role = 'bias') AS bias,
                    (SELECT COUNT(*) FROM archive_operation_files
                     WHERE operation_id = op.id AND frame_role = 'darkflat') AS darkflats
             FROM frames_set fs
             LEFT JOIN archive_operations op ON op.id = fs.archive_operation_id
             WHERE fs.archived_at IS NOT NULL
             ORDER BY fs.archived_at DESC",
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "frames_set_id": row.get::<_, i64>(0)?,
                "name": row.get::<_, Option<String>>(1)?,
                "archived_at": row.get::<_, Option<String>>(2)?,
                "operation_id": row.get::<_, Option<i64>>(3)?,
                "archive_root_path": row.get::<_, Option<String>>(4)?,
                "started_at": row.get::<_, Option<String>>(5)?,
                "lights_count": row.get::<_, i64>(6)?,
                "flats_count": row.get::<_, i64>(7)?,
                "darks_count": row.get::<_, i64>(8)?,
                "bias_count": row.get::<_, i64>(9)?,
                "darkflats_count": row.get::<_, i64>(10)?,
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_restore_operation(
    State(state): State<WebAppState>,
    Json(req): Json<StartRestoreRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                "another archive operation already running".into(),
            ));
        }
    }
    let ctx = state.ctx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(
        req.operation_id,
        ArchiveHandle {
            operation_id: req.operation_id,
            cancel_flag: cancel_flag.clone(),
        },
    );
    let event_tx = state.event_tx.clone();
    let op_id = req.operation_id;
    let target_root_path = req.target_root_path.clone();
    let overwrite_existing = req.overwrite_existing;
    let keep_zip_after_restore = req.keep_zip_after_restore;
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let emitter = SseProgressEmitter::new(event_tx);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            let (outcome, conflicts) = match restore::run_restore(
                &conn,
                op_id,
                Path::new(&target_root_path),
                overwrite_existing,
                keep_zip_after_restore,
                &cancel_flag,
                &emitter,
            ) {
                Ok(result) if result.has_conflicts() => {
                    tracing::warn!(
                        operation_id = op_id,
                        conflicts = result.conflicts.len(),
                        paths = ?result.conflicts.iter().map(|c| c.source_path.as_str()).collect::<Vec<_>>(),
                        "restore completed with conflicts"
                    );
                    ("completed_with_conflicts", result.conflicts.len())
                }
                Ok(_) => ("completed", 0),
                Err(e) => {
                    tracing::error!(operation_id = op_id, error = ?e, "restore failed");
                    let outcome = if format!("{:#}", e).contains("cancelled") { "cancelled" } else { "failed" };
                    (outcome, 0)
                }
            };
            athenaeum_core::events::emit_event(
                &emitter,
                "archive-finished",
                &serde_json::json!({
                    "operation_id": op_id,
                    "outcome": outcome,
                    "kind": "restore",
                    "conflicts": conflicts,
                }),
            );
            ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
        }),
    });
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_archive(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                "another archive operation is already in progress".into(),
            ));
        }
    }
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();

    // Delete zip files. A zip we could not delete (Windows sharing violation,
    // read-only volume) must abort the whole request BEFORE any catalog row
    // goes away — otherwise the zip is orphaned on disk with nothing pointing
    // at it and the archive becomes unrestorable.
    let files = adb::list_operation_files(&conn, req.operation_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    let mut failed: Vec<String> = Vec::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let p = std::path::Path::new(&f.target_zip_path);
            if p.exists() {
                if let Err(e) = std::fs::remove_file(p) {
                    tracing::error!(operation_id = req.operation_id, path = %f.target_zip_path, error = %e, "failed to delete archive zip");
                    failed.push(format!("{}: {}", f.target_zip_path, e));
                }
            }
        }
    }
    if !failed.is_empty() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "could not delete {} zip file(s); catalog rows kept — any zip already removed in this pass is not restorable: {}",
                failed.len(),
                failed.join("; ")
            ),
        ));
    }

    let op = adb::get_operation(&conn, req.operation_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match op.frames_set_id {
        Some(fs_id) => {
            conn.execute("DELETE FROM frames_set WHERE id = ?1", [fs_id])
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            // archive_operations row is also deleted via FK cascade from frames_set_id.
        }
        None => {
            // Calibration-set archive op (Task 14): no frames_set to cascade
            // from — delete the archive_operations row directly.
            conn.execute("DELETE FROM archive_operations WHERE id = ?1", [req.operation_id])
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }

    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_archive_roots(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    migrate_legacy_archive_root(&conn, &state.ctx.settings)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut stmt = conn
        .prepare("SELECT id, path, label, is_default FROM archive_roots ORDER BY id")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "path": row.get::<_, String>(1)?,
                "label": row.get::<_, Option<String>>(2)?,
                "is_default": row.get::<_, i32>(3)? == 1,
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rows))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn add_archive_root(
    State(state): State<WebAppState>,
    Json(req): Json<AddArchiveRootRequest>,
) -> Result<Json<i64>, (StatusCode, String)> {
    if !std::path::Path::new(&req.path).is_dir() {
        return Err((StatusCode::BAD_REQUEST, format!("'{}' is not a directory", req.path)));
    }
    // Store the canonical normalized spelling — archive roots were the one
    // root table whose rows were inserted verbatim from the picker, so
    // `C:\Archive` vs `c:\Archive` (or a \\?\-verbatim form) could coexist as
    // two rows and the exact-string lookup in resolve_archive_root rejected
    // legitimate re-picks of the same folder.
    let path = match std::path::Path::new(&req.path).canonicalize() {
        Ok(c) => athenaeum_core::api::scan_roots::normalize_path(&c)
            .to_string_lossy()
            .to_string(),
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("failed to resolve '{}': {}", req.path, e),
            ))
        }
    };
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    migrate_legacy_archive_root(&conn, &state.ctx.settings)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_roots", [], |r| r.get(0))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let is_default = if count == 0 { 1 } else { 0 };
    conn.execute(
        "INSERT INTO archive_roots (path, label, is_default) VALUES (?1, ?2, ?3)",
        rusqlite::params![path, req.label, is_default],
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(conn.last_insert_rowid()))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_archive_root(
    State(state): State<WebAppState>,
    Json(req): Json<ArchiveRootIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let was_default: i32 = conn
        .query_row("SELECT is_default FROM archive_roots WHERE id = ?1", [req.id], |r| r.get(0))
        .unwrap_or(0);
    conn.execute("DELETE FROM archive_roots WHERE id = ?1", [req.id])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if was_default == 1 {
        if let Ok(new_default_id) = conn.query_row(
            "SELECT id FROM archive_roots ORDER BY id LIMIT 1",
            [],
            |r| r.get::<_, i64>(0),
        ) {
            conn.execute(
                "UPDATE archive_roots SET is_default = 1 WHERE id = ?1",
                [new_default_id],
            )
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_default_archive_root(
    State(state): State<WebAppState>,
    Json(req): Json<ArchiveRootIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    conn.execute("UPDATE archive_roots SET is_default = 0", [])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let n = conn
        .execute("UPDATE archive_roots SET is_default = 1 WHERE id = ?1", [req.id])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if n == 0 {
        return Err((StatusCode::NOT_FOUND, format!("archive root id {} not found", req.id)));
    }
    Ok(StatusCode::OK)
}
