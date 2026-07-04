/// Flat Calibration Group Detection
///
/// This module provides functionality for detecting and grouping flat frames
/// based on time proximity. Flat frames are typically captured in bursts
/// (consecutive exposures with minimal time gaps), and this module clusters
/// them into natural groupings.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::models::Frame;

/// Represents a group of flat frames captured in close temporal proximity
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct FlatGroup {
    /// IDs of frames in this group
    pub frame_ids: Vec<i64>,

    /// Timestamp of first frame in group
    pub start_time: DateTime<Utc>,

    /// Timestamp of last frame in group
    pub end_time: DateTime<Utc>,

    /// Average CCD temperature across all frames (if available)
    pub avg_temp: Option<f64>,

    /// Coldest CCD temperature observed in the group (if any frame has temp).
    /// Persisted as `calibration_set.temp_min` so the matcher / UI can see the
    /// real range, not the synthetic average.
    pub temp_min: Option<f64>,

    /// Warmest CCD temperature observed in the group.
    pub temp_max: Option<f64>,

    /// Number of frames in group
    pub frame_count: usize,

    /// Filter used (if any)
    pub filter: Option<String>,

    /// Camera/instrument name (None if unknown)
    pub instrume: Option<String>,

    /// Binning pattern (e.g., "1x1", "2x2") (None if unknown)
    pub binning: Option<String>,

    /// Gain setting
    pub gain: Option<f64>,

    /// Offset setting
    pub offset: Option<f64>,

    /// Exposure time (seconds)
    pub exptime: Option<f64>,

    /// Focal length
    pub focal_length: Option<f64>,
}

