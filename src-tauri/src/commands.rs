use crate::cache::{CacheManager, CacheStats, StretchParams, StretchMode};
use crate::db::{self, Database};
use crate::models::*;
use crate::scanner::scan_directory;
use crate::settings::SettingsManager;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};

/// App state containing database connection, settings manager, and cache manager
pub struct AppState {
    pub db: Mutex<Option<Database>>,
    pub settings: Arc<SettingsManager>,
    pub cache: Arc<Mutex<Option<CacheManager>>>,
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

    // 1. Check if directory exists
    let path_buf = Path::new(&path);
    if !path_buf.exists() {
        return Err("Directory does not exist".to_string());
    }
    if !path_buf.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    // 2. Canonicalize the new path (resolve symlinks, .., etc.)
    let new_path = path_buf
        .canonicalize()
        .map_err(|e| format!("Failed to resolve path: {}", e))?;

    // 3. Get existing scan roots and check for overlaps
    let existing_roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;

    for root in existing_roots.iter() {
        let existing_path = Path::new(&root.path)
            .canonicalize()
            .map_err(|e| format!("Failed to resolve existing root path: {}", e))?;

        // Check exact match
        if new_path == existing_path {
            return Err("This directory is already being monitored".to_string());
        }

        // Check if new path is a subdirectory of existing root
        if new_path.starts_with(&existing_path) {
            return Err(format!(
                "Cannot add directory: it is a subdirectory of existing scan root '{}'",
                root.path
            ));
        }

        // Check if new path is a parent of existing root
        if existing_path.starts_with(&new_path) {
            return Err(format!(
                "Cannot add directory: existing scan root '{}' is a subdirectory of it",
                root.path
            ));
        }
    }

    // 4. Store the canonicalized path
    let path_str = new_path.to_string_lossy().to_string();
    let id = db::upsert_scan_root(&conn, &path_str).map_err(|e| e.to_string())?;

