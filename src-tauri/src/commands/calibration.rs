// Calibration commands - calibration frame matching and library management

use crate::calibration::scan_integration::{create_calibration_sets_from_scan, CalibrationScanResult};
use crate::db::{self};
use crate::models::*;
use chrono::{DateTime, Utc};
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
    flat_date_warning_days: Option<i64>,
    dark_date_warning_days: Option<i64>,
    flat_pattern: Option<String>,
    manual_flat_selections: Option<std::collections::HashMap<String, i64>>,
    state: State<'_, AppState>,
) -> Result<crate::calibration::processor::ProcessingStats, String> {
    use crate::calibration::processor::process_frame_set;
    use crate::models::CalibrationTolerance;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get frame set metadata (flat_pattern, date ranges)
    let mut stmt = conn.prepare(
        "SELECT flat_pattern, date_obs_start, date_obs_end FROM frames_set WHERE id = ?1"
    ).map_err(|e| e.to_string())?;

    let (stored_flat_pattern, _date_obs_start, _date_obs_end): (Option<String>, Option<String>, Option<String>) =
        stmt.query_row([frame_set_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        }).map_err(|e| format!("Frame set not found: {}", e))?;

    // Use provided flat_pattern or fall back to stored pattern
    let final_flat_pattern = flat_pattern.or(stored_flat_pattern);

    // Load calibration config
    let config = crate::calibration::configurable_matcher::load_config(&conn);

    // Get flat calibration settings from config
    let max_age_days = config.clustering.get("flat")
        .map(|c| c.max_age_days)
        .unwrap_or(30);
    let time_cluster_minutes = config.clustering.get("flat")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(30);
    let temp_weight = config.scoring.temperature_match_weight;

    // Build tolerance from parameters, config warnings, or defaults
    let tolerance = CalibrationTolerance {
        flat_date_warning_days: flat_date_warning_days.unwrap_or(config.warnings.flat_date_warning_days),
        dark_date_warning_days: dark_date_warning_days.unwrap_or(config.warnings.dark_date_warning_days),
    };

    println!(
        "Finding calibration for frame set {} with tolerance: flat_date={} days, dark_date={} days",
        frame_set_id,
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

/// Get calibration hierarchy organized by Date → Camera → Filter for a frame set
#[tauri::command]
pub async fn get_calibration_hierarchy_for_frame_set(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<crate::models::CalibrationHierarchyView, String> {
    use crate::db::calibration_links::get_calibration_hierarchy_for_frame_set as get_hierarchy;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    get_hierarchy(&conn, frame_set_id).map_err(|e| e.to_string())
}

/// Get complete calibration hierarchy for a specific frame
#[tauri::command]
pub async fn get_frame_calibration_hierarchy(
    frame_id: i64,
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
                xbinning, ybinning, ccd_temp, set_temp, focallen, xpixsz, ypixsz,
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
            ypixsz: row.get(22)?,
            naxis1: row.get(23)?,
            naxis2: row.get(24)?,
            sitelat: row.get(25)?,
            lat_obs: row.get(26)?,
            sitelong: row.get(27)?,
            long_obs: row.get(28)?,
            override_: false,
            swcreate: None,
        })
    }).map_err(|e| format!("Frame not found: {}", e))?;

    // Load calibration config
    let config = crate::calibration::configurable_matcher::load_config(&conn);

    // Build tolerance from parameters, config warnings, or defaults
    let tolerance = CalibrationTolerance {
        flat_date_warning_days: flat_date_warning_days.unwrap_or(config.warnings.flat_date_warning_days),
        dark_date_warning_days: dark_date_warning_days.unwrap_or(config.warnings.dark_date_warning_days),
    };

    // Get flat calibration settings from config
    let max_age_days = config.clustering.get("flat")
        .map(|c| c.max_age_days)
        .unwrap_or(30);
    let time_cluster_minutes = config.clustering.get("flat")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(30);
    let temp_weight = config.scoring.temperature_match_weight;

    // Build hierarchy (no pattern or manual selection for single frame view)
    build_complete_hierarchy(
        &conn,
        &frame,
        &tolerance,
        None,  // flat_pattern (defaults to Automatic)
        None,  // manual_flat_set_id
        max_age_days,
        time_cluster_minutes,
        temp_weight,
        &state,
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

    // Load calibration config
    let config = crate::calibration::configurable_matcher::load_config(&conn);

    // Get flat calibration settings from config
    let max_age_days = config.clustering.get("flat")
        .map(|c| c.max_age_days)
        .unwrap_or(30);
    let time_cluster_minutes = config.clustering.get("flat")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(30);

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

    // Get focallen threshold from config
    let focallen_threshold = config.lights.flat
        .as_ref()
        .and_then(|f| f.focallen.matching_threshold);

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
            focallen_threshold,
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

// ========== Calibration Matching Config Commands ==========

const CALIBRATION_CONFIG_KEY: &str = "calibration.matching_config";

/// Get the current calibration matching configuration
#[tauri::command]
pub async fn get_calibration_matching_config(
    state: State<'_, AppState>,
) -> Result<crate::calibration::CalibrationMatchingConfig, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Try to get from settings
    let config_json = crate::db::get_setting(&conn, CALIBRATION_CONFIG_KEY)
        .map_err(|e| e.to_string())?;

    match config_json {
        Some(json) => {
            crate::calibration::CalibrationMatchingConfig::from_json(&json)
                .map_err(|e| format!("Failed to parse calibration config: {}", e))
        }
        None => {
            // Return default config if not set
            Ok(crate::calibration::CalibrationMatchingConfig::default())
        }
    }
}

