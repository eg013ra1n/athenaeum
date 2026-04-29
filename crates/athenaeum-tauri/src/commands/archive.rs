//! Archive feature Tauri commands.

use super::AppState;
use athenaeum_core::archive::{db as adb, executor, planner, resume, restore, rollback, models::*};
use athenaeum_core::services::ArchiveHandle;
use athenaeum_core::settings::keys;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
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
pub async fn set_archive_root_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    state.ctx.settings.persist_setting(&db.conn(), keys::ARCHIVE_ROOT_PATH, &path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
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
pub async fn plan_archive_operation(
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
) -> Result<ArchivePlan, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = state.ctx.settings.get_archive_root_path(&conn)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "archive root path is not set".to_string())?;
    planner::build_plan(
        &conn, frames_set_id, Path::new(&root), &dispositions, compression,
    ).map_err(|e| format!("{:#}", e))
}

#[tauri::command]
pub async fn start_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict_resolution: ConflictResolution,
) -> Result<i64, String> {
    // One-at-a-time enforcement.
    {
        let map = state.ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err("another archive operation is already in progress".into());
        }
    }
    let ctx = state.ctx.clone();
    let db = ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let root = ctx.settings.get_archive_root_path(&conn)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "archive root path is not set".to_string())?;

    // Build + commit the plan synchronously.
    let plan = planner::build_plan(
        &conn, frames_set_id, Path::new(&root), &dispositions, compression,
    ).map_err(|e| format!("{:#}", e))?;
    let op_id = planner::commit_plan(&conn, &plan, conflict_resolution)
        .map_err(|e| format!("{:#}", e))?;

    // Register the cancel flag.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(op_id, ArchiveHandle { operation_id: op_id, cancel_flag: cancel_flag.clone() });
    }

    // Spawn worker. DB connection is obtained inside the thread (connections are not Send).
    let ctx_for_worker = ctx.clone();
    let app_for_emitter = app.clone();
    std::thread::spawn(move || {
        let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
        let db = ctx_for_worker.db.get().expect("db");
        let conn = db.conn();
        let result = executor::run_operation(&conn, op_id, &cancel_flag, &emitter);
        let outcome = match result {
            Ok(()) => {
                eprintln!("archive operation {} completed", op_id);
                "completed"
            }
            Err(e) => {
                let outcome = if executor::was_cancelled(&e) {
                    let _ = adb::update_operation_status(
                        &conn, op_id, ArchiveStatus::Cancelled, None,
                    );
                    "cancelled"
                } else {
                    eprintln!("archive operation {} failed: {:#}", op_id, e);
                    let msg = format!("{:#}", e);
                    let _ = adb::update_operation_status(
                        &conn, op_id, ArchiveStatus::Failed, Some(&msg),
                    );
                    "failed"
                };
                // Auto-rollback on cancel or failure.
                if let Err(rb_err) = rollback::rollback_operation(&conn, op_id, &emitter) {
                    eprintln!("rollback for {} failed: {:#}", op_id, rb_err);
                }
                outcome
            }
        };
        // Tell the UI the operation is over so the progress widget can dismiss.
        athenaeum_core::events::emit_event(
            &emitter,
            "archive-finished",
            &serde_json::json!({ "operation_id": op_id, "outcome": outcome }),
        );
        // Remove from active map regardless of outcome.
        let mut map = ctx_for_worker.active_archives.lock().unwrap();
        map.remove(&op_id);
    });

    Ok(op_id)
}

#[tauri::command]
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
pub async fn list_unfinished_archive_operations(
    state: State<'_, AppState>,
) -> Result<Vec<ArchiveOperationSummary>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    resume::find_unfinished_operations(&db.conn()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resume_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
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
    std::thread::spawn(move || {
        let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = resume::resume_operation(&conn, operation_id, &cancel_flag, &emitter) {
            eprintln!("resume {} failed: {:#}", operation_id, e);
            let msg = format!("{:#}", e);
            let _ = adb::update_operation_status(&conn, operation_id, ArchiveStatus::Failed, Some(&msg));
            let _ = rollback::rollback_operation(&conn, operation_id, &emitter);
        }
        ctx.active_archives.lock().unwrap().remove(&operation_id);
    });

    Ok(())
}

#[tauri::command]
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
                if let Some(stripped) = source_path.strip_suffix(&path_in_zip) {
                    let trimmed = stripped.trim_end_matches('/');
                    Ok(if trimmed.is_empty() { None } else { Some(trimmed.to_string()) })
                } else {
                    Ok(None)
                }
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
pub async fn rollback_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_id: i64,
) -> Result<(), String> {
    let ctx = state.ctx.clone();
    let app_for_emitter = app.clone();
    std::thread::spawn(move || {
        let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = rollback::rollback_operation(&conn, operation_id, &emitter) {
            eprintln!("rollback {} failed: {:#}", operation_id, e);
        }
    });
    Ok(())
}

#[tauri::command]
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
    std::thread::spawn(move || {
        let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        if let Err(e) = restore::run_restore(
            &conn, operation_id, Path::new(&target_root_path),
            overwrite_existing, keep_zip_after_restore, &cancel_flag, &emitter,
        ) {
            eprintln!("restore {} failed: {:#}", operation_id, e);
        }
        ctx.active_archives.lock().unwrap().remove(&operation_id);
    });
    Ok(())
}

#[tauri::command]
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

    // Delete zip files
    let files = adb::list_operation_files(&conn, operation_id).map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    for f in &files {
        if seen.insert(f.target_zip_path.clone()) {
            let _ = std::fs::remove_file(&f.target_zip_path);
        }
    }

    // Get frames_set_id, then delete frame set + cascading rows.
    let op = adb::get_operation(&conn, operation_id).map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM frames_set WHERE id = ?1",
        [op.frames_set_id],
    ).map_err(|e| e.to_string())?;
    // archive_operations row is also deleted via FK cascade from frames_set_id.

    Ok(())
}
