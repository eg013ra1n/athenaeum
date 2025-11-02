use crate::db::{self, Database};
use crate::models::*;
use crate::scanner::scan_directory;
use crate::settings::SettingsManager;
use crate::image_processing;
use std::path::Path;
use std::sync::Mutex;
use std::num::NonZeroUsize;
use tauri::{Manager, State};
use lru::LruCache;
use lazy_static::lazy_static;

/// Cached image data with metadata to avoid re-encoding and re-opening FITS files
#[derive(Clone)]
struct CachedImage {
    image_base64: String,
    width: u32,
    height: u32,
    is_color: bool,
    bit_depth: u8,
}

/// LRU cache for processed FITS images
/// Cache key format: "{path}_{quality}_{midtones}_{black_point}-{white_point}"
struct ImageCache {
    cache: Mutex<LruCache<String, CachedImage>>,
}

impl ImageCache {
    fn new(capacity: usize) -> Self {
        Self {
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(capacity).unwrap())),
        }
    }

    fn get(&self, key: &str) -> Option<CachedImage> {
        self.cache.lock().unwrap().get(key).cloned()
    }

    fn put(&self, key: String, value: CachedImage) {
        self.cache.lock().unwrap().put(key, value);
    }

    fn resize(&self, new_capacity: usize) {
        let mut cache = self.cache.lock().unwrap();
        cache.resize(NonZeroUsize::new(new_capacity).unwrap());
    }
}

// Global image cache with default capacity of 15 images
lazy_static! {
    static ref IMAGE_CACHE: ImageCache = ImageCache::new(15);
}

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

/// Read and process FITS image for blink viewer
/// Returns auto-stretched JPEG image as base64 with caching
#[tauri::command]
pub async fn read_fits_image(
    path: String,
    state: State<'_, AppState>,
) -> Result<image_processing::FitsImageData, String> {
    use std::time::Instant;

    let t_cmd_start = Instant::now();
    let path_buf = std::path::PathBuf::from(&path);

    println!("\n🖼️  === FITS IMAGE REQUEST === ");
    println!("📁 Path: {}", path_buf.display());

    // Read JPEG quality setting from database (default: 50 for speed)
    let quality: u8 = {
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match db::get_setting(&conn, "blink_jpeg_quality") {
                Ok(Some(value)) => value.parse().unwrap_or(50),
                _ => 50,
            }
        } else {
            50
        }
    };

    // Read percentile clipping settings (defaults: 850/64000 for typical 16-bit astro images)
    let black_point: u16 = {
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match db::get_setting(&conn, "blink_black_point") {
                Ok(Some(value)) => value.parse().unwrap_or(850),
                _ => 850,
            }
        } else {
            850
        }
    };

    let white_point: u16 = {
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match db::get_setting(&conn, "blink_white_point") {
                Ok(Some(value)) => value.parse().unwrap_or(64000),
                _ => 64000,
            }
        } else {
            64000
        }
    };

    // Read midtones balance for MTF (0.25 = typical for linear data, lower = darker, higher = brighter)
    let midtones: f32 = {
        let state_lock = state.db.lock().unwrap();
        let raw_value: f32 = if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match db::get_setting(&conn, "blink_midtones") {
                Ok(Some(value)) => value.parse().unwrap_or(0.25),
                _ => 0.25,
            }
        } else {
            0.25
        };
        // Clamp to reasonable range to prevent extreme stretching
        raw_value.max(0.001).min(0.999)
    };

    // Check if cache size setting changed and resize if needed
    {
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            if let Ok(Some(size_str)) = db::get_setting(&conn, "blink_cache_size") {
                if let Ok(size) = size_str.parse::<usize>() {
                    if size >= 5 && size <= 30 {
                        IMAGE_CACHE.resize(size);
                    }
                }
            }
        }
    }

    // Create cache key (include all stretch parameters for proper cache invalidation)
    let cache_key = format!("{}_{}_{}_{}-{}", path, quality, midtones, black_point, white_point);

    println!("⚙️  Settings: quality={}, black={}, white={}, midtones={}", quality, black_point, white_point, midtones);

    // Check cache first
    let t_cache = Instant::now();
    if let Some(cached_image) = IMAGE_CACHE.get(&cache_key) {
        println!("✅ CACHE HIT! (lookup time: {:?})", t_cache.elapsed());
        println!("⏱️  TOTAL COMMAND TIME (cached): {:?}", t_cmd_start.elapsed());
        println!("=== END ===\n");

        return Ok(image_processing::FitsImageData {
            image_base64: cached_image.image_base64,
            width: cached_image.width,
            height: cached_image.height,
            is_color: cached_image.is_color,
            bit_depth: cached_image.bit_depth,
        });
    }

    // Cache miss - process image
    println!("❌ CACHE MISS (lookup time: {:?}) - processing from scratch...", t_cache.elapsed());

    let t_process = Instant::now();
    let result = image_processing::read_and_process_fits(&path_buf, quality, black_point, white_point, midtones)
        .map_err(|e| format!("Failed to read FITS image: {}", e))?;
    println!("⏱️  Processing complete: {:?}", t_process.elapsed());

    // Store in cache (with base64 string and metadata - no re-encoding needed on cache hits)
    let t_cache_store = Instant::now();
    let cached_image = CachedImage {
        image_base64: result.image_base64.clone(),
        width: result.width,
        height: result.height,
        is_color: result.is_color,
        bit_depth: result.bit_depth,
    };
    IMAGE_CACHE.put(cache_key, cached_image);
    println!("💾 Stored in cache: {:?}", t_cache_store.elapsed());

    println!("⏱️  TOTAL COMMAND TIME: {:?}", t_cmd_start.elapsed());
    println!("=== END ===\n");

    Ok(result)
}

