// File commands - file operations and browsing

use crate::db::{self};
use crate::models::*;
use athenaeum_core::file_op::{db as fdb, executor as fexec, models::FileOpStatus, planner as fplan};
use athenaeum_core::services::operation_queue::{OperationKind, QueuedJob};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

use super::AppState;

/// Get files from the database with optional limit
#[tauri::command]
pub async fn get_files(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let files = db::get_files(&conn, limit).map_err(|e| e.to_string())?;

    Ok(files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect())
}

/// Get files from a specific directory
#[tauri::command]
pub async fn get_files_by_directory(
    directory_path: String,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let files = db::get_files_by_directory(&conn, &directory_path, limit).map_err(|e| e.to_string())?;

    Ok(files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect())
}

/// Get frames with missing metadata
/// category: "all", "coordinates", "object", "datetime", "instrument", "frametype"
#[tauri::command]
pub async fn get_frames_with_missing_metadata(
    category: String,
    state: State<'_, AppState>,
) -> Result<Vec<MissingMetadataRow>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_frames_with_missing_metadata(&conn, &category).map_err(|e| {
        eprintln!("get_frames_with_missing_metadata failed: {}", e);
        e.to_string()
    })
}

/// Bulk-update metadata fields (camera / date_obs / imagetyp / is_master) on
/// a set of frames. DB-only — the FITS files on disk are NOT rewritten.
/// Returns the number of rows updated.
#[tauri::command]
pub async fn bulk_update_frame_metadata(
    frame_ids: Vec<i64>,
    edits: FrameMetadataEdits,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::bulk_update_frame_metadata(&conn, &frame_ids, &edits).map_err(|e| {
        eprintln!("bulk_update_frame_metadata command failed: {}", e);
        e.to_string()
    })
}

/// Re-decode the originally-scanned header values for the given frames
/// out of `fits_header` so the UI can show "what the file said before
/// you edited it" and offer per-field revert.
#[tauri::command]
pub async fn get_frame_metadata_originals(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<athenaeum_core::fits_parser::stored_header::FrameOriginalSnapshot>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    athenaeum_core::db::get_frame_metadata_originals(&conn, &frame_ids).map_err(|e| {
        eprintln!("get_frame_metadata_originals failed: {}", e);
        e.to_string()
    })
}

/// Aggregate a summary of which framesets / calibration sets the given
/// frames belong to (member of) or use (consumer of). Drives the metadata
/// pane's "Memberships" panel.
#[tauri::command]
pub async fn get_frame_memberships(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::db::FrameMembershipsSummary, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    athenaeum_core::db::get_frame_memberships_summary(&conn, &frame_ids).map_err(|e| {
        eprintln!("get_frame_memberships failed: {}", e);
        e.to_string()
    })
}

/// Count the calibration-set / session relations the given frames currently
/// have. The dual-pane metadata editor calls this before applying edits so
/// the user can see how many links the cascade will sever.
#[tauri::command]
pub async fn count_frame_metadata_relations(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::db::FrameMetadataRelations, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    athenaeum_core::db::count_frame_metadata_relations(&conn, &frame_ids).map_err(|e| {
        eprintln!("count_frame_metadata_relations failed: {}", e);
        e.to_string()
    })
}

/// Return the distinct non-empty INSTRUME values from the `frames` table,
/// alphabetically sorted. Feeds the Set Camera modal's existing-cameras list.
#[tauri::command]
pub async fn get_distinct_instrumes(
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_distinct_instrumes(&conn).map_err(|e| {
        eprintln!("get_distinct_instrumes command failed: {}", e);
        e.to_string()
    })
}

/// Get duplicate file groups (from cache)
#[tauri::command]
pub async fn get_duplicates(state: State<'_, AppState>) -> Result<Vec<DuplicateGroup>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Check setting to determine which hash type to use
    let use_content_hash = state.ctx.settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

    // Try to get from cache first (fast path)
    if db::has_duplicate_cache(&conn, use_content_hash).unwrap_or(false) {
        return db::get_cached_duplicates(&conn, use_content_hash).map_err(|e| e.to_string());
    }

    // Cache not populated - compute on the fly (slow path, for first load before any scan)
    db::find_duplicate_groups(&conn, use_content_hash).map_err(|e| e.to_string())
}

