//! Archive feature Tauri commands.

use super::AppState;
use athenaeum_core::archive::{db as adb, executor, planner, resume, restore, rollback, models::*};
use athenaeum_core::services::ArchiveHandle;
use athenaeum_core::settings::keys;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Migrate the legacy single-folder `archive.root_path` setting into the
/// archive_roots table on first call. Idempotent: if the table already has
/// rows, do nothing.
fn migrate_legacy_archive_root(conn: &Connection, settings: &athenaeum_core::settings::SettingsManager) -> Result<(), String> {
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
            ).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Resolve which archive root path to use for an operation. If the caller
/// passed an explicit path, validate it's a known archive root. Otherwise
/// pick the default (or the only one if just one exists). Errors when no
/// archive roots are configured or no default is set with multiple roots.
fn resolve_archive_root(
    conn: &Connection,
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
        return Err("no archive folders configured — add one in File Manager → Archive Folders".into());
    }
    if rows.len() == 1 {
        return Ok(rows[0].0.clone());
    }
    if let Some((path, _)) = rows.iter().find(|(_, d)| *d == 1) {
        return Ok(path.clone());
    }
    Err("multiple archive folders configured but no default — pick a destination explicitly".into())
}

#[tauri::command]
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
pub async fn add_archive_root(
    state: State<'_, AppState>,
    path: String,
    label: Option<String>,
) -> Result<i64, String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("'{}' is not a directory", path));
    }
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
pub async fn start_archive_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    frames_set_id: i64,
    dispositions: Dispositions,
    compression: ArchiveCompression,
    conflict_resolution: ConflictResolution,
    archive_root_path: Option<String>,
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
    let root = resolve_archive_root(&conn, &ctx.settings, archive_root_path.as_deref())?;

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
        let outcome = match restore::run_restore(
            &conn, operation_id, Path::new(&target_root_path),
            overwrite_existing, keep_zip_after_restore, &cancel_flag, &emitter,
        ) {
            Ok(()) => "completed",
            Err(e) => {
                eprintln!("restore {} failed: {:#}", operation_id, e);
                if format!("{:#}", e).contains("cancelled") { "cancelled" } else { "failed" }
            }
        };
        // Tell the UI the restore is over so the progress widget can dismiss.
        athenaeum_core::events::emit_event(
            &emitter,
            "archive-finished",
            &serde_json::json!({ "operation_id": operation_id, "outcome": outcome, "kind": "restore" }),
        );
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
