use rusqlite::{Connection, Result, params};
use crate::models::{
    CalibrationLink, CalibrationStats, FrameCalibrationStatus, CalibrationGroup,
    FrameSetCalibrationGroups, CalibrationSetDetail, ImageType, CalibrationWarning,
    CalibrationHierarchyView, CalibrationDateGroup, CalibrationCameraGroup,
    CalibrationFilterGroup, LightFrameWithCalibration, CalibrationSetWithFrameCount,
    SubCalibrationDetail,
};
use crate::calibration::configurable_matcher::load_config;
use crate::calibration::config::{MatchMode, CalibrationMatchingConfig};
use std::collections::HashMap;
use chrono::NaiveDate;

/// Insert a new calibration link
///
/// If `is_manual_override = false` (auto-find), this will skip updating existing manual overrides.
/// If `is_manual_override = true` (manual assignment), this will always update.
pub fn insert_calibration_link(conn: &Connection, link: &CalibrationLink) -> Result<i64> {
    let matched_at = link.matched_at.clone();
    let date_warning = if link.date_warning { 1 } else { 0 };
    let temp_warning = if link.temp_warning { 1 } else { 0 };
    let is_manual_override = if link.is_manual_override { 1 } else { 0 };

    // If this is an automatic assignment, check if there's a manual override we should preserve
    if !link.is_manual_override {
        let has_manual_override: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = ?2 AND calibration_type = ?3 AND is_manual_override = 1",
            params![link.source_id, &link.source_type, &link.calibration_type],
            |row| row.get(0),
        )?;

        if has_manual_override > 0 {
            // Skip - don't overwrite manual override with auto-find result
            println!("⚠️ Skipping auto-find for frame {} {} - manual override exists",
                link.source_id, link.calibration_type);
            return Ok(0);
        }
    }

    conn.execute(
        "INSERT INTO calibration_set_to_frames
         (source_id, source_type, calibration_set_id, calibration_type, matched_at, match_score, date_warning, temp_warning, is_manual_override)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(source_id, source_type, calibration_type) DO UPDATE SET
         calibration_set_id = excluded.calibration_set_id,
         match_score = excluded.match_score,
         date_warning = excluded.date_warning,
         temp_warning = excluded.temp_warning,
         matched_at = excluded.matched_at,
         is_manual_override = excluded.is_manual_override",
        params![
            link.source_id,
            &link.source_type,
            link.calibration_set_id,
            &link.calibration_type,
            &matched_at,
            link.match_score,
            date_warning,
            temp_warning,
            is_manual_override
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Get all calibration links for a specific frame
pub fn get_links_for_frame(conn: &Connection, frame_id: i64) -> Result<Vec<CalibrationLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, source_type, calibration_set_id, calibration_type,
                matched_at, match_score, date_warning, temp_warning, is_manual_override
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'
         ORDER BY calibration_type"
    )?;

    let links = stmt.query_map([frame_id], |row| {
        Ok(CalibrationLink {
            id: Some(row.get(0)?),
            source_id: row.get(1)?,
            source_type: row.get(2)?,
            calibration_set_id: row.get(3)?,
            calibration_type: row.get(4)?,
            matched_at: row.get(5)?,
            match_score: row.get(6)?,
            date_warning: row.get::<_, i32>(7)? == 1,
            temp_warning: row.get::<_, i32>(8)? == 1,
            is_manual_override: row.get::<_, i32>(9)? == 1,
        })
    })?;

    links.collect()
}

/// Get all calibration links for a specific calibration set
pub fn get_links_for_calibration_set(conn: &Connection, set_id: i64) -> Result<Vec<CalibrationLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, source_type, calibration_set_id, calibration_type,
                matched_at, match_score, date_warning, temp_warning, is_manual_override
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'
         ORDER BY calibration_type"
    )?;

    let links = stmt.query_map([set_id], |row| {
        Ok(CalibrationLink {
            id: Some(row.get(0)?),
            source_id: row.get(1)?,
            source_type: row.get(2)?,
            calibration_set_id: row.get(3)?,
            calibration_type: row.get(4)?,
            matched_at: row.get(5)?,
            match_score: row.get(6)?,
            date_warning: row.get::<_, i32>(7)? == 1,
            temp_warning: row.get::<_, i32>(8)? == 1,
            is_manual_override: row.get::<_, i32>(9)? == 1,
        })
    })?;

    links.collect()
}

/// Get sub-calibration details for a calibration set
/// Returns the linked sub-calibrations (e.g., Flat→Dark/Bias, Dark→Bias) with full set details
/// Warnings are calculated dynamically based on date and temperature differences
pub fn get_sub_calibration_details(conn: &Connection, set_id: i64) -> Result<Vec<SubCalibrationDetail>> {
    let links = get_links_for_calibration_set(conn, set_id)?;

    // Get parent set details for warning calculation
    let parent_set = get_calibration_set_detail(conn, set_id)?;

    // Load config for warning thresholds
    let config = load_config(conn);

    let mut sub_calibrations = Vec::new();

    for link in links {
        // Get the full details of the linked calibration set
        if let Ok(set_detail) = get_calibration_set_detail(conn, link.calibration_set_id) {
            // Calculate date warning dynamically
            let date_warning = calculate_sub_calibration_date_warning(
                &parent_set,
                &set_detail,
                &link.calibration_type,
                &config,
            );

            // Calculate temp warning dynamically
            let temp_warning = calculate_sub_calibration_temp_warning(
                &parent_set,
                &set_detail,
                &link.calibration_type,
                &config,
            );

            sub_calibrations.push(SubCalibrationDetail {
                calibration_type: link.calibration_type.clone(),
                set: set_detail,
                date_warning,
                temp_warning,
            });
        }
    }

    Ok(sub_calibrations)
}