/// Get directory contents (subdirectories and files)
#[tauri::command]
pub async fn get_directory_contents(
    directory_path: String,
    state: State<'_, AppState>,
) -> Result<DirectoryContents, String> {
    use std::fs;

    let path = Path::new(&directory_path);

    // Use read_dir directly instead of an exists() pre-check. exists() returns
    // false on ANY error (permission flicker, transient FS state immediately
    // after a sibling rmdir, etc.), which would mislabel a still-present
    // directory as "doesn't exist". read_dir gives a real ErrorKind we can act on.
    let mut subdirectories = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("Directory does not exist".to_string());
        }
        Err(e) => {
            eprintln!("read_dir({}) failed: {}", directory_path, e);
            return Err(format!("Could not read directory: {}", e));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let entry_path = entry.path();
        let metadata = entry.metadata().map_err(|e| e.to_string())?;

        if metadata.is_dir() {
            subdirectories.push(entry_path.to_string_lossy().to_string());
        }
    }

    // Get files from database for this directory
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let db_files = db::get_files_by_directory(&conn, &directory_path, None)
        .map_err(|e| e.to_string())?;

    let files_in_dir: Vec<FileWithFrame> = db_files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect();

    subdirectories.sort();

    Ok(DirectoryContents {
        subdirectories,
        files: files_in_dir,
    })
}

/// Get frame preview image as base64-encoded JPEG
#[tauri::command]
pub async fn get_frame_preview(
    state: State<'_, AppState>,
    frame_id: i64,
    resolution: Option<String>,
) -> Result<String, String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    // Get file path for this frame
    let file_path: String = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        conn.query_row(
            "SELECT f.path FROM files f
             JOIN frames fr ON f.id = fr.file_id
             WHERE fr.id = ?1",
            [frame_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to find frame {}: {}", frame_id, e))?
    };

    // Use the existing rustafits command to get preview
    let jpeg_data = crate::commands_rustafits::read_fits_image_rustafits(
        file_path,
        resolution.or(Some("thumbnail".to_string())),
        state,
    )
    .await
    .map_err(|e| format!("Failed to generate preview: {}", e))?;

    // Encode as base64 for SVG embedding
    let base64_data = STANDARD.encode(&jpeg_data);
    Ok(format!("data:image/jpeg;base64,{}", base64_data))
}

/// Get files with frames by frame IDs
/// Useful for loading full file data when you only have frame IDs
#[tauri::command]
pub async fn get_files_with_frames_by_ids(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let frames = db::get_frames_with_files_by_ids(&conn, &frame_ids)
        .map_err(|e| format!("Failed to get frames: {}", e))?;

    Ok(frames
        .into_iter()
        .map(|(_file_id, file, frame)| FileWithFrame {
            file,
            frame: Some(frame),
        })
        .collect())
}

/// Get the path to the log directory (rolling daily JSONL files live inside it).
#[tauri::command]
pub async fn get_log_path() -> Result<String, String> {
    crate::logging::get_path()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Logging not initialized".to_string())
}

/// Get the path to the database file
#[tauri::command]
pub async fn get_database_path(
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let app_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(app_dir.join("athenaeum.db").to_string_lossy().to_string())
}

/// Get all distinct directory paths containing files for a given camera
#[tauri::command]
pub async fn get_camera_directories(
    instrume: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_camera_directories(&conn, &instrume).map_err(|e| e.to_string())
}

