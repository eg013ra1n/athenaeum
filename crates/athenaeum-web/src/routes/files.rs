// File route handlers — mirrors athenaeum-tauri/src/commands/files.rs

use athenaeum_core::db;
use athenaeum_core::file_op::{db as fdb, executor as fexec, models::FileOpStatus, planner as fplan};
use athenaeum_core::models::{FileWithFrame, FrameMetadataEdits, MissingMetadataRow};
use athenaeum_core::services::operation_queue::{OperationKind, QueuedJob};
use athenaeum_core::services::ArchiveHandle;
use axum::{extract::State, http::StatusCode, Json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::events::SseProgressEmitter;
use crate::WebAppState;

// ── Response DTO ─────────────────────────────────────────────────────────────

/// Mirrors the DirectoryContents DTO from the Tauri files commands.
#[derive(serde::Serialize)]
pub struct DirectoryContents {
    pub subdirectories: Vec<String>,
    pub files: Vec<FileWithFrame>,
}

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GetFilesArgs {
    pub limit: Option<usize>,
}

#[derive(serde::Deserialize)]
pub struct GetFilesByDirectoryArgs {
    #[serde(rename = "directoryPath")]
    pub directory_path: String,
    pub limit: Option<usize>,
}

#[derive(serde::Deserialize)]
pub struct GetDirectoryContentsArgs {
    #[serde(rename = "directoryPath")]
    pub path: String,
}

#[derive(serde::Deserialize)]
pub struct GetCameraDirectoriesArgs {
    pub instrume: String,
}

#[derive(serde::Deserialize)]
pub struct GetCameraDirectoryContentsArgs {
    pub instrume: String,
    #[serde(rename = "directoryPath")]
    pub directory_path: String,
    #[serde(rename = "cameraDirectories")]
    pub camera_directories: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct GetFramesWithMissingMetadataArgs {
    /// Category filter: "all", "coordinates", "object", "datetime", "instrument", "frametype"
    pub category: String,
}

#[derive(serde::Deserialize)]
pub struct GetFilesWithFramesByIdsArgs {
    #[serde(rename = "frameIds")]
    pub frame_ids: Vec<i64>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/get_files
///
/// Returns files with their frame metadata, most recently created first.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_files(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let files = db::get_files(&conn, args.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(
        files
            .into_iter()
            .map(|(file, frame)| FileWithFrame { file, frame })
            .collect(),
    ))
}

/// POST /api/get_files_by_directory
///
/// Returns files directly inside the given directory (non-recursive), with
/// their frame metadata. The `directoryPath` must be an absolute path.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_files_by_directory(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesByDirectoryArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let files = db::get_files_by_directory(&conn, &args.directory_path, args.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(
        files
            .into_iter()
            .map(|(file, frame)| FileWithFrame { file, frame })
            .collect(),
    ))
}

/// POST /api/get_directory_contents
///
/// Returns the immediate subdirectories of `path` (via the filesystem) and the
/// files directly inside it (from the database).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_directory_contents(
    State(state): State<WebAppState>,
    Json(args): Json<GetDirectoryContentsArgs>,
) -> Result<Json<DirectoryContents>, (StatusCode, String)> {
    let path = Path::new(&args.path);

    // Use read_dir directly so we get a real ErrorKind. exists() returns
    // false on permission flicker / transient FS state too, which would
    // wrongly mislabel an existing directory as missing.
    let mut subdirectories = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err((StatusCode::NOT_FOUND, "Directory does not exist".to_string()));
        }
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };

    for entry in entries {
        let entry = entry.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let metadata = entry
            .metadata()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if metadata.is_dir() {
            subdirectories.push(entry.path().to_string_lossy().to_string());
        }
    }
    subdirectories.sort();

    // Look up files from the database for this directory
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let db_files = db::get_files_by_directory(&conn, &args.path, None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let files: Vec<FileWithFrame> = db_files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect();

    Ok(Json(DirectoryContents {
        subdirectories,
        files,
    }))
}

/// POST /api/get_camera_directories
///
/// Returns all distinct directory paths that contain files from the given
/// camera (`instrume`), ordered alphabetically.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_camera_directories(
    State(state): State<WebAppState>,
    Json(args): Json<GetCameraDirectoriesArgs>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let dirs = db::get_camera_directories(&conn, &args.instrume)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(dirs))
}