/// Calculate date warning for sub-calibration
/// Returns true if the sub-calibration is significantly older than the parent
fn calculate_sub_calibration_date_warning(
    parent_set: &CalibrationSetDetail,
    sub_set: &CalibrationSetDetail,
    calibration_type: &str,
    config: &CalibrationMatchingConfig,
) -> bool {
    // Get the date warning threshold based on calibration type
    let threshold_days = match calibration_type {
        "Dark" => config.warnings.dark_date_warning_days,
        "DarkFlat" => config.warnings.darkflat_date_warning_days,
        "Bias" => config.warnings.dark_date_warning_days,  // Use dark threshold for bias
        _ => config.warnings.flat_date_warning_days,
    };

    // If threshold is 0, warnings are disabled
    if threshold_days == 0 {
        return false;
    }

    // Compare dates - date_start is a String (ISO 8601)
    let parent_date = if parent_set.date_start.len() >= 10 {
        NaiveDate::parse_from_str(&parent_set.date_start[..10], "%Y-%m-%d").ok()
    } else {
        None
    };
    let sub_date = if sub_set.date_start.len() >= 10 {
        NaiveDate::parse_from_str(&sub_set.date_start[..10], "%Y-%m-%d").ok()
    } else {
        None
    };

    match (parent_date, sub_date) {
        (Some(parent), Some(sub)) => {
            let diff = (parent - sub).num_days().abs();
            diff > threshold_days
        }
        _ => false,
    }
}

/// Calculate temperature warning for sub-calibration
/// Returns true if there's a significant temperature difference
fn calculate_sub_calibration_temp_warning(
    parent_set: &CalibrationSetDetail,
    sub_set: &CalibrationSetDetail,
    calibration_type: &str,
    config: &CalibrationMatchingConfig,
) -> bool {
    // Get source config based on parent set type
    let source_config = match parent_set.imagetyp {
        ImageType::Flat | ImageType::DarkFlat => &config.flats,
        ImageType::Dark => &config.darks,
        _ => &config.lights,
    };

    // Get calibration type config
    let cal_config = match calibration_type {
        "Flat" => source_config.flat.as_ref(),
        "Dark" => source_config.dark.as_ref(),
        "DarkFlat" => source_config.darkflat.as_ref(),
        "Bias" => source_config.bias.as_ref(),
        _ => None,
    };

    // Get the warning threshold from config
    let threshold = if let Some(cal_config) = cal_config {
        let temp_param = &cal_config.ccd_temp;
        // Only calculate warning if mode is "Warning"
        if temp_param.mode != MatchMode::Warning {
            return false;
        }
        // Use the configured threshold, default to 2.0°C
        temp_param.warning_threshold.unwrap_or(2.0)
    } else {
        return false;  // No calibration type config
    };

    // Calculate temperature difference (ccd_temp is f64, not Option)
    let diff = (parent_set.ccd_temp - sub_set.ccd_temp).abs();
    diff > threshold
}

/// Get calibration status for a specific frame
/// Warnings are calculated dynamically based on current config (not stored values)
pub fn get_frame_calibration_status(conn: &Connection, frame_id: i64) -> Result<FrameCalibrationStatus> {
    let links = get_links_for_frame(conn, frame_id)?;

    let mut status = FrameCalibrationStatus {
        frame_id,
        has_flats: false,
        has_darks: false,
        has_bias: false,
        has_darkflats: false,
        flats_warning: false,
        darks_warning: false,
        bias_warning: false,
        flat_set_id: None,
        dark_set_id: None,
        bias_set_id: None,
        darkflat_set_id: None,
    };

    // First pass: collect set IDs
    for link in &links {
        match link.calibration_type.as_str() {
            "Flat" => {
                status.has_flats = true;
                status.flat_set_id = Some(link.calibration_set_id);
            }
            "Dark" => {
                status.has_darks = true;
                status.dark_set_id = Some(link.calibration_set_id);
            }
            "Bias" => {
                status.has_bias = true;
                status.bias_set_id = Some(link.calibration_set_id);
            }
            "DarkFlat" => {
                status.has_darkflats = true;
                status.darkflat_set_id = Some(link.calibration_set_id);
            }
            _ => {}
        }
    }

    // Second pass: calculate warnings dynamically based on current config
    status.flats_warning = has_calibration_warning(conn, frame_id, status.flat_set_id, "Flat");
    status.darks_warning = has_calibration_warning(conn, frame_id, status.dark_set_id, "Dark");
    status.bias_warning = has_calibration_warning(conn, frame_id, status.bias_set_id, "Bias");

    Ok(status)
}

/// Delete all calibration links for frames in a specific frame set
#[allow(dead_code)]
pub fn delete_links_for_frame_set(conn: &Connection, frame_set_id: i64) -> Result<usize> {
    // First get all frame IDs in the frame set
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.id
         FROM frames f
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1"
    )?;

    let frame_ids: Vec<i64> = stmt.query_map([frame_set_id], |row| row.get(0))?
        .collect::<Result<Vec<i64>>>()?;

    if frame_ids.is_empty() {
        return Ok(0);
    }

    // Build placeholders for IN clause
    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let placeholders_str = placeholders.join(",");

    let delete_query = format!(
        "DELETE FROM calibration_set_to_frames
         WHERE source_id IN ({}) AND source_type = 'frame'",
        placeholders_str
    );

    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let deleted = conn.execute(&delete_query, params.as_slice())?;
    Ok(deleted)
}

/// Delete a specific calibration link
#[allow(dead_code)]
pub fn delete_calibration_link(conn: &Connection, link_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM calibration_set_to_frames WHERE id = ?1",
        [link_id],
    )?;
    Ok(())
}

