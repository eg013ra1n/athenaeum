use crate::cache::{CacheManager, CacheStats, StretchParams, StretchMode};
use crate::db::{self, Database};
use crate::models::*;
use crate::scanner::scan_directory;
use crate::settings::SettingsManager;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};

/// App state containing database connection, settings manager, and cache manager
pub struct AppState {
    pub db: Mutex<Option<Database>>,
    pub settings: Arc<SettingsManager>,
    pub cache: Arc<Mutex<Option<CacheManager>>>,
}

/// Calculate field of view (FOV) from FITS metadata
///
/// # Arguments
/// * `pixel_size_um` - Pixel size in micrometers (XPIXSZ)
/// * `focal_length_mm` - Focal length in millimeters (FOCALLEN)
/// * `naxis` - Sensor dimension in pixels (NAXIS1 or NAXIS2)
/// * `binning` - Binning factor (XBINNING or YBINNING, defaults to 1)
///
/// # Returns
/// FOV in degrees, or None if calculation not possible
fn calculate_fov(
    pixel_size_um: Option<f64>,
    focal_length_mm: Option<f64>,
    naxis: Option<i32>,
    binning: Option<i32>,
) -> Option<f64> {
    match (pixel_size_um, focal_length_mm, naxis) {
        (Some(pixel_size), Some(focal_len), Some(sensor_pixels)) if focal_len > 0.0 && sensor_pixels > 0 => {
            let bin = binning.unwrap_or(1) as f64;

            // Convert pixel size from micrometers to millimeters
            let pixel_size_mm = pixel_size / 1000.0;

            // Calculate sensor dimension in mm (accounting for binning)
            let sensor_mm = pixel_size_mm * sensor_pixels as f64 * bin;

            // FOV formula: FOV = 2 * arctan(sensor_mm / (2 * focal_length_mm)) * (180 / π)
            let fov_radians = 2.0 * (sensor_mm / (2.0 * focal_len)).atan();
            let fov_degrees = fov_radians.to_degrees();

            Some(fov_degrees)
        }
        _ => None,
    }
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
    threshold_deg: Option<f64>,
    state: State<'_, AppState>,
) -> Result<AutoGenerateResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Use provided threshold or get from settings
    let threshold_deg = if let Some(custom_threshold) = threshold_deg {
        custom_threshold
    } else {
        state.settings
            .get_grouping_threshold_deg(&conn)
            .map_err(|e| e.to_string())?
    };

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
        // Calculate metadata from cluster frames
        let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
            &cluster.member_frame_ids,
            &conn,
        ).map_err(|e| e.to_string())?;

        // Create frames_set
        let set_id = db::create_frames_set(
            &conn,
            cluster.name.as_deref(),
            false, // is_custom = false for auto-generated sets
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
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

/// Delete all auto-generated frames_sets (is_custom = false)
#[tauri::command]
pub async fn delete_auto_generated_frame_sets(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_auto_generated_frame_sets(&conn).map_err(|e| e.to_string())
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

/// Mark a frames_set as custom (one-way conversion from auto-generated to custom)
#[tauri::command]
pub async fn mark_frame_set_custom(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FramesSet, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get current metadata to preserve it
    let metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(frames_set_id, &conn)
        .map_err(|e| format!("Failed to calculate metadata: {}", e))?;

    // Update frame set to mark as custom
    db::update_frames_set_metadata(
        &conn,
        frames_set_id,
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        true, // Mark as custom
    ).map_err(|e| format!("Failed to update frame set: {}", e))?;

    // Return the updated frame set
    let sets = db::get_frames_sets_by_project(&conn, 1)
        .map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    Ok(frames_set)
}

/// Recalculate frame set metadata from all member frames
#[tauri::command]
pub async fn recalculate_frame_set_metadata(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FramesSet, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Calculate metadata from all frames in the set
    let metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(
        frames_set_id,
        &conn,
    ).map_err(|e| format!("Failed to calculate metadata: {}", e))?;

    // Update the frame set with new metadata (mark as custom since it's been manually recalculated)
    db::update_frames_set_metadata(
        &conn,
        frames_set_id,
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        true, // Mark as custom after manual recalculation
    ).map_err(|e| format!("Failed to update metadata: {}", e))?;

    // Return the updated frame set
    let sets = db::get_frames_sets_by_project(&conn, 1)
        .map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    Ok(frames_set)
}

/// Update the flat_pattern preference for a frame set
#[tauri::command]
pub async fn update_frame_set_flat_pattern(
    frame_set_id: i64,
    flat_pattern: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_frames_set_flat_pattern(&conn, frame_set_id, Some(&flat_pattern))
        .map_err(|e| format!("Failed to update flat pattern: {}", e))?;

    Ok(())
}

/// Merge source frame set into target frame set
/// Dragged frame set (source) is merged into drop target (target)
/// Source frame set is deleted after merge
#[tauri::command]
pub async fn merge_frame_sets(
    source_id: i64,
    target_id: i64,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    println!("Merging frame set {} into {}", source_id, target_id);

    if source_id == target_id {
        return Err("Cannot merge a frame set into itself".to_string());
    }

    // Perform all database operations in a scope so conn is dropped before we call get_frame_set_detail
    {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        // Get all nights from source frame set
        let source_nights = db::get_imaging_nights_for_set(&conn, source_id)
            .map_err(|e| format!("Failed to get source nights: {}", e))?;

        println!("Found {} nights in source frame set", source_nights.len());

        // Get all nights from target frame set
        let target_nights = db::get_imaging_nights_for_set(&conn, target_id)
            .map_err(|e| format!("Failed to get target nights: {}", e))?;

        println!("Found {} nights in target frame set", target_nights.len());

        // Process each source night
        for source_night in source_nights {
            let source_night_id = source_night.id.ok_or("Source night has no ID")?;

            // Try to find a matching night in target
            let matching_target_night_id = crate::frames_set_merge::find_matching_night(
                &source_night,
                &target_nights,
            ).map_err(|e| format!("Failed to find matching night: {}", e))?;

            if let Some(target_night_id) = matching_target_night_id {
                println!("Night {} matches target night {}", source_night_id, target_night_id);

                // Get the target night details for time range calculation
                let target_night = target_nights
                    .iter()
                    .find(|n| n.id == Some(target_night_id))
                    .ok_or("Target night not found")?;

                // Calculate union of time ranges
                let (new_start, new_end) = crate::frames_set_merge::calculate_time_range_union(
                    &source_night.start_time,
                    &source_night.end_time,
                    &target_night.start_time,
                    &target_night.end_time,
                ).map_err(|e| format!("Failed to calculate time range union: {}", e))?;

                println!("Updating target night {} time range to {} - {}", target_night_id, new_start, new_end);

                // Update target night time range
                db::update_imaging_night_time_range(&conn, target_night_id, &new_start, &new_end)
                    .map_err(|e| format!("Failed to update time range: {}", e))?;

                // Get all sessions from source night and move them to target night
                let source_sessions = db::get_sessions_for_night(&conn, source_night_id)
                    .map_err(|e| format!("Failed to get source sessions: {}", e))?;

                let session_ids: Vec<i64> = source_sessions
                    .iter()
                    .filter_map(|s| s.id)
                    .collect();

                println!("Moving {} sessions from source night {} to target night {}", session_ids.len(), source_night_id, target_night_id);

                if !session_ids.is_empty() {
                    db::move_sessions_to_night(&conn, &session_ids, target_night_id)
                        .map_err(|e| format!("Failed to move sessions: {}", e))?;
                }

                // Delete the now-empty source night
                // Note: This will cascade delete via FK constraints, but sessions were already moved
                // So we need to delete the night manually since it's now empty
                // Actually, we moved the sessions, so the night is empty and can be deleted
                // But the FK constraint will prevent deletion if there are still sessions
                // Wait, we already moved sessions, so the night should be empty now
                // Let's just leave it for now and let the frame set deletion handle it
            } else {
                println!("Night {} has no match in target, reassigning to target frame set", source_night_id);

                // No matching night found, reassign this night to target frame set
                db::reassign_imaging_night_to_frame_set(&conn, source_night_id, target_id)
                    .map_err(|e| format!("Failed to reassign night: {}", e))?;
            }
        }

        // Deduplicate frames in the target frame set
        println!("Deduplicating frames in target frame set");
        let duplicates_removed = db::deduplicate_session_members_in_set(&conn, target_id)
            .map_err(|e| format!("Failed to deduplicate: {}", e))?;

        println!("Removed {} duplicate frame references", duplicates_removed);

        // Recalculate metadata for target frame set and mark as custom
        println!("Recalculating metadata for target frame set");
        let metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(target_id, &conn)
            .map_err(|e| format!("Failed to calculate metadata: {}", e))?;

        db::update_frames_set_metadata(
            &conn,
            target_id,
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
            true, // Mark as custom after merge
        ).map_err(|e| format!("Failed to update metadata: {}", e))?;

        // Delete source frame set (cascade will handle cleanup)
        println!("Deleting source frame set {}", source_id);
        db::delete_frames_set(&conn, source_id)
            .map_err(|e| format!("Failed to delete source frame set: {}", e))?;

        println!("✅ Merge completed successfully");
    } // state_lock and conn are dropped here

    // Return the updated target frame set detail
    get_frame_set_detail(target_id, state).await
}

/// Check if a split operation is valid (won't leave source set empty)
#[tauri::command]
pub async fn can_split(
    source_set_id: i64,
    selection: crate::models::SplitSelection,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get all nights in the source set
    let all_nights = db::get_imaging_nights_for_set(&conn, source_set_id)
        .map_err(|e| format!("Failed to get nights: {}", e))?;

    match selection {
        crate::models::SplitSelection::Nights { ids } => {
            // Check if we're splitting all nights
            Ok(ids.len() < all_nights.len())
        }
        crate::models::SplitSelection::Sessions { ids } => {
            // Count total sessions in the set
            let mut total_sessions = 0;
            for night in all_nights {
                if let Some(night_id) = night.id {
                    let sessions = db::get_sessions_for_night(&conn, night_id)
                        .map_err(|e| format!("Failed to get sessions: {}", e))?;
                    total_sessions += sessions.len();
                }
            }

            // Can split if not all sessions are selected
            Ok(ids.len() < total_sessions)
        }
        crate::models::SplitSelection::Frames { ids } => {
            // Count total frames in the set
            let total_frames: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT sm.frame_id)
                 FROM session_members sm
                 JOIN sessions s ON sm.session_id = s.id
                 JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
                 WHERE in_tbl.frames_set_id = ?1",
                [source_set_id],
                |row| row.get(0),
            ).map_err(|e| format!("Failed to count frames: {}", e))?;

            // Can split if not all frames are selected
            Ok(ids.len() < total_frames as usize)
        }
    }
}