/// POST /api/get_camera_directory_contents
///
/// Returns the immediate subdirectories of `directoryPath` that are ancestors
/// of (or equal to) a camera directory, plus the files directly inside it
/// that belong to the given camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_camera_directory_contents(
    State(state): State<WebAppState>,
    Json(args): Json<GetCameraDirectoryContentsArgs>,
) -> Result<Json<DirectoryContents>, (StatusCode, String)> {
    let path = Path::new(&args.directory_path);

    let mut subdirectories = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err((StatusCode::NOT_FOUND, "Directory does not exist".to_string()));
        }
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };

    for entry in entries {
        let entry = entry.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let metadata = entry
            .metadata()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if metadata.is_dir() {
            let subdir_str = entry.path().to_string_lossy().to_string();
            let is_relevant = args
                .camera_directories
                .iter()
                .any(|cam_dir| cam_dir.starts_with(&subdir_str));
            if is_relevant {
                subdirectories.push(subdir_str);
            }
        }
    }
    subdirectories.sort();

    // Look up files for this camera from the database
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let db_files =
        db::get_files_by_directory_for_camera(&conn, &args.directory_path, &args.instrume, None)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let files: Vec<FileWithFrame> = db_files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect();

    Ok(Json(DirectoryContents {
        subdirectories,
        files,
    }))
}

/// POST /api/get_frames_with_missing_metadata
///
/// Returns light frames (and optionally frames with no imagetyp) that have
/// incomplete metadata. The `category` field controls which metadata gap to
/// filter on: "all", "coordinates", "object", "datetime", "instrument", or
/// "frametype".
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frames_with_missing_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramesWithMissingMetadataArgs>,
) -> Result<Json<Vec<MissingMetadataRow>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    // Error logged once by the `#[tracing::instrument(err(Debug))]` attribute
    // on this handler — see the T7 sweep report.
    let rows = db::get_frames_with_missing_metadata(&conn, &args.category)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(rows))
}

// ── Bulk frame metadata edits ────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct BulkUpdateFrameMetadataArgs {
    #[serde(rename = "frameIds")]
    pub frame_ids: Vec<i64>,
    pub edits: FrameMetadataEdits,
}

#[derive(serde::Deserialize)]
pub struct CountFrameMetadataRelationsArgs {
    #[serde(rename = "frameIds")]
    pub frame_ids: Vec<i64>,
}

/// POST /api/get_frame_metadata_originals
///
/// Re-decode the originally-scanned header values for given frames so the
/// UI can compare against current edits and revert per field.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_metadata_originals(
    State(state): State<WebAppState>,
    Json(args): Json<CountFrameMetadataRelationsArgs>,
) -> Result<Json<Vec<athenaeum_core::fits_parser::stored_header::FrameOriginalSnapshot>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();
    let snaps = athenaeum_core::db::get_frame_metadata_originals(&conn, &args.frame_ids)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(snaps))
}

/// POST /api/get_frame_memberships
///
/// Aggregate which framesets / calibration sets the given frames belong to.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_memberships(
    State(state): State<WebAppState>,
    Json(args): Json<CountFrameMetadataRelationsArgs>,
) -> Result<Json<athenaeum_core::db::FrameMembershipsSummary>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();
    let summary = athenaeum_core::db::get_frame_memberships_summary(&conn, &args.frame_ids)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(summary))
}

/// POST /api/count_frame_metadata_relations
///
/// Counts the calibration-set / session relations for the given frames so
/// the metadata editor can warn the user before applying edits that will
/// unlink them.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn count_frame_metadata_relations(
    State(state): State<WebAppState>,
    Json(args): Json<CountFrameMetadataRelationsArgs>,
) -> Result<Json<athenaeum_core::db::FrameMetadataRelations>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();
    let rel = athenaeum_core::db::count_frame_metadata_relations(&conn, &args.frame_ids)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(rel))
}

/// POST /api/bulk_update_frame_metadata
///
/// DB-only bulk update of camera / date_obs / imagetyp / is_master on the given
/// frames. Used by the Missing Metadata page's Set Camera / Set Date / Set
/// Frame Type actions. Returns the number of rows updated.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn bulk_update_frame_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<BulkUpdateFrameMetadataArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let count = db::bulk_update_frame_metadata(&conn, &args.frame_ids, &args.edits)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(count))
}