/// Get calibration statistics for a frame set
pub fn get_calibration_statistics(conn: &Connection, frame_set_id: i64) -> Result<CalibrationStats> {
    // Get all frame IDs in the frame set
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.id
         FROM frames f
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'"
    )?;

    let frame_ids: Vec<i64> = stmt.query_map([frame_set_id], |row| row.get(0))?
        .collect::<Result<Vec<i64>>>()?;

    let total_frames = frame_ids.len();

    let mut frames_with_flats = 0;
    let mut frames_with_darks = 0;
    let mut frames_with_bias = 0;
    let mut frames_complete = 0;
    let mut frames_partial = 0;
    let mut frames_none = 0;
    let mut total_warnings = 0;

    for frame_id in frame_ids {
        let status = get_frame_calibration_status(conn, frame_id)?;

        if status.has_flats { frames_with_flats += 1; }
        if status.has_darks { frames_with_darks += 1; }
        if status.has_bias { frames_with_bias += 1; }

        if status.flats_warning || status.darks_warning || status.bias_warning {
            total_warnings += 1;
        }

        // Check if frame has complete calibration
        let has_any = status.has_flats || status.has_darks || status.has_bias;
        let has_complete = status.has_flats && (status.has_darks || status.has_bias);

        if has_complete {
            frames_complete += 1;
        } else if has_any {
            frames_partial += 1;
        } else {
            frames_none += 1;
        }
    }

    Ok(CalibrationStats {
        total_frames,
        frames_with_flats,
        frames_with_darks,
        frames_with_bias,
        frames_complete,
        frames_partial,
        frames_none,
        total_warnings,
    })
}

/// Get all frames that use a specific calibration set
#[allow(dead_code)]
pub fn get_frames_using_calibration_set(conn: &Connection, set_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT source_id
         FROM calibration_set_to_frames
         WHERE calibration_set_id = ?1 AND source_type = 'frame'
         ORDER BY source_id"
    )?;

    let frame_ids = stmt.query_map([set_id], |row| row.get(0))?;
    frame_ids.collect()
}

/// Check if a calibration link exists
#[allow(dead_code)]
pub fn link_exists(
    conn: &Connection,
    source_id: i64,
    source_type: &str,
    calibration_type: &str
) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = ?2 AND calibration_type = ?3",
        params![source_id, source_type, calibration_type],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get frames grouped by their calibration set combinations for a frame set
