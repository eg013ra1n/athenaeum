// File route handlers — mirrors athenaeum-tauri/src/commands/files.rs

use athenaeum_core::db;
use athenaeum_core::models::FileWithFrame;
use axum::{extract::State, http::StatusCode, Json};
use std::fs;
use std::path::Path;

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
pub async fn get_files(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
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
pub async fn get_files_by_directory(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesByDirectoryArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
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
pub async fn get_directory_contents(
    State(state): State<WebAppState>,
    Json(args): Json<GetDirectoryContentsArgs>,
) -> Result<Json<DirectoryContents>, (StatusCode, String)> {
    let path = Path::new(&args.path);

    if !path.exists() {
        return Err((StatusCode::NOT_FOUND, "Directory does not exist".to_string()));
    }

    // Collect subdirectories from the filesystem
    let mut subdirectories = Vec::new();
    let entries =
        fs::read_dir(path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
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
pub async fn get_camera_directories(
    State(state): State<WebAppState>,
    Json(args): Json<GetCameraDirectoriesArgs>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
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
pub async fn get_camera_directory_contents(
    State(state): State<WebAppState>,
    Json(args): Json<GetCameraDirectoryContentsArgs>,
) -> Result<Json<DirectoryContents>, (StatusCode, String)> {
    let path = Path::new(&args.directory_path);

    if !path.exists() {
        return Err((StatusCode::NOT_FOUND, "Directory does not exist".to_string()));
    }

    // Filter subdirectories to those relevant for this camera
    let mut subdirectories = Vec::new();
    let entries =
        fs::read_dir(path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
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
pub async fn get_frames_with_missing_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramesWithMissingMetadataArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let files = db::get_frames_with_missing_metadata(&conn, &args.category)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(
        files
            .into_iter()
            .map(|(file, frame)| FileWithFrame {
                file,
                frame: Some(frame),
            })
            .collect(),
    ))
}

/// POST /api/get_files_with_frames_by_ids
///
/// Bulk-loads full file and frame records for the given list of frame IDs.
/// Useful when you have frame IDs from a frame set and need the complete
/// metadata for display.
pub async fn get_files_with_frames_by_ids(
    State(state): State<WebAppState>,
    Json(args): Json<GetFilesWithFramesByIdsArgs>,
) -> Result<Json<Vec<FileWithFrame>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
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
