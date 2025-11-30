use rusqlite::{Connection, Result, params};
use crate::models::{
    CalibrationLink, CalibrationStats, FrameCalibrationStatus, CalibrationGroup,
    FrameSetCalibrationGroups, CalibrationSetDetail, ImageType, CalibrationWarning,
    CalibrationHierarchyView, CalibrationDateGroup, CalibrationCameraGroup,
    CalibrationFilterGroup, LightFrameWithCalibration,
};
use crate::calibration::configurable_matcher::load_config;
use crate::calibration::config::{MatchMode, CalibrationMatchingConfig};
use std::collections::HashMap;
use chrono::NaiveDate;

/// Insert a new calibration link
pub fn insert_calibration_link(conn: &Connection, link: &CalibrationLink) -> Result<i64> {
    let matched_at = link.matched_at.clone();
    let date_warning = if link.date_warning { 1 } else { 0 };
    let temp_warning = if link.temp_warning { 1 } else { 0 };

    conn.execute(
        "INSERT INTO calibration_set_to_frames
         (source_id, source_type, calibration_set_id, calibration_type, matched_at, match_score, date_warning, temp_warning)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(source_id, source_type, calibration_type) DO UPDATE SET
         calibration_set_id = excluded.calibration_set_id,
         match_score = excluded.match_score,
         date_warning = excluded.date_warning,
         temp_warning = excluded.temp_warning,
         matched_at = excluded.matched_at",
        params![
            link.source_id,
            &link.source_type,
            link.calibration_set_id,
            &link.calibration_type,
            &matched_at,
            link.match_score,
            date_warning,
            temp_warning
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Get all calibration links for a specific frame
pub fn get_links_for_frame(conn: &Connection, frame_id: i64) -> Result<Vec<CalibrationLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, source_type, calibration_set_id, calibration_type,
                matched_at, match_score, date_warning, temp_warning
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
        })
    })?;

    links.collect()
}

/// Get all calibration links for a specific calibration set
pub fn get_links_for_calibration_set(conn: &Connection, set_id: i64) -> Result<Vec<CalibrationLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, source_type, calibration_set_id, calibration_type,
                matched_at, match_score, date_warning, temp_warning
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
        })
    })?;

    links.collect()
}

/// Get calibration status for a specific frame
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

    for link in links {
        match link.calibration_type.as_str() {
            "Flat" => {
                status.has_flats = true;
                status.flats_warning = link.date_warning || link.temp_warning;
                status.flat_set_id = Some(link.calibration_set_id);
            }
            "Dark" => {
                status.has_darks = true;
                status.darks_warning = link.date_warning || link.temp_warning;
                status.dark_set_id = Some(link.calibration_set_id);
            }
            "Bias" => {
                status.has_bias = true;
                status.bias_warning = link.date_warning || link.temp_warning;
                status.bias_set_id = Some(link.calibration_set_id);
            }
            "DarkFlat" => {
                status.has_darkflats = true;
                status.darkflat_set_id = Some(link.calibration_set_id);
            }
            _ => {}
        }
    }

    Ok(status)
}

/// Delete all calibration links for frames in a specific frame set
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

/// Helper: Collect detailed calibration warnings for a group
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

    // Load current configuration to check if warnings are enabled
    let config = load_config(conn);

    // Query for calibration links with warnings
    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let query = format!(
        "SELECT calibration_type, date_warning, temp_warning, calibration_set_id
         FROM calibration_set_to_frames
         WHERE source_id IN ({}) AND source_type = 'frame'
         AND (date_warning = 1 OR temp_warning = 1)",
        placeholders.join(",")
    );

    let mut stmt = conn.prepare(&query)?;
    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

    let warnings_data: Vec<(String, bool, bool, i64)> = stmt
        .query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,  // calibration_type
                row.get::<_, i64>(1)? == 1,  // date_warning
                row.get::<_, i64>(2)? == 1,  // temp_warning
                row.get::<_, i64>(3)?,  // calibration_set_id
            ))
        })?
        .collect::<Result<Vec<_>>>()?;

    // Group warnings by calibration type and build warning messages
    for (cal_type, has_date_warning, has_temp_warning, set_id) in warnings_data {
        let warnings_vec = match cal_type.as_str() {
            "Flat" => &mut flat_warnings,
            "Dark" => &mut dark_warnings,
            "Bias" => &mut bias_warnings,
            _ => continue,
        };

        // Create contextual warning messages ONLY if enabled in current config
        if has_temp_warning && is_temp_warning_enabled(&config, &cal_type) {
            warnings_vec.push(CalibrationWarning {
                warning_type: "temperature".to_string(),
                message: format!("{} temperature for light calibration differs significantly", cal_type),
                calibration_type: cal_type.clone(),
                set_id,
            });
        }

        if has_date_warning && is_date_warning_enabled(&config, &cal_type) {
            warnings_vec.push(CalibrationWarning {
                warning_type: "date".to_string(),
                message: format!("{} calibration may be outdated", cal_type),
                calibration_type: cal_type.clone(),
                set_id,
            });
        }
    }

    // Deduplicate warnings (same set_id + warning_type)
    flat_warnings.dedup_by(|a, b| a.set_id == b.set_id && a.warning_type == b.warning_type);
    dark_warnings.dedup_by(|a, b| a.set_id == b.set_id && a.warning_type == b.warning_type);
    bias_warnings.dedup_by(|a, b| a.set_id == b.set_id && a.warning_type == b.warning_type);

    Ok((flat_warnings, dark_warnings, bias_warnings))
}

