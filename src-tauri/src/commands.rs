use crate::db::{self, Database};
use crate::models::*;
use crate::scanner::scan_directory;
use crate::settings::SettingsManager;
use std::path::Path;
use std::sync::Mutex;
use tauri::{Manager, State};

/// App state containing database connection and settings manager
pub struct AppState {
    pub db: Mutex<Option<Database>>,
    pub settings: SettingsManager,
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

// ============================================================================
// Settings Commands
// ============================================================================

/// Get a setting value by key (with precedence: runtime > DB > default)
#[tauri::command]
pub async fn get_setting(
    key: String,
    default_value: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let default = default_value.unwrap_or_default();
    state.settings
        .get_with_precedence(&conn, &key, &default)
        .map_err(|e| e.to_string())
}

/// Set a setting value (persists to database)
#[tauri::command]
pub async fn set_setting(
    key: String,
    value: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    state.settings
        .persist_setting(&conn, &key, &value)
        .map_err(|e| e.to_string())
}

/// Get all settings from database
#[tauri::command]
pub async fn get_all_settings(state: State<'_, AppState>) -> Result<Vec<Setting>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_all_settings(&conn).map_err(|e| e.to_string())
}

/// Delete a setting by key
#[tauri::command]
pub async fn delete_setting(key: String, state: State<'_, AppState>) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_setting(&conn, &key).map_err(|e| e.to_string())
}

/// Get the grouping threshold in degrees (with unit conversion)
#[tauri::command]
pub async fn get_grouping_threshold_deg(state: State<'_, AppState>) -> Result<f64, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    state.settings
        .get_grouping_threshold_deg(&conn)
        .map_err(|e| e.to_string())
}

// ============================================================================
// Frame Sets Commands
// ============================================================================

/// Auto-generate frame sets by clustering LIGHT frames
#[tauri::command]
pub async fn auto_generate_frame_sets(
    project_id: i64,
    state: State<'_, AppState>,
) -> Result<AutoGenerateResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get threshold from settings
    let threshold_deg = state.settings
        .get_grouping_threshold_deg(&conn)
        .map_err(|e| e.to_string())?;

    // Fetch all LIGHT frames
    let all_frames = db::get_light_frames_for_project(&conn, project_id)
        .map_err(|e| e.to_string())?;

    // Get all frame IDs that are already in any set
    let existing_member_ids = db::get_all_frames_set_member_ids(&conn)
        .map_err(|e| e.to_string())?;
    let existing_members_set: std::collections::HashSet<i64> = existing_member_ids.into_iter().collect();

    // Filter out frames that are already in sets
    let mut frames_already_in_sets = 0;
    let frames: Vec<(i64, crate::models::Frame)> = all_frames
        .into_iter()
        .filter(|(_, frame)| {
            if let Some(frame_id) = frame.id {
                if existing_members_set.contains(&frame_id) {
                    frames_already_in_sets += 1;
                    return false;
                }
            }
            true
        })
        .collect();

    if frames.is_empty() {
        return Ok(AutoGenerateResult {
            sets_created: 0,
            frames_clustered: 0,
            frames_excluded: 0,
            frames_already_in_sets,
            exclusion_reasons: Vec::new(),
        });
    }

    // Run clustering
    let (clusters, excluded) = crate::clustering::auto_generate_frame_sets(frames, threshold_deg)
        .map_err(|e| e.to_string())?;

    // Create frame sets in a transaction
    let mut sets_created = 0;
    let mut frames_clustered = 0;

    // Get session gap threshold from settings
    let gap_threshold_hours: f64 = state.settings
        .get_with_precedence(&conn, "session_gap_threshold_hours", "6.0")
        .map_err(|e| e.to_string())?
        .parse()
        .unwrap_or(6.0);

    for cluster in clusters {
        // Create frames_set (without project assignment - can be added later)
        let set_id = db::create_frames_set(
            &conn,
            cluster.name.as_deref(),
            cluster.date_obs.as_deref(),
            Some(&cluster.objctra),
            Some(&cluster.objctdec),
            cluster.total_exp_time,
            None, // Project can be assigned later from the interface
        ).map_err(|e| e.to_string())?;

        // Get frames for session detection
        let frames = db::get_frames_with_files_by_ids(&conn, &cluster.member_frame_ids)
            .map_err(|e| e.to_string())?;

        // Detect sessions
        let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
            .map_err(|e| e.to_string())?;

        // Create imaging nights and sessions
        for night in detected_nights {
            let night_id = db::create_imaging_night(
                &conn,
                set_id,
                &night.start_time,
                &night.end_time,
            ).map_err(|e| e.to_string())?;

            for session in night.sessions {
                let session_id = db::create_session(
                    &conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                ).map_err(|e| e.to_string())?;

                db::insert_session_members(&conn, session_id, &session.frame_ids)
                    .map_err(|e| e.to_string())?;
            }
        }

        sets_created += 1;
        frames_clustered += cluster.member_frame_ids.len();
    }

    Ok(AutoGenerateResult {
        sets_created,
        frames_clustered,
        frames_excluded: excluded.len(),
        frames_already_in_sets,
        exclusion_reasons: excluded.into_iter().map(|(_, reason)| reason).collect(),
    })
}

/// Get all frame sets for a project
#[tauri::command]
pub async fn get_frames_sets(
    project_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<FramesSetWithCount>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let sets = db::get_frames_sets_by_project(&conn, project_id)
        .map_err(|e| e.to_string())?;

    Ok(sets
        .into_iter()
        .map(|(set, count)| FramesSetWithCount {
            frames_set: set,
            member_count: count,
        })
        .collect())
}

/// Delete a frames_set
#[tauri::command]
pub async fn delete_frames_set(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_frames_set(&conn, frames_set_id).map_err(|e| e.to_string())
}

/// Rename a frames_set
#[tauri::command]
pub async fn rename_frames_set(
    frames_set_id: i64,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_frames_set_name(&conn, frames_set_id, &new_name).map_err(|e| e.to_string())
}

/// Get frame set detail with imaging nights and sessions
#[tauri::command]
pub async fn get_frame_set_detail(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get the frame set
    let sets = db::get_frames_sets_by_project(&conn, 1)
        .map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    // Check if sessions exist
    let sessions_exist = db::sessions_exist_for_frame_set(&conn, frames_set_id)
        .map_err(|e| e.to_string())?;

    if !sessions_exist {
        // Return empty nights instead of error
        return Ok(FrameSetDetail {
            frames_set,
            nights: Vec::new(),
        });
    }

    // Get the complete structure from database
    let nights = db::get_imaging_nights_with_sessions(&conn, frames_set_id)
        .map_err(|e| e.to_string())?;

    Ok(FrameSetDetail {
        frames_set,
        nights,
    })
}

// DTOs for frame sets
#[derive(serde::Serialize)]
pub struct AutoGenerateResult {
    pub sets_created: usize,
    pub frames_clustered: usize,
    pub frames_excluded: usize,
    pub frames_already_in_sets: usize,
    pub exclusion_reasons: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct FramesSetWithCount {
    pub frames_set: FramesSet,
    pub member_count: usize,
}