/// Split selected items from a frame set into a new frame set
#[tauri::command]
pub async fn split_frame_set(
    source_set_id: i64,
    selection: crate::models::SplitSelection,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    println!("Splitting from frame set {}: new_name='{}'", source_set_id, new_name);

    // Perform all database operations in a scope
    let new_set_id = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        // Validate that split won't leave source empty (inline to avoid nested locks)
        let all_nights = db::get_imaging_nights_for_set(&conn, source_set_id)
            .map_err(|e| format!("Failed to get nights: {}", e))?;

        let can_split_result = match &selection {
            crate::models::SplitSelection::Nights { ids } => {
                // Check if we're splitting all nights
                ids.len() < all_nights.len()
            }
            crate::models::SplitSelection::Sessions { ids } => {
                // Count total sessions in the set
                let mut total_sessions = 0;
                for night in &all_nights {
                    if let Some(night_id) = night.id {
                        let sessions = db::get_sessions_for_night(&conn, night_id)
                            .map_err(|e| format!("Failed to get sessions: {}", e))?;
                        total_sessions += sessions.len();
                    }
                }

                // Can split if not all sessions are selected
                ids.len() < total_sessions
            }
            crate::models::SplitSelection::Frames { ids } => {
                // Count total frames in the set
                let total_frames: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT sm.frame_id)
                     FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
                     WHERE in_tbl.frames_set_id = ?1",
                    [source_set_id],
                    |row| row.get(0),
                ).map_err(|e| format!("Failed to count frames: {}", e))?;

                // Can split if not all frames are selected
                ids.len() < total_frames as usize
            }
        };

        if !can_split_result {
            return Err("Cannot split: operation would leave the source frame set empty".to_string());
        }

        // Get session gap threshold from settings
        let gap_threshold_hours: f64 = state.settings
            .get_with_precedence(&conn, "session_gap_threshold_hours", "6.0")
            .map_err(|e| e.to_string())?
            .parse()
            .unwrap_or(6.0);

        // Collect frame IDs based on selection type
        let frame_ids: Vec<i64> = match &selection {
            crate::models::SplitSelection::Nights { ids } => {
                // Get all frames from selected nights
                let mut frames = Vec::new();
                for night_id in ids {
                    let mut stmt = conn.prepare(
                        "SELECT DISTINCT sm.frame_id
                         FROM session_members sm
                         JOIN sessions s ON sm.session_id = s.id
                         WHERE s.imaging_night_id = ?1"
                    ).map_err(|e| format!("Failed to prepare query: {}", e))?;

                    let night_frames: Vec<i64> = stmt
                        .query_map([night_id], |row| row.get(0))
                        .map_err(|e| format!("Failed to query frames: {}", e))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| format!("Failed to collect frames: {}", e))?;

                    frames.extend(night_frames);
                }
                frames
            }
            crate::models::SplitSelection::Sessions { ids } => {
                // Get all frames from selected sessions
                let mut frames = Vec::new();
                for session_id in ids {
                    let session_frames = db::get_frame_ids_for_session(&conn, *session_id)
                        .map_err(|e| format!("Failed to get session frames: {}", e))?;
                    frames.extend(session_frames);
                }
                frames
            }
            crate::models::SplitSelection::Frames { ids } => {
                // Use frame IDs directly
                ids.clone()
            }
        };

        println!("Split will move {} frames", frame_ids.len());

        if frame_ids.is_empty() {
            return Err("No frames to split".to_string());
        }

        // Calculate metadata for new frame set
        let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
            &frame_ids,
            &conn,
        ).map_err(|e| format!("Failed to calculate metadata: {}", e))?;

        // Create new frame set (always custom)
        let new_set_id = db::create_frames_set(
            &conn,
            Some(&new_name),
            true, // Always custom after split
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
        ).map_err(|e| format!("Failed to create frame set: {}", e))?;

        println!("Created new frame set with id {}", new_set_id);

        // Get frames with file info for session detection
        let frames = db::get_frames_with_files_by_ids(&conn, &frame_ids)
            .map_err(|e| format!("Failed to get frames: {}", e))?;

        // Detect nights for new frame set
        let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
            .map_err(|e| format!("Failed to detect sessions: {}", e))?;

        println!("Detected {} nights for new frame set", detected_nights.len());

        // Create nights and sessions for new frame set
        for night in detected_nights {
            let night_id = db::create_imaging_night(
                &conn,
                new_set_id,
                &night.start_time,
                &night.end_time,
            ).map_err(|e| format!("Failed to create night: {}", e))?;

            for session in night.sessions {
                let session_id = db::create_session(
                    &conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                ).map_err(|e| format!("Failed to create session: {}", e))?;

                db::insert_session_members(&conn, session_id, &session.frame_ids)
                    .map_err(|e| format!("Failed to insert session members: {}", e))?;
            }
        }

        // Remove split frames from source frame set based on selection type
        match selection {
            crate::models::SplitSelection::Nights { ids } => {
                // Delete entire nights from source
                for night_id in ids {
                    conn.execute(
                        "DELETE FROM imaging_nights WHERE id = ?1",
                        [night_id],
                    ).map_err(|e| format!("Failed to delete night: {}", e))?;
                }
            }
            crate::models::SplitSelection::Sessions { ids } => {
                // Delete sessions from source
                for session_id in ids {
                    conn.execute(
                        "DELETE FROM sessions WHERE id = ?1",
                        [session_id],
                    ).map_err(|e| format!("Failed to delete session: {}", e))?;
                }
            }
            crate::models::SplitSelection::Frames { ids } => {
                // Remove specific frames from all sessions
                for frame_id in ids {
                    conn.execute(
                        "DELETE FROM session_members WHERE frame_id = ?1",
                        [frame_id],
                    ).map_err(|e| format!("Failed to remove frame: {}", e))?;
                }
            }
        }

        // Recalculate metadata for source frame set and mark as custom
        println!("Recalculating metadata for source frame set");
        let source_metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(
            source_set_id,
            &conn,
        ).map_err(|e| format!("Failed to calculate source metadata: {}", e))?;

        db::update_frames_set_metadata(
            &conn,
            source_set_id,
            source_metadata.date_obs_start.as_deref(),
            source_metadata.date_obs_end.as_deref(),
            source_metadata.objctra.as_deref(),
            source_metadata.objctdec.as_deref(),
            source_metadata.total_exp_time,
            true, // Mark as custom after split
        ).map_err(|e| format!("Failed to update source metadata: {}", e))?;

        println!("✅ Split completed successfully");

        new_set_id
    }; // state_lock and conn are dropped here

    // Return the new frame set detail
    get_frame_set_detail(new_set_id, state).await
}