/// Set the calibration matching configuration
#[tauri::command]
pub async fn set_calibration_matching_config(
    config: crate::calibration::CalibrationMatchingConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Validate config before saving (ensures warning_threshold <= matching_threshold)
    config.validate()?;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let json = config.to_json()
        .map_err(|e| format!("Failed to serialize calibration config: {}", e))?;

    crate::db::set_setting(&conn, CALIBRATION_CONFIG_KEY, &json)
        .map_err(|e| e.to_string())
}

/// Reset calibration matching configuration to defaults
#[tauri::command]
pub async fn reset_calibration_matching_config(
    state: State<'_, AppState>,
) -> Result<crate::calibration::CalibrationMatchingConfig, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let default_config = crate::calibration::CalibrationMatchingConfig::default();
    let json = default_config.to_json()
        .map_err(|e| format!("Failed to serialize default config: {}", e))?;

    crate::db::set_setting(&conn, CALIBRATION_CONFIG_KEY, &json)
        .map_err(|e| e.to_string())?;

    Ok(default_config)
}

// ========== Manual Calibration Selection Commands ==========

/// Get average parameters of light frames for manual selection display
#[tauri::command]
pub async fn get_light_frame_parameters(
    frame_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<LightFrameParameters, String> {
    use crate::db::calibration_links::get_links_for_frame;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err("No frame IDs provided".to_string());
    }

    // Query frames
    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT id, instrume, binning, gain, offset, filter, ccd_temp, exptime, date_obs
         FROM frames WHERE id IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;

    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let mut rows = stmt.query(params.as_slice()).map_err(|e| e.to_string())?;

    let mut instrumes: Vec<Option<String>> = Vec::new();
    let mut binnings: Vec<Option<String>> = Vec::new();
    let mut gains: Vec<Option<f64>> = Vec::new();
    let mut offsets: Vec<Option<f64>> = Vec::new();
    let mut filters: Vec<Option<String>> = Vec::new();
    let mut temps: Vec<f64> = Vec::new();
    let mut exptimes: Vec<f64> = Vec::new();
    let mut dates: Vec<String> = Vec::new();
    let mut frame_count = 0;

    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        frame_count += 1;
        instrumes.push(row.get(1).ok());
        binnings.push(row.get(2).ok());
        gains.push(row.get(3).ok());
        offsets.push(row.get(4).ok());
        filters.push(row.get(5).ok());
        if let Ok(Some(temp)) = row.get::<_, Option<f64>>(6) {
            temps.push(temp);
        }
        if let Ok(Some(exp)) = row.get::<_, Option<f64>>(7) {
            exptimes.push(exp);
        }
        if let Ok(Some(date)) = row.get::<_, Option<String>>(8) {
            dates.push(date);
        }
    }

    // Get most common values
    let instrume = most_common_option(&instrumes);
    let binning = most_common_option(&binnings);
    let gain = most_common_f64(&gains);
    let offset = most_common_f64(&offsets);
    let filter = most_common_option(&filters);

    // Calculate averages and ranges
    let avg_ccd_temp = if temps.is_empty() { None } else {
        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    };

    let avg_exptime = if exptimes.is_empty() { None } else {
        Some(exptimes.iter().sum::<f64>() / exptimes.len() as f64)
    };

    let exptime_range = if exptimes.is_empty() { None } else {
        let min = exptimes.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = exptimes.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        Some((min, max))
    };

    let date_range = if dates.is_empty() { None } else {
        let mut sorted = dates.clone();
        sorted.sort();
        Some((sorted.first().unwrap().clone(), sorted.last().unwrap().clone()))
    };

    // Get current calibration links for first frame (representative)
    let first_frame_id = frame_ids[0];
    let links = get_links_for_frame(&conn, first_frame_id).map_err(|e| e.to_string())?;

    let current_flat_set_id = links.iter()
        .find(|l| l.calibration_type == "Flat")
        .map(|l| l.calibration_set_id);
    let current_dark_set_id = links.iter()
        .find(|l| l.calibration_type == "Dark")
        .map(|l| l.calibration_set_id);
    let current_bias_set_id = links.iter()
        .find(|l| l.calibration_type == "Bias")
        .map(|l| l.calibration_set_id);

    Ok(LightFrameParameters {
        instrume,
        binning,
        gain,
        offset,
        filter,
        avg_ccd_temp,
        avg_exptime,
        exptime_range,
        frame_count,
        date_range,
        current_flat_set_id,
        current_dark_set_id,
        current_bias_set_id,
    })
}