/// POST /api/get_distinct_instrumes
///
/// Returns the distinct non-empty INSTRUME values from the frames table,
/// alphabetically sorted. Feeds the Set Camera modal dropdown.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_distinct_instrumes(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let cameras = db::get_distinct_instrumes(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(cameras))
}

/// POST /api/get_files_with_frames_by_ids
///
/// Bulk-loads full file and frame records for the given list of frame IDs.
/// Useful when you have frame IDs from a frame set and need the complete
/// metadata for display.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_files_with_frames_by_ids(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesWithFramesByIdsArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let frames = db::get_frames_with_files_by_ids(&conn, &args.frame_ids)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(
        frames
            .into_iter()
            .map(|(_file_id, file, frame)| FileWithFrame {
                file,
                frame: Some(frame),
            })
            .collect(),
    ))
}

// ── Browse directories (web-only) ──────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct BrowseDirectoriesArgs {
    pub path: Option<String>,
    /// `"scan"` (default) validates against `allowed_paths`;
    /// `"export"` validates against the configured export directory.
    pub scope: Option<String>,
}

#[derive(serde::Serialize)]
pub struct BrowseDirectoryEntry {
    pub name: String,
    pub path: String,
}

#[derive(serde::Serialize)]
pub struct BrowseDirectoriesResponse {
    pub current: String,
    pub parent: Option<String>,
    pub directories: Vec<BrowseDirectoryEntry>,
}

/// POST /api/browse_directories
///
/// Returns subdirectories of the given path. If path is empty or omitted,
/// returns the root entries for the requested scope.
///
/// `scope = "scan"` (default): validates against `state.allowed_paths`.
/// `scope = "export"`: validates against the configured export directory.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn browse_directories(
    State(state): State<WebAppState>,
    Json(args): Json<BrowseDirectoriesArgs>,
) -> Result<Json<BrowseDirectoriesResponse>, (StatusCode, String)> {
    let path_str = args.path.unwrap_or_default();
    let scope = args.scope.as_deref().unwrap_or("scan");

    // Resolve the set of root paths for this scope
    let root_paths: Vec<PathBuf> = match scope {
        "export" => {
            match state.export_dir {
                Some(ref dir) => vec![dir.clone()],
                None => return Err((StatusCode::BAD_REQUEST, "No export directory configured".to_string())),
            }
        }
        _ => state.allowed_paths.clone(),
    };

    // If no path provided, return root paths as top-level entries
    if path_str.is_empty() || path_str == "/" {
        let directories: Vec<BrowseDirectoryEntry> = root_paths
            .iter()
            .filter(|p| p.is_dir())
            .map(|p| {
                let s = p.to_string_lossy().to_string();
                BrowseDirectoryEntry {
                    name: p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| s.clone()),
                    path: s,
                }
            })
            .collect();

        return Ok(Json(BrowseDirectoriesResponse {
            current: "/".to_string(),
            parent: None,
            directories,
        }));
    }

    let target = PathBuf::from(&path_str);
    let canonical = target
        .canonicalize()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid path: {}", e)))?;

    // Security: validate path is within scope roots
    let is_allowed = root_paths.iter().any(|allowed| {
        allowed.canonicalize().map(|a| canonical.starts_with(&a)).unwrap_or(false)
    });

    if !is_allowed {
        return Err((StatusCode::FORBIDDEN, "Path is outside allowed directories".to_string()));
    }

    if !canonical.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "Path is not a directory".to_string()));
    }

    let mut directories = Vec::new();
    let entries = fs::read_dir(&canonical)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to read directory: {}", e)))?;

    for entry in entries {
        let entry = entry.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let metadata = entry.metadata().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if metadata.is_dir() {
            let entry_path = entry.path();
            directories.push(BrowseDirectoryEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry_path.to_string_lossy().to_string(),
            });
        }
    }
    directories.sort_by(|a, b| a.name.cmp(&b.name));

    let parent = canonical.parent().and_then(|p| {
        let parent_str = p.to_string_lossy().to_string();
        // Only return parent if it's still within a scope root
        let parent_within = root_paths.iter().any(|allowed| {
            allowed.canonicalize().map(|a| p.starts_with(&a)).unwrap_or(false)
        });
        if parent_within {
            Some(parent_str)
        } else {
            // Parent is at or above a root — go back to root listing
            None
        }
    });

    Ok(Json(BrowseDirectoriesResponse {
        current: canonical.to_string_lossy().to_string(),
        parent,
        directories,
    }))
}