/// Read FITS image and return as PNG binary data (no base64 encoding)
/// Optimized for faster transfer and no encoding overhead
#[tauri::command]
pub async fn read_fits_image_png(
    path: String,
    state: State<'_, AppState>,
) -> Result<image_processing::FitsImageBinary, String> {
    use std::time::Instant;

    let t_cmd_start = Instant::now();
    let path_buf = std::path::PathBuf::from(&path);

    println!("\n🖼️  === FITS PNG IMAGE REQUEST === ");
    println!("📁 Path: {}", path_buf.display());

    // Read percentile clipping settings (defaults: 850/64000 for typical 16-bit astro images)
    let black_point: u16 = {
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match db::get_setting(&conn, "blink_black_point") {
                Ok(Some(value)) => value.parse().unwrap_or(850),
                _ => 850,
            }
        } else {
            850
        }
    };

    let white_point: u16 = {
        let state_lock = state.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match db::get_setting(&conn, "blink_white_point") {
                Ok(Some(value)) => value.parse().unwrap_or(64000),
                _ => 64000,
            }
        } else {
            64000
        }
    };

    // Read midtones balance for MTF (0.25 = typical for linear data, lower = darker, higher = brighter)
    let midtones: f32 = {
        let state_lock = state.db.lock().unwrap();
        let raw_value: f32 = if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            match db::get_setting(&conn, "blink_midtones") {
                Ok(Some(value)) => value.parse().unwrap_or(0.25),
                _ => 0.25,
            }
        } else {
            0.25
        };
        // Clamp to reasonable range to prevent extreme stretching
        raw_value.max(0.001).min(0.999)
    };

    // Check if we should use auto-stretch (when user hasn't configured settings)
    let use_auto_stretch = black_point == 850 && white_point == 64000;

    let t_process = Instant::now();
    let result = if use_auto_stretch {
        println!("⚙️  Using auto-stretch (AutoSTF mode)");
        // Pass 0,0 to trigger auto-stretch in the processing function
        image_processing::read_and_process_fits_png(&path_buf, 0, 0, 0.25)
    } else {
        println!("⚙️  Settings: black={}, white={}, midtones={}", black_point, white_point, midtones);
        image_processing::read_and_process_fits_png(&path_buf, black_point, white_point, midtones)
    }.map_err(|e| format!("Failed to read FITS image: {}", e))?;

    println!("⏱️  Processing complete: {:?}", t_process.elapsed());

    println!("⏱️  TOTAL COMMAND TIME: {:?}", t_cmd_start.elapsed());
    println!("=== END ===\n");

    Ok(result)
}
