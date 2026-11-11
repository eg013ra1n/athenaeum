// Archive feature route handlers for the Axum web API.
//
// Mirrors the Tauri archive commands. Progress events are broadcast via SSE
// instead of Tauri's emit mechanism.

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;
use athenaeum_core::archive::{planner, resume, rollback, models::*};
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
    // The web refuses a second archive while one is active (409); the desktop
    // queues it. The difference is the `refuse_when_busy` argument.
    athenaeum_core::api::archive::start_archive_operation(
        state.ctx.clone(),
        Arc::new(SseProgressEmitter::new(state.event_tx.clone())),
        req.frames_set_id,
        &req.dispositions,
        req.compression,
        req.conflict_resolution,
        req.archive_root_path.as_deref(),
        true,
    )
    .map(Json)
    .map_err(api_err)
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
    athenaeum_core::api::archive::get_restore_suggestions(&state.ctx, req.operation_id)
        .map(Json)
        .map_err(api_err)
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
            // Terminal event + failure bookkeeping in core — kept in lockstep
            // with the desktop resume worker. Errors logged inside.
            let _ = resume::resume_operation_standalone(&conn, op_id, &cancel_flag, &emitter);
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
    athenaeum_core::api::archive::list_archive_zips(&state.ctx, req.operation_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_archived_frame_sets(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    athenaeum_core::api::archive::list_archived_frame_sets(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_restore_operation(
    State(state): State<WebAppState>,
    Json(req): Json<StartRestoreRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    athenaeum_core::api::archive::start_restore_operation(
        state.ctx.clone(),
        emitter,
        req.operation_id,
        req.target_root_path,
        req.overwrite_existing,
        req.keep_zip_after_restore,
    )
    .map_err(api_err)?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_archive(
    State(state): State<WebAppState>,
    Json(req): Json<OperationIdRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    athenaeum_core::api::archive::delete_archive(&state.ctx, req.operation_id).map_err(api_err)?;
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