/// Helper function to calculate angular distance between two points on celestial sphere
fn angular_distance(ra1_deg: f64, dec1_deg: f64, ra2_deg: f64, dec2_deg: f64) -> f64 {
    let ra1 = ra1_deg.to_radians();
    let dec1 = dec1_deg.to_radians();
    let ra2 = ra2_deg.to_radians();
    let dec2 = dec2_deg.to_radians();

    let delta_ra = ra1 - ra2;

    // Haversine formula
    let a = (dec1 - dec2).sin() / 2.0;
    let b = (delta_ra).sin() / 2.0;
    let c = dec1.cos() * dec2.cos();

    let angular_dist = 2.0 * ((a * a + c * b * b).sqrt()).asin();

    angular_dist.to_degrees()
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
    state: State<'_, AppState>,
) -> Result<i64, String> {
    println!("Creating custom frames set: name='{}', session_ids={:?}", name, session_ids);

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
    let mut all_frame_ids = Vec::new();

    for (session_id, frames) in all_session_frames {
        // Extract frame IDs for metadata calculation
        for (_, _, frame) in &frames {
            if let Some(frame_id) = frame.id {
                all_frame_ids.push(frame_id);
            }
        }
        session_frame_map.insert(session_id, frames.clone());
        all_frames.extend(frames);
    }

    // Calculate metadata from all frames
    println!("Calculating metadata from {} frames", all_frame_ids.len());
    let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
        &all_frame_ids,
        &conn,
    ).map_err(|e| {
        let err_msg = format!("Failed to calculate metadata: {}", e);
        println!("{}", err_msg);
        err_msg
    })?;

    println!("Calculated metadata: date_obs_start={:?}, date_obs_end={:?}, coordinates={:?}/{:?}, total_exp_time={:?}",
             metadata.date_obs_start, metadata.date_obs_end, metadata.objctra, metadata.objctdec, metadata.total_exp_time);

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

    // Create the custom frames_set
    println!("Creating frames_set with name '{}'", name);
    let set_id = db::create_frames_set(
        &conn,
        Some(&name),
        true, // is_custom = true
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
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

#[tauri::command]
pub async fn get_calibration_set_frames(
    state: State<'_, AppState>,
    set_id: i64,
) -> Result<Vec<crate::models::FileWithFrame>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_frames_for_calibration_set(&conn, set_id).map_err(|e| e.to_string())
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

    // Query both organized frame sets AND unorganized frames
    // This enables users to see all frames with coordinates immediately,
    // without needing to auto-generate frame sets first
    let mut stmt = conn.prepare("
        -- Organized locations: Frames in frame sets
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
            AVG(fr.focallen) as avg_focallen,
            AVG(fr.naxis1) as avg_naxis1,
            AVG(fr.naxis2) as avg_naxis2,
            AVG(fr.xbinning) as avg_xbinning,
            AVG(fr.ybinning) as avg_ybinning,
            'frameset' as location_type,
            GROUP_CONCAT(DISTINCT fr.instrume) as cameras,
            GROUP_CONCAT(DISTINCT CAST(fr.focallen AS TEXT)) as focal_lengths,
            fs.is_custom as is_custom
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

        UNION ALL

        -- Unorganized locations: Frames NOT in any session, clustered by location
        SELECT
            NULL as frame_set_id,
            COALESCE(fr.object, 'Unknown') as object_name,
            AVG(fr.ra) as avg_ra,
            AVG(fr.dec) as avg_dec,
            COUNT(DISTINCT fr.id) as frame_count,
            SUM(fr.exptime) as total_exposure,
            GROUP_CONCAT(DISTINCT fr.filter) as filters,
            MIN(fr.date_obs) as first_date,
            MAX(fr.date_obs) as last_date,
            AVG(fr.xpixsz) as avg_xpixsz,
            AVG(fr.focallen) as avg_focallen,
            AVG(fr.naxis1) as avg_naxis1,
            AVG(fr.naxis2) as avg_naxis2,
            AVG(fr.xbinning) as avg_xbinning,
            AVG(fr.ybinning) as avg_ybinning,
            'cluster' as location_type,
            GROUP_CONCAT(DISTINCT fr.instrume) as cameras,
            GROUP_CONCAT(DISTINCT CAST(fr.focallen AS TEXT)) as focal_lengths,
            0 as is_custom
        FROM frames fr
        WHERE fr.ra IS NOT NULL
          AND fr.dec IS NOT NULL
          AND fr.imagetyp = 'Light'
          AND NOT EXISTS (
              SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
          )
        GROUP BY COALESCE(fr.object, 'Unknown'), ROUND(fr.ra, 1), ROUND(fr.dec, 1)
        HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL
    ").map_err(|e| format!("Failed to prepare query: {}", e))?;

    let locations = stmt.query_map([], |row| {
        let frame_set_id: Option<i64> = row.get(0)?;
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
        let avg_naxis1: Option<f64> = row.get(11)?;
        let avg_naxis2: Option<f64> = row.get(12)?;
        let avg_xbinning: Option<f64> = row.get(13)?;
        let avg_ybinning: Option<f64> = row.get(14)?;
        let location_type: String = row.get(15)?;
        let cameras_str: Option<String> = row.get(16)?;
        let focal_lengths_str: Option<String> = row.get(17)?;
        let is_custom: i64 = row.get(18)?;

        // Parse filters from comma-separated string
        let filters: Vec<String> = filters_str
            .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
            .unwrap_or_default();

        // Calculate FOV using actual sensor dimensions from FITS metadata
        let fov_width = calculate_fov(
            avg_xpixsz,
            avg_focallen,
            avg_naxis1.map(|n| n.round() as i32),
            avg_xbinning.map(|b| b.round() as i32),
        );

        let fov_height = calculate_fov(
            avg_xpixsz,
            avg_focallen,
            avg_naxis2.map(|n| n.round() as i32),
            avg_ybinning.map(|b| b.round() as i32),
        );

        // Use a deterministic ID based on location for clusters
        let id = if let Some(fs_id) = frame_set_id {
            fs_id
        } else {
            // Create a pseudo-ID for clusters based on coordinates
            ((ra.to_bits() as i64) ^ (dec.to_bits() as i64)).abs()
        };

        Ok(ImagingLocation {
            id,
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
            frame_set_id,
            fov_width,
            fov_height,
            location_type,
            cameras: cameras_str,
            focal_lengths: focal_lengths_str,
            is_custom: is_custom != 0,
        })
    }).map_err(|e| format!("Failed to query imaging locations: {}", e))?;

    let result: Vec<ImagingLocation> = locations
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect results: {}", e))?;

    println!("Found {} imaging locations ({} framesets, {} clusters)",
        result.len(),
        result.iter().filter(|l| l.location_type == "frameset").count(),
        result.iter().filter(|l| l.location_type == "cluster").count()
    );

    Ok(result)
}

/// Get preview image for a frame by frame_id
/// Returns JPEG data as base64-encoded string for embedding in SVG <image> tags
#[tauri::command(rename_all = "snake_case")]
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

/// Query frames within a circular region of the sky
#[tauri::command(rename_all = "snake_case")]
pub async fn query_frames_in_circle(
    state: State<'_, AppState>,
    ra: f64,
    dec: f64,
    radius_degrees: f64,
) -> Result<SelectionResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Query all LIGHT frames with coordinates
    let mut stmt = conn
        .prepare(
            "SELECT id, ra, dec, exptime FROM frames
             WHERE ra IS NOT NULL
             AND dec IS NOT NULL
             AND imagetyp = 'Light'",
        )
        .map_err(|e| e.to_string())?;

    let frame_ids: Vec<i64> = stmt
        .query_map([], |row| {
            let frame_id: i64 = row.get(0)?;
            let frame_ra: f64 = row.get(1)?;
            let frame_dec: f64 = row.get(2)?;

            Ok((frame_id, frame_ra, frame_dec))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(_, frame_ra, frame_dec)| {
            let distance = crate::selection::angular_distance(ra, dec, *frame_ra, *frame_dec);
            distance <= radius_degrees
        })
        .map(|(id, _, _)| id)
        .collect();

    // Calculate total exposure
    let total_exposure: f64 = if !frame_ids.is_empty() {
        // Query total exposure by summing selected frames
        let mut total: f64 = 0.0;
        let mut stmt = conn
            .prepare("SELECT COALESCE(exptime, 0) FROM frames WHERE id = ?1")
            .map_err(|e| e.to_string())?;

        for frame_id in &frame_ids {
            let exp: f64 = stmt
                .query_row(rusqlite::params![frame_id], |row| row.get::<_, f64>(0))
                .unwrap_or(0.0);
            total += exp;
        }
        total
    } else {
        0.0
    };

    let count = frame_ids.len();

    Ok(SelectionResult {
        frame_ids,
        count,
        total_exposure_seconds: total_exposure,
    })
}

/// Query frames within a rectangular region of the sky
#[tauri::command(rename_all = "snake_case")]
pub async fn query_frames_in_bounds(
    state: State<'_, AppState>,
    bounds: SelectionBounds,
) -> Result<SelectionResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Handle RA wrap-around at 0°/360° boundary
    // Use explicit crosses_meridian flag if provided, otherwise detect from ra_min > ra_max
    let ra_wrap_around = bounds.crosses_meridian.unwrap_or_else(|| bounds.ra_min > bounds.ra_max);

    println!(
        "Querying frames in bounds: ra_min={}, ra_max={}, dec_min={}, dec_max={}, crosses_meridian={}",
        bounds.ra_min, bounds.ra_max, bounds.dec_min, bounds.dec_max, ra_wrap_around
    );

    let query = if ra_wrap_around {
        // Wrap-around case: select frames where ra >= ra_min OR ra <= ra_max
        "SELECT id FROM frames
         WHERE ra IS NOT NULL
         AND dec IS NOT NULL
         AND imagetyp = 'Light'
         AND (ra >= ?1 OR ra <= ?2)
         AND dec BETWEEN ?3 AND ?4".to_string()
    } else {
        // Normal case: select frames where ra is between ra_min and ra_max
        "SELECT id FROM frames
         WHERE ra IS NOT NULL
         AND dec IS NOT NULL
         AND imagetyp = 'Light'
         AND ra BETWEEN ?1 AND ?2
         AND dec BETWEEN ?3 AND ?4".to_string()
    };

    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| e.to_string())?;

    let frame_ids: Vec<i64> = stmt
        .query_map(
            rusqlite::params![bounds.ra_min, bounds.ra_max, bounds.dec_min, bounds.dec_max],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    println!(
        "Found {} frames (ra_wrap_around={})",
        frame_ids.len(),
        ra_wrap_around
    );

    // Calculate total exposure
    let total_exposure: f64 = if !frame_ids.is_empty() {
        let mut total: f64 = 0.0;
        let mut stmt = conn
            .prepare("SELECT COALESCE(exptime, 0) FROM frames WHERE id = ?1")
            .map_err(|e| e.to_string())?;

        for frame_id in &frame_ids {
            let exp: f64 = stmt
                .query_row(rusqlite::params![frame_id], |row| row.get::<_, f64>(0))
                .unwrap_or(0.0);
            total += exp;
        }
        total
    } else {
        0.0
    };

    let count = frame_ids.len();

    Ok(SelectionResult {
        frame_ids,
        count,
        total_exposure_seconds: total_exposure,
    })
}