// ============================================================================
// Dual-pane file browser routes (Phase 1: Move + catalog search)
// ============================================================================

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

#[derive(serde::Deserialize)]
pub struct EnqueueMoveArgs {
    pub sources: Vec<String>,
    #[serde(rename = "destDir")]
    pub dest_dir: String,
}

#[derive(serde::Deserialize)]
pub struct SearchCatalogArgs {
    pub query: String,
    pub limit: Option<u32>,
    /// When set, restricts hits to a single camera (frames.instrume equality).
    /// Used by the per-camera dual-pane file browser so a camera-scoped view
    /// can't surface frames from other cameras. Mirrors the same parameter on
    /// the Tauri side.
    #[serde(default)]
    pub instrume_filter: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct MkdirArgs {
    pub path: String,
}

#[derive(serde::Deserialize)]
pub struct RenamePathArgs {
    #[serde(rename = "oldPath")]
    pub old_path: String,
    #[serde(rename = "newName")]
    pub new_name: String,
}

/// `pub(crate)` (not private) so other route modules — e.g.
/// `scan_roots::relink_scan_root` — can reuse the same allowed-paths check
/// instead of copy-pasting another inline variant.
pub(crate) fn path_inside_allowed(path: &Path, allowed: &[PathBuf]) -> bool {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    allowed.iter().any(|root| {
        let rc = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        canonical.starts_with(&rc)
    })
}

/// POST /api/enqueue_move_operation
#[tracing::instrument(skip_all, err(Debug))]
pub async fn enqueue_move_operation(
    State(state): State<WebAppState>,
    Json(args): Json<EnqueueMoveArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    // Path validation against ATHENAEUM_ALLOWED_PATHS.
    let allowed = &state.allowed_paths;
    if !allowed.is_empty() {
        let dest = PathBuf::from(&args.dest_dir);
        if !path_inside_allowed(&dest, allowed) {
            return Err((StatusCode::FORBIDDEN, format!("dest '{}' not allowed", args.dest_dir)));
        }
        for s in &args.sources {
            let sp = PathBuf::from(s);
            if !path_inside_allowed(&sp, allowed) {
                return Err((StatusCode::FORBIDDEN, format!("source '{}' not allowed", s)));
            }
        }
    }

    let ctx = state.ctx.clone();
    let db = ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();

    let source_paths: Vec<PathBuf> = args.sources.iter().map(PathBuf::from).collect();
    let plan = fplan::build_move_plan(&conn, source_paths, PathBuf::from(&args.dest_dir))
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{:#}", e)))?;
    let op_id = plan.operation_id;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(op_id, ArchiveHandle { operation_id: op_id, cancel_flag: cancel_flag.clone() });
    }

    let event_tx = state.event_tx.clone();
    let ctx_for_worker = ctx.clone();
    let cancel_for_worker = cancel_flag.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::FileOpMove,
        operation_id: op_id,
        run: Box::new(move || {
            let emitter = SseProgressEmitter::new(event_tx);
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
                Err(e) => tracing::warn!(operation_id = op_id, error = ?e, "file_op reconcile (pre-enqueue) failed"),
            }

            let result = fexec::run_operation(&conn, op_id, &cancel_for_worker, &emitter);
            match result {
                Ok(()) => {}
                Err(e) => {
                    if fexec::was_cancelled(&e) {
                        let _ = fdb::update_operation_status(
                            &conn, op_id, FileOpStatus::Cancelled, None,
                        );
                    } else {
                        tracing::error!(operation_id = op_id, error = ?e, "file_op failed");
                        let msg = format!("{:#}", e);
                        let _ = fdb::update_operation_status(
                            &conn, op_id, FileOpStatus::Failed, Some(&msg),
                        );
                    }
                }
            }
        }),
    });

    Ok(Json(op_id))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn search_catalog(
    State(state): State<WebAppState>,
    Json(args): Json<SearchCatalogArgs>,
) -> Result<Json<Vec<CatalogSearchHit>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();

    let trimmed = args.query.trim();
    if trimmed.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let limit = args.limit.unwrap_or(200).min(500) as i64;
    let pattern = format!("%{}%", trimmed.to_lowercase());

    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<CatalogSearchHit> {
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
    };

    // Two SQL branches so `instrume_filter` either pins fr.instrume or is
    // skipped entirely. The Vec is bound to a local before the block returns
    // so `stmt` outlives the iterator (rustc E0597).
    let rows: Vec<CatalogSearchHit> = if let Some(cam) = args.instrume_filter.as_deref() {
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
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let hits: Vec<CatalogSearchHit> = stmt
            .query_map(rusqlite::params![pattern, limit, cam], map_row)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let hits: Vec<CatalogSearchHit> = stmt
            .query_map(rusqlite::params![pattern, limit], map_row)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        hits
    };

    Ok(Json(rows))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn mkdir_in_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<MkdirArgs>,
) -> Result<StatusCode, (StatusCode, String)> {
    let allowed = &state.allowed_paths;
    let target = PathBuf::from(&args.path);
    if !allowed.is_empty() && !path_inside_allowed(&target, allowed) {
        return Err((StatusCode::FORBIDDEN, format!("'{}' not allowed", args.path)));
    }
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();
    let scan_roots = athenaeum_core::db::get_scan_roots(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let inside_root = scan_roots.iter().filter(|r| r.enabled).any(|r| {
        let root = PathBuf::from(&r.path);
        let rc = fs::canonicalize(&root).unwrap_or(root);
        let tc = fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
        tc.starts_with(&rc)
    });
    if !inside_root {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("'{}' is not inside any scan root", args.path),
        ));
    }
    fs::create_dir_all(&target).map_err(|e| {
        tracing::error!(path = %args.path, error = %e, "mkdir failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    Ok(StatusCode::OK)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn rename_path(
    State(state): State<WebAppState>,
    Json(args): Json<RenamePathArgs>,
) -> Result<StatusCode, (StatusCode, String)> {
    if args.new_name.contains('/') || args.new_name.contains('\\') || args.new_name.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "new name must be a single path component".into(),
        ));
    }
    let allowed = &state.allowed_paths;
    let old = PathBuf::from(&args.old_path);
    if !allowed.is_empty() && !path_inside_allowed(&old, allowed) {
        return Err((StatusCode::FORBIDDEN, format!("'{}' not allowed", args.old_path)));
    }
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "db not init".into()))?;
    let conn = db.conn();

    let parent = old
        .parent()
        .ok_or((StatusCode::BAD_REQUEST, "source has no parent dir".into()))?
        .to_path_buf();
    let new = parent.join(&args.new_name);
    if new.exists() {
        return Err((StatusCode::CONFLICT, format!("target already exists: {}", new.display())));
    }
    let scan_roots = athenaeum_core::db::get_scan_roots(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let inside_root = scan_roots.iter().filter(|r| r.enabled).any(|r| {
        let root = PathBuf::from(&r.path);
        let rc = fs::canonicalize(&root).unwrap_or(root);
        let oc = fs::canonicalize(&old).unwrap_or_else(|_| old.clone());
        oc.starts_with(&rc)
    });
    if !inside_root {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("'{}' is not inside any scan root", args.old_path),
        ));
    }
    let is_dir = old.is_dir();
    fs::rename(&old, &new).map_err(|e| {
        tracing::error!(src = %old.display(), dest = %new.display(), error = %e, "rename failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let old_str = old.to_string_lossy().to_string();
    let new_str = new.to_string_lossy().to_string();
    if is_dir {
        let prefix_old = format!("{}/", old_str);
        let prefix_new = format!("{}/", new_str);
        let _ = athenaeum_core::db::rename_files_path_prefix(&conn, &prefix_old, &prefix_new);
    } else {
        let _ = conn.execute(
            "UPDATE files SET path = ?1, filename = ?2 WHERE path = ?3",
            rusqlite::params![&new_str, &args.new_name, &old_str],
        );
    }
    Ok(StatusCode::OK)
}
