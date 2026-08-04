//! Archive feature Tauri commands.

use super::AppState;
use athenaeum_core::archive::{db as adb, executor, planner, resume, restore, rollback, models::*};
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
    let ctx = state.ctx.clone();
    let db = ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = resolve_archive_root(&conn, &ctx.settings, archive_root_path.as_deref())?;

    // Build + commit the plan synchronously.
    let plan = planner::build_plan(
        &conn, frames_set_id, Path::new(&root), &dispositions, compression,
    ).map_err(|e| format!("{:#}", e))?;
    let op_id = planner::commit_plan(&conn, &plan, conflict_resolution)
        .map_err(|e| format!("{:#}", e))?;

    // Register the cancel flag so cancel_archive_operation can find it.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(op_id, ArchiveHandle { operation_id: op_id, cancel_flag: cancel_flag.clone() });
    }

    // Enqueue. Single global worker (shared with file ops) drives execution
    // serially. DB connection is obtained inside the closure since rusqlite
    // connections are not Send across thread boundaries.
    let ctx_for_worker = ctx.clone();
    let app_for_emitter = app.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();
            // Outcome bookkeeping + terminal `archive-finished` live in core
            // (`run_operation_standalone`) — shared with the web worker so the
            // two backends can't drift again. Errors are logged (and rolled
            // back) inside; nothing to add here.
            let _ = executor::run_operation_standalone(&conn, op_id, &cancel_flag, &emitter);
            let mut map = ctx_for_worker.active_archives.lock().unwrap();
            map.remove(&op_id);
        }),
    });

    Ok(op_id)
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
            // to emit nothing, so the (future) resume widget could never
            // dismiss. Errors logged inside.
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
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Take the first operation file and strip the target_path_in_zip from
    // the source_path; what remains is the original parent directory.
    // All files in a single operation share the same parent (since prefixes
    // are scan-root-relative).
    let suggested: Option<String> = conn
        .query_row(
            "SELECT source_path, target_path_in_zip
             FROM archive_operation_files
             WHERE operation_id = ?1
             LIMIT 1",
            [operation_id],
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
        .map_err(|e| e.to_string())?;

    // Existing scan roots
    let mut stmt = conn
        .prepare("SELECT path FROM scan_roots ORDER BY path")
        .map_err(|e| e.to_string())?;
    let scan_roots: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;

    // Whether the suggested original path still exists and is writable.
    let suggested_exists = suggested
        .as_ref()
        .map(|p| std::path::Path::new(p).is_dir())
        .unwrap_or(false);

    Ok(serde_json::json!({
        "suggested_original_path": suggested,
        "suggested_original_exists": suggested_exists,
        "scan_roots": scan_roots,
    }))
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
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    // Unique target_zip_paths for the operation, ordered by frame_role priority.
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
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([operation_id], |row| {
            let path: String = row.get(0)?;
            let frame_role: String = row.get(1)?;
            let p = std::path::Path::new(&path);
            let filename = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
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
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_archived_frame_sets(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let mut stmt = conn.prepare(
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
    ).map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = stmt.query_map([], |row| {
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
    }).map_err(|e| e.to_string())?
      .collect::<rusqlite::Result<Vec<_>>>()
      .map_err(|e| e.to_string())?;
    Ok(rows)
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
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err("another archive operation is already in progress".into());
        }
    }
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
            let (outcome, conflicts) = match restore::run_restore(
                &conn, operation_id, Path::new(&target_root_path),
                overwrite_existing, keep_zip_after_restore, &cancel_flag, &emitter,
            ) {
                Ok(result) if result.has_conflicts() => {
                    tracing::warn!(
                        operation_id,
                        conflicts = result.conflicts.len(),
                        paths = ?result.conflicts.iter().map(|c| c.source_path.as_str()).collect::<Vec<_>>(),
                        "restore completed with conflicts"
                    );
                    ("completed_with_conflicts", result.conflicts.len())
                }
                Ok(_) => ("completed", 0),
                Err(e) => {
                    tracing::error!(operation_id, error = ?e, "restore failed");
                    let outcome = if format!("{:#}", e).contains("cancelled") { "cancelled" } else { "failed" };
                    (outcome, 0)
                }
            };
            athenaeum_core::events::emit_event(
                &emitter,
                "archive-finished",
                &serde_json::json!({
                    "operation_id": operation_id,
                    "outcome": outcome,
                    "kind": "restore",
                    "conflicts": conflicts,
                }),
            );
            ctx_for_worker.active_archives.lock().unwrap().remove(&operation_id);
        }),
    });
    Ok(())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_archive(
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err("another archive operation is already in progress".into());
        }
    }
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Delete zip files. A zip we could not delete (Windows sharing violation,
    // read-only volume) must abort the whole command BEFORE any catalog row
    // goes away — otherwise the zip is orphaned on disk with nothing pointing
    // at it and the archive becomes unrestorable.
    let files = adb::list_operation_files(&conn, operation_id).map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut failed: Vec<String> = Vec::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let p = std::path::Path::new(&f.target_zip_path);
            if p.exists() {
                if let Err(e) = std::fs::remove_file(p) {
                    tracing::error!(operation_id, path = %f.target_zip_path, error = %e, "failed to delete archive zip");
                    failed.push(format!("{}: {}", f.target_zip_path, e));
                }
            }
        }
    }
    if !failed.is_empty() {
        return Err(format!(
            "could not delete {} zip file(s); catalog rows kept — any zip already removed in this pass is not restorable: {}",
            failed.len(),
            failed.join("; ")
        ));
    }

    // Get frames_set_id, then delete frame set + cascading rows.
    let op = adb::get_operation(&conn, operation_id).map_err(|e| e.to_string())?;
    match op.frames_set_id {
        Some(fs_id) => {
            conn.execute("DELETE FROM frames_set WHERE id = ?1", [fs_id])
                .map_err(|e| e.to_string())?;
            // archive_operations row is also deleted via FK cascade from frames_set_id.
        }
        None => {
            // Calibration-set archive op (Task 14): no frames_set to cascade
            // from — delete the archive_operations row directly (cascades to
            // archive_operation_files/steps via their own FKs).
            conn.execute("DELETE FROM archive_operations WHERE id = ?1", [operation_id])
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(())
}