/// Query frames within a polygonal region of the sky
#[tauri::command(rename_all = "snake_case")]
pub async fn query_frames_in_polygon(
    state: State<'_, AppState>,
    vertices: Vec<(f64, f64)>,
) -> Result<SelectionResult, String> {
    if vertices.len() < 3 {
        return Err("Polygon must have at least 3 vertices".to_string());
    }

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Query all LIGHT frames with coordinates
    let mut stmt = conn
        .prepare(
            "SELECT id, ra, dec, exptime FROM frames
             WHERE ra IS NOT NULL
             AND dec IS NOT NULL
             AND imagetyp = 'Light'",
        )
        .map_err(|e| e.to_string())?;

    let frame_ids: Vec<i64> = stmt
        .query_map([], |row| {
            let frame_id: i64 = row.get(0)?;
            let frame_ra: f64 = row.get(1)?;
            let frame_dec: f64 = row.get(2)?;

            Ok((frame_id, frame_ra, frame_dec))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(_, frame_ra, frame_dec)| {
            crate::selection::point_in_polygon(*frame_ra, *frame_dec, &vertices)
        })
        .map(|(id, _, _)| id)
        .collect();

    // Calculate total exposure
    let total_exposure: f64 = if !frame_ids.is_empty() {
        let mut total: f64 = 0.0;
        let mut stmt = conn
            .prepare("SELECT COALESCE(exptime, 0) FROM frames WHERE id = ?1")
            .map_err(|e| e.to_string())?;

        for frame_id in &frame_ids {
            let exp: f64 = stmt
                .query_row(rusqlite::params![frame_id], |row| row.get::<_, f64>(0))
                .unwrap_or(0.0);
            total += exp;
        }
        total
    } else {
        0.0
    };

    let count = frame_ids.len();

    Ok(SelectionResult {
        frame_ids,
        count,
        total_exposure_seconds: total_exposure,
    })
}

/// Create a custom frame set from selected frames
#[tauri::command(rename_all = "snake_case")]
pub async fn create_frame_set_from_selection(
    state: State<'_, AppState>,
    name: String,
    frame_ids: Vec<i64>,
    description: Option<String>,
) -> Result<i64, String> {
    println!(
        "Creating frame set from selection: name='{}', frame_count={}",
        name,
        frame_ids.len()
    );

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Verify frames exist
    if frame_ids.is_empty() {
        return Err("Cannot create frame set with no frames".to_string());
    }

    // Calculate metadata from frame IDs
    let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
        &frame_ids,
        &conn,
    ).map_err(|e| format!("Failed to calculate metadata: {}", e))?;

    println!("Calculated metadata: date_obs_start={:?}, date_obs_end={:?}, coordinates={:?}/{:?}, total_exp_time={:?}",
             metadata.date_obs_start, metadata.date_obs_end, metadata.objctra, metadata.objctdec, metadata.total_exp_time);

    // Create the custom frames_set
    let set_id = db::create_frames_set(
        &conn,
        Some(&name),
        true, // is_custom = true
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
    ).map_err(|e| format!("Failed to create frames_set: {}", e))?;

    println!("Created frames_set with id {}", set_id);

    // Get frames with file info for session detection
    let frames = db::get_frames_with_files_by_ids(&conn, &frame_ids)
        .map_err(|e| format!("Failed to get frames: {}", e))?;

    // Detect nights from selected frames using gap threshold
    let gap_threshold_hours: f64 = state.settings
        .get_with_precedence(&conn, "session_gap_threshold_hours", "6.0")
        .map_err(|e| format!("Failed to get settings: {}", e))?
        .parse()
        .unwrap_or(6.0);

    println!("Detecting nights from {} frames with gap threshold {} hours", frames.len(), gap_threshold_hours);

    let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
        .map_err(|e| format!("Failed to detect sessions: {}", e))?;

    println!("Detected {} nights", detected_nights.len());

    if detected_nights.is_empty() {
        // If no nights detected, create a single night/session with all frames
        println!("No nights detected, creating single night with all frames");

        let now = chrono::Utc::now();
        let night_start = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let night_end = (now + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let night_id = db::create_imaging_night(&conn, set_id, &night_start, &night_end)
            .map_err(|e| format!("Failed to create imaging_night: {}", e))?;

        let session_id = db::create_session(&conn, night_id, "Unknown", frame_ids.len() as i32, metadata.total_exp_time)
            .map_err(|e| format!("Failed to create session: {}", e))?;

        db::insert_session_members(&conn, session_id, &frame_ids)
            .map_err(|e| format!("Failed to add frames to session: {}", e))?;

        println!("✅ Created custom frame set '{}' (id {}) with {} frames", name, set_id, frame_ids.len());
    } else {
        // Create imaging nights and sessions for detected nights
        for (night_idx, night) in detected_nights.iter().enumerate() {
            println!("Processing night {}/{}: {} to {}", night_idx + 1, detected_nights.len(), night.start_time, night.end_time);

            let night_id = db::create_imaging_night(&conn, set_id, &night.start_time, &night.end_time)
                .map_err(|e| format!("Failed to create imaging_night: {}", e))?;

            println!("Created imaging_night with id {}", night_id);

            // Process sessions within this night
            for session in &night.sessions {
                let session_id = db::create_session(
                    &conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                ).map_err(|e| format!("Failed to create session: {}", e))?;

                db::insert_session_members(&conn, session_id, &session.frame_ids)
                    .map_err(|e| format!("Failed to add frames to session: {}", e))?;
            }
        }

        println!("✅ Created custom frame set '{}' (id {}) with {} nights and {} frames", name, set_id, detected_nights.len(), frame_ids.len());
    }

    Ok(set_id)
}

// ===== CALIBRATION FINDER COMMANDS =====

/// Find and link calibration for all light frames in a frame set
#[tauri::command]
pub async fn find_calibration_for_frame_set(
    frame_set_id: i64,
    temp_delta_celsius: Option<f64>,
    flat_date_warning_days: Option<i64>,
    dark_date_warning_days: Option<i64>,
    flat_pattern: Option<String>,
    manual_flat_selections: Option<std::collections::HashMap<String, i64>>,
    state: State<'_, AppState>,
) -> Result<crate::calibration::processor::ProcessingStats, String> {
    use crate::calibration::processor::process_frame_set;
    use crate::models::CalibrationTolerance;
    use chrono::{DateTime, Utc};

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get frame set metadata (flat_pattern, date ranges)
    let mut stmt = conn.prepare(
        "SELECT flat_pattern, date_obs_start, date_obs_end FROM frames_set WHERE id = ?1"
    ).map_err(|e| e.to_string())?;

    let (stored_flat_pattern, date_obs_start, date_obs_end): (Option<String>, Option<String>, Option<String>) =
        stmt.query_row([frame_set_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        }).map_err(|e| format!("Frame set not found: {}", e))?;

    // Use provided flat_pattern or fall back to stored pattern
    let final_flat_pattern = flat_pattern.or(stored_flat_pattern);

    // Parse session dates
    let session_start = date_obs_start
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let session_end = date_obs_end
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    // Get flat calibration settings
    let max_age_days = state.settings.get_flats_max_age_days(&conn)
        .map_err(|e| format!("Failed to get flats max age: {}", e))?;
    let time_cluster_minutes = state.settings.get_flats_time_cluster_minutes(&conn)
        .map_err(|e| format!("Failed to get time cluster threshold: {}", e))?;
    let temp_weight = state.settings.get_temperature_match_weight(&conn)
        .map_err(|e| format!("Failed to get temperature match weight: {}", e))?;

    // Build tolerance from parameters, settings, or defaults
    let tolerance = CalibrationTolerance {
        temp_delta_celsius: temp_delta_celsius.unwrap_or_else(||
            state.settings.get_calibration_temp_delta_celsius(&conn).unwrap_or(2.0)
        ),
        flat_date_warning_days: flat_date_warning_days.unwrap_or_else(||
            state.settings.get_calibration_flat_date_warning_days(&conn).unwrap_or(30)
        ),
        dark_date_warning_days: dark_date_warning_days.unwrap_or_else(||
            state.settings.get_calibration_dark_date_warning_days(&conn).unwrap_or(365)
        ),
    };

    println!(
        "Finding calibration for frame set {} with tolerance: temp=±{}°C, flat_date={} days, dark_date={} days",
        frame_set_id,
        tolerance.temp_delta_celsius,
        tolerance.flat_date_warning_days,
        tolerance.dark_date_warning_days
    );
    println!(
        "Flat settings: pattern={:?}, max_age={} days, time_cluster={} min, temp_weight={}",
        final_flat_pattern, max_age_days, time_cluster_minutes, temp_weight
    );

    let stats = process_frame_set(
        &conn,
        frame_set_id,
        &tolerance,
        final_flat_pattern.as_deref(),
        manual_flat_selections.as_ref(),
        max_age_days,
        time_cluster_minutes,
        temp_weight,
        session_start,
        session_end,
        &state,
    ).map_err(|e| format!("Failed to process frame set: {}", e))?;

    println!(
        "✅ Calibration processing complete: {} frames, {} with full calibration",
        stats.total_frames, stats.frames_with_full_calibration
    );

    Ok(stats)
}

/// Get calibration status/statistics for a frame set
#[tauri::command]
pub async fn get_calibration_status(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<crate::models::CalibrationStats, String> {
    use crate::db::calibration_links::get_calibration_statistics;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    get_calibration_statistics(&conn, frame_set_id).map_err(|e| e.to_string())
}

/// Get frames grouped by their calibration set combinations for a frame set
#[tauri::command]
pub async fn get_frame_set_calibration_groups(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<crate::models::FrameSetCalibrationGroups, String> {
    use crate::db::calibration_links::get_calibration_groups_for_frame_set;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    get_calibration_groups_for_frame_set(&conn, frame_set_id).map_err(|e| e.to_string())
}

/// Get complete calibration hierarchy for a specific frame
#[tauri::command]
pub async fn get_frame_calibration_hierarchy(
    frame_id: i64,
    temp_delta_celsius: Option<f64>,
    flat_date_warning_days: Option<i64>,
    dark_date_warning_days: Option<i64>,
    state: State<'_, AppState>,
) -> Result<crate::models::CalibrationHierarchy, String> {
    use crate::calibration::hierarchy::build_complete_hierarchy;
    use crate::models::CalibrationTolerance;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get frame data
    let mut stmt = conn.prepare(
        "SELECT id, file_id, object, date_obs, telescop, instrume, exptime, filter,
                imagetyp, is_master, ra, dec, objctra, objctdec, gain, offset,
                xbinning, ybinning, ccd_temp, set_temp, focallen, xpixsz, pixsz,
                naxis1, naxis2, sitelat, lat_obs, sitelong, long_obs
         FROM frames WHERE id = ?1"
    ).map_err(|e| e.to_string())?;

    let frame = stmt.query_row([frame_id], |row| {
        let date_obs_str: Option<String> = row.get(3)?;
        let date_obs = date_obs_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let imagetyp_str: Option<String> = row.get(8)?;
        let imagetyp = imagetyp_str.and_then(|s| ImageType::from_str(&s));

        let xbinning: Option<i32> = row.get(16)?;
        let ybinning: Option<i32> = row.get(17)?;
        let binning = match (xbinning, ybinning) {
            (Some(x), Some(y)) => Some(format!("{}x{}", x, y)),
            _ => None,
        };

        Ok(Frame {
            id: Some(row.get(0)?),
            file_id: row.get(1)?,
            object: row.get(2)?,
            date_obs,
            telescop: row.get(4)?,
            instrume: row.get(5)?,
            exptime: row.get(6)?,
            filter: row.get(7)?,
            imagetyp,
            is_master: row.get(9)?,
            ra: row.get(10)?,
            dec: row.get(11)?,
            objctra: row.get(12)?,
            objctdec: row.get(13)?,
            gain: row.get(14)?,
            offset: row.get(15)?,
            xbinning,
            ybinning,
            binning,
            ccd_temp: row.get(18)?,
            set_temp: row.get(19)?,
            focallen: row.get(20)?,
            xpixsz: row.get(21)?,
            pixsz: row.get(22)?,
            naxis1: row.get(23)?,
            naxis2: row.get(24)?,
            sitelat: row.get(25)?,
            lat_obs: row.get(26)?,
            sitelong: row.get(27)?,
            long_obs: row.get(28)?,
            override_: false,
        })
    }).map_err(|e| format!("Frame not found: {}", e))?;

    // Build tolerance
    let tolerance = CalibrationTolerance {
        temp_delta_celsius: temp_delta_celsius.unwrap_or(2.0),
        flat_date_warning_days: flat_date_warning_days.unwrap_or(30),
        dark_date_warning_days: dark_date_warning_days.unwrap_or(365),
    };

    // Get flat calibration settings
    let max_age_days = state.settings.get_flats_max_age_days(&conn)
        .map_err(|e| format!("Failed to get flats max age: {}", e))?;
    let time_cluster_minutes = state.settings.get_flats_time_cluster_minutes(&conn)
        .map_err(|e| format!("Failed to get time cluster threshold: {}", e))?;
    let temp_weight = state.settings.get_temperature_match_weight(&conn)
        .map_err(|e| format!("Failed to get temperature match weight: {}", e))?;

    // Build hierarchy (no pattern or manual selection for single frame view)
    build_complete_hierarchy(
        &conn,
        &frame,
        &tolerance,
        None,  // flat_pattern
        None,  // manual_flat_set_id
        max_age_days,
        time_cluster_minutes,
        temp_weight,
        None,  // session_start
        None,  // session_end
        &state, // AppState for on-demand calibration creation
    ).map_err(|e| format!("Failed to build hierarchy: {}", e))
}

/// Get available flat group options for a frame set (for manual selection)
///
/// Returns a map of filter -> Vec<FlatGroup>, where each FlatGroup represents a group of flats
/// that were taken in temporal proximity.
#[tauri::command]
pub async fn get_flat_group_options_for_frame_set(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, Vec<crate::calibration::flat_groups::FlatGroup>>, String> {
    use crate::calibration::flat_groups::detect_flat_groups;
    use crate::calibration::processor::get_light_frames_from_frame_set;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get flat calibration settings
    let max_age_days = state.settings.get_flats_max_age_days(&conn)
        .map_err(|e| format!("Failed to get flats max age: {}", e))?;
    let time_cluster_minutes = state.settings.get_flats_time_cluster_minutes(&conn)
        .map_err(|e| format!("Failed to get time cluster threshold: {}", e))?;

    // Get all light frames from the frame set to determine unique filters
    let frames = get_light_frames_from_frame_set(&conn, frame_set_id)
        .map_err(|e| format!("Failed to get light frames: {}", e))?;

    if frames.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    // Get unique camera/binning combinations from frames
    let first_frame = &frames[0];
    let instrume = first_frame.instrume.as_ref()
        .ok_or("Light frames missing instrume")?;
    let binning = first_frame.binning.as_ref()
        .ok_or("Light frames missing binning")?;
    let gain = first_frame.gain;
    let focal_length = first_frame.focallen;

    // Calculate date range (±max_age_days from frame set dates)
    let dates: Vec<chrono::DateTime<chrono::Utc>> = frames
        .iter()
        .filter_map(|f| f.date_obs)
        .collect();

    let date_range = if !dates.is_empty() {
        let earliest = *dates.iter().min().unwrap();
        let latest = *dates.iter().max().unwrap();
        let start = earliest - chrono::Duration::days(max_age_days);
        let end = latest + chrono::Duration::days(max_age_days);
        Some((start, end))
    } else {
        None
    };

    // Get unique filters from light frames
    let mut filters: Vec<Option<String>> = frames
        .iter()
        .map(|f| f.filter.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    filters.sort();

    // Detect flat groups for each filter
    let mut results = std::collections::HashMap::new();

    for filter in filters {
        let flat_groups = detect_flat_groups(
            &conn,
            instrume,
            filter.as_deref(),
            binning,
            gain,
            focal_length,
            time_cluster_minutes,
            date_range,
        ).map_err(|e| format!("Failed to detect flat groups: {}", e))?;

        // Use filter name or "No Filter" as key
        let key = filter.unwrap_or_else(|| "No Filter".to_string());
        results.insert(key, flat_groups);
    }

    Ok(results)
}

/// Clear all calibration links for a frame set
#[tauri::command]
pub async fn clear_calibration_links(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    use crate::calibration::processor::clear_calibration_links_for_frame_set;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    println!("Clearing calibration links for frame set {}", frame_set_id);

    let deleted_count = clear_calibration_links_for_frame_set(&conn, frame_set_id)
        .map_err(|e| format!("Failed to clear calibration links: {}", e))?;

    println!("✅ Cleared {} calibration links", deleted_count);

    Ok(deleted_count)
}

/// Get calibration links for a specific frame
#[tauri::command]
pub async fn get_frame_calibration_links(
    frame_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::CalibrationLink>, String> {
    use crate::db::calibration_links::get_links_for_frame;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    get_links_for_frame(&conn, frame_id).map_err(|e| e.to_string())
}

/// Get frame calibration status (which calibrations are linked)
#[tauri::command]
pub async fn get_frame_status(
    frame_id: i64,
    state: State<'_, AppState>,
) -> Result<crate::models::FrameCalibrationStatus, String> {
    use crate::db::calibration_links::get_frame_calibration_status;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    get_frame_calibration_status(&conn, frame_id).map_err(|e| e.to_string())
}