    Ok(ScanRoot {
        id: Some(id),
        path: path_str,
        enabled: true,
        find_duplicates: true,
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

    // Check if content hash should be computed
    let use_content_hash = state.settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

    // Perform the scan
    let result = scan_directory(Path::new(&root.path), &conn, None, use_content_hash);

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
pub async fn rescan_all_for_content_hash(state: State<'_, AppState>) -> Result<RescanResultDto, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    println!("Starting content hash rescan for all files...");

    // Get all files from database
    let all_files = db::get_files(&conn, None).map_err(|e| e.to_string())?;
    let total = all_files.len();

    let mut updated = 0;
    let mut skipped = 0;
    let mut missing = 0;
    let mut errors = Vec::new();

    for (file, _frame) in all_files {
        let path_buf = std::path::PathBuf::from(&file.path);

        // Skip if file doesn't exist on disk
        if !path_buf.exists() {
            missing += 1;
            continue;
        }

        // Skip if already has content hash
        if file.content_hash.is_some() {
            skipped += 1;
            continue;
        }

        // Compute content hash
        match crate::duplicates::compute_xxhash(&path_buf) {
            Ok(hash) => {
                // Update database
                match conn.execute(
                    "UPDATE files SET content_hash = ?1 WHERE id = ?2",
                    rusqlite::params![hash, file.id],
                ) {
                    Ok(_) => {
                        updated += 1;
                        if updated % 100 == 0 {
                            println!("Progress: {}/{} files processed", updated + skipped + missing, total);
                        }
                    }
                    Err(e) => {
                        let error_msg = format!("{}: Failed to update database: {}", file.path, e);
                        errors.push(error_msg);
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("{}: Failed to compute hash: {}", file.path, e);
                errors.push(error_msg);
            }
        }
    }

    println!("Content hash rescan complete!");
    println!("Total: {}, Updated: {}, Skipped: {}, Missing: {}, Errors: {}",
        total, updated, skipped, missing, errors.len());

    // Mark content hash rescan as completed
    if updated > 0 || skipped > 0 {
        // Only set flag if we actually processed files successfully
        state.settings
            .persist_setting(&conn, "duplicates.content_hash_rescanned", "true")
            .map_err(|e| format!("Failed to set rescan flag: {}", e))?;
        println!("Content hash rescan flag set to true");
    }

    Ok(RescanResultDto {
        files_total: total,
        files_updated: updated,
        files_skipped: skipped,
        files_missing: missing,
        errors,
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

    // Check setting to determine which hash type to use
    let use_content_hash = state.settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

    db::find_duplicate_groups(&conn, use_content_hash).map_err(|e| e.to_string())
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
pub struct RescanResultDto {
    pub files_total: usize,
    pub files_updated: usize,
    pub files_skipped: usize,
    pub files_missing: usize,
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
            false, // is_custom = false for auto-generated sets
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

/// Create a custom frames set with selected sessions
#[tauri::command]
pub async fn create_custom_frames_set(
    name: String,
    session_ids: Vec<i64>,
    project_id: Option<i64>,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    println!("Creating custom frames set: name='{}', session_ids={:?}, project_id={:?}", name, session_ids, project_id);

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get frames for all selected sessions to determine date ranges
    let mut all_session_frames = Vec::new();
    for session_id in &session_ids {
        println!("Getting frames for session {}", session_id);
        let frame_ids = db::get_frame_ids_for_session(&conn, *session_id)
            .map_err(|e| e.to_string())?;

        println!("Session {} has {} frames", session_id, frame_ids.len());

        if !frame_ids.is_empty() {
            let frames = db::get_frames_with_files_by_ids(&conn, &frame_ids)
                .map_err(|e| e.to_string())?;

            all_session_frames.push((*session_id, frames));
        }
    }

    if all_session_frames.is_empty() {
        return Err("No frames found in selected sessions".to_string());
    }

    println!("Total sessions with frames: {}", all_session_frames.len());

    // Get session gap threshold from settings
    let gap_threshold_hours: f64 = state.settings
        .get_with_precedence(&conn, "session_gap_threshold_hours", "6.0")
        .map_err(|e| e.to_string())?
        .parse()
        .unwrap_or(6.0);

    // Flatten all frames and group by session_id for later
    let mut session_frame_map: std::collections::HashMap<i64, Vec<(i64, crate::models::File, crate::models::Frame)>> =
        std::collections::HashMap::new();
    let mut all_frames = Vec::new();

    for (session_id, frames) in all_session_frames {
        session_frame_map.insert(session_id, frames.clone());
        all_frames.extend(frames);
    }

    // Detect nights from all frames combined
    println!("Detecting nights from {} frames with gap threshold {} hours", all_frames.len(), gap_threshold_hours);
    let detected_nights = crate::sessions::detect_sessions(all_frames, gap_threshold_hours)
        .map_err(|e| {
            let err_msg = format!("Failed to detect sessions: {}", e);
            println!("{}", err_msg);
            err_msg
        })?;

    println!("Detected {} nights", detected_nights.len());

    if detected_nights.is_empty() {
        return Err("No imaging nights could be detected from the selected sessions. Frames may be missing date/time information.".to_string());
    }

    // Calculate aggregate values from all frames
    let mut total_exp_time: f64 = 0.0;
    let mut earliest_date_obs: Option<String> = None;
    let mut ra_values: Vec<f64> = Vec::new();
    let mut dec_values: Vec<f64> = Vec::new();

    for (_, frames) in &session_frame_map {
        for (_, _, frame) in frames {
            // Sum exposure time
            if let Some(exp) = frame.exptime {
                total_exp_time += exp;
            }

            // Track earliest date
            if let Some(date) = &frame.date_obs {
                let date_str = date.to_rfc3339();
                if earliest_date_obs.is_none() || date_str < earliest_date_obs.as_ref().unwrap().clone() {
                    earliest_date_obs = Some(date_str);
                }
            }

            // Collect RA/Dec for averaging (use objctra/objctdec as strings)
            // We'll just use the first frame's coordinates for simplicity
            if ra_values.is_empty() {
                if let Some(ra) = &frame.objctra {
                    ra_values.push(0.0); // Placeholder, we'll use the string directly
                }
            }
        }
    }

    // Get first frame's coordinates (simpler than averaging coordinate strings)
    let (first_ra, first_dec) = session_frame_map.values()
        .flat_map(|frames| frames.iter())
        .find_map(|(_, _, frame)| {
            if let (Some(ra), Some(dec)) = (&frame.objctra, &frame.objctdec) {
                Some((ra.clone(), dec.clone()))
            } else {
                None
            }
        })
        .unwrap_or((String::new(), String::new()));

    println!("Calculated aggregates: total_exp_time={}, earliest_date={:?}, coordinates={}/{}",
             total_exp_time, earliest_date_obs, first_ra, first_dec);

    // Create the custom frames_set
    println!("Creating frames_set with name '{}'", name);
    let set_id = db::create_frames_set(
        &conn,
        Some(&name),
        true, // is_custom = true
        earliest_date_obs.as_deref(),
        if first_ra.is_empty() { None } else { Some(&first_ra) },
        if first_dec.is_empty() { None } else { Some(&first_dec) },
        Some(total_exp_time),
        project_id,
    ).map_err(|e| {
        let err_msg = format!("Failed to create frames_set: {}", e);
        println!("{}", err_msg);
        err_msg
    })?;

    println!("Created frames_set with id {}", set_id);

    // Create imaging_nights and clone sessions
    let mut total_sessions_cloned = 0;
    for (night_idx, night) in detected_nights.iter().enumerate() {
        println!("Processing night {}/{}: {} to {}", night_idx + 1, detected_nights.len(), night.start_time, night.end_time);

        let night_id = db::create_imaging_night(
            &conn,
            set_id,
            &night.start_time,
            &night.end_time,
        ).map_err(|e| {
            let err_msg = format!("Failed to create imaging_night: {}", e);
            println!("{}", err_msg);
            err_msg
        })?;

        println!("Created imaging_night with id {}", night_id);

        // For each original session, check if any of its frames belong to this night
        for &session_id in &session_ids {
            if let Some(session_frames) = session_frame_map.get(&session_id) {
                // Check if any frames from this session are in this night
                let session_frame_ids: std::collections::HashSet<i64> =
                    session_frames.iter().map(|(_, _, frame)| frame.id.unwrap()).collect();

                let night_frame_ids: std::collections::HashSet<i64> =
                    night.sessions.iter()
                        .flat_map(|s| &s.frame_ids)
                        .copied()
                        .collect();

                let intersection: Vec<i64> = session_frame_ids.intersection(&night_frame_ids)
                    .copied()
                    .collect();

                // If this session has frames in this night, clone it
                if !intersection.is_empty() {
                    println!("Cloning session {} to night {} ({} overlapping frames)", session_id, night_id, intersection.len());
                    db::clone_session(&conn, session_id, night_id)
                        .map_err(|e| {
                            let err_msg = format!("Failed to clone session {}: {}", session_id, e);
                            println!("{}", err_msg);
                            err_msg
                        })?;
                    total_sessions_cloned += 1;
                } else {
                    println!("Session {} has no frames in night {}", session_id, night_id);
                }
            }
        }
    }

    println!("Successfully created custom frames_set '{}' (id {}) with {} imaging nights and {} cloned sessions",
             name, set_id, detected_nights.len(), total_sessions_cloned);

    Ok(set_id)
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

// ========== Equipment & Dark Library Commands ==========

#[tauri::command]
pub async fn get_equipment_cameras(
    state: State<'_, AppState>
) -> Result<Vec<CameraStats>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_all_cameras(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_dark_library(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<DarkLibraryResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get thresholds from settings
    let date_threshold = state.settings
        .get_dark_library_date_threshold(&conn)
        .map_err(|e| e.to_string())?;

    let temp_threshold = state.settings
        .get_dark_library_temp_threshold(&conn)
        .map_err(|e| e.to_string())?;

    // Create the dark library
    crate::calibration::create_dark_library(
        &conn,
        &instrume,
        date_threshold,
        temp_threshold,
    ).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_dark_library(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<Vec<CalibrationSetDetail>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_camera_dark_library(&conn, &instrume).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_dark_library(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_camera_dark_library(&conn, &instrume).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn has_dark_library(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<bool, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::has_dark_library(&conn, &instrume).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_master_dark_library(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<DarkLibraryResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get thresholds from settings
    let date_threshold = state.settings
        .get_dark_library_date_threshold(&conn)
        .map_err(|e| e.to_string())?;

    let temp_threshold = state.settings
        .get_dark_library_temp_threshold(&conn)
        .map_err(|e| e.to_string())?;

    // Create the master dark library
    crate::calibration::create_master_dark_library(
        &conn,
        &instrume,
        date_threshold,
        temp_threshold,
    ).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_master_dark_library(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<Vec<CalibrationSetDetail>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_camera_master_dark_library(&conn, &instrume).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn has_master_dark_library(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<bool, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::has_master_dark_library(&conn, &instrume).map_err(|e| e.to_string())
}

/// Helper function to format bytes in human-readable format
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// Get cache statistics (for display in UI)
#[tauri::command]
pub async fn get_cache_stats(state: State<'_, AppState>) -> Result<CacheStats, String> {
    let cache_arc = state.cache.clone();
    let stats_result = {
        let cache_guard = cache_arc.lock().unwrap();
        if let Some(cache_mgr) = cache_guard.as_ref() {
            use tokio::runtime::Handle;
            use tokio::task;

            task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.get_stats().await
                })
            })
        } else {
            Err(anyhow::anyhow!("Cache manager not available"))
        }
    };

    stats_result.map_err(|e| e.to_string())
}

/// Clear all cached images
#[tauri::command]
pub async fn clear_image_cache(state: State<'_, AppState>) -> Result<String, String> {
    println!("🗑️  Clearing image cache...");

    let cache_arc = state.cache.clone();

    // Get cache stats before clearing
    let stats_result = {
        let cache_guard = cache_arc.lock().unwrap();
        if let Some(cache_mgr) = cache_guard.as_ref() {
            use tokio::runtime::Handle;
            use tokio::task;

            task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.get_stats().await
                })
            })
        } else {
            Err(anyhow::anyhow!("Cache manager not available"))
        }
    };

    let (total_entries, total_size) = match stats_result {
        Ok(stats) => {
            println!("📊 Cache stats: {} entries, {}", stats.total_entries, format_bytes(stats.total_size_bytes));
            (stats.total_entries, stats.total_size_bytes)
        }
        Err(e) => {
            println!("⚠️  Could not get cache stats: {}", e);
            (0, 0)
        }
    };

    // Now clear the cache
    let result = {
        let cache_guard = cache_arc.lock().unwrap();
        if let Some(cache_mgr) = cache_guard.as_ref() {
            use tokio::runtime::Handle;
            use tokio::task;

            task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.invalidate_all().await
                })
            })
        } else {
            Err(anyhow::anyhow!("Cache manager not available"))
        }
    };

    match result {
        Ok(_) => {
            let msg = if total_size > 0 {
                format!("Cache cleared successfully. Freed {} ({} entries)",
                    format_bytes(total_size), total_entries)
            } else {
                "Cache cleared successfully".to_string()
            };
            println!("✅ {}", msg);
            Ok(msg)
        }
        Err(e) => {
            let error_msg = format!("Failed to clear cache: {}", e);
            eprintln!("❌ ERROR: {}", error_msg);
            Err(error_msg)
        }
    }
}

// ===== BLACK HOLE & SCAN ROOT COMMANDS =====

/// Update scan root find_duplicates flag
#[tauri::command]
pub async fn set_scan_root_duplicates_flag(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_scan_root_duplicates_flag(&conn, id, enabled).map_err(|e| e.to_string())
}

/// Move a file to the black hole (soft delete)
#[tauri::command]
pub async fn move_to_black_hole(
    file_id: i64,
    from_where: String,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get original path
    let original_path: String = conn
        .query_row("SELECT path FROM files WHERE id = ?1", [file_id], |row| {
            row.get(0)
        })
        .map_err(|e| e.to_string())?;

    db::add_to_black_hole(&conn, file_id, &from_where, &original_path).map_err(|e| e.to_string())
}

/// Get all files in the black hole
#[tauri::command]
pub async fn get_black_hole_files(
    filter: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::BlackHoleEntry>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_black_hole_files(&conn, filter).map_err(|e| e.to_string())
}

/// Restore a file from the black hole
#[tauri::command]
pub async fn restore_from_black_hole(
    file_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::remove_from_black_hole(&conn, file_id).map_err(|e| e.to_string())
}

/// Permanently delete a file (send to void)
#[tauri::command]
pub async fn send_to_void(file_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::send_to_void(&conn, file_id).map_err(|e| e.to_string())
}

/// Permanently delete all files in black hole (send all to void)
#[tauri::command]
pub async fn send_all_to_void(state: State<'_, AppState>) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::send_all_to_void(&conn).map_err(|e| e.to_string())
}

/// Get folders with high duplicate file similarity
#[tauri::command]
pub async fn get_duplicate_folders(
    threshold: Option<f64>,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::FolderSimilarity>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let similarity_threshold = threshold.unwrap_or(70.0);
    db::find_duplicate_folders(&conn, similarity_threshold).map_err(|e| e.to_string())
}

/// Backfill fingerprints for existing FITS headers
#[tauri::command]
pub async fn backfill_header_fingerprints(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get all headers without fingerprints
    let mut stmt = conn
        .prepare("SELECT id, header FROM fits_header WHERE header_fingerprint IS NULL")
        .map_err(|e| e.to_string())?;

    let headers: Vec<(i64, String)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let total = headers.len();
    println!("Backfilling fingerprints for {} headers", total);

    // Compute and update fingerprints
    for (id, header) in headers {
        let fingerprint = crate::fingerprint::compute_header_fingerprint(&header);
        conn.execute(
            "UPDATE fits_header SET header_fingerprint = ?1 WHERE id = ?2",
            rusqlite::params![fingerprint, id],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(total)
}

/// Relink files from old scan root to new location
#[tauri::command]
pub async fn relink_scan_root(
    root_id: i64,
    new_path: String,
    state: State<'_, AppState>,
) -> Result<crate::models::RelinkResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get old root path
    let old_path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![root_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get scan root: {}", e))?;

    println!("Relinking root {} from '{}' to '{}'", root_id, old_path, new_path);

    // Perform relinking
    let result = crate::relinking::relink_files(&conn, &old_path, &new_path)
        .map_err(|e| format!("Relinking failed: {}", e))?;

    // Update scan root path if all files were matched
    if result.files_orphaned == 0 || result.files_matched > 0 {
        conn.execute(
            "UPDATE scan_roots SET path = ?1 WHERE id = ?2",
            rusqlite::params![new_path, root_id],
        )
        .map_err(|e| format!("Failed to update scan root path: {}", e))?;
        println!("Updated scan root path to '{}'", new_path);
    }

    Ok(result)
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

/// Check if a scan root directory is available/accessible
#[tauri::command]
pub async fn check_scan_root_availability(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get scan root path
    let path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![root_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get scan root: {}", e))?;

    // Check if path exists
    Ok(std::path::Path::new(&path).exists())
}

/// Check availability of all scan roots
#[tauri::command]
pub async fn check_all_scan_roots_availability(
    state: State<'_, AppState>,
) -> Result<Vec<(i64, bool)>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let roots = db::get_scan_roots(&conn).map_err(|e| e.to_string())?;

    let availability: Vec<(i64, bool)> = roots
        .into_iter()
        .map(|root| {
            let exists = std::path::Path::new(&root.path).exists();
            (root.id.unwrap_or(0), exists)
        })
        .collect();

    Ok(availability)
}

/// Check for missing files within a scan root
#[tauri::command]
pub async fn check_missing_files_in_scan_root(
    root_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::OrphanedFile>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get scan root path
    let path: String = conn
        .query_row(
            "SELECT path FROM scan_roots WHERE id = ?1",
            rusqlite::params![root_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get scan root: {}", e))?;

    // Get all files under this scan root
    let mut stmt = conn
        .prepare(
            "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                    EXISTS(SELECT 1 FROM frames fr WHERE fr.file_id = f.id) as has_frame,
                    (SELECT fr.object FROM frames fr WHERE fr.file_id = f.id LIMIT 1) as object,
                    (SELECT fr.date_obs FROM frames fr WHERE fr.file_id = f.id LIMIT 1) as date_obs
             FROM files f
             WHERE f.path LIKE ?1"
        )
        .map_err(|e| e.to_string())?;

    let path_prefix = format!("{}%", path);
    let files = stmt
        .query_map(rusqlite::params![path_prefix], |row| {
            Ok(crate::models::OrphanedFile {
                id: row.get(0)?,
                path: row.get(1)?,
                filename: row.get(2)?,
                size: row.get(3)?,
                modified_at: row.get(4)?,
                has_frame: row.get::<_, i64>(5)? != 0,
                object: row.get(6).ok(),
                date_obs: row.get(7).ok(),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    // Filter to only files that don't exist on disk
    let missing_files: Vec<crate::models::OrphanedFile> = files
        .into_iter()
        .filter(|file| !std::path::Path::new(&file.path).exists())
        .collect();

    println!("Found {} missing files in scan root {}", missing_files.len(), root_id);

    Ok(missing_files)
}

#[tauri::command]
pub async fn get_imaging_locations(state: State<'_, AppState>) -> Result<Vec<ImagingLocation>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Query all LIGHT frames with coordinates, grouped by frame set
    // Schema: frames_set -> imaging_nights -> sessions -> session_members -> frames
    let mut stmt = conn.prepare("
        SELECT
            fs.id as frame_set_id,
            fs.name as object_name,
            AVG(fr.ra) as avg_ra,
            AVG(fr.dec) as avg_dec,
            COUNT(DISTINCT fr.id) as frame_count,
            SUM(fr.exptime) as total_exposure,
            GROUP_CONCAT(DISTINCT fr.filter) as filters,
            MIN(fr.date_obs) as first_date,
            MAX(fr.date_obs) as last_date,
            AVG(fr.xpixsz) as avg_xpixsz,
            AVG(fr.focallen) as avg_focallen
        FROM frames_set fs
        JOIN imaging_nights ino ON ino.frames_set_id = fs.id
        JOIN sessions s ON s.imaging_night_id = ino.id
        JOIN session_members sm ON sm.session_id = s.id
        JOIN frames fr ON fr.id = sm.frame_id
        WHERE fr.ra IS NOT NULL
          AND fr.dec IS NOT NULL
          AND fr.imagetyp = 'Light'
        GROUP BY fs.id
        HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL
    ").map_err(|e| format!("Failed to prepare query: {}", e))?;

    let locations = stmt.query_map([], |row| {
        let frame_set_id: i64 = row.get(0)?;
        let object_name: Option<String> = row.get(1)?;
        let ra: f64 = row.get(2)?;
        let dec: f64 = row.get(3)?;
        let frame_count: i32 = row.get(4)?;
        let total_exposure: f64 = row.get(5)?;
        let filters_str: Option<String> = row.get(6)?;
        let first_date: Option<String> = row.get(7)?;
        let last_date: Option<String> = row.get(8)?;
        let avg_xpixsz: Option<f64> = row.get(9)?;
        let avg_focallen: Option<f64> = row.get(10)?;

        // Parse filters from comma-separated string
        let filters: Vec<String> = filters_str
            .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
            .unwrap_or_default();

        // Calculate FOV if we have the necessary metadata
        let (fov_width, fov_height) = if let (Some(pixel_size), Some(focal_len)) = (avg_xpixsz, avg_focallen) {
            if focal_len > 0.0 {
                // Assuming typical sensor size or using a default
                // FOV (degrees) = 2 * arctan(sensor_size / (2 * focal_length)) * (180 / π)
                // For now, using a simplified calculation with assumed 4096x4096 sensor
                let pixel_scale_arcsec = (pixel_size / focal_len) * 206.265;
                let fov_w = (pixel_scale_arcsec * 4096.0) / 3600.0;
                let fov_h = (pixel_scale_arcsec * 4096.0) / 3600.0;
                (Some(fov_w), Some(fov_h))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        Ok(ImagingLocation {
            id: frame_set_id,
            ra,
            dec,
            object_name,
            frame_count,
            total_exposure,
            filters,
            date_range: (
                first_date.unwrap_or_default(),
                last_date.unwrap_or_default()
            ),
            frame_set_id: Some(frame_set_id),
            fov_width,
            fov_height,
        })
    }).map_err(|e| format!("Failed to query imaging locations: {}", e))?;

    let result: Vec<ImagingLocation> = locations
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect results: {}", e))?;

    println!("Found {} imaging locations", result.len());

    Ok(result)
}