/// Get calibration sets with match scores for manual selection
#[tauri::command]
pub async fn get_calibration_sets_for_manual_selection(
    frame_ids: Vec<i64>,
    calibration_type: String,  // "flat", "dark", "bias"
    show_all: bool,
    state: State<'_, AppState>,
) -> Result<Vec<CalibrationSetWithScore>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err("No frame IDs provided".to_string());
    }

    // Get light frame parameters first
    let params = get_light_params_internal(&conn, &frame_ids)?;

    // Get all calibration sets of the specified type
    let imagetyp = match calibration_type.to_lowercase().as_str() {
        "flat" => "FLAT",
        "dark" => "DARK",
        "bias" => "BIAS",
        "darkflat" => "DARKFLAT",
        _ => return Err(format!("Invalid calibration type: {}", calibration_type)),
    };

    // Query all sets of this type
    let mut stmt = conn.prepare(
        "SELECT cs.id, cs.imagetyp, cs.exptime, cs.ccd_temp, cs.gain, cs.offset,
                cs.binning, cs.instrume, cs.filter, cs.date_start, cs.date_end,
                cs.temp_min, cs.temp_max, cs.frame_count, cs.focallen
         FROM calibration_set cs
         WHERE UPPER(cs.imagetyp) = ?1
         ORDER BY cs.date_start DESC"
    ).map_err(|e| e.to_string())?;

    let sets_iter = stmt.query_map([imagetyp], |row| {
        Ok(CalibrationSetDetail {
            id: row.get(0)?,
            imagetyp: ImageType::from_str(row.get::<_, String>(1)?.as_str())
                .unwrap_or(ImageType::Flat),
            exptime: row.get(2)?,
            ccd_temp: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            gain: row.get(4)?,
            offset: row.get(5)?,
            binning: row.get(6)?,
            instrume: row.get(7)?,
            filter: row.get(8)?,
            date_start: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            date_end: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
            temp_min: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
            temp_max: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
            frame_count: row.get::<_, Option<i64>>(13)?.unwrap_or(0),
            date_display: "".to_string(),  // Will be calculated
        })
    }).map_err(|e| e.to_string())?;

    let sets: Vec<CalibrationSetDetail> = sets_iter
        .filter_map(|r| r.ok())
        .map(|mut set| {
            // Calculate date display
            if set.date_start == set.date_end {
                set.date_display = set.date_start.chars().take(10).collect();
            } else {
                let start: String = set.date_start.chars().take(10).collect();
                let end: String = set.date_end.chars().take(10).collect();
                set.date_display = format!("{} - {}", start, end);
            }
            set
        })
        .collect();

    // Calculate match score for each set
    let mut scored_sets: Vec<CalibrationSetWithScore> = Vec::new();

    for set in sets {
        let match_details = calculate_match_details(&params, &set, &calibration_type);
        let match_score = calculate_match_score(&match_details, &calibration_type);

        // Skip non-matching sets unless show_all is true
        if !show_all && match_score < 0.1 {
            continue;
        }

        scored_sets.push(CalibrationSetWithScore {
            set,
            match_score,
            match_details,
        });
    }

    // Sort by match score (highest first)
    scored_sets.sort_by(|a, b| b.match_score.partial_cmp(&a.match_score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(scored_sets)
}

/// Manually assign a calibration set to light frames
#[tauri::command]
pub async fn manual_assign_calibration(
    frame_ids: Vec<i64>,
    calibration_set_id: i64,
    calibration_type: String,  // "Flat", "Dark", "Bias"
    state: State<'_, AppState>,
) -> Result<usize, String> {
    use crate::db::calibration_links::insert_calibration_link;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err("No frame IDs provided".to_string());
    }

    // Validate calibration_type
    let valid_types = ["Flat", "Dark", "Bias", "DarkFlat"];
    if !valid_types.contains(&calibration_type.as_str()) {
        return Err(format!("Invalid calibration type: {}", calibration_type));
    }

    let mut assigned_count = 0;

    for frame_id in &frame_ids {
        // Create the calibration link with is_manual_override = true
        let link = CalibrationLink {
            id: None,
            source_id: *frame_id,
            source_type: "frame".to_string(),
            calibration_set_id,
            calibration_type: calibration_type.clone(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(1.0),  // Manual assignment gets perfect score
            date_warning: false,
            temp_warning: false,
            is_manual_override: true,
        };

        match insert_calibration_link(&conn, &link) {
            Ok(_) => assigned_count += 1,
            Err(e) => {
                eprintln!("Failed to assign calibration to frame {}: {}", frame_id, e);
            }
        }
    }

    println!(
        "✅ Manually assigned {} set {} to {} frames ({} of type {})",
        calibration_type, calibration_set_id, assigned_count, frame_ids.len(), calibration_type
    );

    Ok(assigned_count)
}

/// Remove manual calibration override and allow auto-find to reassign
#[tauri::command]
pub async fn clear_manual_calibration_override(
    frame_ids: Vec<i64>,
    calibration_type: Option<String>,  // None = clear all types
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err("No frame IDs provided".to_string());
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();

    let sql = match &calibration_type {
        Some(_ct) => format!(
            "DELETE FROM calibration_set_to_frames
             WHERE source_id IN ({}) AND source_type = 'frame'
             AND calibration_type = ?{} AND is_manual_override = 1",
            placeholders.join(", "),
            frame_ids.len() + 1
        ),
        None => format!(
            "DELETE FROM calibration_set_to_frames
             WHERE source_id IN ({}) AND source_type = 'frame' AND is_manual_override = 1",
            placeholders.join(", ")
        ),
    };

    let mut params: Vec<Box<dyn rusqlite::ToSql>> = frame_ids.iter()
        .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>)
        .collect();

    if let Some(ct) = &calibration_type {
        params.push(Box::new(ct.clone()));
    }

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter()
        .map(|p| p.as_ref())
        .collect();

    let deleted = conn.execute(&sql, param_refs.as_slice())
        .map_err(|e| e.to_string())?;

    println!(
        "✅ Cleared {} manual calibration override(s) from {} frames",
        deleted, frame_ids.len()
    );

    Ok(deleted)
}

// Helper functions for manual selection

