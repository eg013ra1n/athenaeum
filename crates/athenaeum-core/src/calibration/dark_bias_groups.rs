/// Dark and Bias Calibration Group Detection
///
/// This module provides functionality for detecting and grouping dark and bias frames
/// based on time proximity. Dark and Bias frames are typically captured in bursts
/// (consecutive exposures with minimal time gaps), and this module clusters
/// them into natural groupings.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::models::Frame;

/// Represents a group of dark frames captured in close temporal proximity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DarkGroup {
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
    /// real range, not the synthetic average. Stored as `Option<f64>` to keep
    /// "no-temp" groups distinct from "single-temperature" groups.
    pub temp_min: Option<f64>,

    /// Warmest CCD temperature observed in the group.
    pub temp_max: Option<f64>,

    /// Number of frames in group
    pub frame_count: usize,

    /// Camera/instrument name (None if missing from frame headers)
    pub instrume: Option<String>,

    /// Binning pattern (e.g., "1x1", "2x2") (None if missing from frame headers)
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

/// Represents a group of bias frames captured in close temporal proximity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiasGroup {
    /// IDs of frames in this group
    pub frame_ids: Vec<i64>,

    /// Timestamp of first frame in group
    pub start_time: DateTime<Utc>,

    /// Timestamp of last frame in group
    pub end_time: DateTime<Utc>,

    /// Average CCD temperature across all frames (if available)
    pub avg_temp: Option<f64>,

    /// Coldest CCD temperature observed in the group. See `DarkGroup::temp_min`.
    pub temp_min: Option<f64>,

    /// Warmest CCD temperature observed in the group.
    pub temp_max: Option<f64>,

    /// Number of frames in group
    pub frame_count: usize,

    /// Camera/instrument name (None if missing from frame headers)
    pub instrume: Option<String>,

    /// Binning pattern (e.g., "1x1", "2x2") (None if missing from frame headers)
    pub binning: Option<String>,

    /// Gain setting
    pub gain: Option<f64>,

    /// Offset setting
    pub offset: Option<f64>,

    /// Focal length
    pub focal_length: Option<f64>,
}

/// Detects dark groups by clustering frames with close temporal proximity
///
/// # Arguments
/// * `conn` - Database connection
/// * `instrume` - Camera/instrument name (REQUIRED - exact match)
/// * `binning` - Binning pattern (REQUIRED - exact match)
/// * `gain` - Gain setting (REQUIRED - exact match)
/// * `offset` - Offset setting (REQUIRED - exact match)
/// * `exptime` - Exposure time (REQUIRED - exact match)
/// * `_focal_length` - Not used for Dark matching (sensor-only calibration)
/// * `time_cluster_minutes` - Time threshold for clustering (default: 30 minutes)
/// * `date_range` - Optional date range to limit search (start, end)
///
/// # Returns
/// Vector of DarkGroup objects, sorted by start_time (newest first)
/// Returns empty vector if gain or offset is None (required parameters)
pub fn detect_dark_groups(
    conn: &Connection,
    instrume: &str,
    binning: &str,
    gain: Option<f64>,
    offset: Option<f64>,
    exptime: Option<f64>,
    _focal_length: Option<f64>, // Not used for Dark matching (sensor-only calibration)
    time_cluster_minutes: i64,
    date_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    temp_threshold: Option<f64>,
) -> Result<Vec<DarkGroup>> {
    // Gain and offset are REQUIRED for Dark matching - return empty if not provided
    let gain_val = match gain {
        Some(g) => g,
        None => {
            println!("    ⚠️  Dark detection skipped: gain is required but not provided");
            return Ok(Vec::new());
        }
    };
    let offset_val = match offset {
        Some(o) => o,
        None => {
            println!("    ⚠️  Dark detection skipped: offset is required but not provided");
            return Ok(Vec::new());
        }
    };

    // Build query with parameter matching
    // Dark matching requires: instrume, binning, gain, offset, exptime
    // Focal length is NOT used (Dark is sensor-only, not optical)
    let mut query = String::from(
        "SELECT id, file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp,
                is_master, ra, dec, objctra, objctdec, gain, offset, xbinning, ybinning,
                ccd_temp, set_temp, focallen, xpixsz, ypixsz, naxis1, naxis2,
                sitelat, lat_obs, sitelong, long_obs
         FROM frames
         WHERE imagetyp = 'Dark' AND instrume = ?1 AND binning = ?2 AND gain = ?3 AND offset = ?4"
    );

    let mut param_count = 4;

    // Exposure time parameter - REQUIRED for Dark matching
    if exptime.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND exptime = ?{}", param_count));
    }

    // Focal length is NOT checked for Dark frames (sensor-only calibration)

    // Date range filter (optional)
    if date_range.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND date_obs >= ?{}", param_count));
        param_count += 1;
        query.push_str(&format!(" AND date_obs <= ?{}", param_count));
    }

    query.push_str(" ORDER BY date_obs ASC");

    // Log the complete SQL query
    println!("    🔍 Executing Dark SQL query:");
    println!("       instrume={}, binning={}, gain={}, offset={}, exptime={:?}",
        instrume, binning, gain_val, offset_val, exptime);
    if let Some((start, end)) = date_range {
        println!("       date_range: {} to {}", start, end);
    }

    // Execute query with parameter binding
    let frames = execute_dark_query(conn, &query, instrume, binning, gain_val, offset_val, exptime, date_range)?;

    println!("    📊 SQL query found {} dark frames", frames.len());

    // Cluster frames by time proximity, with optional temperature-drift split.
    let groups = cluster_dark_frames_by_time(
        frames,
        time_cluster_minutes,
        instrume,
        binning,
        gain,
        offset,
        exptime,
        None, // focal_length not used for Dark
        temp_threshold,
    );

    println!("    🗂️  Clustered into {} dark groups", groups.len());

    Ok(groups)
}