/// Get directory contents filtered to only files from a specific camera
#[tauri::command]
pub async fn get_camera_directory_contents(
    directory_path: String,
    instrume: String,
    camera_directories: Vec<String>,
    state: State<'_, AppState>,
) -> Result<DirectoryContents, String> {
    use std::fs;

    let path = Path::new(&directory_path);

    let mut subdirectories = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("Directory does not exist".to_string());
        }
        Err(e) => {
            eprintln!("read_dir({}) failed: {}", directory_path, e);
            return Err(format!("Could not read directory: {}", e));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let entry_path = entry.path();
        let metadata = entry.metadata().map_err(|e| e.to_string())?;

        if metadata.is_dir() {
            let subdir_str = entry_path.to_string_lossy().to_string();
            // Include this subdir if any camera directory starts with it or equals it
            let is_relevant = camera_directories.iter().any(|cam_dir| {
                cam_dir.starts_with(&subdir_str)
            });
            if is_relevant {
                subdirectories.push(subdir_str);
            }
        }
    }

    // Get files from database filtered by camera
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let db_files = db::get_files_by_directory_for_camera(&conn, &directory_path, &instrume, None)
        .map_err(|e| e.to_string())?;

    let files_in_dir: Vec<FileWithFrame> = db_files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect();

    subdirectories.sort();

    Ok(DirectoryContents {
        subdirectories,
        files: files_in_dir,
    })
}

// DTOs for files commands
#[derive(serde::Serialize)]
pub struct DirectoryContents {
    pub subdirectories: Vec<String>,
    pub files: Vec<FileWithFrame>,
}

// ============================================================================
// Dual-pane file browser commands (Phase 1: Move + catalog search)
// ============================================================================

/// One catalog search hit. Returns enough data for the UI to navigate the
/// active pane to the parent directory and highlight the file.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSearchHit {
    pub file_id: i64,
    pub path: String,
    pub filename: String,
    pub object: Option<String>,
    pub filter: Option<String>,
    pub imagetyp: Option<String>,
    pub instrume: Option<String>,
    pub telescop: Option<String>,
    pub date_obs: Option<String>,
}

/// Plan + immediately enqueue a Move operation. Returns the operation_id
/// once the plan is committed. The actual move runs on the global queue.
#[tauri::command]
pub async fn enqueue_move_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    sources: Vec<String>,
    dest_dir: String,
) -> Result<i64, String> {
    let ctx = state.ctx.clone();
    let db = ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let source_paths: Vec<PathBuf> = sources.iter().map(PathBuf::from).collect();
    let plan = fplan::build_move_plan(&conn, source_paths, PathBuf::from(&dest_dir))
        .map_err(|e| {
            eprintln!("build_move_plan failed: {:#}", e);
            format!("{:#}", e)
        })?;
    let op_id = plan.operation_id;

    // Cancel flag — stored in active_archives map until file_op gets its own
    // map (intentional reuse since the map is just "active op handles" and
    // already keyed by operation_id; the kind is recorded on the row).
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let ctx_for_worker = ctx.clone();
    let app_for_emitter = app.clone();
    let cancel_for_worker = cancel_flag.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::FileOpMove,
        operation_id: op_id,
        run: Box::new(move || {
            let emitter = crate::tauri_events::TauriProgressEmitter(app_for_emitter);
            let db = ctx_for_worker.db.get().expect("db");
            let conn = db.conn();

            // Heal any abandoned cross-volume moves left over from a prior
            // crash before running this one. Cheap no-op when there's
            // nothing to heal; errors here must not fail this user's move.
            match athenaeum_core::file_op::reconcile::reconcile_abandoned_commit_moves(&conn) {
                Ok(summary) if summary.healed > 0 || !summary.skipped.is_empty() => {
                    athenaeum_core::events::emit_event(
                        &emitter,
                        "file-op-reconciled",
                        &athenaeum_core::file_op::models::FileOpReconciled {
                            healed: summary.healed,
                            skipped: summary.skipped.len(),
                            operation_ids: summary.touched_operation_ids(),
                        },
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("file_op reconcile (pre-enqueue) failed: {:#}", e),
            }

            let result = fexec::run_operation(&conn, op_id, &cancel_for_worker, &emitter);
            let outcome = match result {
                Ok(()) => "completed",
                Err(e) => {
                    if fexec::was_cancelled(&e) {
                        let _ = fdb::update_operation_status(
                            &conn, op_id, FileOpStatus::Cancelled, None,
                        );
                        "cancelled"
                    } else {
                        eprintln!("file_op {} failed: {:#}", op_id, e);
                        let msg = format!("{:#}", e);
                        let _ = fdb::update_operation_status(
                            &conn, op_id, FileOpStatus::Failed, Some(&msg),
                        );
                        "failed"
                    }
                }
            };
            athenaeum_core::events::emit_event(
                &emitter,
                "file-op-finished",
                &serde_json::json!({ "operation_id": op_id, "outcome": outcome, "kind": "move" }),
            );
        }),
    });

    // Stash the cancel flag against the operation_id. We piggy-back on the
    // existing active_archives map for now — see Phase 2 for a dedicated
    // file-op handle map. (Reuse keeps cancel command shape symmetric.)
    {
        use athenaeum_core::services::ArchiveHandle;
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(op_id, ArchiveHandle { operation_id: op_id, cancel_flag });
    }

    Ok(op_id)
}