/// Detects flat groups by clustering frames with close temporal proximity
///
/// # Algorithm
/// 1. Query all flat frames matching exact parameters
/// 2. Sort by date_obs (chronological order)
/// 3. Cluster consecutive frames with time gap <= threshold
/// 4. Calculate metadata for each group
///
/// # Arguments
/// * `conn` - Database connection
/// * `instrume` - Camera/instrument name (exact match)
/// * `filter` - Filter name (exact match, None for no filter)
/// * `binning` - Binning pattern (exact match)
/// * `gain` - Gain setting (exact match if Some)
/// * `focal_length` - Focal length (range match if threshold provided, else exact)
/// * `focallen_threshold` - Optional threshold for focal length matching (from config)
/// * `time_cluster_minutes` - Time threshold for clustering (default: 30 minutes)
/// * `date_range` - Optional date range to limit search (start, end)
///
/// # Returns
/// Vector of FlatGroup objects, sorted by start_time (newest first)
pub fn detect_flat_groups(
    conn: &Connection,
    instrume: &str,
    filter: Option<&str>,
    binning: &str,
    gain: Option<f64>,
    focal_length: Option<f64>,
    focallen_threshold: Option<f64>,
    time_cluster_minutes: i64,
    date_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> Result<Vec<FlatGroup>> {
    // Build query with parameter matching
    let mut query = String::from(
        "SELECT id, file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp,
                is_master, ra, dec, objctra, objctdec, gain, offset, xbinning, ybinning,
                ccd_temp, set_temp, focallen, xpixsz, ypixsz, naxis1, naxis2,
                sitelat, lat_obs, sitelong, long_obs, uuid, updated_at
         FROM frames
         WHERE imagetyp = 'Flat' AND instrume = ?1 AND binning = ?2"
    );

    let mut param_count = 2;

    // Filter parameter (exact match or NULL)
    if filter.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND filter = ?{}", param_count));
    } else {
        query.push_str(" AND filter IS NULL");
    }

    // Gain parameter (exact match if provided)
    if gain.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND gain = ?{}", param_count));
    }

    // Focal length parameter (range match if threshold provided, else exact)
    if focal_length.is_some() {
        if let Some(_threshold) = focallen_threshold {
            // Range-based matching using threshold from config
            param_count += 1;
            let focal_param = param_count;
            param_count += 1;
            let threshold_param = param_count;
            query.push_str(&format!(
                " AND focallen IS NOT NULL AND ABS(focallen - ?{}) <= ?{}",
                focal_param, threshold_param
            ));
        } else {
            // Exact match (fallback)
            param_count += 1;
            query.push_str(&format!(" AND focallen = ?{}", param_count));
        }
    }

    // Date range filter (optional)
    if date_range.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND date_obs >= ?{}", param_count));
        param_count += 1;
        query.push_str(&format!(" AND date_obs <= ?{}", param_count));
    }

    query.push_str(" ORDER BY date_obs ASC");

    tracing::debug!(
        instrume,
        binning,
        filter = ?filter,
        gain = ?gain,
        focal_length = ?focal_length,
        focallen_threshold = ?focallen_threshold,
        date_start = ?date_range.map(|(s, _)| s.to_rfc3339()),
        date_end = ?date_range.map(|(_, e)| e.to_rfc3339()),
        "executing flat search query"
    );

    // Execute query
    let mut stmt = conn.prepare(&query)?;

    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, instrume)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, binning)?;
    param_idx += 1;

    if let Some(f) = filter {
        stmt.raw_bind_parameter(param_idx, f)?;
        param_idx += 1;
    }

    if let Some(g) = gain {
        stmt.raw_bind_parameter(param_idx, g)?;
        param_idx += 1;
    }

    if let Some(fl) = focal_length {
        stmt.raw_bind_parameter(param_idx, fl)?;
        param_idx += 1;
        if let Some(threshold) = focallen_threshold {
            stmt.raw_bind_parameter(param_idx, threshold)?;
            param_idx += 1;
        }
    }

    if let Some((start, end)) = date_range {
        stmt.raw_bind_parameter(param_idx, start.to_rfc3339())?;
        param_idx += 1;
        stmt.raw_bind_parameter(param_idx, end.to_rfc3339())?;
    }

    let mut frames = Vec::new();
    let mut rows = stmt.raw_query();

    while let Some(row) = rows.next()? {
        use crate::models::ImageType;

        // Parse date_obs
        let date_obs_str: Option<String> = row.get(3)?;
        let date_obs = date_obs_str.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        // Parse imagetyp
        let imagetyp_str: Option<String> = row.get(8)?;
        let imagetyp = imagetyp_str.and_then(|s| ImageType::from_str(&s));

        // Calculate binning
        let xbinning: Option<i32> = row.get(16)?;
        let ybinning: Option<i32> = row.get(17)?;
        let binning = match (xbinning, ybinning) {
            (Some(x), Some(y)) => Some(format!("{}x{}", x, y)),
            _ => None,
        };

        // Convert is_master from SQL INTEGER to bool
        let is_master_int: i32 = row.get(9)?;
        let is_master = is_master_int != 0;

        let frame = Frame {
            id: Some(row.get(0)?),
            file_id: row.get(1)?,
            object: row.get(2)?,
            date_obs,
            telescop: row.get(4)?,
            instrume: row.get(5)?,
            exptime: row.get(6)?,
            filter: row.get(7)?,
            imagetyp,
            is_master,
            gain: row.get(14)?,
            offset: row.get(15)?,
            binning,
            xbinning,
            ybinning,
            ccd_temp: row.get(18)?,
            set_temp: row.get(19)?,
            focallen: row.get(20)?,
            xpixsz: row.get(21)?,
            ypixsz: row.get(22)?,
            naxis1: row.get(23)?,
            naxis2: row.get(24)?,
            ra: row.get(10)?,
            dec: row.get(11)?,
            sitelat: row.get(25)?,
            lat_obs: row.get(26)?,
            sitelong: row.get(27)?,
            long_obs: row.get(28)?,
            objctra: row.get(12)?,
            objctdec: row.get(13)?,
            override_: false,
            swcreate: None,
            bayerpat: None,
            rotation: None,
            uuid: row.get(29)?,
            updated_at: row.get(30)?,
        };

        frames.push(frame);
    }

    tracing::debug!(count = frames.len(), "flat search query found frames");

    // Cluster frames by time proximity
    let groups = cluster_frames_by_time(
        frames,
        time_cluster_minutes,
        instrume,
        filter,
        binning,
        gain,
        focal_length,
    );

    tracing::debug!(count = groups.len(), "clustered flat frames into groups");

    Ok(groups)
}