/// Detects bias groups by clustering frames with close temporal proximity
///
/// # Arguments
/// * `conn` - Database connection
/// * `instrume` - Camera/instrument name (REQUIRED - exact match)
/// * `binning` - Binning pattern (REQUIRED - exact match)
/// * `gain` - Gain setting (REQUIRED - exact match)
/// * `offset` - Offset setting (REQUIRED - exact match)
/// * `_focal_length` - Not used for Bias matching (sensor-only calibration)
/// * `time_cluster_minutes` - Time threshold for clustering (default: 30 minutes)
/// * `date_range` - Optional date range to limit search (start, end)
///
/// # Returns
/// Vector of BiasGroup objects, sorted by start_time (newest first)
/// Returns empty vector if gain or offset is None (required parameters)
pub fn detect_bias_groups(
    conn: &Connection,
    instrume: &str,
    binning: &str,
    gain: Option<f64>,
    offset: Option<f64>,
    _focal_length: Option<f64>, // Not used for Bias matching (sensor-only calibration)
    time_cluster_minutes: i64,
    date_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    temp_threshold: Option<f64>,
) -> Result<Vec<BiasGroup>> {
    // Gain and offset are REQUIRED for Bias matching - return empty if not provided
    let gain_val = match gain {
        Some(g) => g,
        None => {
            println!("    ⚠️  Bias detection skipped: gain is required but not provided");
            return Ok(Vec::new());
        }
    };
    let offset_val = match offset {
        Some(o) => o,
        None => {
            println!("    ⚠️  Bias detection skipped: offset is required but not provided");
            return Ok(Vec::new());
        }
    };

    // Build query with parameter matching
    // Bias matching requires: instrume, binning, gain, offset
    // NO exptime (bias frames have ~0 exposure)
    // NO focal_length (Bias is sensor-only, not optical)
    let mut query = String::from(
        "SELECT id, file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp,
                is_master, ra, dec, objctra, objctdec, gain, offset, xbinning, ybinning,
                ccd_temp, set_temp, focallen, xpixsz, ypixsz, naxis1, naxis2,
                sitelat, lat_obs, sitelong, long_obs
         FROM frames
         WHERE imagetyp = 'Bias' AND instrume = ?1 AND binning = ?2 AND gain = ?3 AND offset = ?4"
    );

    let mut param_count = 4;

    // Date range filter (optional)
    if date_range.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND date_obs >= ?{}", param_count));
        param_count += 1;
        query.push_str(&format!(" AND date_obs <= ?{}", param_count));
    }

    query.push_str(" ORDER BY date_obs ASC");

    // Log the complete SQL query
    println!("    🔍 Executing Bias SQL query:");
    println!("       instrume={}, binning={}, gain={}, offset={}",
        instrume, binning, gain_val, offset_val);
    if let Some((start, end)) = date_range {
        println!("       date_range: {} to {}", start, end);
    }

    // Execute query with parameter binding
    let frames = execute_bias_query(conn, &query, instrume, binning, gain_val, offset_val, date_range)?;

    println!("    📊 SQL query found {} bias frames", frames.len());

    // Cluster frames by time proximity, with optional temperature-drift split.
    let groups = cluster_bias_frames_by_time(
        frames,
        time_cluster_minutes,
        instrume,
        binning,
        gain,
        offset,
        None, // focal_length not used for Bias
        temp_threshold,
    );

    println!("    🗂️  Clustered into {} bias groups", groups.len());

    Ok(groups)
}