fn get_light_params_internal(
    conn: &rusqlite::Connection,
    frame_ids: &[i64],
) -> Result<LightFrameParameters, String> {
    if frame_ids.is_empty() {
        return Err("No frame IDs provided".to_string());
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT id, instrume, binning, gain, offset, filter, ccd_temp, exptime, date_obs
         FROM frames WHERE id IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;

    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let mut rows = stmt.query(params.as_slice()).map_err(|e| e.to_string())?;

    let mut instrumes: Vec<Option<String>> = Vec::new();
    let mut binnings: Vec<Option<String>> = Vec::new();
    let mut gains: Vec<Option<f64>> = Vec::new();
    let mut offsets: Vec<Option<f64>> = Vec::new();
    let mut filters: Vec<Option<String>> = Vec::new();
    let mut temps: Vec<f64> = Vec::new();
    let mut exptimes: Vec<f64> = Vec::new();
    let mut dates: Vec<String> = Vec::new();
    let mut frame_count = 0;

    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        frame_count += 1;
        instrumes.push(row.get(1).ok());
        binnings.push(row.get(2).ok());
        gains.push(row.get(3).ok());
        offsets.push(row.get(4).ok());
        filters.push(row.get(5).ok());
        if let Ok(Some(temp)) = row.get::<_, Option<f64>>(6) {
            temps.push(temp);
        }
        if let Ok(Some(exp)) = row.get::<_, Option<f64>>(7) {
            exptimes.push(exp);
        }
        if let Ok(Some(date)) = row.get::<_, Option<String>>(8) {
            dates.push(date);
        }
    }

    let instrume = most_common_option(&instrumes);
    let binning = most_common_option(&binnings);
    let gain = most_common_f64(&gains);
    let offset = most_common_f64(&offsets);
    let filter = most_common_option(&filters);

    let avg_ccd_temp = if temps.is_empty() { None } else {
        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    };

    let avg_exptime = if exptimes.is_empty() { None } else {
        Some(exptimes.iter().sum::<f64>() / exptimes.len() as f64)
    };

    let exptime_range = if exptimes.is_empty() { None } else {
        let min = exptimes.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = exptimes.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        Some((min, max))
    };

    let date_range = if dates.is_empty() { None } else {
        let mut sorted = dates.clone();
        sorted.sort();
        Some((sorted.first().unwrap().clone(), sorted.last().unwrap().clone()))
    };

    Ok(LightFrameParameters {
        instrume,
        binning,
        gain,
        offset,
        filter,
        avg_ccd_temp,
        avg_exptime,
        exptime_range,
        frame_count,
        date_range,
        current_flat_set_id: None,
        current_dark_set_id: None,
        current_bias_set_id: None,
    })
}

fn calculate_match_details(
    params: &LightFrameParameters,
    set: &CalibrationSetDetail,
    calibration_type: &str,
) -> MatchDetails {
    // Instrume match
    let instrume_match = match (&params.instrume, &set.instrume) {
        (Some(p), Some(s)) => p.to_lowercase() == s.to_lowercase(),
        (None, None) => true,
        _ => false,
    };

    // Binning match
    let binning_match = match (&params.binning, &set.binning) {
        (Some(p), Some(s)) => p == s,
        (None, None) => true,
        _ => false,
    };

    // Gain match (with small tolerance)
    let gain_match = match (params.gain, set.gain) {
        (Some(p), Some(s)) => (p - s).abs() < 0.01,
        (None, None) => true,
        _ => false,
    };

    // Offset match (with small tolerance)
    let offset_match = match (params.offset, set.offset) {
        (Some(p), Some(s)) => (p - s).abs() < 0.01,
        (None, None) => true,
        _ => false,
    };

    // Exposure time match - CRITICAL for darks! (with 0.1s tolerance)
    // For bias, exptime doesn't matter (they're typically 0s or very short)
    let exptime_match = if calibration_type.to_lowercase() == "bias" {
        true  // Bias frames don't need exptime match
    } else {
        match (params.avg_exptime, set.exptime) {
            (Some(p), Some(s)) => (p - s).abs() < 0.1,
            (None, None) => true,
            _ => false,
        }
    };

    // Filter match (only relevant for flats)
    let filter_match = if calibration_type.to_lowercase() == "flat" {
        match (&params.filter, &set.filter) {
            (Some(p), Some(s)) => p.to_lowercase() == s.to_lowercase(),
            (None, None) => true,
            _ => false,
        }
    } else {
        true  // Non-flat types don't need filter match
    };

    // Temperature difference
    let temp_diff = match (params.avg_ccd_temp, set.ccd_temp) {
        (Some(p), c) if c != 0.0 => Some((p - c).abs()),
        _ => None,
    };

    // Date difference (calculate from date ranges)
    let date_diff_days = calculate_date_diff_days(params, set);

    MatchDetails {
        instrume_match,
        binning_match,
        gain_match,
        offset_match,
        exptime_match,
        filter_match,
        temp_diff,
        date_diff_days,
    }
}

fn calculate_date_diff_days(
    params: &LightFrameParameters,
    set: &CalibrationSetDetail,
) -> i64 {
    use chrono::NaiveDate;

    let parse_date = |s: &str| -> Option<NaiveDate> {
        // Try parsing as full datetime first, then just date
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(dt.date_naive());
        }
        NaiveDate::parse_from_str(&s.chars().take(10).collect::<String>(), "%Y-%m-%d").ok()
    };

    // Get light frame date range
    let light_dates = match &params.date_range {
        Some((start, end)) => (parse_date(start), parse_date(end)),
        None => return 365 * 10,  // No date info = very large diff
    };

    // Get calibration set dates
    let set_start = parse_date(&set.date_start);
    let set_end = parse_date(&set.date_end);

    match (light_dates, set_start, set_end) {
        ((Some(l_start), Some(l_end)), Some(s_start), Some(s_end)) => {
            // Calculate minimum distance between ranges
            if l_end < s_start {
                // Light frames are before calibration
                (s_start - l_end).num_days()
            } else if l_start > s_end {
                // Light frames are after calibration
                (l_start - s_end).num_days()
            } else {
                // Ranges overlap
                0
            }
        }
        _ => 365 * 10,  // Missing dates = very large diff
    }
}

