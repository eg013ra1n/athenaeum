use crate::db::{self, Database};
use crate::models::*;
use crate::scanner::scan_directory;
use std::path::Path;
use std::sync::Mutex;
use tauri::{Manager, State};

/// App state containing database connection
pub struct AppState {
    pub db: Mutex<Option<Database>>,
}

#[tauri::command]
pub fn greet(name: &str) -> String {
    format!("Hello, {}! Welcome to Athenaeum!", name)
}

#[tauri::command]
pub async fn initialize_database(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let app_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;

    std::fs::create_dir_all(&app_dir).map_err(|e| e.to_string())?;

    let db_path = app_dir.join("athenaeum.db");
    let db = Database::new(db_path.clone()).map_err(|e| e.to_string())?;

    *state.db.lock().unwrap() = Some(db);

    Ok(db_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn add_scan_root(path: String, state: State<'_, AppState>) -> Result<ScanRoot, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Check if path already exists
    let existing_roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;
    if existing_roots.iter().any(|r| r.path == path) {
        return Err("This directory is already being monitored".to_string());
    }

    let id = db::upsert_scan_root(&conn, &path).map_err(|e| e.to_string())?;

    Ok(ScanRoot {
        id: Some(id),
        path,
        enabled: true,
        last_scan: None,
    })
}

#[tauri::command]
pub async fn get_scan_roots(state: State<'_, AppState>) -> Result<Vec<ScanRoot>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_scan_roots(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_scan_root(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_scan_root(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_scan(root_id: i64, state: State<'_, AppState>) -> Result<ScanResultDto, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get the scan root path
    let roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;
    let root = roots
        .into_iter()
        .find(|r| r.id == Some(root_id))
        .ok_or("Scan root not found")?;

    // Perform the scan
    let result = scan_directory(Path::new(&root.path), &conn, None);

    // Update last_scan timestamp
    db::update_scan_root_timestamp(&conn, root_id).map_err(|e| e.to_string())?;

    Ok(ScanResultDto {
        files_found: result.files_found,
        files_processed: result.files_processed,
        files_skipped: result.files_skipped,
        errors: result.errors,
    })
}

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

#[tauri::command]
pub async fn get_duplicates(state: State<'_, AppState>) -> Result<Vec<DuplicateGroup>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::find_duplicate_groups(&conn).map_err(|e| e.to_string())
}

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

// DTOs for serialization
#[derive(serde::Serialize)]
pub struct ScanResultDto {
    pub files_found: usize,
    pub files_processed: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct FileWithFrame {
    pub file: File,
    pub frame: Option<Frame>,
}

#[derive(serde::Serialize)]
pub struct DirectoryContents {
    pub subdirectories: Vec<String>,
    pub files: Vec<FileWithFrame>,
}