pub fn get_calibration_groups_for_frame_set(
    conn: &Connection,
    frame_set_id: i64
) -> Result<FrameSetCalibrationGroups> {
    // Step 1: Get all LIGHT frame IDs in the frame set
    // Navigate: frames_set → imaging_nights → sessions → session_members → frames
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY sm.frame_id"
    )?;

    let frame_ids: Vec<i64> = stmt
        .query_map([frame_set_id], |row| row.get(0))?
        .collect::<Result<Vec<i64>>>()?;

    let total_frames = frame_ids.len();

    // Step 2: For each frame, get its calibration set IDs
    type CalibKey = (Option<i64>, Option<i64>, Option<i64>); // (flat_set_id, dark_set_id, bias_set_id)
    let mut groups_map: HashMap<CalibKey, Vec<i64>> = HashMap::new();
    let mut uncalibrated_frames: Vec<i64> = Vec::new();

    for frame_id in &frame_ids {
        let mut flat_set_id: Option<i64> = None;
        let mut dark_set_id: Option<i64> = None;
        let mut bias_set_id: Option<i64> = None;

        // Get calibration links for this frame
        let mut link_stmt = conn.prepare(
            "SELECT calibration_type, calibration_set_id
             FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = 'frame'"
        )?;

        let links = link_stmt.query_map([frame_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        for link_result in links {
            let (cal_type, cal_set_id) = link_result?;
            match cal_type.as_str() {
                "Flat" => flat_set_id = Some(cal_set_id),
                "Dark" => dark_set_id = Some(cal_set_id),
                "Bias" => bias_set_id = Some(cal_set_id),
                _ => {}
            }
        }

        // Group by calibration combination
        let key = (flat_set_id, dark_set_id, bias_set_id);

        // Track uncalibrated frames (no calibration at all)
        if flat_set_id.is_none() && dark_set_id.is_none() && bias_set_id.is_none() {
            uncalibrated_frames.push(*frame_id);
        } else {
            groups_map.entry(key).or_insert_with(Vec::new).push(*frame_id);
        }
    }

    // Step 3: Build CalibrationGroup objects with full set details
    let mut groups: Vec<CalibrationGroup> = Vec::new();

    for ((flat_set_id, dark_set_id, bias_set_id), frame_ids_in_group) in groups_map {
        let flat_set_detail = if let Some(set_id) = flat_set_id {
            get_calibration_set_detail(conn, set_id).ok()
        } else {
            None
        };

        let dark_set_detail = if let Some(set_id) = dark_set_id {
            get_calibration_set_detail(conn, set_id).ok()
        } else {
            None
        };

        let bias_set_detail = if let Some(set_id) = bias_set_id {
            get_calibration_set_detail(conn, set_id).ok()
        } else {
            None
        };

        // Check if any frames in this group have warnings
        let has_warnings = check_group_warnings(conn, &frame_ids_in_group)?;

        // Collect detailed warnings for each calibration type
        let (flat_warnings, dark_warnings, bias_warnings) =
            get_calibration_warnings_for_group(conn, &frame_ids_in_group, flat_set_id, dark_set_id, bias_set_id)?;

        groups.push(CalibrationGroup {
            flat_set_id,
            dark_set_id,
            bias_set_id,
            flat_set_detail,
            dark_set_detail,
            bias_set_detail,
            frame_count: frame_ids_in_group.len(),
            frame_ids: frame_ids_in_group,
            has_warnings,
            flat_warnings,
            dark_warnings,
            bias_warnings,
        });
    }

    // Sort groups by frame count (largest first)
    groups.sort_by(|a, b| b.frame_count.cmp(&a.frame_count));

    Ok(FrameSetCalibrationGroups {
        groups,
        uncalibrated_frame_count: uncalibrated_frames.len(),
        uncalibrated_frame_ids: uncalibrated_frames,
        total_frames,
    })
}

/// Helper: Get detailed info for a calibration set
fn get_calibration_set_detail(conn: &Connection, set_id: i64) -> Result<CalibrationSetDetail> {
    let mut stmt = conn.prepare(
        "SELECT id, imagetyp, exptime, ccd_temp, temp_min, temp_max, gain, offset,
                binning, instrume, filter, date_start, date_end, date, frame_count
         FROM calibration_set
         WHERE id = ?1"
    )?;

    stmt.query_row([set_id], |row| {
        let imagetyp_str: String = row.get(1)?;
        let imagetyp = ImageType::from_str(&imagetyp_str).unwrap_or(ImageType::Light);

        Ok(CalibrationSetDetail {
            id: Some(row.get(0)?),
            imagetyp,
            exptime: row.get(2)?,
            ccd_temp: row.get(3)?,
            temp_min: row.get(4)?,
            temp_max: row.get(5)?,
            gain: row.get(6)?,
            offset: row.get(7)?,
            binning: row.get(8)?,
            instrume: row.get(9)?,
            filter: row.get(10)?,
            date_start: row.get(11)?,
            date_end: row.get(12)?,
            date_display: row.get(13)?,
            frame_count: row.get(14)?,
        })
    })
}

/// Helper: Check if temperature warnings are enabled for a given calibration path
fn is_temp_warning_enabled(config: &CalibrationMatchingConfig, cal_type: &str) -> bool {
    match cal_type {
        "Flat" => {
            config.lights.flat.as_ref()
                .map(|c| c.ccd_temp.mode == MatchMode::Warning)
                .unwrap_or(false)
        }
        "Dark" => {
            config.lights.dark.as_ref()
                .map(|c| c.ccd_temp.mode == MatchMode::Warning)
                .unwrap_or(false)
        }
        "Bias" => {
            config.lights.bias.as_ref()
                .map(|c| c.ccd_temp.mode == MatchMode::Warning)
                .unwrap_or(false)
        }
        "DarkFlat" => {
            config.flats.darkflat.as_ref()
                .map(|c| c.ccd_temp.mode == MatchMode::Warning)
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Helper: Check if date warnings are enabled (threshold > 0 and reasonable)
fn is_date_warning_enabled(config: &CalibrationMatchingConfig, cal_type: &str) -> bool {
    match cal_type {
        "Flat" => {
            let threshold = config.warnings.flat_date_warning_days;
            threshold > 0 && threshold < 10000  // Reasonable threshold
        }
        "Dark" => {
            let threshold = config.warnings.dark_date_warning_days;
            threshold > 0 && threshold < 10000
        }
        "Bias" => {
            // Bias warnings typically use dark threshold
            let threshold = config.warnings.dark_date_warning_days;
            threshold > 0 && threshold < 10000
        }
        "DarkFlat" => {
            let threshold = config.warnings.darkflat_date_warning_days;
            threshold > 0 && threshold < 10000
        }
        _ => false,
    }
}

// ============================================================================
// Dynamic Warning Calculation Functions
// ============================================================================

/// Get frame temperature from database
fn get_frame_temp(conn: &Connection, frame_id: i64) -> Option<f64> {
    conn.query_row(
        "SELECT ccd_temp FROM frames WHERE id = ?1",
        [frame_id],
        |row| row.get(0),
    ).ok()
}

/// Get frame date from database
fn get_frame_date(conn: &Connection, frame_id: i64) -> Option<NaiveDate> {
    conn.query_row(
        "SELECT date(date_obs) FROM frames WHERE id = ?1",
        [frame_id],
        |row| {
            let date_str: Option<String> = row.get(0)?;
            Ok(date_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()))
        },
    ).ok().flatten()
}

/// Get calibration set average temperature
fn get_calibration_set_temp(conn: &Connection, set_id: i64) -> Option<f64> {
    // Use the average temperature of frames in the calibration set
    conn.query_row(
        "SELECT AVG(f.ccd_temp)
         FROM frames f
         JOIN calibration_set_frames csf ON f.id = csf.frame_id
         WHERE csf.set_id = ?1",
        [set_id],
        |row| row.get(0),
    ).ok()
}

/// Get calibration set date (average date of frames)
fn get_calibration_set_date(conn: &Connection, set_id: i64) -> Option<NaiveDate> {
    // Get the middle date of the calibration set frames
    conn.query_row(
        "SELECT date(MIN(date_obs)) FROM frames f
         JOIN calibration_set_frames csf ON f.id = csf.frame_id
         WHERE csf.set_id = ?1",
        [set_id],
        |row| {
            let date_str: Option<String> = row.get(0)?;
            Ok(date_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()))
        },
    ).ok().flatten()
}

/// Get temperature warning threshold from config for a specific calibration path
fn get_temp_warning_threshold(config: &CalibrationMatchingConfig, cal_type: &str) -> Option<f64> {
    match cal_type {
        "Flat" => config.lights.flat.as_ref().and_then(|c| c.ccd_temp.warning_threshold),
        "Dark" => config.lights.dark.as_ref().and_then(|c| c.ccd_temp.warning_threshold),
        "Bias" => config.lights.bias.as_ref().and_then(|c| c.ccd_temp.warning_threshold),
        "DarkFlat" => config.flats.darkflat.as_ref().and_then(|c| c.ccd_temp.warning_threshold),
        _ => None,
    }
}

/// Get date warning threshold from config for a specific calibration type
fn get_date_warning_threshold(config: &CalibrationMatchingConfig, cal_type: &str) -> i64 {
    match cal_type {
        "Flat" => config.warnings.flat_date_warning_days,
        "Dark" => config.warnings.dark_date_warning_days,
        "Bias" => config.warnings.dark_date_warning_days,  // Use dark threshold for bias
        "DarkFlat" => config.warnings.darkflat_date_warning_days,
        _ => 0,
    }
}

/// Calculate if there should be a temperature warning for a frame-to-calibration-set link
/// Returns true if mode is "Warning" AND temp diff exceeds threshold
fn calculate_temp_warning(
    conn: &Connection,
    frame_id: i64,
    set_id: i64,
    cal_type: &str,
    config: &CalibrationMatchingConfig,
) -> (bool, Option<f64>) {
    // Check if mode is "Warning" (not Ignore or Exact)
    let temp_mode_enabled = is_temp_warning_enabled(config, cal_type);
    if !temp_mode_enabled {
        println!("  🔇 Temp warning DISABLED by config for {} (mode != Warning)", cal_type);
        return (false, None);
    }

    let frame_temp = match get_frame_temp(conn, frame_id) {
        Some(t) => t,
        None => {
            println!("  ⚠️ Frame {} has NO ccd_temp data", frame_id);
            return (false, None);
        }
    };

    let set_temp = match get_calibration_set_temp(conn, set_id) {
        Some(t) => t,
        None => {
            println!("  ⚠️ Cal set {} has NO ccd_temp data", set_id);
            return (false, None);
        }
    };

    let temp_diff = (frame_temp - set_temp).abs();

    // Only warn if threshold is explicitly set in the config
    let threshold = match get_temp_warning_threshold(config, cal_type) {
        Some(t) => t,
        None => {
            println!("  🔇 Temp warning DISABLED for {} (no threshold set)", cal_type);
            return (false, Some(temp_diff));
        }
    };

    let has_warning = temp_diff > threshold;
    println!("  🌡️ Temp check for {} set {}: frame={:.1}°C, set={:.1}°C, diff={:.1}°C, threshold={:.1}°C -> warning={}",
        cal_type, set_id, frame_temp, set_temp, temp_diff, threshold, has_warning);

    (has_warning, Some(temp_diff))
}

/// Calculate if there should be a date warning for a frame-to-calibration-set link
/// Returns true if date diff exceeds threshold and warnings are enabled
fn calculate_date_warning(
    conn: &Connection,
    frame_id: i64,
    set_id: i64,
    cal_type: &str,
    config: &CalibrationMatchingConfig,
) -> (bool, Option<i64>) {
    let date_warning_enabled = is_date_warning_enabled(config, cal_type);
    if !date_warning_enabled {
        println!("  🔇 Date warning DISABLED by config for {} (threshold=0 or >10000)", cal_type);
        return (false, None);
    }

    let frame_date = match get_frame_date(conn, frame_id) {
        Some(d) => d,
        None => {
            println!("  ⚠️ Frame {} has NO date_obs data", frame_id);
            return (false, None);
        }
    };

    let set_date = match get_calibration_set_date(conn, set_id) {
        Some(d) => d,
        None => {
            println!("  ⚠️ Cal set {} has NO date data", set_id);
            return (false, None);
        }
    };

    let date_diff = (frame_date - set_date).num_days().abs();
    let threshold = get_date_warning_threshold(config, cal_type);

    let has_warning = date_diff > threshold;
    println!("  📅 Date check for {} set {}: diff={} days, threshold={} days -> warning={}",
        cal_type, set_id, date_diff, threshold, has_warning);

    (has_warning, Some(date_diff))
}

/// Calculate calibration warnings dynamically for a frame-to-set link
/// This is the main function for dynamic warning calculation
pub fn calculate_calibration_warnings(
    conn: &Connection,
    frame_id: i64,
    set_id: i64,
    cal_type: &str,
) -> Vec<CalibrationWarning> {
    println!("📊 calculate_calibration_warnings: frame {} -> {} set {}", frame_id, cal_type, set_id);
    let config = load_config(conn);
    let mut warnings = Vec::new();

    // Check temperature warning
    let (has_temp_warning, temp_diff) = calculate_temp_warning(conn, frame_id, set_id, cal_type, &config);
    if has_temp_warning {
        if let Some(diff) = temp_diff {
            warnings.push(CalibrationWarning {
                warning_type: "temperature".to_string(),
                message: format!("{} temperature differs by {:.1}°C", cal_type, diff),
                calibration_type: cal_type.to_string(),
                set_id,
            });
        }
    }

    // Check date warning
    let (has_date_warning, date_diff) = calculate_date_warning(conn, frame_id, set_id, cal_type, &config);
    if has_date_warning {
        if let Some(diff) = date_diff {
            let threshold = get_date_warning_threshold(&config, cal_type);
            warnings.push(CalibrationWarning {
                warning_type: "date".to_string(),
                message: format!("{} calibration is {} days old (>{} days recommended)", cal_type, diff, threshold),
                calibration_type: cal_type.to_string(),
                set_id,
            });
        }
    }

    println!("  ✅ Generated {} warnings for frame {} -> {} set {}", warnings.len(), frame_id, cal_type, set_id);
    warnings
}

/// Check if a frame has any warnings for a specific calibration set (used for status flags)
fn has_calibration_warning(
    conn: &Connection,
    frame_id: i64,
    set_id: Option<i64>,
    cal_type: &str,
) -> bool {
    match set_id {
        Some(id) => !calculate_calibration_warnings(conn, frame_id, id, cal_type).is_empty(),
        None => false,
    }
}

/// Helper: Collect detailed calibration warnings for a group
/// Warnings are calculated dynamically based on current config (not stored values)
fn get_calibration_warnings_for_group(
    conn: &Connection,
    frame_ids: &[i64],
    flat_set_id: Option<i64>,
    dark_set_id: Option<i64>,
    bias_set_id: Option<i64>,
) -> Result<(Vec<CalibrationWarning>, Vec<CalibrationWarning>, Vec<CalibrationWarning>)> {
    let mut flat_warnings = Vec::new();
    let mut dark_warnings = Vec::new();
    let mut bias_warnings = Vec::new();

    if frame_ids.is_empty() {
        return Ok((flat_warnings, dark_warnings, bias_warnings));
    }

    // Calculate warnings dynamically for each calibration type
    // Check flat warnings
    if let Some(set_id) = flat_set_id {
        for &frame_id in frame_ids {
            let warnings = calculate_calibration_warnings(conn, frame_id, set_id, "Flat");
            for warning in warnings {
                if !flat_warnings.iter().any(|w| w.warning_type == warning.warning_type) {
                    flat_warnings.push(warning);
                }
            }
            if flat_warnings.len() >= 2 {
                break;
            }
        }
    }

    // Check dark warnings
    if let Some(set_id) = dark_set_id {
        for &frame_id in frame_ids {
            let warnings = calculate_calibration_warnings(conn, frame_id, set_id, "Dark");
            for warning in warnings {
                if !dark_warnings.iter().any(|w| w.warning_type == warning.warning_type) {
                    dark_warnings.push(warning);
                }
            }
            if dark_warnings.len() >= 2 {
                break;
            }
        }
    }

    // Check bias warnings
    if let Some(set_id) = bias_set_id {
        for &frame_id in frame_ids {
            let warnings = calculate_calibration_warnings(conn, frame_id, set_id, "Bias");
            for warning in warnings {
                if !bias_warnings.iter().any(|w| w.warning_type == warning.warning_type) {
                    bias_warnings.push(warning);
                }
            }
            if bias_warnings.len() >= 2 {
                break;
            }
        }
    }

    Ok((flat_warnings, dark_warnings, bias_warnings))
}

/// Helper: Get warnings for a specific calibration set and its associated frames
/// Warnings are calculated dynamically based on current config (not stored values)
fn get_warnings_for_calibration_set(
    conn: &Connection,
    set_id: i64,
    frame_ids: &[i64],
    calibration_type: &str,
) -> Vec<CalibrationWarning> {
    let mut warnings = Vec::new();

    if frame_ids.is_empty() {
        return warnings;
    }

    // Calculate warnings dynamically for each frame
    // We only need to find ONE frame that triggers a warning for this set
    for &frame_id in frame_ids {
        let frame_warnings = calculate_calibration_warnings(conn, frame_id, set_id, calibration_type);
        for warning in frame_warnings {
            // Check if we already have this warning type
            if !warnings.iter().any(|w| w.warning_type == warning.warning_type) {
                warnings.push(warning);
            }
        }
        // Once we have both warning types, we can stop checking
        if warnings.len() >= 2 {
            break;
        }
    }

    warnings
}

/// Helper: Check if any frames in the group have calibration warnings
/// Warnings are calculated dynamically based on current config (not stored values)
fn check_group_warnings(conn: &Connection, frame_ids: &[i64]) -> Result<bool> {
    if frame_ids.is_empty() {
        return Ok(false);
    }

    // Check each frame for any warnings using dynamic calculation
    for &frame_id in frame_ids {
        let status = get_frame_calibration_status(conn, frame_id)?;
        if status.flats_warning || status.darks_warning || status.bias_warning {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Get calibration hierarchy organized by Date → Camera → Filter for a frame set
pub fn get_calibration_hierarchy_for_frame_set(
    conn: &Connection,
    frame_set_id: i64
) -> Result<CalibrationHierarchyView> {
    // Step 1: Get all LIGHT frames in the frame set with metadata
    let mut stmt = conn.prepare(
        "SELECT
            f.id,
            f.date_obs,
            f.instrume,
            f.filter,
            f.exptime,
            fi.filename,
            fi.path,
            f.telescop,
            f.focallen,
            f.binning,
            DATE(f.date_obs) as session_date,
            f.ccd_temp,
            f.swcreate
         FROM frames f
         JOIN files fi ON f.file_id = fi.id
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY DATE(f.date_obs) DESC, f.instrume, COALESCE(f.filter, ''), f.date_obs"
    )?;

    // Collect raw frame data
    struct RawFrame {
        id: i64,
        date_obs: Option<String>,
        instrume: Option<String>,
        filter: Option<String>,
        exptime: Option<f64>,
        filename: String,
        file_path: String,
        telescop: Option<String>,
        focallen: Option<f64>,
        binning: Option<String>,
        session_date: Option<String>,
        ccd_temp: Option<f64>,
        swcreate: Option<String>,
    }

    let frames: Vec<RawFrame> = stmt
        .query_map([frame_set_id], |row| {
            Ok(RawFrame {
                id: row.get(0)?,
                date_obs: row.get(1)?,
                instrume: row.get(2)?,
                filter: row.get(3)?,
                exptime: row.get(4)?,
                filename: row.get(5)?,
                file_path: row.get(6)?,
                telescop: row.get(7)?,
                focallen: row.get(8)?,
                binning: row.get(9)?,
                session_date: row.get(10)?,
                ccd_temp: row.get(11)?,
                swcreate: row.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;

    let total_frames = frames.len();

    // Step 2: Build hierarchy using nested HashMaps
    // date -> camera -> filter -> frames
    type FilterMap = HashMap<Option<String>, Vec<RawFrame>>;
    type CameraMap = HashMap<String, FilterMap>;
    type DateMap = HashMap<String, CameraMap>;

    let mut date_map: DateMap = HashMap::new();

    for frame in frames {
        let date_key = frame.session_date.clone().unwrap_or_else(|| "Unknown Date".to_string());
        let camera_key = frame.instrume.clone().unwrap_or_else(|| "Unknown Camera".to_string());
        let filter_key = frame.filter.clone();

        date_map
            .entry(date_key)
            .or_insert_with(HashMap::new)
            .entry(camera_key)
            .or_insert_with(HashMap::new)
            .entry(filter_key)
            .or_insert_with(Vec::new)
            .push(frame);
    }

    // Step 3: Build CalibrationHierarchyView from the nested maps
    let mut calibrated_frames = 0;
    let mut uncalibrated_frames = 0;
    let mut date_groups: Vec<CalibrationDateGroup> = Vec::new();

    // Sort dates in descending order (most recent first)
    let mut dates: Vec<&String> = date_map.keys().collect();
    dates.sort_by(|a, b| b.cmp(a));

    for date_str in dates {
        let camera_map = &date_map[date_str];
        let mut camera_groups: Vec<CalibrationCameraGroup> = Vec::new();
        let mut date_frame_count = 0;
        let mut date_has_warnings = false;

        // Sort cameras alphabetically
        let mut cameras: Vec<&String> = camera_map.keys().collect();
        cameras.sort();

        for camera in cameras {
            let filter_map = &camera_map[camera];
            let mut filter_groups: Vec<CalibrationFilterGroup> = Vec::new();
            let mut camera_frame_count = 0;
            let mut camera_has_warnings = false;

            // Sort filters (None/"No Filter" first, then alphabetically)
            let mut filters: Vec<&Option<String>> = filter_map.keys().collect();
            filters.sort_by(|a, b| {
                match (a, b) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    (Some(a), Some(b)) => a.cmp(b),
                }
            });

            for filter in filters {
                let raw_frames = &filter_map[filter];

                // Check if we need to split by exposure time
                // Convert to centiseconds (i64) for hashing, then back to f64
                let unique_exptime_keys: std::collections::HashSet<Option<i64>> = raw_frames
                    .iter()
                    .map(|f| f.exptime.map(|e| (e * 10.0).round() as i64)) // Round to 0.1s as centiseconds
                    .collect();

                // Determine if we need to split: more than 1 unique non-null exptime
                let non_null_count = unique_exptime_keys.iter().filter(|e| e.is_some()).count();
                let should_split = non_null_count > 1;

                // Group frames by exptime if needed
                let exptime_groups: Vec<(Option<f64>, Vec<&RawFrame>)> = if should_split {
                    // Split by exptime
                    let mut groups: HashMap<Option<i64>, Vec<&RawFrame>> = HashMap::new();
                    for frame in raw_frames {
                        // Use rounded exptime as key (in centiseconds to avoid float key issues)
                        let key = frame.exptime.map(|e| (e * 10.0).round() as i64);
                        groups.entry(key).or_default().push(frame);
                    }
                    let mut result: Vec<(Option<f64>, Vec<&RawFrame>)> = groups
                        .into_iter()
                        .map(|(key, frames)| {
                            let exptime = key.map(|k| k as f64 / 10.0);
                            (exptime, frames)
                        })
                        .collect();
                    // Sort by exptime
                    result.sort_by(|a, b| {
                        match (&a.0, &b.0) {
                            (None, None) => std::cmp::Ordering::Equal,
                            (None, Some(_)) => std::cmp::Ordering::Less,
                            (Some(_), None) => std::cmp::Ordering::Greater,
                            (Some(a), Some(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
                        }
                    });
                    result
                } else {
                    // No split needed - single group with all frames
                    vec![(None, raw_frames.iter().collect())]
                };

                // Process each exptime sub-group
                for (group_exptime, group_frames) in exptime_groups {
                    let group_frame_count = group_frames.len();
                    camera_frame_count += group_frame_count;

                    // Get calibration status for all frames in this group
                    let mut light_frames: Vec<LightFrameWithCalibration> = Vec::new();
                    let mut group_has_warnings = false;

                    // Collect ALL unique calibration set IDs with their frame lists
                    let mut flat_set_frames: HashMap<i64, Vec<i64>> = HashMap::new();
                    let mut dark_set_frames: HashMap<i64, Vec<i64>> = HashMap::new();
                    let mut bias_set_frames: HashMap<i64, Vec<i64>> = HashMap::new();

                    for raw_frame in &group_frames {
                        let status = get_frame_calibration_status(conn, raw_frame.id)?;

                        // Track if we have calibration
                        let has_any_calibration = status.has_flats || status.has_darks || status.has_bias;
                        if has_any_calibration {
                            calibrated_frames += 1;
                        } else {
                            uncalibrated_frames += 1;
                        }

                        // Track warnings
                        if status.flats_warning || status.darks_warning || status.bias_warning {
                            group_has_warnings = true;
                        }

                        // Collect ALL unique calibration set IDs (not just the first one)
                        if let Some(set_id) = status.flat_set_id {
                            flat_set_frames.entry(set_id).or_default().push(raw_frame.id);
                        }
                        if let Some(set_id) = status.dark_set_id {
                            dark_set_frames.entry(set_id).or_default().push(raw_frame.id);
                        }
                        if let Some(set_id) = status.bias_set_id {
                            bias_set_frames.entry(set_id).or_default().push(raw_frame.id);
                        }

                        light_frames.push(LightFrameWithCalibration {
                            frame_id: raw_frame.id,
                            filename: raw_frame.filename.clone(),
                            file_path: raw_frame.file_path.clone(),
                            date_obs: raw_frame.date_obs.clone(),
                            exptime: raw_frame.exptime,
                            telescop: raw_frame.telescop.clone(),
                            focallen: raw_frame.focallen,
                            binning: raw_frame.binning.clone(),
                            ccd_temp: raw_frame.ccd_temp,
                            swcreate: raw_frame.swcreate.clone(),
                            calibration_status: status,
                        });
                    }

                    // Build CalibrationSetWithFrameCount for each unique flat set
                    let flat_sets: Vec<CalibrationSetWithFrameCount> = flat_set_frames
                        .into_iter()
                        .filter_map(|(set_id, frame_ids)| {
                            let set = get_calibration_set_detail(conn, set_id).ok()?;
                            let warnings = get_warnings_for_calibration_set(conn, set_id, &frame_ids, "Flat");
                            let sub_calibration = get_sub_calibration_details(conn, set_id).unwrap_or_default();
                            Some(CalibrationSetWithFrameCount {
                                set,
                                frame_count: frame_ids.len() as i64,
                                frame_ids,
                                warnings,
                                sub_calibration,
                            })
                        })
                        .collect();

                    // Build CalibrationSetWithFrameCount for each unique dark set
                    let dark_sets: Vec<CalibrationSetWithFrameCount> = dark_set_frames
                        .into_iter()
                        .filter_map(|(set_id, frame_ids)| {
                            let set = get_calibration_set_detail(conn, set_id).ok()?;
                            let warnings = get_warnings_for_calibration_set(conn, set_id, &frame_ids, "Dark");
                            let sub_calibration = get_sub_calibration_details(conn, set_id).unwrap_or_default();
                            Some(CalibrationSetWithFrameCount {
                                set,
                                frame_count: frame_ids.len() as i64,
                                frame_ids,
                                warnings,
                                sub_calibration,
                            })
                        })
                        .collect();

                    // Build CalibrationSetWithFrameCount for each unique bias set
                    // Bias sets don't have sub-calibrations
                    let bias_sets: Vec<CalibrationSetWithFrameCount> = bias_set_frames
                        .into_iter()
                        .filter_map(|(set_id, frame_ids)| {
                            let set = get_calibration_set_detail(conn, set_id).ok()?;
                            let warnings = get_warnings_for_calibration_set(conn, set_id, &frame_ids, "Bias");
                            Some(CalibrationSetWithFrameCount {
                                set,
                                frame_count: frame_ids.len() as i64,
                                frame_ids,
                                warnings,
                                sub_calibration: Vec::new(),  // Bias sets don't have sub-calibrations
                            })
                        })
                        .collect();

                    if group_has_warnings {
                        camera_has_warnings = true;
                    }

                    // Build filter display string - include exptime if split
                    let base_filter_display = filter.clone().unwrap_or_else(|| "No Filter".to_string());
                    let filter_display = if should_split {
                        match group_exptime {
                            Some(exp) => format!("{} ({}s)", base_filter_display, exp),
                            None => format!("{} (unknown exp)", base_filter_display),
                        }
                    } else {
                        base_filter_display
                    };

                    filter_groups.push(CalibrationFilterGroup {
                        filter: filter.clone(),
                        filter_display,
                        exptime: group_exptime,
                        light_frames,
                        flat_sets,
                        dark_sets,
                        bias_sets,
                        has_warnings: group_has_warnings,
                        frame_count: group_frame_count,
                    });
                }
            }

            if camera_has_warnings {
                date_has_warnings = true;
            }

            date_frame_count += camera_frame_count;

            camera_groups.push(CalibrationCameraGroup {
                instrume: camera.clone(),
                filter_groups,
                frame_count: camera_frame_count,
                has_warnings: camera_has_warnings,
            });
        }

        // Format date display
        let date_display = format_date_display(date_str);

        date_groups.push(CalibrationDateGroup {
            date: date_str.clone(),
            date_display,
            camera_groups,
            frame_count: date_frame_count,
            has_warnings: date_has_warnings,
        });
    }

    Ok(CalibrationHierarchyView {
        date_groups,
        total_frames,
        calibrated_frames,
        uncalibrated_frames,
    })
}

/// Helper: Format date string to human-readable display
fn format_date_display(date_str: &str) -> String {
    // Try to parse as YYYY-MM-DD and format nicely
    if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        date.format("%B %d, %Y").to_string()
    } else {
        date_str.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use chrono::Utc;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_insert_and_get_link() {
        let conn = create_test_db();

        let link = CalibrationLink {
            id: None,
            source_id: 1,
            source_type: "frame".to_string(),
            calibration_set_id: 10,
            calibration_type: "Dark".to_string(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(0.95),
            date_warning: false,
            temp_warning: false,
            is_manual_override: false,
        };

        let link_id = insert_calibration_link(&conn, &link).unwrap();
        assert!(link_id > 0);

        let links = get_links_for_frame(&conn, 1).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].calibration_type, "Dark");
    }

    #[test]
    fn test_link_upsert() {
        let conn = create_test_db();

        let link1 = CalibrationLink {
            id: None,
            source_id: 1,
            source_type: "frame".to_string(),
            calibration_set_id: 10,
            calibration_type: "Dark".to_string(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(0.95),
            date_warning: false,
            temp_warning: false,
            is_manual_override: false,
        };

        insert_calibration_link(&conn, &link1).unwrap();

        // Insert again with different set ID - should update
        let link2 = CalibrationLink {
            calibration_set_id: 20,
            ..link1
        };

        insert_calibration_link(&conn, &link2).unwrap();

        let links = get_links_for_frame(&conn, 1).unwrap();
        assert_eq!(links.len(), 1);  // Still only one link
        assert_eq!(links[0].calibration_set_id, 20);  // Updated set ID
    }

    #[test]
    fn test_link_exists() {
        let conn = create_test_db();

        let link = CalibrationLink {
            id: None,
            source_id: 1,
            source_type: "frame".to_string(),
            calibration_set_id: 10,
            calibration_type: "Dark".to_string(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(0.95),
            date_warning: false,
            temp_warning: false,
            is_manual_override: false,
        };

        assert!(!link_exists(&conn, 1, "frame", "Dark").unwrap());
        insert_calibration_link(&conn, &link).unwrap();
        assert!(link_exists(&conn, 1, "frame", "Dark").unwrap());
    }
}