/// Execute Dark query with parameter binding
/// gain and offset are REQUIRED for Dark matching
fn execute_dark_query(
    conn: &Connection,
    query: &str,
    instrume: &str,
    binning: &str,
    gain: f64,    // REQUIRED
    offset: f64,  // REQUIRED
    exptime: Option<f64>,
    date_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> Result<Vec<Frame>> {
    let mut stmt = conn.prepare(query)?;

    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, instrume)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, binning)?;
    param_idx += 1;

    // Gain and offset are always bound (required parameters)
    stmt.raw_bind_parameter(param_idx, gain)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, offset)?;
    param_idx += 1;

    if let Some(e) = exptime {
        stmt.raw_bind_parameter(param_idx, e)?;
        param_idx += 1;
    }

    // focal_length is NOT used for Dark matching (sensor-only calibration)

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
        };

        frames.push(frame);
    }

    Ok(frames)
}

/// Execute Bias query with parameter binding
/// gain and offset are REQUIRED for Bias matching
fn execute_bias_query(
    conn: &Connection,
    query: &str,
    instrume: &str,
    binning: &str,
    gain: f64,    // REQUIRED
    offset: f64,  // REQUIRED
    date_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> Result<Vec<Frame>> {
    let mut stmt = conn.prepare(query)?;

    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, instrume)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, binning)?;
    param_idx += 1;

    // Gain and offset are always bound (required parameters)
    stmt.raw_bind_parameter(param_idx, gain)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, offset)?;
    param_idx += 1;

    // focal_length is NOT used for Bias matching (sensor-only calibration)

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
        };

        frames.push(frame);
    }

    Ok(frames)
}

/// Clusters dark frames into groups based on time proximity
fn cluster_dark_frames_by_time(
    frames: Vec<Frame>,
    time_cluster_minutes: i64,
    instrume: &str,
    binning: &str,
    gain: Option<f64>,
    offset: Option<f64>,
    exptime: Option<f64>,
    focal_length: Option<f64>,
    temp_threshold: Option<f64>,
) -> Vec<DarkGroup> {
    if frames.is_empty() {
        return Vec::new();
    }

    let threshold_seconds = time_cluster_minutes * 60;
    let mut time_clusters: Vec<Vec<Frame>> = Vec::new();
    let mut current_group: Vec<Frame> = Vec::new();

    for frame in frames {
        if current_group.is_empty() {
            current_group.push(frame);
        } else {
            let last_frame = current_group.last().unwrap();
            if let (Some(last_date), Some(curr_date)) = (&last_frame.date_obs, &frame.date_obs) {
                let gap_seconds = (*curr_date - *last_date).num_seconds();
                if gap_seconds <= threshold_seconds {
                    current_group.push(frame);
                } else {
                    time_clusters.push(std::mem::take(&mut current_group));
                    current_group = vec![frame];
                }
            } else {
                // Missing date — keep with current cluster (preserves legacy behavior)
                current_group.push(frame);
            }
        }
    }
    if !current_group.is_empty() {
        time_clusters.push(current_group);
    }

    // Sub-cluster each time-cluster by temperature drift. A 90-frame dark
    // sequence drifting -10 → -8.5°C is one continuous time cluster but the
    // ccd_temp range exceeds the user's threshold, so it must be split into
    // two distinct dark sets — otherwise the master averages dark current at
    // a temperature that was never measured. When `temp_threshold` is None
    // (existing test paths), no drift split happens.
    let mut groups = Vec::new();
    for time_cluster in time_clusters {
        for sub in split_by_temp_drift(time_cluster, temp_threshold) {
            if !sub.is_empty() {
                groups.push(create_dark_group(
                    sub, instrume, binning, gain, offset, exptime, focal_length,
                ));
            }
        }
    }

    // Sort groups by start_time (newest first)
    groups.sort_by(|a, b| b.start_time.cmp(&a.start_time));
    groups
}