fn calculate_match_score(details: &MatchDetails, calibration_type: &str) -> f64 {
    // Base score starts at 1.0 and is reduced for mismatches
    let mut score: f64 = 1.0;
    let cal_type_lower = calibration_type.to_lowercase();

    // Critical parameters (must match for any score)
    if !details.instrume_match {
        score -= 0.5;  // Major penalty
    }
    if !details.binning_match {
        score -= 0.3;
    }
    if !details.gain_match {
        score -= 0.2;
    }
    if !details.offset_match {
        score -= 0.2;
    }

    // CRITICAL: Exposure time MUST match for darks and darkflats!
    // A dark with wrong exposure time is COMPLETELY USELESS for calibration
    if (cal_type_lower == "dark" || cal_type_lower == "darkflat") && !details.exptime_match {
        score -= 1.0;  // Complete disqualification - score will be 0 or negative
    }

    // CRITICAL: Filter MUST match for flats!
    // A flat with the wrong filter is COMPLETELY USELESS for calibration
    if cal_type_lower == "flat" && !details.filter_match {
        score -= 1.0;  // Complete disqualification - score will be 0 or negative
    }

    // Temperature penalty (smaller penalty for close temps)
    if let Some(temp_diff) = details.temp_diff {
        if temp_diff > 10.0 {
            score -= 0.15;
        } else if temp_diff > 5.0 {
            score -= 0.1;
        } else if temp_diff > 2.0 {
            score -= 0.05;
        }
    }

    // Date penalty (prefer recent calibrations)
    if details.date_diff_days > 365 {
        score -= 0.15;
    } else if details.date_diff_days > 90 {
        score -= 0.1;
    } else if details.date_diff_days > 30 {
        score -= 0.05;
    }

    // Clamp to 0.0-1.0
    score.max(0.0).min(1.0)
}

fn most_common_option(values: &[Option<String>]) -> Option<String> {
    use std::collections::HashMap;

    let mut counts: HashMap<&String, usize> = HashMap::new();
    for v in values.iter().flatten() {
        *counts.entry(v).or_insert(0) += 1;
    }

    counts.into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(v, _)| v.clone())
}

fn most_common_f64(values: &[Option<f64>]) -> Option<f64> {
    use std::collections::HashMap;

    // Round to 2 decimal places for comparison
    let mut counts: HashMap<i64, (f64, usize)> = HashMap::new();
    for v in values.iter().flatten() {
        let key = (*v * 100.0).round() as i64;
        let entry = counts.entry(key).or_insert((*v, 0));
        entry.1 += 1;
    }

    counts.into_iter()
        .max_by_key(|(_, (_, count))| *count)
        .map(|(_, (v, _))| v)
}

// ========== Refresh Calibration Library Command ==========

/// Refresh calibration library for a specific camera
///
/// This command preserves calibration set IDs by:
/// 1. Clearing frame memberships (but keeping the sets)
/// 2. Queries all calibration frame IDs for the camera (Flat, Dark, Bias, DarkFlat)
/// 3. Reclusters and assigns frames to sets - sets are matched by params + date overlap
/// 4. Deletes orphaned sets (sets with 0 frames after reclustering)
#[tauri::command]
pub async fn refresh_calibration_library_for_camera(
    state: State<'_, AppState>,
    instrume: String,
) -> Result<CalibrationScanResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    println!("🔄 Refreshing calibration library for camera: {}", instrume);

    // Step 1: Clear frame memberships for this camera's sets (but keep the sets)
    // This allows reclustering to rebuild the memberships while preserving set IDs
    conn.execute(
        "DELETE FROM calibration_set_frames
         WHERE set_id IN (
             SELECT id FROM calibration_set WHERE instrume = ?1 AND is_master_library = 0
         )",
        rusqlite::params![instrume],
    ).map_err(|e| format!("Failed to clear frame memberships: {}", e))?;

    // Reset frame counts to 0 for these sets
    conn.execute(
        "UPDATE calibration_set SET frame_count = 0
         WHERE instrume = ?1 AND is_master_library = 0",
        rusqlite::params![instrume],
    ).map_err(|e| format!("Failed to reset frame counts: {}", e))?;

    println!("   ✅ Cleared existing frame memberships (sets preserved for ID stability)");

    // Step 2: Query all calibration frame IDs for this camera
    let flat_frame_ids = query_frame_ids_by_type(&conn, &instrume, "FLAT")
        .map_err(|e| format!("Failed to query flat frames: {}", e))?;

    let dark_frame_ids = query_frame_ids_by_type(&conn, &instrume, "DARK")
        .map_err(|e| format!("Failed to query dark frames: {}", e))?;

    let bias_frame_ids = query_frame_ids_by_type(&conn, &instrume, "BIAS")
        .map_err(|e| format!("Failed to query bias frames: {}", e))?;

    let darkflat_frame_ids = query_frame_ids_by_type(&conn, &instrume, "DARKFLAT")
        .map_err(|e| format!("Failed to query darkflat frames: {}", e))?;

    println!("   📊 Found frames - Flats: {}, Darks: {}, Bias: {}, DarkFlats: {}",
        flat_frame_ids.len(), dark_frame_ids.len(), bias_frame_ids.len(), darkflat_frame_ids.len());

    // Step 3: Recreate calibration sets using the same algorithm as folder scanning
    // Sets will be matched by params + date range overlap, preserving IDs
    let result = create_calibration_sets_from_scan(
        &conn,
        flat_frame_ids,
        dark_frame_ids,
        bias_frame_ids,
        darkflat_frame_ids,
    ).map_err(|e| format!("Failed to create calibration sets: {}", e))?;

    // Step 4: Delete orphaned sets (sets with no frames after reclustering)
    let deleted_orphans = conn.execute(
        "DELETE FROM calibration_set
         WHERE instrume = ?1 AND is_master_library = 0 AND frame_count = 0",
        rusqlite::params![instrume],
    ).map_err(|e| format!("Failed to delete orphaned sets: {}", e))?;

    if deleted_orphans > 0 {
        println!("   🗑️  Deleted {} orphaned sets", deleted_orphans);
    }

    println!("🎉 Refresh complete - {} calibration sets active (IDs preserved)", result.sets_created);

    Ok(result)
}