/// Helper: Check if any frames in the group have calibration warnings
fn check_group_warnings(conn: &Connection, frame_ids: &[i64]) -> Result<bool> {
    if frame_ids.is_empty() {
        return Ok(false);
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let query = format!(
        "SELECT COUNT(*) FROM calibration_set_to_frames
         WHERE source_id IN ({}) AND source_type = 'frame'
         AND (date_warning = 1 OR temp_warning = 1)",
        placeholders.join(",")
    );

    let mut stmt = conn.prepare(&query)?;
    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

    let count: i64 = stmt.query_row(params.as_slice(), |row| row.get(0))?;
    Ok(count > 0)
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
            DATE(f.date_obs) as session_date
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
        session_date: Option<String>,
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
                session_date: row.get(6)?,
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
                let filter_frame_count = raw_frames.len();
                camera_frame_count += filter_frame_count;

                // Get calibration status for all frames in this filter group
                let frame_ids: Vec<i64> = raw_frames.iter().map(|f| f.id).collect();
                let mut light_frames: Vec<LightFrameWithCalibration> = Vec::new();
                let mut filter_has_warnings = false;

                // Collect calibration set IDs for the group
                let mut flat_set_id: Option<i64> = None;
                let mut dark_set_id: Option<i64> = None;
                let mut bias_set_id: Option<i64> = None;

                for raw_frame in raw_frames {
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
                        filter_has_warnings = true;
                    }

                    // Capture calibration set IDs (use first frame's sets as representative)
                    if flat_set_id.is_none() && status.flat_set_id.is_some() {
                        flat_set_id = status.flat_set_id;
                    }
                    if dark_set_id.is_none() && status.dark_set_id.is_some() {
                        dark_set_id = status.dark_set_id;
                    }
                    if bias_set_id.is_none() && status.bias_set_id.is_some() {
                        bias_set_id = status.bias_set_id;
                    }

                    light_frames.push(LightFrameWithCalibration {
                        frame_id: raw_frame.id,
                        filename: raw_frame.filename.clone(),
                        date_obs: raw_frame.date_obs.clone(),
                        exptime: raw_frame.exptime,
                        calibration_status: status,
                    });
                }

                // Get calibration set details
                let flat_set = if let Some(set_id) = flat_set_id {
                    get_calibration_set_detail(conn, set_id).ok()
                } else {
                    None
                };

                let dark_set = if let Some(set_id) = dark_set_id {
                    get_calibration_set_detail(conn, set_id).ok()
                } else {
                    None
                };

                let bias_set = if let Some(set_id) = bias_set_id {
                    get_calibration_set_detail(conn, set_id).ok()
                } else {
                    None
                };

                // Collect warnings
                let (flat_warnings, dark_warnings, bias_warnings) =
                    get_calibration_warnings_for_group(conn, &frame_ids, flat_set_id, dark_set_id, bias_set_id)?;

                if filter_has_warnings {
                    camera_has_warnings = true;
                }

                // Build filter display string
                let filter_display = filter.clone().unwrap_or_else(|| "No Filter".to_string());

                filter_groups.push(CalibrationFilterGroup {
                    filter: filter.clone(),
                    filter_display,
                    light_frames,
                    flat_set,
                    dark_set,
                    bias_set,
                    flat_warnings,
                    dark_warnings,
                    bias_warnings,
                    has_warnings: filter_has_warnings,
                    frame_count: filter_frame_count,
                });
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
        };

        assert!(!link_exists(&conn, 1, "frame", "Dark").unwrap());
        insert_calibration_link(&conn, &link).unwrap();
        assert!(link_exists(&conn, 1, "frame", "Dark").unwrap());
    }
}
