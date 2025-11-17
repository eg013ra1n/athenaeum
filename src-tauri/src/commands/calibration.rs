// Calibration commands - calibration frame matching and library management

use crate::db::{self, Database};
use crate::models::*;
use chrono::{DateTime, Utc};
use std::sync::Mutex;
use tauri::State;

use super::AppState;

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