/// Query frame IDs for a specific camera and image type
fn query_frame_ids_by_type(
    conn: &rusqlite::Connection,
    instrume: &str,
    imagetyp: &str,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id FROM frames
         WHERE instrume = ?1 AND UPPER(imagetyp) = UPPER(?2)"
    )?;

    let ids: Vec<i64> = stmt
        .query_map([instrume, imagetyp], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ids)
}

// ========== Cleanup Commands ==========

/// Result of cleanup operation
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubCalibrationCleanupResult {
    pub sets_cleaned: usize,
    pub details: Vec<String>,
}

/// Clean up duplicate sub-calibration links for flat calibration sets
/// This removes Bias links when a Dark link exists (respecting fallback chain priority)
#[tauri::command]
pub async fn cleanup_duplicate_flat_subcalibrations(
    state: State<'_, AppState>,
) -> Result<SubCalibrationCleanupResult, String> {
    use crate::calibration::configurable_matcher::load_config;
    use rusqlite::params;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Load config to determine fallback priority
    let config = load_config(&conn);
    let use_bias = config.get_behavioral_options("flats")
        .map(|opts| opts.use_bias_if_no_darks)
        .unwrap_or(false);

    println!("🧹 Starting sub-calibration cleanup (use_bias_if_no_darks={})", use_bias);

    // Find calibration sets with multiple sub-calibrations
    let mut stmt = conn.prepare(
        "SELECT source_id, GROUP_CONCAT(calibration_type) as types
         FROM calibration_set_to_frames
         WHERE source_type = 'calibration_set'
         GROUP BY source_id
         HAVING COUNT(*) > 1"
    ).map_err(|e| e.to_string())?;

    let duplicates: Vec<(i64, String)> = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?))
    }).map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;

    println!("  Found {} calibration sets with multiple sub-calibrations", duplicates.len());

    let mut cleaned = 0;
    let mut details: Vec<String> = Vec::new();

    for (source_id, types) in duplicates {
        // Priority: DarkFlat > Dark > Bias
        let has_darkflat = types.contains("DarkFlat");
        let has_dark = types.contains("Dark");
        let has_bias = types.contains("Bias");

        // Determine which to keep based on priority
        let keep_type = if has_darkflat {
            "DarkFlat"
        } else if has_dark {
            "Dark"
        } else if has_bias && use_bias {
            "Bias"
        } else {
            continue; // Skip if nothing to keep
        };

        // Count what we're deleting
        let types_to_delete: Vec<&str> = ["DarkFlat", "Dark", "Bias"]
            .iter()
            .filter(|t| **t != keep_type && types.contains(*t))
            .copied()
            .collect();

        if types_to_delete.is_empty() {
            continue;
        }

        // Delete all except the one we're keeping
        let deleted = conn.execute(
            "DELETE FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = 'calibration_set' AND calibration_type != ?2",
            params![source_id, keep_type],
        ).map_err(|e| e.to_string())?;

        if deleted > 0 {
            let detail = format!(
                "Set #{}: kept {}, removed {} ({})",
                source_id,
                keep_type,
                types_to_delete.len(),
                types_to_delete.join(", ")
            );
            println!("  ✅ {}", detail);
            details.push(detail);
            cleaned += 1;
        }
    }

    println!("🎉 Cleanup complete - cleaned {} sets", cleaned);

    Ok(SubCalibrationCleanupResult {
        sets_cleaned: cleaned,
        details,
    })
}

// ========== Sub-Calibration Selection Commands ==========