/// Splits a time-clustered batch of frames into sub-clusters whose ccd_temp
/// range never exceeds `threshold`. Frames without a `ccd_temp` reading stay
/// with the current sub-cluster (so a single missing reading doesn't
/// fracture an otherwise-coherent group). When `threshold` is None, returns
/// the input as a single sub-cluster — the historic no-drift-check behavior.
fn split_by_temp_drift(frames: Vec<Frame>, threshold: Option<f64>) -> Vec<Vec<Frame>> {
    let threshold = match threshold {
        Some(t) if t > 0.0 => t,
        _ => return if frames.is_empty() { Vec::new() } else { vec![frames] },
    };
    let mut result: Vec<Vec<Frame>> = Vec::new();
    let mut current: Vec<Frame> = Vec::new();
    let mut cur_min: Option<f64> = None;
    let mut cur_max: Option<f64> = None;

    for frame in frames {
        // A4: use the measured ccd_temp if available, else fall back to the
        // commanded set_temp. Without this, master frames (ccd_temp NULL,
        // set_temp populated) bypass the drift check entirely.
        match crate::models::effective_temp(frame.ccd_temp, frame.set_temp) {
            Some(t) => {
                let new_min = cur_min.map_or(t, |m| m.min(t));
                let new_max = cur_max.map_or(t, |m| m.max(t));
                if !current.is_empty() && (new_max - new_min) > threshold {
                    // Adding this frame would push the cluster over threshold —
                    // split here and start a fresh sub-cluster from this frame.
                    result.push(std::mem::take(&mut current));
                    cur_min = Some(t);
                    cur_max = Some(t);
                } else {
                    cur_min = Some(new_min);
                    cur_max = Some(new_max);
                }
                current.push(frame);
            }
            None => {
                // No reading — keep with current sub-cluster (don't disrupt
                // grouping over a single dropped temperature record).
                current.push(frame);
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// Clusters bias frames into groups based on time proximity
fn cluster_bias_frames_by_time(
    frames: Vec<Frame>,
    time_cluster_minutes: i64,
    instrume: &str,
    binning: &str,
    gain: Option<f64>,
    offset: Option<f64>,
    focal_length: Option<f64>,
    temp_threshold: Option<f64>,
) -> Vec<BiasGroup> {
    if frames.is_empty() {
        return Vec::new();
    }

    let threshold_seconds = time_cluster_minutes * 60;
    let mut time_clusters: Vec<Vec<Frame>> = Vec::new();
    let mut current_group: Vec<Frame> = Vec::new();

    for frame in frames {
        if current_group.is_empty() {
            current_group.push(frame);
        } else {
            let last_frame = current_group.last().unwrap();
            if let (Some(last_date), Some(curr_date)) = (&last_frame.date_obs, &frame.date_obs) {
                let gap_seconds = (*curr_date - *last_date).num_seconds();
                if gap_seconds <= threshold_seconds {
                    current_group.push(frame);
                } else {
                    time_clusters.push(std::mem::take(&mut current_group));
                    current_group = vec![frame];
                }
            } else {
                current_group.push(frame);
            }
        }
    }
    if !current_group.is_empty() {
        time_clusters.push(current_group);
    }

    // Bias drift is usually small but the same threshold-based split applies
    // — see `cluster_dark_frames_by_time` for the full rationale.
    let mut groups = Vec::new();
    for time_cluster in time_clusters {
        for sub in split_by_temp_drift(time_cluster, temp_threshold) {
            if !sub.is_empty() {
                groups.push(create_bias_group(
                    sub, instrume, binning, gain, offset, focal_length,
                ));
            }
        }
    }

    // Sort groups by start_time (newest first)
    groups.sort_by(|a, b| b.start_time.cmp(&a.start_time));
    groups
}

/// Creates a DarkGroup from a vector of frames
fn create_dark_group(
    frames: Vec<Frame>,
    instrume: &str,
    binning: &str,
    gain: Option<f64>,
    offset: Option<f64>,
    exptime: Option<f64>,
    focal_length: Option<f64>,
) -> DarkGroup {
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
    // A4: prefer measured ccd_temp but fall back to set_temp (commanded
    // cooler value) so master frames / uncooled emergency captures don't all
    // collapse into one no-temperature bucket.
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

    // Convert empty strings to None for proper NULL handling
    let instrume_opt = if instrume.is_empty() { None } else { Some(instrume.to_string()) };
    let binning_opt = if binning.is_empty() { None } else { Some(binning.to_string()) };

    DarkGroup {
        frame_ids,
        start_time,
        end_time,
        avg_temp,
        temp_min,
        temp_max,
        frame_count,
        instrume: instrume_opt,
        binning: binning_opt,
        gain,
        offset,
        exptime,
        focal_length,
    }
}

/// Creates a BiasGroup from a vector of frames
fn create_bias_group(
    frames: Vec<Frame>,
    instrume: &str,
    binning: &str,
    gain: Option<f64>,
    offset: Option<f64>,
    focal_length: Option<f64>,
) -> BiasGroup {
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
    // A4: prefer measured ccd_temp but fall back to set_temp (commanded
    // cooler value) so master frames / uncooled emergency captures don't all
    // collapse into one no-temperature bucket.
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

    // Convert empty strings to None for proper NULL handling
    let instrume_opt = if instrume.is_empty() { None } else { Some(instrume.to_string()) };
    let binning_opt = if binning.is_empty() { None } else { Some(binning.to_string()) };

    BiasGroup {
        frame_ids,
        start_time,
        end_time,
        avg_temp,
        temp_min,
        temp_max,
        frame_count,
        instrume: instrume_opt,
        binning: binning_opt,
        gain,
        offset,
        focal_length,
    }
}

/// Creates a dark calibration set from a DarkGroup
///
/// # Arguments
/// * `conn` - Database connection
/// * `dark_group` - The dark group to convert to a calibration set
/// * `allow_modify` - If true, add frames to existing sets (scanning context).
///                    If false, just return existing set ID without modification (find calibration context).
///
/// # Returns
/// The ID of the created (or existing) calibration set
pub fn create_dark_calibration_set(
    conn: &Connection,
    dark_group: &DarkGroup,
    allow_modify: bool,
) -> Result<i64> {
    // Check if set already exists with same parameters
    let existing_set_id = check_for_existing_dark_set(conn, dark_group)?;
    println!("    🔍 Existing dark set check: {:?}", existing_set_id);

    if let Some(set_id) = existing_set_id {
        if allow_modify {
            // Scanning context: link new frames to existing set
            println!("    ♻️  Reusing existing dark calibration set ID: {}", set_id);
            for frame_id in &dark_group.frame_ids {
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
            println!("    ♻️  Found existing dark calibration set ID: {}", set_id);
        }
        return Ok(set_id);
    }

    // Create new calibration set
    let date = dark_group.start_time.format("%Y-%m-%d").to_string();
    let date_start = dark_group.start_time.to_rfc3339();
    let date_end = dark_group.end_time.to_rfc3339();
    let frame_count = dark_group.frame_ids.len() as i64;

    // Persist the actual coldest/warmest CCD temperature observed across the
    // group. Falling back to avg_temp keeps existing rows readable when only
    // an average is known (e.g., legacy data without per-frame temps).
    let temp_min = dark_group.temp_min.or(dark_group.avg_temp);
    let temp_max = dark_group.temp_max.or(dark_group.avg_temp);

    println!("    📝 Creating new dark calibration set:");
    println!("       date={}, exptime={:?}, gain={:?}, offset={:?}, binning={:?}, instrume={:?}",
        date, dark_group.exptime, dark_group.gain, dark_group.offset, dark_group.binning, dark_group.instrume);
    println!("       frames={}, dates: {} to {}", frame_count, date_start, date_end);

    conn.execute(
        "INSERT INTO calibration_set
         (imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume, date,
          date_start, date_end, temp_min, temp_max, frame_count, focallen)
         VALUES ('Dark', ?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        (
            &dark_group.exptime,
            &dark_group.avg_temp,
            &dark_group.gain,
            &dark_group.offset,
            &dark_group.binning,
            &dark_group.instrume,
            &date,
            &date_start,
            &date_end,
            &temp_min,
            &temp_max,
            &frame_count,
            &dark_group.focal_length,
        ),
    )?;

    let set_id = conn.last_insert_rowid();
    println!("    ✅ Created dark calibration set with ID: {}", set_id);

    // Link frames to set
    println!("    🔗 Linking {} frames to set {}", dark_group.frame_ids.len(), set_id);
    for (idx, frame_id) in dark_group.frame_ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            (set_id, frame_id),
        ).map_err(|e| {
            eprintln!("    ❌ Failed to link frame {} (index {}/{}): {}",
                frame_id, idx + 1, dark_group.frame_ids.len(), e);
            e
        })?;
    }
    println!("    ✅ Linked all {} frames successfully", dark_group.frame_ids.len());

    Ok(set_id)
}

/// Creates a bias calibration set from a BiasGroup
///
/// # Arguments
/// * `conn` - Database connection
/// * `bias_group` - The bias group to convert to a calibration set
/// * `allow_modify` - If true, add frames to existing sets (scanning context).
///                    If false, just return existing set ID without modification (find calibration context).
///
/// # Returns
/// The ID of the created (or existing) calibration set
pub fn create_bias_calibration_set(
    conn: &Connection,
    bias_group: &BiasGroup,
    allow_modify: bool,
) -> Result<i64> {
    // Check if set already exists with same parameters
    let existing_set_id = check_for_existing_bias_set(conn, bias_group)?;
    println!("    🔍 Existing bias set check: {:?}", existing_set_id);

    if let Some(set_id) = existing_set_id {
        if allow_modify {
            // Scanning context: link new frames to existing set
            println!("    ♻️  Reusing existing bias calibration set ID: {}", set_id);
            for frame_id in &bias_group.frame_ids {
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
            println!("    ♻️  Found existing bias calibration set ID: {}", set_id);
        }
        return Ok(set_id);
    }

    // Create new calibration set
    let date = bias_group.start_time.format("%Y-%m-%d").to_string();
    let date_start = bias_group.start_time.to_rfc3339();
    let date_end = bias_group.end_time.to_rfc3339();
    let frame_count = bias_group.frame_ids.len() as i64;

    // Persist the actual coldest/warmest CCD temperature observed across the
    // group; fall back to avg_temp for legacy single-temperature groups.
    let temp_min = bias_group.temp_min.or(bias_group.avg_temp);
    let temp_max = bias_group.temp_max.or(bias_group.avg_temp);

    println!("    📝 Creating new bias calibration set:");
    println!("       date={}, gain={:?}, offset={:?}, binning={:?}, instrume={:?}",
        date, bias_group.gain, bias_group.offset, bias_group.binning, bias_group.instrume);
    println!("       frames={}, dates: {} to {}", frame_count, date_start, date_end);

    conn.execute(
        "INSERT INTO calibration_set
         (imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume, date,
          date_start, date_end, temp_min, temp_max, frame_count, focallen)
         VALUES ('Bias', NULL, NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        (
            &bias_group.avg_temp,
            &bias_group.gain,
            &bias_group.offset,
            &bias_group.binning,
            &bias_group.instrume,
            &date,
            &date_start,
            &date_end,
            &temp_min,
            &temp_max,
            &frame_count,
            &bias_group.focal_length,
        ),
    )?;

    let set_id = conn.last_insert_rowid();
    println!("    ✅ Created bias calibration set with ID: {}", set_id);

    // Link frames to set
    println!("    🔗 Linking {} frames to set {}", bias_group.frame_ids.len(), set_id);
    for (idx, frame_id) in bias_group.frame_ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            (set_id, frame_id),
        ).map_err(|e| {
            eprintln!("    ❌ Failed to link frame {} (index {}/{}): {}",
                frame_id, idx + 1, bias_group.frame_ids.len(), e);
            e
        })?;
    }
    println!("    ✅ Linked all {} frames successfully", bias_group.frame_ids.len());

    Ok(set_id)
}

/// Checks if a calibration set already exists for this dark group
/// Uses date range overlap instead of exact date match to preserve set identity
fn check_for_existing_dark_set(
    conn: &Connection,
    dark_group: &DarkGroup,
) -> Result<Option<i64>> {
    let cluster_start = dark_group.start_time.to_rfc3339();
    let cluster_end = dark_group.end_time.to_rfc3339();

    // Build query with NULL-aware comparisons for nullable fields
    // Use date range overlap: existing set overlaps with new cluster if
    // existing.date_start <= cluster.end AND existing.date_end >= cluster.start
    let mut query = String::from(
        "SELECT cs.id
         FROM calibration_set cs
         WHERE cs.imagetyp = 'Dark'
           AND cs.date_start IS NOT NULL
           AND cs.date_end IS NOT NULL
           AND cs.date_start <= ?1
           AND cs.date_end >= ?2"
    );

    let mut param_count = 2;

    // NULL-aware comparison for binning
    if dark_group.binning.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.binning = ?{}", param_count));
    } else {
        query.push_str(" AND cs.binning IS NULL");
    }

    // NULL-aware comparison for instrume
    if dark_group.instrume.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.instrume = ?{}", param_count));
    } else {
        query.push_str(" AND cs.instrume IS NULL");
    }

    if dark_group.gain.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.gain = ?{}", param_count));
    } else {
        query.push_str(" AND cs.gain IS NULL");
    }

    if dark_group.offset.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.offset = ?{}", param_count));
    } else {
        query.push_str(" AND cs.offset IS NULL");
    }

    if dark_group.exptime.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.exptime = ?{}", param_count));
    } else {
        query.push_str(" AND cs.exptime IS NULL");
    }

    if dark_group.focal_length.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.focallen = ?{}", param_count));
    } else {
        query.push_str(" AND cs.focallen IS NULL");
    }

    query.push_str(" LIMIT 1");

    let mut stmt = conn.prepare(&query)?;

    // Bind date range parameters (cluster_end for ?1, cluster_start for ?2)
    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, &cluster_end)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, &cluster_start)?;
    param_idx += 1;

    if let Some(ref binning) = dark_group.binning {
        stmt.raw_bind_parameter(param_idx, binning)?;
        param_idx += 1;
    }

    if let Some(ref instrume) = dark_group.instrume {
        stmt.raw_bind_parameter(param_idx, instrume)?;
        param_idx += 1;
    }

    if let Some(gain) = dark_group.gain {
        stmt.raw_bind_parameter(param_idx, gain)?;
        param_idx += 1;
    }

    if let Some(offset) = dark_group.offset {
        stmt.raw_bind_parameter(param_idx, offset)?;
        param_idx += 1;
    }

    if let Some(exptime) = dark_group.exptime {
        stmt.raw_bind_parameter(param_idx, exptime)?;
        param_idx += 1;
    }

    if let Some(focal_length) = dark_group.focal_length {
        stmt.raw_bind_parameter(param_idx, focal_length)?;
    }

    let mut rows = stmt.raw_query();
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, i64>(0)?))
    } else {
        Ok(None)
    }
}

