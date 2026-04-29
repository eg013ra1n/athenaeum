// Archive feature route handlers for the Axum web API.
//
// Mirrors the Tauri archive commands. Progress events are broadcast via SSE
// instead of Tauri's emit mechanism.

use crate::events::SseProgressEmitter;
use crate::WebAppState;
use athenaeum_core::archive::{db as adb, executor, planner, resume, restore, rollback, models::*};
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

fn migrate_legacy_archive_root(
    conn: &rusqlite::Connection,
    settings: &athenaeum_core::settings::SettingsManager,
) -> Result<(), String> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_roots", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if count > 0 {
        return Ok(());
    }
    if let Some(legacy) = settings.get_archive_root_path(conn).map_err(|e| e.to_string())? {
        if !legacy.trim().is_empty() {
            conn.execute(
                "INSERT OR IGNORE INTO archive_roots (path, label, is_default) VALUES (?1, NULL, 1)",
                [&legacy],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn resolve_archive_root(
    conn: &rusqlite::Connection,
    settings: &athenaeum_core::settings::SettingsManager,
    requested: Option<&str>,
) -> Result<String, String> {
    migrate_legacy_archive_root(conn, settings)?;
    if let Some(p) = requested {
        let known: i64 = conn
            .query_row("SELECT COUNT(*) FROM archive_roots WHERE path = ?1", [p], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if known == 0 {
            return Err(format!("'{}' is not a configured archive folder", p));
        }
        return Ok(p.to_string());
    }
    let rows: Vec<(String, i32)> = {
        let mut stmt = conn
            .prepare("SELECT path, is_default FROM archive_roots ORDER BY id")
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        mapped
    };
    if rows.is_empty() {
        return Err("no archive folders configured".into());
    }
    if rows.len() == 1 {
        return Ok(rows[0].0.clone());
    }
    if let Some((path, _)) = rows.iter().find(|(_, d)| *d == 1) {
        return Ok(path.clone());
    }
    Err("multiple archive folders configured but no default — pick a destination explicitly".into())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

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
    tokio::task::spawn_blocking(move || {
        let emitter = SseProgressEmitter::new(event_tx);
        let db = ctx_for_worker.db.get().expect("db");
        let conn = db.conn();
        match executor::run_operation(&conn, op_id, &cancel_flag, &emitter) {
            Ok(()) => {
                eprintln!("archive operation {} completed", op_id);
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
                    eprintln!("archive operation {} failed: {:#}", op_id, e);
                    let msg = format!("{:#}", e);
                    let _ = adb::update_operation_status(
                        &conn,
                        op_id,
                        ArchiveStatus::Failed,
                        Some(&msg),
                    );
                }
                if let Err(rb_err) = rollback::rollback_operation(&conn, op_id, &emitter) {
                    eprintln!("rollback for {} failed: {:#}", op_id, rb_err);
                }
            }
        }
        ctx_for_worker
            .active_archives
            .lock()
            .unwrap()
            .remove(&op_id);
    });

    Ok(Json(op_id))
}

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
                if let Some(stripped) = source_path.strip_suffix(&path_in_zip) {
                    let trimmed = stripped.trim_end_matches('/');
                    Ok(if trimmed.is_empty() { None } else { Some(trimmed.to_string()) })
                } else {
                    Ok(None)
                }
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

pub async fn resume_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
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
    tokio::task::spawn_blocking(move || {
        let emitter = SseProgressEmitter::new(event_tx);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = resume::resume_operation(&conn, op_id, &cancel_flag, &emitter) {
            eprintln!("resume {} failed: {:#}", op_id, e);
            let msg = format!("{:#}", e);
            let _ = adb::update_operation_status(&conn, op_id, ArchiveStatus::Failed, Some(&msg));
            let _ = rollback::rollback_operation(&conn, op_id, &emitter);
        }
        ctx.active_archives.lock().unwrap().remove(&op_id);
    });
    Ok(StatusCode::OK)
}

pub async fn rollback_archive_operation(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let event_tx = state.event_tx.clone();
    let op_id = req.operation_id;
    tokio::task::spawn_blocking(move || {
        let emitter = SseProgressEmitter::new(event_tx);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = rollback::rollback_operation(&conn, op_id, &emitter) {
            eprintln!("rollback {} failed: {:#}", op_id, e);
        }
    });
    Ok(StatusCode::OK)
}

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
    tokio::task::spawn_blocking(move || {
        let emitter = SseProgressEmitter::new(event_tx);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = restore::run_restore(
            &conn,
            op_id,
            Path::new(&target_root_path),
            overwrite_existing,
            keep_zip_after_restore,
            &cancel_flag,
            &emitter,
        ) {
            eprintln!("restore {} failed: {:#}", op_id, e);
        }
        ctx.active_archives.lock().unwrap().remove(&op_id);
    });
    Ok(StatusCode::OK)
}

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

    let files = adb::list_operation_files(&conn, req.operation_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let _ = std::fs::remove_file(&f.target_zip_path);
        }
    }

    let op = adb::get_operation(&conn, req.operation_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    conn.execute("DELETE FROM frames_set WHERE id = ?1", [op.frames_set_id])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::OK)
}

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

pub async fn add_archive_root(
    State(state): State<WebAppState>,
    Json(req): Json<AddArchiveRootRequest>,
) -> Result<Json<i64>, (StatusCode, String)> {
    if !std::path::Path::new(&req.path).is_dir() {
        return Err((StatusCode::BAD_REQUEST, format!("'{}' is not a directory", req.path)));
    }
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
        rusqlite::params![req.path, req.label, is_default],
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(conn.last_insert_rowid()))
}

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