/// Clusters frames into groups based on time proximity
fn cluster_frames_by_time(
    frames: Vec<Frame>,
    time_cluster_minutes: i64,
    instrume: &str,
    filter: Option<&str>,
    binning: &str,
    gain: Option<f64>,
    focal_length: Option<f64>,
) -> Vec<FlatGroup> {
    if frames.is_empty() {
        return Vec::new();
    }

    let threshold_seconds = time_cluster_minutes * 60;
    let mut groups = Vec::new();
    let mut current_group: Vec<Frame> = Vec::new();

    for frame in frames {
        if current_group.is_empty() {
            // Start first group
            current_group.push(frame);
        } else {
            // Check time gap from last frame in current group
            let last_frame = current_group.last().unwrap();

            if let (Some(last_date), Some(curr_date)) = (&last_frame.date_obs, &frame.date_obs) {
                let gap_seconds = (*curr_date - *last_date).num_seconds();

                // Check if exposure time matches (with small tolerance for floating point)
                let exptime_matches = match (last_frame.exptime, frame.exptime) {
                    (Some(last_exp), Some(curr_exp)) => (last_exp - curr_exp).abs() < 0.1,
                    (None, None) => true,
                    _ => false, // One has exptime, other doesn't - treat as different
                };

                if gap_seconds <= threshold_seconds && exptime_matches {
                    // Within threshold AND same exposure time - add to current group
                    current_group.push(frame);
                } else {
                    // Gap too large - close current group and start new one
                    if !current_group.is_empty() {
                        groups.push(create_flat_group(
                            current_group,
                            instrume,
                            filter,
                            binning,
                            gain,
                            focal_length,
                        ));
                    }
                    current_group = vec![frame];
                }
            } else {
                // Missing date - add to current group anyway
                current_group.push(frame);
            }
        }
    }

    // Don't forget last group
    if !current_group.is_empty() {
        groups.push(create_flat_group(
            current_group,
            instrume,
            filter,
            binning,
            gain,
            focal_length,
        ));
    }

    // Sort groups by start_time (newest first)
    groups.sort_by(|a, b| b.start_time.cmp(&a.start_time));

    groups
}

/// Creates a FlatGroup from a vector of frames
fn create_flat_group(
    frames: Vec<Frame>,
    instrume: &str,
    filter: Option<&str>,
    binning: &str,
    gain: Option<f64>,
    focal_length: Option<f64>,
) -> FlatGroup {
    let frame_count = frames.len();
    let frame_ids: Vec<i64> = frames.iter().filter_map(|f| f.id).collect();

    // Get first and last timestamps
    let start_time = frames.first()
        .and_then(|f| f.date_obs)
        .unwrap_or_else(Utc::now);

    let end_time = frames.last()
        .and_then(|f| f.date_obs)
        .unwrap_or_else(Utc::now);

    // Calculate average temperature (if available) and the actual range.
    // A4: prefer measured ccd_temp; fall back to commanded set_temp.
    let temps: Vec<f64> = frames.iter()
        .filter_map(|f| crate::models::effective_temp(f.ccd_temp, f.set_temp))
        .collect();

    let (avg_temp, temp_min, temp_max) = if !temps.is_empty() {
        let avg = temps.iter().sum::<f64>() / temps.len() as f64;
        let min = temps.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (Some(avg), Some(min), Some(max))
    } else {
        (None, None, None)
    };

    // Extract offset and exptime from first frame (should be same for all frames in group)
    let offset = frames.first().and_then(|f| f.offset);
    let exptime = frames.first().and_then(|f| f.exptime);

    // Convert empty strings to None for proper NULL handling
    let instrume_opt = if instrume.is_empty() { None } else { Some(instrume.to_string()) };
    let binning_opt = if binning.is_empty() { None } else { Some(binning.to_string()) };

    FlatGroup {
        frame_ids,
        start_time,
        end_time,
        avg_temp,
        temp_min,
        temp_max,
        frame_count,
        filter: filter.map(String::from),
        instrume: instrume_opt,
        binning: binning_opt,
        gain,
        offset,
        exptime,
        focal_length,
    }
}