/// Get parameters of a calibration set for sub-calibration selection display
#[tauri::command]
pub async fn get_calibration_set_parameters(
    set_id: i64,
    state: State<'_, AppState>,
) -> Result<CalibrationSetParameters, String> {
    use crate::db::calibration_links::get_links_for_calibration_set;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get calibration set details
    let mut stmt = conn.prepare(
        "SELECT id, imagetyp, instrume, binning, gain, offset, exptime, filter,
                ccd_temp, date_start, date_end, frame_count
         FROM calibration_set WHERE id = ?1"
    ).map_err(|e| e.to_string())?;

    let (imagetyp, instrume, binning, gain, offset, exptime, filter, ccd_temp, date_start, date_end, frame_count): (
        String, Option<String>, Option<String>, Option<f64>, Option<f64>,
        Option<f64>, Option<String>, Option<f64>, Option<String>, Option<String>, i64
    ) = stmt.query_row([set_id], |row| {
        Ok((
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
            row.get(11)?,
        ))
    }).map_err(|e| format!("Calibration set not found: {}", e))?;

    // Get current sub-calibration links
    let links = get_links_for_calibration_set(&conn, set_id).map_err(|e| e.to_string())?;

    let current_dark_set_id = links.iter()
        .find(|l| l.calibration_type == "Dark")
        .map(|l| l.calibration_set_id);
    let current_darkflat_set_id = links.iter()
        .find(|l| l.calibration_type == "DarkFlat")
        .map(|l| l.calibration_set_id);
    let current_bias_set_id = links.iter()
        .find(|l| l.calibration_type == "Bias")
        .map(|l| l.calibration_set_id);

    Ok(CalibrationSetParameters {
        set_id,
        imagetyp,
        instrume,
        binning,
        gain,
        offset,
        exptime,
        filter,
        ccd_temp,
        date_start,
        date_end,
        frame_count,
        current_dark_set_id,
        current_darkflat_set_id,
        current_bias_set_id,
    })
}

