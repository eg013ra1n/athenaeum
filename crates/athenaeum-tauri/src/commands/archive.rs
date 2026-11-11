//! Archive feature Tauri commands.

use super::AppState;
use athenaeum_core::archive::{planner, resume, rollback, models::*};
use athenaeum_core::services::operation_queue::{OperationKind, QueuedJob};
use athenaeum_core::services::ArchiveHandle;
use athenaeum_core::settings::keys;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Migrate the legacy single-folder `archive.root_path` setting into the
/// archive_roots table on first call. Idempotent: if the table already has
/// rows, do nothing. Thin `Result<_, String>` shim over the shared core
/// helper (`athenaeum_core::archive::migrate_legacy_archive_root`) so the
/// existing `?`-into-`String` call sites below don't change.
fn migrate_legacy_archive_root(conn: &Connection, settings: &athenaeum_core::settings::SettingsManager) -> Result<(), String> {
    athenaeum_core::archive::migrate_legacy_archive_root(conn, settings).map_err(|e| format!("{:#}", e))
}

/// Resolve which archive root path to use for an operation. Thin
/// `Result<_, String>` shim over the shared core helper
/// (`athenaeum_core::archive::resolve_archive_root`) — moved there (Task 14)
/// so `api::masters::archive_originals` can share the exact same
/// default-root selection logic instead of a third copy.
fn resolve_archive_root(
    conn: &Connection,
    settings: &athenaeum_core::settings::SettingsManager,
    requested: Option<&str>,
) -> Result<String, String> {
    athenaeum_core::archive::resolve_archive_root(conn, settings, requested).map_err(|e| format!("{:#}", e))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_archive_roots(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    migrate_legacy_archive_root(&conn, &state.ctx.settings)?;
    let mut stmt = conn
        .prepare("SELECT id, path, label, is_default FROM archive_roots ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "path": row.get::<_, String>(1)?,
                "label": row.get::<_, Option<String>>(2)?,
                "is_default": row.get::<_, i32>(3)? == 1,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn add_archive_root(
    state: State<'_, AppState>,
    path: String,
    label: Option<String>,
) -> Result<i64, String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("'{}' is not a directory", path));
    }
    // Store the canonical normalized spelling — archive roots were the one
    // root table whose rows were inserted verbatim from the picker, so
    // `C:\Archive` vs `c:\Archive` (or a \\?\-verbatim form) could coexist as
    // two rows and the exact-string lookup in resolve_archive_root rejected
    // legitimate re-picks of the same folder.
    let path = match std::path::Path::new(&path).canonicalize() {
        Ok(c) => athenaeum_core::api::scan_roots::normalize_path(&c)
            .to_string_lossy()
            .to_string(),
        Err(e) => return Err(format!("failed to resolve '{}': {}", path, e)),
    };
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    migrate_legacy_archive_root(&conn, &state.ctx.settings)?;
    // First root added becomes default automatically.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_roots", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let is_default = if count == 0 { 1 } else { 0 };
    conn.execute(
        "INSERT INTO archive_roots (path, label, is_default) VALUES (?1, ?2, ?3)",
        params![path, label, is_default],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_archive_root(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let was_default: i32 = conn
        .query_row("SELECT is_default FROM archive_roots WHERE id = ?1", [id], |r| r.get(0))
        .unwrap_or(0);
    conn.execute("DELETE FROM archive_roots WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    // If we deleted the default, promote the lowest-id remaining row to default.
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
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_default_archive_root(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    conn.execute("UPDATE archive_roots SET is_default = 0", [])
        .map_err(|e| e.to_string())?;
    let n = conn
        .execute("UPDATE archive_roots SET is_default = 1 WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err(format!("archive root id {} not found", id));
    }
    Ok(())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_archive_settings(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let ctx = state.ctx.clone();
    let db = ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = ctx.settings.get_archive_root_path(&conn).map_err(|e| e.to_string())?;
    let compression = ctx.settings.get_archive_compression(&conn).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "rootPath": root,
        "compression": compression,
    }))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_archive_root_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    state.ctx.settings.persist_setting(&db.conn(), keys::ARCHIVE_ROOT_PATH, &path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_archive_compression(
    state: State<'_, AppState>,
    compression: String,
) -> Result<(), String> {
    if !matches!(compression.as_str(), "store" | "deflate") {
        return Err(format!("invalid compression value: {}", compression));
    }
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    state.ctx.settings.persist_setting(&db.conn(), keys::ARCHIVE_COMPRESSION, &compression)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn plan_archive_operation(
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    archive_root_path: Option<String>,
) -> Result<ArchivePlan, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = resolve_archive_root(&conn, &state.ctx.settings, archive_root_path.as_deref())?;
    planner::build_plan(
        &conn, frames_set_id, Path::new(&root), &dispositions, compression,
    ).map_err(|e| format!("{:#}", e))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict_resolution: ConflictResolution,
    archive_root_path: Option<String>,
) -> Result<i64, String> {
    athenaeum_core::api::archive::start_archive_operation(
        state.ctx.clone(),
        Arc::new(crate::tauri_events::TauriProgressEmitter(app)),
        frames_set_id,
        &dispositions,
        compression,
        conflict_resolution,
        archive_root_path.as_deref(),
        false,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_archive_operation(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    let map = state.ctx.active_archives.lock().unwrap();
    if let Some(handle) = map.get(&operation_id) {
        handle.cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    } else {
        Err(format!("no active archive operation with id {}", operation_id))
    }
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_unfinished_archive_operations(
    state: State<'_, AppState>,
) -> Result<Vec<ArchiveOperationSummary>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    resume::find_unfinished_operations(&db.conn()).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn resume_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    let ctx = state.ctx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    ctx.active_archives.lock().unwrap().insert(operation_id, ArchiveHandle {
        operation_id, cancel_flag: cancel_flag.clone(),
    });

    let app_for_emitter = app.clone();
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id,
        run: Box::new(move || {
            let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            // Terminal event + failure bookkeeping in core — this worker used
            // to emit nothing, so the resume widget could never dismiss.
            // Errors logged inside.
            let _ = resume::resume_operation_standalone(&conn, operation_id, &cancel_flag, &emitter);
            ctx_for_worker.active_archives.lock().unwrap().remove(&operation_id);
        }),
    });

    Ok(())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_restore_suggestions(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<serde_json::Value, String> {
    athenaeum_core::api::archive::get_restore_suggestions(&state.ctx, operation_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn rollback_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    let ctx = state.ctx.clone();
    let app_for_emitter = app.clone();
    let ctx_for_worker = ctx.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id,
        run: Box::new(move || {
            let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            if let Err(e) = rollback::rollback_operation_standalone(&conn, operation_id, &emitter) {
                tracing::error!(operation_id, error = ?e, "archive rollback failed");
            }
        }),
    });
    Ok(())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_archive_zips(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<Vec<serde_json::Value>, String> {
    athenaeum_core::api::archive::list_archive_zips(&state.ctx, operation_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_archived_frame_sets(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    athenaeum_core::api::archive::list_archived_frame_sets(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_restore_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
    target_root_path: String,
    overwrite_existing: bool,
    keep_zip_after_restore: bool,
) -> Result<(), String> {
    let emitter = Arc::new(crate::tauri_events::TauriProgressEmitter(app));
    athenaeum_core::api::archive::start_restore_operation(
        state.ctx.clone(),
        emitter,
        operation_id,
        target_root_path,
        overwrite_existing,
        keep_zip_after_restore,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_archive(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    athenaeum_core::api::archive::delete_archive(&state.ctx, operation_id)
        .map_err(|e| e.to_string())
}