/// Creates a flat calibration set from a FlatGroup
///
/// # Arguments
/// * `conn` - Database connection
/// * `flat_group` - The flat group to convert to a calibration set
/// * `allow_modify` - If true, add frames to existing sets (scanning context).
///                    If false, just return existing set ID without modification (find calibration context).
/// * `focallen_tolerance` - How to match focal length when checking for existing sets:
///   - `Some(0.0)` → exact match (Exact mode)
///   - `Some(tol)` → range match within tolerance (Warning mode)
///   - `None` → ignore focallen when matching (Ignore mode)
///
/// # Returns
/// The ID of the created (or existing) calibration set
pub fn create_flat_calibration_set(
    conn: &Connection,
    flat_group: &FlatGroup,
    allow_modify: bool,
    focallen_tolerance: Option<f64>,
) -> Result<i64> {
    // Check if set already exists with same parameters
    let existing_set_id = check_for_existing_flat_set(conn, flat_group, focallen_tolerance)?;
    tracing::debug!(existing_set_id = existing_set_id.unwrap_or(-1), "existing flat set check");

    if let Some(set_id) = existing_set_id {
        if allow_modify {
            // Scanning context: link new frames to existing set
            tracing::debug!(set_id, "reusing existing flat calibration set");
            for frame_id in &flat_group.frame_ids {
                conn.execute(
                    "INSERT OR IGNORE INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                    (set_id, frame_id),
                )?;
            }
            // Update frame_count to reflect actual linked frames
            conn.execute(
                "UPDATE calibration_set SET frame_count = (
                    SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1
                ) WHERE id = ?1",
                [set_id],
            )?;
        } else {
            // Find calibration context: just return existing set ID without modification
            tracing::debug!(set_id, "found existing flat calibration set");
        }
        return Ok(set_id);
    }

    // Create new calibration set
    let date = flat_group.start_time.format("%Y-%m-%d").to_string();
    let date_start = flat_group.start_time.to_rfc3339();
    let date_end = flat_group.end_time.to_rfc3339();
    let frame_count = flat_group.frame_ids.len() as i64;

    // Persist the actual cold/warm range from the cluster so the matcher and
    // UI see real temperature spread, not the synthetic average. Fall back to
    // avg_temp for legacy single-temperature groups.
    let temp_min = flat_group.temp_min.or(flat_group.avg_temp);
    let temp_max = flat_group.temp_max.or(flat_group.avg_temp);

    tracing::debug!(
        date,
        filter = ?flat_group.filter,
        gain = ?flat_group.gain,
        offset = ?flat_group.offset,
        exptime = ?flat_group.exptime,
        binning = ?flat_group.binning,
        instrume = ?flat_group.instrume,
        frame_count,
        date_start,
        date_end,
        "creating new flat calibration set"
    );

    conn.execute(
        "INSERT INTO calibration_set
         (imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume, date,
          date_start, date_end, temp_min, temp_max, frame_count, focallen)
         VALUES ('Flat', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        (
            &flat_group.exptime,
            &flat_group.filter,
            &flat_group.avg_temp,
            &flat_group.gain,
            &flat_group.offset,
            &flat_group.binning,
            &flat_group.instrume,
            &date,
            &date_start,
            &date_end,
            &temp_min,
            &temp_max,
            &frame_count,
            &flat_group.focal_length,
        ),
    )?;

    let set_id = conn.last_insert_rowid();
    tracing::info!(set_id, frame_count, "created flat calibration set");

    // Link frames to set
    tracing::debug!(set_id, count = flat_group.frame_ids.len(), "linking frames to flat calibration set");
    for (idx, frame_id) in flat_group.frame_ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            (set_id, frame_id),
        ).map_err(|e| {
            tracing::error!(
                frame_id,
                set_id,
                index = idx + 1,
                total = flat_group.frame_ids.len(),
                error = %e,
                "failed to link frame to flat calibration set"
            );
            e
        })?;
    }
    tracing::debug!(set_id, count = flat_group.frame_ids.len(), "linked all frames to flat calibration set");

    Ok(set_id)
}

