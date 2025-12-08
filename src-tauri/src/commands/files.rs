// File commands - file operations and browsing

use crate::db::{self, Database};
use crate::models::*;
use std::path::Path;
use std::sync::Mutex;
use tauri::State;

use super::AppState;

/// Get files from the database with optional limit
#[tauri::command]
pub async fn get_files(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
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
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let files = db::get_files_by_directory(&conn, &directory_path, limit).map_err(|e| e.to_string())?;

    Ok(files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect())
}

/// Get frames with missing metadata
/// category: "all", "coordinates", "object", "datetime", "instrument"
#[tauri::command]
pub async fn get_frames_with_missing_metadata(
    category: String,
    state: State<'_, AppState>,
) -> Result<Vec<FileWithFrame>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let files = db::get_frames_with_missing_metadata(&conn, &category).map_err(|e| e.to_string())?;

    Ok(files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file: file, frame: Some(frame) })
        .collect())
}

/// Get duplicate file groups
#[tauri::command]
pub async fn get_duplicates(state: State<'_, AppState>) -> Result<Vec<DuplicateGroup>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Check setting to determine which hash type to use
    let use_content_hash = state.settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

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

    if !path.exists() {
        return Err("Directory does not exist".to_string());
    }

    let mut subdirectories = Vec::new();
    let mut files_in_dir = Vec::new();

    let entries = fs::read_dir(path).map_err(|e| e.to_string())?;

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let entry_path = entry.path();
        let metadata = entry.metadata().map_err(|e| e.to_string())?;

        if metadata.is_dir() {
            subdirectories.push(entry_path.to_string_lossy().to_string());
        }
    }

    // Get files from database for this directory
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let db_files = db::get_files_by_directory(&conn, &directory_path, None)
        .map_err(|e| e.to_string())?;

    files_in_dir = db_files
        .into_iter()
        .map(|(file, frame)| FileWithFrame { file, frame })
        .collect();

    subdirectories.sort();

    Ok(DirectoryContents {
        subdirectories,
        files: files_in_dir,
    })
}

/// Get details about orphaned files for user review
#[tauri::command]
pub async fn get_orphaned_files(
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::OrphanedFile>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    crate::relinking::get_orphaned_file_details(&conn, &file_ids)
        .map_err(|e| format!("Failed to get orphaned file details: {}", e))
}

/// Delete orphaned files from database
#[tauri::command]
pub async fn delete_orphaned_files(
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    crate::relinking::delete_orphaned_files(&conn, &file_ids)
        .map_err(|e| format!("Failed to delete orphaned files: {}", e))
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
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
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

// DTOs for files commands
#[derive(serde::Serialize)]
pub struct DirectoryContents {
    pub subdirectories: Vec<String>,
    pub files: Vec<FileWithFrame>,
}