/// Free-text catalog search across all scan roots. Returns up to `limit`
/// hits matching by filename, OBJECT, FILTER, IMAGETYP, INSTRUME, or
/// TELESCOP (case-insensitive substring match).
///
/// `instrume_filter` constrains hits to a single camera (frames.instrume
/// equality). Used by the per-camera dual-pane file browser so a camera-
/// scoped view's search bar can't surface frames from other cameras.
#[tauri::command]
pub async fn search_catalog(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
    instrume_filter: Option<String>,
) -> Result<Vec<CatalogSearchHit>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.unwrap_or(200).min(500) as i64;
    let pattern = format!("%{}%", trimmed.to_lowercase());

    // Two SQL branches so `instrume_filter` either pins fr.instrume or is
    // skipped entirely (no `OR instrume IS NULL` ambiguity, no wasted bind).
    // The Vec is bound to a local before the block returns so `stmt`
    // outlives the iterator (rustc E0597).
    let rows = if let Some(cam) = instrume_filter.as_deref() {
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.filename,
                        fr.object, fr.filter, fr.imagetyp, fr.instrume, fr.telescop, fr.date_obs
                 FROM files f
                 LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE fr.instrume = ?3
                   AND (LOWER(f.filename) LIKE ?1
                     OR LOWER(f.path) LIKE ?1
                     OR LOWER(IFNULL(fr.object,''))    LIKE ?1
                     OR LOWER(IFNULL(fr.filter,''))    LIKE ?1
                     OR LOWER(IFNULL(fr.imagetyp,''))  LIKE ?1
                     OR LOWER(IFNULL(fr.instrume,''))  LIKE ?1
                     OR LOWER(IFNULL(fr.telescop,''))  LIKE ?1)
                 ORDER BY fr.date_obs DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let hits: Vec<CatalogSearchHit> = stmt
            .query_map(rusqlite::params![pattern, limit, cam], map_search_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        hits
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.filename,
                        fr.object, fr.filter, fr.imagetyp, fr.instrume, fr.telescop, fr.date_obs
                 FROM files f
                 LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE LOWER(f.filename) LIKE ?1
                    OR LOWER(f.path) LIKE ?1
                    OR LOWER(IFNULL(fr.object,''))    LIKE ?1
                    OR LOWER(IFNULL(fr.filter,''))    LIKE ?1
                    OR LOWER(IFNULL(fr.imagetyp,''))  LIKE ?1
                    OR LOWER(IFNULL(fr.instrume,''))  LIKE ?1
                    OR LOWER(IFNULL(fr.telescop,''))  LIKE ?1
                 ORDER BY fr.date_obs DESC
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let hits: Vec<CatalogSearchHit> = stmt
            .query_map(rusqlite::params![pattern, limit], map_search_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        hits
    };

    Ok(rows)
}

/// Row → CatalogSearchHit mapper shared between the two SQL branches above.
fn map_search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogSearchHit> {
    Ok(CatalogSearchHit {
        file_id: row.get(0)?,
        path: row.get(1)?,
        filename: row.get(2)?,
        object: row.get(3)?,
        filter: row.get(4)?,
        imagetyp: row.get(5)?,
        instrume: row.get(6)?,
        telescop: row.get(7)?,
        date_obs: row.get(8)?,
    })
}

/// Create a directory inside a scan root. Validated to refuse paths that
/// would land outside any configured scan root.
#[tauri::command]
pub async fn mkdir_in_scan_root(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let target = PathBuf::from(&path);
    let scan_roots = athenaeum_core::db::get_scan_roots(&conn).map_err(|e| e.to_string())?;
    let inside_root = scan_roots.iter().filter(|r| r.enabled).any(|r| {
        let root = PathBuf::from(&r.path);
        let rc = std::fs::canonicalize(&root).unwrap_or(root);
        let tc = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
        tc.starts_with(&rc)
    });
    if !inside_root {
        return Err(format!("'{}' is not inside any scan root", path));
    }
    std::fs::create_dir_all(&target).map_err(|e| {
        eprintln!("mkdir {} failed: {}", path, e);
        e.to_string()
    })
}

/// Rename a file or directory in place. Same-folder rename only (i.e., no
/// path traversal). Hot-syncs the catalog: for a file, updates `files.path`
/// and `files.filename`; for a directory, updates every `files.path` whose
/// path begins with the old directory path.
#[tauri::command]
pub async fn rename_path(
    state: State<'_, AppState>,
    old_path: String,
    new_name: String,
) -> Result<(), String> {
    if new_name.contains('/') || new_name.contains('\\') || new_name.trim().is_empty() {
        return Err("new name must be a single path component".into());
    }
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let old = PathBuf::from(&old_path);
    let parent = old
        .parent()
        .ok_or("source has no parent directory")?
        .to_path_buf();
    let new = parent.join(&new_name);
    if new.exists() {
        return Err(format!("target already exists: {}", new.display()));
    }

    // Validate inside scan root.
    let scan_roots = athenaeum_core::db::get_scan_roots(&conn).map_err(|e| e.to_string())?;
    let inside_root = scan_roots.iter().filter(|r| r.enabled).any(|r| {
        let root = PathBuf::from(&r.path);
        let rc = std::fs::canonicalize(&root).unwrap_or(root);
        let oc = std::fs::canonicalize(&old).unwrap_or_else(|_| old.clone());
        oc.starts_with(&rc)
    });
    if !inside_root {
        return Err(format!("'{}' is not inside any scan root", old_path));
    }

    let is_dir = old.is_dir();
    std::fs::rename(&old, &new).map_err(|e| {
        eprintln!("rename {} → {} failed: {}", old.display(), new.display(), e);
        e.to_string()
    })?;

    // Hot-sync catalog rows.
    let old_str = old.to_string_lossy().to_string();
    let new_str = new.to_string_lossy().to_string();
    if is_dir {
        // Recursively rewire every catalog row under the renamed folder via
        // the SUBSTR-based prefix swap (REPLACE is unsafe — see
        // `rename_files_path_prefix` doc).
        let prefix_old = format!("{}/", old_str);
        let prefix_new = format!("{}/", new_str);
        let updated = athenaeum_core::db::rename_files_path_prefix(&conn, &prefix_old, &prefix_new)
            .map_err(|e| {
                eprintln!("rename hot-sync (dir) failed: {}", e);
                e.to_string()
            })?;
        eprintln!("rename: hot-synced {} catalog rows under {}", updated, new_str);
    } else {
        let updated = conn
            .execute(
                "UPDATE files SET path = ?1, filename = ?2 WHERE path = ?3",
                rusqlite::params![&new_str, &new_name, &old_str],
            )
            .map_err(|e| {
                eprintln!("rename hot-sync (file) failed: {}", e);
                e.to_string()
            })?;
        eprintln!("rename: hot-synced {} catalog rows for {}", updated, new_str);
    }
    Ok(())
}