/// Checks if a calibration set already exists for this flat group
/// Uses date range overlap instead of exact date match to preserve set identity
/// when frames are added/removed
///
/// `focallen_tolerance` controls focal length matching:
/// - `Some(0.0)` → exact match
/// - `Some(tol)` → range: ABS(cs.focallen - value) <= tol
/// - `None` → ignore focallen entirely
fn check_for_existing_flat_set(
    conn: &Connection,
    flat_group: &FlatGroup,
    focallen_tolerance: Option<f64>,
) -> Result<Option<i64>> {
    let cluster_start = flat_group.start_time.to_rfc3339();
    let cluster_end = flat_group.end_time.to_rfc3339();

    // Build query with NULL-aware comparisons for binning and instrume
    // Use date range overlap: existing set overlaps with new cluster if
    // existing.date_start <= cluster.end AND existing.date_end >= cluster.start
    let mut query = String::from(
        "SELECT cs.id
         FROM calibration_set cs
         WHERE cs.imagetyp = 'Flat'
           AND cs.date_start IS NOT NULL
           AND cs.date_end IS NOT NULL
           AND cs.date_start <= ?1
           AND cs.date_end >= ?2"
    );

    let mut param_count = 2;

    // NULL-aware comparison for binning
    if flat_group.binning.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.binning = ?{}", param_count));
    } else {
        query.push_str(" AND cs.binning IS NULL");
    }

    // NULL-aware comparison for instrume
    if flat_group.instrume.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.instrume = ?{}", param_count));
    } else {
        query.push_str(" AND cs.instrume IS NULL");
    }

    // NULL-aware comparison for filter
    if flat_group.filter.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.filter = ?{}", param_count));
    } else {
        query.push_str(" AND cs.filter IS NULL");
    }

    if flat_group.gain.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.gain = ?{}", param_count));
    }

    if flat_group.offset.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.offset = ?{}", param_count));
    }

    if flat_group.exptime.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.exptime = ?{}", param_count));
    }

    // Focallen matching depends on tolerance mode
    match focallen_tolerance {
        None => {
            // Ignore mode: don't filter by focallen at all
        }
        Some(tol) if tol < 0.01 => {
            // Exact mode
            if flat_group.focal_length.is_some() {
                param_count += 1;
                query.push_str(&format!(" AND cs.focallen = ?{}", param_count));
            }
        }
        Some(_tol) => {
            // Warning/range mode
            if flat_group.focal_length.is_some() {
                param_count += 1;
                let focal_param = param_count;
                param_count += 1;
                let tol_param = param_count;
                query.push_str(&format!(
                    " AND cs.focallen IS NOT NULL AND ABS(cs.focallen - ?{}) <= ?{}",
                    focal_param, tol_param
                ));
            }
        }
    }

    query.push_str(" LIMIT 1");

    let mut stmt = conn.prepare(&query)?;

    // Bind date range parameters (cluster_end for ?1, cluster_start for ?2)
    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, &cluster_end)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, &cluster_start)?;
    param_idx += 1;

    if let Some(ref binning) = flat_group.binning {
        stmt.raw_bind_parameter(param_idx, binning)?;
        param_idx += 1;
    }

    if let Some(ref instrume) = flat_group.instrume {
        stmt.raw_bind_parameter(param_idx, instrume)?;
        param_idx += 1;
    }

    if let Some(ref filter) = flat_group.filter {
        stmt.raw_bind_parameter(param_idx, filter)?;
        param_idx += 1;
    }

    if let Some(gain) = flat_group.gain {
        stmt.raw_bind_parameter(param_idx, gain)?;
        param_idx += 1;
    }

    if let Some(offset) = flat_group.offset {
        stmt.raw_bind_parameter(param_idx, offset)?;
        param_idx += 1;
    }

    if let Some(exptime) = flat_group.exptime {
        stmt.raw_bind_parameter(param_idx, exptime)?;
        param_idx += 1;
    }

    // Bind focallen parameters based on tolerance mode
    match focallen_tolerance {
        None => {
            // Ignore mode: no focallen parameter to bind
        }
        Some(tol) if tol < 0.01 => {
            // Exact mode
            if let Some(focal_length) = flat_group.focal_length {
                stmt.raw_bind_parameter(param_idx, focal_length)?;
            }
        }
        Some(tol) => {
            // Warning/range mode
            if let Some(focal_length) = flat_group.focal_length {
                stmt.raw_bind_parameter(param_idx, focal_length)?;
                param_idx += 1;
                stmt.raw_bind_parameter(param_idx, tol)?;
            }
        }
    }

    let mut rows = stmt.raw_query();
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, i64>(0)?))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_frames() {
        let groups = cluster_frames_by_time(
            Vec::new(),
            30,
            "TestCamera",
            None,
            "1x1",
            None,
            None,
        );
        assert_eq!(groups.len(), 0);
    }

    // Additional tests would go here with mock frames
}