/// Checks if a calibration set already exists for this bias group
/// Uses date range overlap instead of exact date match to preserve set identity
fn check_for_existing_bias_set(
    conn: &Connection,
    bias_group: &BiasGroup,
) -> Result<Option<i64>> {
    let cluster_start = bias_group.start_time.to_rfc3339();
    let cluster_end = bias_group.end_time.to_rfc3339();

    // Build query with NULL-aware comparisons for nullable fields
    // Use date range overlap: existing set overlaps with new cluster if
    // existing.date_start <= cluster.end AND existing.date_end >= cluster.start
    let mut query = String::from(
        "SELECT cs.id
         FROM calibration_set cs
         WHERE cs.imagetyp = 'Bias'
           AND cs.date_start IS NOT NULL
           AND cs.date_end IS NOT NULL
           AND cs.date_start <= ?1
           AND cs.date_end >= ?2"
    );

    let mut param_count = 2;

    // NULL-aware comparison for binning
    if bias_group.binning.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.binning = ?{}", param_count));
    } else {
        query.push_str(" AND cs.binning IS NULL");
    }

    // NULL-aware comparison for instrume
    if bias_group.instrume.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.instrume = ?{}", param_count));
    } else {
        query.push_str(" AND cs.instrume IS NULL");
    }

    if bias_group.gain.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.gain = ?{}", param_count));
    } else {
        query.push_str(" AND cs.gain IS NULL");
    }

    if bias_group.offset.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.offset = ?{}", param_count));
    } else {
        query.push_str(" AND cs.offset IS NULL");
    }

    if bias_group.focal_length.is_some() {
        param_count += 1;
        query.push_str(&format!(" AND cs.focallen = ?{}", param_count));
    } else {
        query.push_str(" AND cs.focallen IS NULL");
    }

    query.push_str(" LIMIT 1");

    let mut stmt = conn.prepare(&query)?;

    // Bind date range parameters (cluster_end for ?1, cluster_start for ?2)
    let mut param_idx = 1;
    stmt.raw_bind_parameter(param_idx, &cluster_end)?;
    param_idx += 1;
    stmt.raw_bind_parameter(param_idx, &cluster_start)?;
    param_idx += 1;

    if let Some(ref binning) = bias_group.binning {
        stmt.raw_bind_parameter(param_idx, binning)?;
        param_idx += 1;
    }

    if let Some(ref instrume) = bias_group.instrume {
        stmt.raw_bind_parameter(param_idx, instrume)?;
        param_idx += 1;
    }

    if let Some(gain) = bias_group.gain {
        stmt.raw_bind_parameter(param_idx, gain)?;
        param_idx += 1;
    }

    if let Some(offset) = bias_group.offset {
        stmt.raw_bind_parameter(param_idx, offset)?;
        param_idx += 1;
    }

    if let Some(focal_length) = bias_group.focal_length {
        stmt.raw_bind_parameter(param_idx, focal_length)?;
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
    fn test_empty_dark_frames() {
        let groups = cluster_dark_frames_by_time(
            Vec::new(),
            30,
            "TestCamera",
            "1x1",
            None,
            None,
            Some(300.0),
            None,
            None,
        );
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_empty_bias_frames() {
        let groups = cluster_bias_frames_by_time(
            Vec::new(),
            30,
            "TestCamera",
            "1x1",
            None,
            None,
            None,
            None,
        );
        assert_eq!(groups.len(), 0);
    }

    fn make_dark_frame(id: i64, date_offset_seconds: i64, ccd_temp: Option<f64>) -> Frame {
        Frame {
            id: Some(id),
            file_id: id,
            object: None,
            date_obs: Some(
                chrono::DateTime::parse_from_rfc3339("2025-09-25T00:00:00+00:00").unwrap().with_timezone(&Utc)
                    + chrono::Duration::seconds(date_offset_seconds),
            ),
            telescop: None,
            instrume: Some("TestCamera".to_string()),
            exptime: Some(300.0),
            filter: None,
            imagetyp: Some(crate::models::ImageType::Dark),
            is_master: false,
            gain: Some(56.0),
            offset: Some(50.0),
            binning: Some("1x1".to_string()),
            xbinning: Some(1),
            ybinning: Some(1),
            ccd_temp,
            set_temp: None,
            focallen: None,
            xpixsz: None,
            ypixsz: None,
            naxis1: None,
            naxis2: None,
            ra: None,
            dec: None,
            sitelat: None,
            lat_obs: None,
            sitelong: None,
            long_obs: None,
            objctra: None,
            objctdec: None,
            override_: false,
            swcreate: None,
            bayerpat: None,
            rotation: None,
        }
    }

    #[test]
    fn dark_group_records_real_temp_range_not_just_avg() {
        // A2 regression: when a dark sequence drifts, the resulting set must
        // record the actual coldest and warmest frame, not the synthetic avg.
        // Frames at -10.0, -9.5, -8.5 → avg ≈ -9.33, min = -10.0, max = -8.5.
        let frames = vec![
            make_dark_frame(1, 0, Some(-10.0)),
            make_dark_frame(2, 60, Some(-9.5)),
            make_dark_frame(3, 120, Some(-8.5)),
        ];
        let groups = cluster_dark_frames_by_time(
            frames, 30, "TestCamera", "1x1", Some(56.0), Some(50.0), Some(300.0), None, None,
        );
        assert_eq!(groups.len(), 1, "frames within time threshold should cluster together");
        let g = &groups[0];

        let avg = g.avg_temp.expect("avg_temp populated");
        assert!((avg - (-9.333333_f64)).abs() < 1e-3, "avg_temp = {}", avg);
        assert_eq!(g.temp_min, Some(-10.0), "temp_min must be the coldest frame");
        assert_eq!(g.temp_max, Some(-8.5), "temp_max must be the warmest frame");
    }

    #[test]
    fn dark_clustering_splits_on_temperature_drift() {
        // A1 regression: a long sequence within the time window but drifting
        // beyond the temperature threshold should split into separate groups.
        // Threshold = 2°C. Frames at -10.0, -10.5, -11.0, -11.5, -12.0, -12.5
        // → range -10.0 to -12.5 (2.5°C span) → split at the point where the
        // cluster's range exceeds 2°C.
        let frames = vec![
            make_dark_frame(1, 0,   Some(-10.0)),
            make_dark_frame(2, 60,  Some(-10.5)),
            make_dark_frame(3, 120, Some(-11.0)),
            make_dark_frame(4, 180, Some(-11.5)),
            make_dark_frame(5, 240, Some(-12.0)),
            make_dark_frame(6, 300, Some(-12.5)),
        ];
        let groups = cluster_dark_frames_by_time(
            frames, 30, "TestCamera", "1x1", Some(56.0), Some(50.0), Some(300.0), None,
            Some(2.0),
        );
        assert!(
            groups.len() >= 2,
            "drift across 2.5°C with 2°C threshold must split, got {} groups",
            groups.len()
        );
        for g in &groups {
            let span = g.temp_max.unwrap() - g.temp_min.unwrap();
            assert!(span <= 2.0 + 1e-9, "sub-cluster span {} exceeds threshold", span);
        }
    }

    fn make_dark_frame_with_set_temp(id: i64, date_offset_seconds: i64, ccd_temp: Option<f64>, set_temp: Option<f64>) -> Frame {
        let mut f = make_dark_frame(id, date_offset_seconds, ccd_temp);
        f.set_temp = set_temp;
        f
    }

    #[test]
    fn dark_group_uses_set_temp_when_ccd_temp_missing() {
        // A4 regression: a master dark / uncooled frame with ccd_temp=NULL but
        // set_temp populated must contribute a temperature to the group rather
        // than collapsing into a "no-temperature" bucket.
        let frames = vec![
            make_dark_frame_with_set_temp(1, 0,   None, Some(-10.0)),
            make_dark_frame_with_set_temp(2, 60,  None, Some(-10.0)),
            make_dark_frame_with_set_temp(3, 120, None, Some(-10.0)),
        ];
        let groups = cluster_dark_frames_by_time(
            frames, 30, "TestCamera", "1x1", Some(56.0), Some(50.0), Some(300.0), None, None,
        );
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.avg_temp, Some(-10.0), "set_temp must be used when ccd_temp is NULL");
        assert_eq!(g.temp_min, Some(-10.0));
        assert_eq!(g.temp_max, Some(-10.0));
    }

    #[test]
    fn dark_drift_split_uses_set_temp_when_ccd_temp_missing() {
        // A4 + A1: drift split must also see set_temp when ccd_temp is absent,
        // otherwise master frames slip past the drift check entirely.
        let frames = vec![
            make_dark_frame_with_set_temp(1, 0,   None, Some(-10.0)),
            make_dark_frame_with_set_temp(2, 60,  None, Some(-13.0)),
        ];
        let groups = cluster_dark_frames_by_time(
            frames, 30, "TestCamera", "1x1", Some(56.0), Some(50.0), Some(300.0), None,
            Some(2.0),
        );
        assert!(
            groups.len() >= 2,
            "set_temp drift of 3°C with 2°C threshold must split, got {} groups",
            groups.len()
        );
    }

    #[test]
    fn dark_clustering_no_threshold_keeps_legacy_behavior() {
        // When temp_threshold is None, the function must behave exactly like
        // the pre-A1 version — no drift split.
        let frames = vec![
            make_dark_frame(1, 0,   Some(-10.0)),
            make_dark_frame(2, 60,  Some(-12.5)),
        ];
        let groups = cluster_dark_frames_by_time(
            frames, 30, "TestCamera", "1x1", Some(56.0), Some(50.0), Some(300.0), None, None,
        );
        assert_eq!(groups.len(), 1, "None threshold preserves single-cluster behavior");
    }

    #[test]
    fn dark_group_with_no_temps_keeps_range_as_none() {
        let frames = vec![
            make_dark_frame(1, 0, None),
            make_dark_frame(2, 60, None),
        ];
        let groups = cluster_dark_frames_by_time(
            frames, 30, "TestCamera", "1x1", Some(56.0), Some(50.0), Some(300.0), None, None,
        );
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert!(g.avg_temp.is_none());
        assert!(g.temp_min.is_none());
        assert!(g.temp_max.is_none());
    }
}