/// Get compatible sub-calibration sets with match scores for a calibration set
#[tauri::command]
pub async fn get_subcalibration_sets_for_manual_selection(
    set_id: i64,
    calibration_type: String,  // "dark", "darkflat", "bias"
    show_all: bool,
    state: State<'_, AppState>,
) -> Result<Vec<CalibrationSetWithScore>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get the source calibration set to use as reference
    let mut stmt = conn.prepare(
        "SELECT instrume, binning, gain, offset, exptime, filter, ccd_temp, date_start, date_end
         FROM calibration_set WHERE id = ?1"
    ).map_err(|e| e.to_string())?;

    let (instrume, binning, gain, offset, exptime, filter, ccd_temp, date_start, date_end): (
        Option<String>, Option<String>, Option<f64>, Option<f64>,
        Option<f64>, Option<String>, Option<f64>, Option<String>, Option<String>
    ) = stmt.query_row([set_id], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
        ))
    }).map_err(|e| format!("Calibration set not found: {}", e))?;

    // Create pseudo light frame params for matching
    let params = LightFrameParameters {
        instrume: instrume.clone(),
        binning: binning.clone(),
        gain,
        offset,
        filter: filter.clone(),
        avg_ccd_temp: ccd_temp,
        avg_exptime: exptime,
        exptime_range: exptime.map(|e| (e, e)),
        frame_count: 1,
        date_range: match (&date_start, &date_end) {
            (Some(s), Some(e)) => Some((s.clone(), e.clone())),
            _ => None,
        },
        current_flat_set_id: None,
        current_dark_set_id: None,
        current_bias_set_id: None,
    };

    // Get target calibration type
    let imagetyp = match calibration_type.to_lowercase().as_str() {
        "dark" => "DARK",
        "darkflat" => "DARKFLAT",
        "bias" => "BIAS",
        _ => return Err(format!("Invalid calibration type for sub-calibration: {}", calibration_type)),
    };

    // Query all sets of this type
    let mut stmt = conn.prepare(
        "SELECT cs.id, cs.imagetyp, cs.exptime, cs.ccd_temp, cs.gain, cs.offset,
                cs.binning, cs.instrume, cs.filter, cs.date_start, cs.date_end,
                cs.temp_min, cs.temp_max, cs.frame_count, cs.focallen
         FROM calibration_set cs
         WHERE UPPER(cs.imagetyp) = ?1
         ORDER BY cs.date_start DESC"
    ).map_err(|e| e.to_string())?;

    let sets_iter = stmt.query_map([imagetyp], |row| {
        Ok(CalibrationSetDetail {
            id: row.get(0)?,
            imagetyp: ImageType::from_str(row.get::<_, String>(1)?.as_str())
                .unwrap_or(ImageType::Dark),
            exptime: row.get(2)?,
            ccd_temp: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            gain: row.get(4)?,
            offset: row.get(5)?,
            binning: row.get(6)?,
            instrume: row.get(7)?,
            filter: row.get(8)?,
            date_start: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            date_end: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
            temp_min: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
            temp_max: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
            frame_count: row.get::<_, Option<i64>>(13)?.unwrap_or(0),
            date_display: "".to_string(),
        })
    }).map_err(|e| e.to_string())?;

    let sets: Vec<CalibrationSetDetail> = sets_iter
        .filter_map(|r| r.ok())
        .map(|mut set| {
            if set.date_start == set.date_end {
                set.date_display = set.date_start.chars().take(10).collect();
            } else {
                let start: String = set.date_start.chars().take(10).collect();
                let end: String = set.date_end.chars().take(10).collect();
                set.date_display = format!("{} - {}", start, end);
            }
            set
        })
        .collect();

    // Calculate match score for each set (use "dark" type logic for all sub-calibrations)
    // Sub-calibration matching: instrume, binning, gain, offset must match
    // For dark/darkflat: exptime must also match the source set's exptime
    let mut scored_sets: Vec<CalibrationSetWithScore> = Vec::new();

    for set in sets {
        let match_details = calculate_subcal_match_details(&params, &set, &calibration_type);
        let match_score = calculate_subcal_match_score(&match_details, &calibration_type);

        // Skip non-matching sets unless show_all is true
        if !show_all && match_score < 0.1 {
            continue;
        }

        scored_sets.push(CalibrationSetWithScore {
            set,
            match_score,
            match_details,
        });
    }

    // Sort by match score (highest first)
    scored_sets.sort_by(|a, b| b.match_score.partial_cmp(&a.match_score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(scored_sets)
}

/// Calculate match details for sub-calibration
fn calculate_subcal_match_details(
    params: &LightFrameParameters,
    set: &CalibrationSetDetail,
    calibration_type: &str,
) -> MatchDetails {
    let instrume_match = match (&params.instrume, &set.instrume) {
        (Some(p), Some(s)) => p.to_lowercase() == s.to_lowercase(),
        (None, None) => true,
        _ => false,
    };

    let binning_match = match (&params.binning, &set.binning) {
        (Some(p), Some(s)) => p == s,
        (None, None) => true,
        _ => false,
    };

    let gain_match = match (params.gain, set.gain) {
        (Some(p), Some(s)) => (p - s).abs() < 0.01,
        (None, None) => true,
        _ => false,
    };

    let offset_match = match (params.offset, set.offset) {
        (Some(p), Some(s)) => (p - s).abs() < 0.01,
        (None, None) => true,
        _ => false,
    };

    // For Dark/DarkFlat sub-calibration of Flats, we need to match exposure time
    // Bias doesn't need exptime match
    let exptime_match = if calibration_type.to_lowercase() == "bias" {
        true
    } else {
        match (params.avg_exptime, set.exptime) {
            (Some(p), Some(s)) => (p - s).abs() < 0.1,
            (None, None) => true,
            _ => false,
        }
    };

    // Filter doesn't need to match for sub-calibration (Dark/Bias don't have filters)
    let filter_match = true;

    let temp_diff = match (params.avg_ccd_temp, set.ccd_temp) {
        (Some(p), c) if c != 0.0 => Some((p - c).abs()),
        _ => None,
    };

    let date_diff_days = calculate_date_diff_days(params, set);

    MatchDetails {
        instrume_match,
        binning_match,
        gain_match,
        offset_match,
        exptime_match,
        filter_match,
        temp_diff,
        date_diff_days,
    }
}

/// Calculate match score for sub-calibration
fn calculate_subcal_match_score(details: &MatchDetails, calibration_type: &str) -> f64 {
    let mut score: f64 = 1.0;
    let cal_type_lower = calibration_type.to_lowercase();

    // Critical parameters
    if !details.instrume_match {
        score -= 0.5;
    }
    if !details.binning_match {
        score -= 0.3;
    }
    if !details.gain_match {
        score -= 0.2;
    }
    if !details.offset_match {
        score -= 0.2;
    }

    // Exposure time MUST match for dark/darkflat
    if (cal_type_lower == "dark" || cal_type_lower == "darkflat") && !details.exptime_match {
        score -= 1.0;
    }

    // Temperature penalty
    if let Some(temp_diff) = details.temp_diff {
        if temp_diff > 10.0 {
            score -= 0.15;
        } else if temp_diff > 5.0 {
            score -= 0.1;
        } else if temp_diff > 2.0 {
            score -= 0.05;
        }
    }

    // Date penalty
    if details.date_diff_days > 365 {
        score -= 0.15;
    } else if details.date_diff_days > 90 {
        score -= 0.1;
    } else if details.date_diff_days > 30 {
        score -= 0.05;
    }

    score.max(0.0).min(1.0)
}

/// Manually assign a sub-calibration set to a calibration set
#[tauri::command]
pub async fn manual_assign_subcalibration(
    source_set_id: i64,
    calibration_set_id: i64,
    calibration_type: String,  // "Dark", "DarkFlat", "Bias"
    state: State<'_, AppState>,
) -> Result<(), String> {
    use crate::db::calibration_links::insert_calibration_link;

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Validate calibration_type
    let valid_types = ["Dark", "DarkFlat", "Bias"];
    if !valid_types.contains(&calibration_type.as_str()) {
        return Err(format!("Invalid sub-calibration type: {}", calibration_type));
    }

    // Create the calibration link with source_type = 'calibration_set'
    let link = CalibrationLink {
        id: None,
        source_id: source_set_id,
        source_type: "calibration_set".to_string(),
        calibration_set_id,
        calibration_type: calibration_type.clone(),
        matched_at: Utc::now().to_rfc3339(),
        match_score: Some(1.0),  // Manual assignment gets perfect score
        date_warning: false,
        temp_warning: false,
        is_manual_override: true,
    };

    insert_calibration_link(&conn, &link)
        .map_err(|e| format!("Failed to assign sub-calibration: {}", e))?;

    println!(
        "✅ Manually assigned {} set #{} as sub-calibration for set #{}",
        calibration_type, calibration_set_id, source_set_id
    );

    Ok(())
}

/// Clear sub-calibration override for a calibration set
#[tauri::command]
pub async fn clear_subcalibration_override(
    source_set_id: i64,
    calibration_type: Option<String>,  // None = clear all sub-calibrations
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let deleted = match &calibration_type {
        Some(ct) => {
            conn.execute(
                "DELETE FROM calibration_set_to_frames
                 WHERE source_id = ?1 AND source_type = 'calibration_set' AND calibration_type = ?2",
                rusqlite::params![source_set_id, ct],
            ).map_err(|e| e.to_string())?
        }
        None => {
            conn.execute(
                "DELETE FROM calibration_set_to_frames
                 WHERE source_id = ?1 AND source_type = 'calibration_set'",
                rusqlite::params![source_set_id],
            ).map_err(|e| e.to_string())?
        }
    };

    println!(
        "✅ Cleared {} sub-calibration link(s) for set #{}",
        deleted, source_set_id
    );

    Ok(deleted)
}
