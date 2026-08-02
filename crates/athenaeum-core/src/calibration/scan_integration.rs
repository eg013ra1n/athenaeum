/// Scan Integration for Calibration Set Creation
///
/// This module handles automatic creation of calibration sets during directory scanning.
/// When calibration frames (Flat, Dark, Bias, DarkFlat) are scanned, they are automatically
/// grouped into calibration sets based on their parameters and time proximity.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::flat_groups::{create_flat_calibration_set, FlatGroup};
use super::dark_bias_groups::{create_bias_calibration_set, DarkGroup, BiasGroup};
use super::configurable_matcher::load_config;
use super::config::MatchMode;

/// Result of calibration set creation during scan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationScanResult {
    pub sets_created: i64,
    pub flat_sets_created: i64,
    pub dark_sets_created: i64,
    pub bias_sets_created: i64,
    pub darkflat_sets_created: i64,
    // Master calibration sets (1 file = 1 set)
    pub master_dark_sets_created: i64,
    pub master_flat_sets_created: i64,
    pub master_bias_sets_created: i64,
    pub master_darkflat_sets_created: i64,
}

/// Default time clustering threshold in minutes
const DEFAULT_TIME_CLUSTER_MINUTES: i64 = 30;

/// Default temperature threshold for clustering (degrees Celsius)
const DEFAULT_TEMP_THRESHOLD: f64 = 2.0;

/// Frame data for grouping
#[derive(Debug, Clone)]
struct CalibrationFrameData {
    id: i64,
    instrume: Option<String>,
    filter: Option<String>,
    binning: Option<String>,
    gain: Option<f64>,
    offset: Option<f64>,
    exptime: Option<f64>,
    focallen: Option<f64>,
    ccd_temp: Option<f64>,
    /// Commanded cooler set-point. Used as fallback when ccd_temp is NULL
    /// (master frames, uncooled emergency captures) — see A4.
    set_temp: Option<f64>,
    date_obs: Option<DateTime<Utc>>,
}

/// Grouping key for flats (exact match parameters)
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct FlatGroupKey {
    instrume: String,
    filter: String,
    binning: String,
    gain: String,
    offset: String,
    focallen: String,
    exptime: String,
}

/// Grouping key for darks (exact match parameters)
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct DarkGroupKey {
    instrume: String,
    binning: String,
    gain: String,
    offset: String,
    exptime: String,
}

/// Grouping key for bias (exact match parameters)
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct BiasGroupKey {
    instrume: String,
    binning: String,
    gain: String,
    offset: String,
}

/// Master frame IDs for creating master calibration sets
#[derive(Debug, Clone, Default)]
pub struct MasterFrameIds {
    pub master_dark_ids: Vec<i64>,
    pub master_flat_ids: Vec<i64>,
    pub master_bias_ids: Vec<i64>,
    pub master_darkflat_ids: Vec<i64>,
}

impl MasterFrameIds {
    pub fn is_empty(&self) -> bool {
        self.master_dark_ids.is_empty()
            && self.master_flat_ids.is_empty()
            && self.master_bias_ids.is_empty()
            && self.master_darkflat_ids.is_empty()
    }

    pub fn total_count(&self) -> usize {
        self.master_dark_ids.len()
            + self.master_flat_ids.len()
            + self.master_bias_ids.len()
            + self.master_darkflat_ids.len()
    }
}

/// Create calibration sets from newly scanned frames, including master frames
///
/// This extended function handles both regular calibration frames (grouped by parameters)
/// and master frames (1 file = 1 set with is_master_library = 1).
pub fn create_calibration_sets_from_scan_with_masters(
    conn: &Connection,
    flat_frame_ids: Vec<i64>,
    dark_frame_ids: Vec<i64>,
    bias_frame_ids: Vec<i64>,
    darkflat_frame_ids: Vec<i64>,
    master_frame_ids: MasterFrameIds,
) -> Result<CalibrationScanResult> {
    tracing::info!(
        flats = flat_frame_ids.len(),
        darks = dark_frame_ids.len(),
        bias = bias_frame_ids.len(),
        darkflats = darkflat_frame_ids.len(),
        master_total = master_frame_ids.total_count(),
        master_dark = master_frame_ids.master_dark_ids.len(),
        master_flat = master_frame_ids.master_flat_ids.len(),
        master_bias = master_frame_ids.master_bias_ids.len(),
        master_darkflat = master_frame_ids.master_darkflat_ids.len(),
        "creating calibration sets from scan"
    );

    // Load clustering settings from user config
    let config = load_config(conn);

    // Get per-type clustering thresholds (in minutes)
    let flat_cluster_mins = config.clustering.get("flat")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(DEFAULT_TIME_CLUSTER_MINUTES);
    let dark_cluster_mins = config.clustering.get("dark")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(DEFAULT_TIME_CLUSTER_MINUTES);
    let bias_cluster_mins = config.clustering.get("bias")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(DEFAULT_TIME_CLUSTER_MINUTES);
    let darkflat_cluster_mins = config.clustering.get("darkflat")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(DEFAULT_TIME_CLUSTER_MINUTES);

    // Get per-type temperature thresholds
    let flat_temp_threshold = config.clustering.get("flat")
        .map(|c| c.temp_threshold_celsius)
        .unwrap_or(DEFAULT_TEMP_THRESHOLD);
    let dark_temp_threshold = config.clustering.get("dark")
        .map(|c| c.temp_threshold_celsius)
        .unwrap_or(DEFAULT_TEMP_THRESHOLD);
    let bias_temp_threshold = config.clustering.get("bias")
        .map(|c| c.temp_threshold_celsius)
        .unwrap_or(DEFAULT_TEMP_THRESHOLD);
    let darkflat_temp_threshold = config.clustering.get("darkflat")
        .map(|c| c.temp_threshold_celsius)
        .unwrap_or(DEFAULT_TEMP_THRESHOLD);

    // Derive focallen tolerance from lights→flat config
    // Exact → Some(0.0), Warning → Some(warning_threshold), Ignore → None
    let focallen_tolerance: Option<f64> = config.lights.flat.as_ref()
        .map(|flat_cfg| match flat_cfg.focallen.mode {
            MatchMode::Exact => Some(0.0),
            MatchMode::Warning => Some(flat_cfg.focallen.warning_threshold.unwrap_or(5.0)),
            MatchMode::Ignore => None,
        })
        .unwrap_or(None);

    tracing::debug!(
        flat_cluster_mins,
        flat_temp_threshold,
        dark_cluster_mins,
        dark_temp_threshold,
        bias_cluster_mins,
        bias_temp_threshold,
        darkflat_cluster_mins,
        darkflat_temp_threshold,
        focallen_tolerance = ?focallen_tolerance,
        "clustering thresholds for scan-triggered calibration set creation"
    );

    let mut result = CalibrationScanResult {
        sets_created: 0,
        flat_sets_created: 0,
        dark_sets_created: 0,
        bias_sets_created: 0,
        darkflat_sets_created: 0,
        master_dark_sets_created: 0,
        master_flat_sets_created: 0,
        master_bias_sets_created: 0,
        master_darkflat_sets_created: 0,
    };

    // Process each calibration type with its specific clustering threshold
    if !flat_frame_ids.is_empty() {
        result.flat_sets_created = create_flat_sets_from_frames(conn, &flat_frame_ids, flat_cluster_mins, flat_temp_threshold, focallen_tolerance)?;
        result.sets_created += result.flat_sets_created;
    }

    if !dark_frame_ids.is_empty() {
        result.dark_sets_created = create_dark_sets_from_frames(conn, &dark_frame_ids, "Dark", dark_cluster_mins, dark_temp_threshold)?;
        result.sets_created += result.dark_sets_created;
    }

    if !bias_frame_ids.is_empty() {
        result.bias_sets_created = create_bias_sets_from_frames(conn, &bias_frame_ids, bias_cluster_mins, bias_temp_threshold)?;
        result.sets_created += result.bias_sets_created;
    }

    if !darkflat_frame_ids.is_empty() {
        result.darkflat_sets_created = create_dark_sets_from_frames(conn, &darkflat_frame_ids, "DarkFlat", darkflat_cluster_mins, darkflat_temp_threshold)?;
        result.sets_created += result.darkflat_sets_created;
    }

    // Process master frames (1 file = 1 set, no clustering needed)
    if !master_frame_ids.is_empty() {
        result.master_dark_sets_created = create_master_sets_from_frames(conn, &master_frame_ids.master_dark_ids, "MasterDark")?;
        result.master_flat_sets_created = create_master_sets_from_frames(conn, &master_frame_ids.master_flat_ids, "MasterFlat")?;
        result.master_bias_sets_created = create_master_sets_from_frames(conn, &master_frame_ids.master_bias_ids, "MasterBias")?;
        result.master_darkflat_sets_created = create_master_sets_from_frames(conn, &master_frame_ids.master_darkflat_ids, "MasterDarkFlat")?;

        let total_master_sets = result.master_dark_sets_created
            + result.master_flat_sets_created
            + result.master_bias_sets_created
            + result.master_darkflat_sets_created;
        result.sets_created += total_master_sets;
    }

    tracing::info!(count = result.sets_created, "created total calibration sets from scan");

    Ok(result)
}

/// Query frame data for given frame IDs
fn query_frame_data(conn: &Connection, frame_ids: &[i64]) -> Result<Vec<CalibrationFrameData>> {
    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let query = format!(
        "SELECT id, instrume, filter, binning, gain, offset, exptime, focallen, ccd_temp, set_temp, date_obs
         FROM frames
         WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut stmt = conn.prepare(&query)?;

    // Bind parameters
    for (idx, frame_id) in frame_ids.iter().enumerate() {
        stmt.raw_bind_parameter(idx + 1, frame_id)?;
    }

    let mut frames = Vec::new();
    let mut rows = stmt.raw_query();

    while let Some(row) = rows.next()? {
        let date_obs_str: Option<String> = row.get(10)?;
        let date_obs = date_obs_str.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        frames.push(CalibrationFrameData {
            id: row.get(0)?,
            instrume: row.get(1)?,
            filter: row.get(2)?,
            binning: row.get(3)?,
            gain: row.get(4)?,
            offset: row.get(5)?,
            exptime: row.get(6)?,
            focallen: row.get(7)?,
            ccd_temp: row.get(8)?,
            set_temp: row.get(9)?,
            date_obs,
        });
    }

    Ok(frames)
}

/// Create flat calibration sets from frame IDs
///
/// `focallen_tolerance` controls how focal length is used for grouping:
/// - `Some(0.0)` → exact match (Exact mode)
/// - `Some(tol)` → bucket by tolerance (Warning mode, e.g. 5.0mm)
/// - `None` → ignore focallen in grouping (Ignore mode)
fn create_flat_sets_from_frames(conn: &Connection, frame_ids: &[i64], time_cluster_minutes: i64, temp_threshold: f64, focallen_tolerance: Option<f64>) -> Result<i64> {
    let frames = query_frame_data(conn, frame_ids)?;
    if frames.is_empty() {
        return Ok(0);
    }

    tracing::debug!(count = frames.len(), focallen_tolerance = ?focallen_tolerance, "processing flat frames from scan");

    // Group by exact match parameters
    let mut groups: HashMap<FlatGroupKey, Vec<CalibrationFrameData>> = HashMap::new();

    for frame in frames {
        let key = FlatGroupKey {
            instrume: frame.instrume.clone().unwrap_or_default(),
            filter: frame.filter.clone().unwrap_or_default(),
            binning: frame.binning.clone().unwrap_or_default(),
            gain: frame.gain.map(|g| format!("{:.2}", g)).unwrap_or_default(),
            offset: frame.offset.map(|o| format!("{:.2}", o)).unwrap_or_default(),
            focallen: match focallen_tolerance {
                None => String::new(), // Ignore: all frames group together regardless of focallen
                Some(tol) if tol < 0.01 => frame.focallen.map(|f| format!("{:.1}", f)).unwrap_or_default(), // Exact
                Some(tol) => frame.focallen.map(|f| {
                    let bucket = (f / tol).round() * tol;
                    format!("{:.1}", bucket)
                }).unwrap_or_default(), // Warning: bucket by tolerance
            },
            exptime: frame.exptime.map(|e| format!("{:.2}", e)).unwrap_or_default(),
        };

        groups.entry(key).or_insert_with(Vec::new).push(frame);
    }

    let mut sets_created = 0i64;

    // For each group, cluster by time and temperature, then create sets
    for (key, group_frames) in groups {
        let clusters = cluster_frames_by_time_and_temp(
            group_frames,
            time_cluster_minutes,
            temp_threshold,
        );

        for cluster in clusters {
            let flat_group = create_flat_group_from_cluster(&key, &cluster);
            match create_flat_calibration_set(conn, &flat_group, true, focallen_tolerance) {
                Ok(_set_id) => {
                    sets_created += 1;
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to create flat calibration set from scan");
                }
            }
        }
    }

    tracing::debug!(count = sets_created, "created flat calibration sets from scan");
    Ok(sets_created)
}

/// Create dark calibration sets from frame IDs
fn create_dark_sets_from_frames(conn: &Connection, frame_ids: &[i64], imagetyp: &str, time_cluster_minutes: i64, temp_threshold: f64) -> Result<i64> {
    let frames = query_frame_data(conn, frame_ids)?;
    if frames.is_empty() {
        return Ok(0);
    }

    tracing::debug!(count = frames.len(), imagetyp = %imagetyp.to_lowercase(), "processing dark/darkflat frames from scan");

    // Group by exact match parameters
    let mut groups: HashMap<DarkGroupKey, Vec<CalibrationFrameData>> = HashMap::new();

    for frame in frames {
        let key = DarkGroupKey {
            instrume: frame.instrume.clone().unwrap_or_default(),
            binning: frame.binning.clone().unwrap_or_default(),
            gain: frame.gain.map(|g| format!("{:.2}", g)).unwrap_or_default(),
            offset: frame.offset.map(|o| format!("{:.2}", o)).unwrap_or_default(),
            exptime: frame.exptime.map(|e| format!("{:.2}", e)).unwrap_or_default(),
        };

        groups.entry(key).or_insert_with(Vec::new).push(frame);
    }

    let mut sets_created = 0i64;

    // For each group, cluster by time and temperature, then create sets
    for (key, group_frames) in groups {
        let clusters = cluster_frames_by_time_and_temp(
            group_frames,
            time_cluster_minutes,
            temp_threshold,
        );

        for cluster in clusters {
            let dark_group = create_dark_group_from_cluster(&key, &cluster);
            match create_dark_calibration_set_with_type(conn, &dark_group, imagetyp) {
                Ok(_set_id) => {
                    sets_created += 1;
                }
                Err(e) => {
                    tracing::error!(imagetyp = %imagetyp.to_lowercase(), error = %e, "failed to create calibration set from scan");
                }
            }
        }
    }

    tracing::debug!(count = sets_created, imagetyp = %imagetyp.to_lowercase(), "created calibration sets from scan");
    Ok(sets_created)
}

/// Create bias calibration sets from frame IDs
fn create_bias_sets_from_frames(conn: &Connection, frame_ids: &[i64], time_cluster_minutes: i64, temp_threshold: f64) -> Result<i64> {
    let frames = query_frame_data(conn, frame_ids)?;
    if frames.is_empty() {
        return Ok(0);
    }

    tracing::debug!(count = frames.len(), "processing bias frames from scan");

    // Group by exact match parameters
    let mut groups: HashMap<BiasGroupKey, Vec<CalibrationFrameData>> = HashMap::new();

    for frame in frames {
        let key = BiasGroupKey {
            instrume: frame.instrume.clone().unwrap_or_default(),
            binning: frame.binning.clone().unwrap_or_default(),
            gain: frame.gain.map(|g| format!("{:.2}", g)).unwrap_or_default(),
            offset: frame.offset.map(|o| format!("{:.2}", o)).unwrap_or_default(),
        };

        groups.entry(key).or_insert_with(Vec::new).push(frame);
    }

    let mut sets_created = 0i64;

    // For each group, cluster by time and temperature, then create sets
    for (key, group_frames) in groups {
        let clusters = cluster_frames_by_time_and_temp(
            group_frames,
            time_cluster_minutes,
            temp_threshold,
        );

        for cluster in clusters {
            let bias_group = create_bias_group_from_cluster(&key, &cluster);
            match create_bias_calibration_set(conn, &bias_group, true) {
                Ok(_set_id) => {
                    sets_created += 1;
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to create bias calibration set from scan");
                }
            }
        }
    }

    tracing::debug!(count = sets_created, "created bias calibration sets from scan");
    Ok(sets_created)
}

/// Cluster frames by time proximity and temperature
fn cluster_frames_by_time_and_temp(
    mut frames: Vec<CalibrationFrameData>,
    time_cluster_minutes: i64,
    temp_threshold: f64,
) -> Vec<Vec<CalibrationFrameData>> {
    if frames.is_empty() {
        return Vec::new();
    }

    // Sort by date_obs
    frames.sort_by(|a, b| {
        match (&a.date_obs, &b.date_obs) {
            (Some(da), Some(db)) => da.cmp(db),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    let threshold_seconds = time_cluster_minutes * 60;
    let mut clusters: Vec<Vec<CalibrationFrameData>> = Vec::new();
    let mut current_cluster: Vec<CalibrationFrameData> = Vec::new();

    for frame in frames {
        if current_cluster.is_empty() {
            current_cluster.push(frame);
        } else {
            let last_frame = current_cluster.last().unwrap();

            let should_split = match (&last_frame.date_obs, &frame.date_obs) {
                (Some(last_date), Some(curr_date)) => {
                    let gap_seconds = (*curr_date - *last_date).num_seconds();

                    // Check time gap
                    if gap_seconds > threshold_seconds {
                        true
                    } else {
                        // Check temperature difference
                        match (last_frame.ccd_temp, frame.ccd_temp) {
                            (Some(last_temp), Some(curr_temp)) => {
                                (last_temp - curr_temp).abs() > temp_threshold
                            }
                            _ => false, // Don't split if temperature is missing
                        }
                    }
                }
                _ => false, // Don't split if dates are missing
            };

            if should_split {
                if !current_cluster.is_empty() {
                    clusters.push(current_cluster);
                }
                current_cluster = vec![frame];
            } else {
                current_cluster.push(frame);
            }
        }
    }

    // Don't forget the last cluster
    if !current_cluster.is_empty() {
        clusters.push(current_cluster);
    }

    clusters
}

/// Create a FlatGroup from a cluster of frames
fn create_flat_group_from_cluster(_key: &FlatGroupKey, frames: &[CalibrationFrameData]) -> FlatGroup {
    let frame_ids: Vec<i64> = frames.iter().map(|f| f.id).collect();
    let frame_count = frames.len();

    let start_time = frames.iter()
        .filter_map(|f| f.date_obs)
        .min()
        .unwrap_or_else(Utc::now);

    let end_time = frames.iter()
        .filter_map(|f| f.date_obs)
        .max()
        .unwrap_or_else(Utc::now);

    // A4: prefer ccd_temp, fall back to set_temp so master frames don't
    // collapse into a single no-temperature bucket.
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

    // Get values from first frame to preserve Option<T> (NULL) semantics
    let first_frame = frames.first();
    let gain = first_frame.and_then(|f| f.gain);
    let offset = first_frame.and_then(|f| f.offset);
    let exptime = first_frame.and_then(|f| f.exptime);
    let focal_length = first_frame.and_then(|f| f.focallen);
    let filter = first_frame.and_then(|f| f.filter.clone());
    let instrume = first_frame.and_then(|f| f.instrume.clone());
    let binning = first_frame.and_then(|f| f.binning.clone());

    FlatGroup {
        frame_ids,
        start_time,
        end_time,
        avg_temp,
        temp_min,
        temp_max,
        frame_count,
        filter,
        instrume,
        binning,
        gain,
        offset,
        exptime,
        focal_length,
    }
}

/// Create a DarkGroup from a cluster of frames
fn create_dark_group_from_cluster(_key: &DarkGroupKey, frames: &[CalibrationFrameData]) -> DarkGroup {
    let frame_ids: Vec<i64> = frames.iter().map(|f| f.id).collect();
    let frame_count = frames.len();

    let start_time = frames.iter()
        .filter_map(|f| f.date_obs)
        .min()
        .unwrap_or_else(Utc::now);

    let end_time = frames.iter()
        .filter_map(|f| f.date_obs)
        .max()
        .unwrap_or_else(Utc::now);

    // A4: prefer ccd_temp, fall back to set_temp so master frames don't
    // collapse into a single no-temperature bucket.
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

    // Get values from first frame to preserve Option<T> (NULL) semantics
    let first_frame = frames.first();
    let gain = first_frame.and_then(|f| f.gain);
    let offset = first_frame.and_then(|f| f.offset);
    let exptime = first_frame.and_then(|f| f.exptime);
    let instrume = first_frame.and_then(|f| f.instrume.clone());
    let binning = first_frame.and_then(|f| f.binning.clone());

    DarkGroup {
        frame_ids,
        start_time,
        end_time,
        avg_temp,
        temp_min,
        temp_max,
        frame_count,
        instrume,
        binning,
        gain,
        offset,
        exptime,
        focal_length: None, // Not used for darks
    }
}

/// Create a BiasGroup from a cluster of frames
fn create_bias_group_from_cluster(_key: &BiasGroupKey, frames: &[CalibrationFrameData]) -> BiasGroup {
    let frame_ids: Vec<i64> = frames.iter().map(|f| f.id).collect();
    let frame_count = frames.len();

    let start_time = frames.iter()
        .filter_map(|f| f.date_obs)
        .min()
        .unwrap_or_else(Utc::now);

    let end_time = frames.iter()
        .filter_map(|f| f.date_obs)
        .max()
        .unwrap_or_else(Utc::now);

    // A4: prefer ccd_temp, fall back to set_temp so master frames don't
    // collapse into a single no-temperature bucket.
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

    // Get values from first frame to preserve Option<T> (NULL) semantics
    let first_frame = frames.first();
    let gain = first_frame.and_then(|f| f.gain);
    let offset = first_frame.and_then(|f| f.offset);
    let instrume = first_frame.and_then(|f| f.instrume.clone());
    let binning = first_frame.and_then(|f| f.binning.clone());

    BiasGroup {
        frame_ids,
        start_time,
        end_time,
        avg_temp,
        temp_min,
        temp_max,
        frame_count,
        instrume,
        binning,
        gain,
        offset,
        focal_length: None, // Not used for bias
    }
}

/// Create a dark calibration set with specific imagetyp (Dark or DarkFlat)
fn create_dark_calibration_set_with_type(
    conn: &Connection,
    dark_group: &DarkGroup,
    imagetyp: &str,
) -> Result<i64> {
    // Same guard as the on-demand creation paths: the existence query below
    // excludes superseded rows, so a re-scan of a lineage a master already
    // replaced would fall through and mint a duplicate raw set.
    if let Some(master_id) =
        crate::calibration::superseded_guard::superseding_master_for_frames(conn, &dark_group.frame_ids)?
    {
        tracing::info!(
            master_set_id = master_id,
            frames = dark_group.frame_ids.len(),
            imagetyp,
            "group belongs to a superseded lineage — reusing its master instead of minting a duplicate raw set"
        );
        return Ok(master_id);
    }

    // Check if set already exists with same parameters using date range overlap
    let cluster_start = dark_group.start_time.to_rfc3339();
    let cluster_end = dark_group.end_time.to_rfc3339();

    // Build query with NULL-aware comparisons for ALL parameters
    // Use date range overlap: existing set overlaps with new cluster if
    // existing.date_start <= cluster.end AND existing.date_end >= cluster.start
    let existing_set_id: Option<i64> = {
        let mut query = String::from(
            "SELECT id FROM calibration_set
             WHERE imagetyp = ?1
             AND superseded_by_set_id IS NULL
             AND date_start IS NOT NULL AND date_end IS NOT NULL
             AND date_start <= ?2 AND date_end >= ?3"
        );

        let mut param_count = 3;

        // NULL-aware comparison for binning
        if dark_group.binning.is_some() {
            param_count += 1;
            query.push_str(&format!(" AND binning = ?{}", param_count));
        } else {
            query.push_str(" AND binning IS NULL");
        }

        // NULL-aware comparison for instrume
        if dark_group.instrume.is_some() {
            param_count += 1;
            query.push_str(&format!(" AND instrume = ?{}", param_count));
        } else {
            query.push_str(" AND instrume IS NULL");
        }

        // NULL-aware comparison for exptime - CRITICAL for Dark matching!
        if dark_group.exptime.is_some() {
            param_count += 1;
            query.push_str(&format!(" AND exptime = ?{}", param_count));
        } else {
            query.push_str(" AND exptime IS NULL");
        }

        // NULL-aware comparison for gain
        if dark_group.gain.is_some() {
            param_count += 1;
            query.push_str(&format!(" AND gain = ?{}", param_count));
        } else {
            query.push_str(" AND gain IS NULL");
        }

        // NULL-aware comparison for offset
        if dark_group.offset.is_some() {
            param_count += 1;
            query.push_str(&format!(" AND offset = ?{}", param_count));
        } else {
            query.push_str(" AND offset IS NULL");
        }

        query.push_str(" LIMIT 1");

        // Execute query with dynamic parameter binding
        // On any error, we'll just create a new set (None result triggers new set creation)
        (|| -> Option<i64> {
            let mut stmt = conn.prepare(&query).ok()?;

            let mut param_idx = 1;
            stmt.raw_bind_parameter(param_idx, imagetyp).ok()?;
            param_idx += 1;
            stmt.raw_bind_parameter(param_idx, &cluster_end).ok()?;
            param_idx += 1;
            stmt.raw_bind_parameter(param_idx, &cluster_start).ok()?;
            param_idx += 1;

            if let Some(ref binning) = dark_group.binning {
                stmt.raw_bind_parameter(param_idx, binning).ok()?;
                param_idx += 1;
            }

            if let Some(ref instrume) = dark_group.instrume {
                stmt.raw_bind_parameter(param_idx, instrume).ok()?;
                param_idx += 1;
            }

            if let Some(exptime) = dark_group.exptime {
                stmt.raw_bind_parameter(param_idx, exptime).ok()?;
                param_idx += 1;
            }

            if let Some(gain) = dark_group.gain {
                stmt.raw_bind_parameter(param_idx, gain).ok()?;
                param_idx += 1;
            }

            if let Some(offset) = dark_group.offset {
                stmt.raw_bind_parameter(param_idx, offset).ok()?;
            }

            let mut rows = stmt.raw_query();
            let row = rows.next().ok()??;
            row.get::<_, i64>(0).ok()
        })()
    };

    if let Some(set_id) = existing_set_id {
        tracing::debug!(set_id, imagetyp = %imagetyp.to_lowercase(), "reusing existing calibration set");

        // Link new frames to existing set (using INSERT OR IGNORE to avoid duplicates)
        for frame_id in &dark_group.frame_ids {
            conn.execute(
                "INSERT OR IGNORE INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            )?;
        }

        // Update frame_count to reflect actual linked frames
        conn.execute(
            "UPDATE calibration_set SET frame_count = (
                SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1
            ) WHERE id = ?1",
            rusqlite::params![set_id],
        )?;

        return Ok(set_id);
    }

    // Create new calibration set
    let date = dark_group.start_time.format("%Y-%m-%d").to_string();
    let date_start = dark_group.start_time.to_rfc3339();
    let date_end = dark_group.end_time.to_rfc3339();
    let frame_count = dark_group.frame_ids.len() as i64;

    // Use the real cold/warm range; fall back to avg_temp for groups that
    // pre-date the per-frame range tracking.
    let temp_min = dark_group.temp_min.or(dark_group.avg_temp);
    let temp_max = dark_group.temp_max.or(dark_group.avg_temp);
    conn.execute(
        "INSERT INTO calibration_set
         (imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume, date,
          date_start, date_end, temp_min, temp_max, frame_count, focallen)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
            imagetyp,
            dark_group.exptime,
            dark_group.avg_temp,
            dark_group.gain,
            dark_group.offset,
            dark_group.binning,
            dark_group.instrume,
            date,
            date_start,
            date_end,
            temp_min,
            temp_max,
            frame_count,
            dark_group.focal_length,
        ],
    )?;

    let set_id = conn.last_insert_rowid();

    // Link frames to set
    for frame_id in &dark_group.frame_ids {
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        )?;
    }

    Ok(set_id)
}

/// Create master calibration sets from frame IDs (1 file = 1 set)
///
/// Master frames are already calibrated and don't need sub-calibration.
/// Each master frame becomes its own calibration set with is_master_library = 1.
/// Skips any frame already in a set.
///
/// `pub` since Task 11 (`calibration_library::register::register_master`)
/// calls this directly to guarantee direct-registration and scanner
/// ingestion produce identical master `calibration_set` rows by construction.
pub fn create_master_sets_from_frames(
    conn: &Connection,
    frame_ids: &[i64],
    imagetyp: &str,
) -> Result<i64> {
    if frame_ids.is_empty() {
        return Ok(0);
    }

    tracing::debug!(count = frame_ids.len(), imagetyp, "processing master frames from scan");

    let frames = query_frame_data(conn, frame_ids)?;
    let mut sets_created = 0i64;

    for frame in frames {
        // Check if set already exists for this exact frame (based on date_obs and params)
        let date_obs_str = frame.date_obs.map(|d| d.to_rfc3339());

        // For masters, we check if a set already contains this exact frame
        let existing_set_id: Option<i64> = {
            let mut stmt = conn.prepare(
                "SELECT set_id FROM calibration_set_frames WHERE frame_id = ?1"
            )?;
            let mut rows = stmt.query(rusqlite::params![frame.id])?;
            match rows.next()? {
                Some(row) => Some(row.get(0)?),
                None => None,
            }
        };

        if existing_set_id.is_some() {
            // Frame already in a set, skip
            continue;
        }

        // Create a new calibration set for this single master frame
        let date = frame.date_obs
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let date_start = date_obs_str.clone().unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let date_end = date_start.clone();

        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, filter, ccd_temp, gain, offset, binning, instrume, date,
              date_start, date_end, temp_min, temp_max, frame_count, focallen, is_master_library)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, 1)",
            rusqlite::params![
                imagetyp,
                frame.exptime,
                frame.filter,
                frame.ccd_temp,
                frame.gain,
                frame.offset,
                frame.binning,
                frame.instrume,
                date,
                date_start,
                date_end,
                frame.ccd_temp,
                frame.ccd_temp,
                frame.focallen,
            ],
        )?;

        let set_id = conn.last_insert_rowid();

        // Link the single frame to the set
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame.id],
        )?;

        sets_created += 1;
    }

    tracing::debug!(count = sets_created, imagetyp, "created master calibration sets from scan");
    Ok(sets_created)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seeds a raw dark/darkflat set a master already replaced, plus the master.
    /// Returns the master's set id.
    fn seed_superseded_dark_lineage(
        conn: &Connection,
        raw_id: i64,
        master_id: i64,
        imagetyp: &str,
        frame_ids: &[i64],
    ) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, date_start, date_end, is_master_library)
             VALUES (?1, ?2, '2025-09-25', '2025-09-25T00:00:00+00:00', '2025-09-25T00:10:00+00:00', 0)",
            rusqlite::params![raw_id, imagetyp],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2025-09-25', 1)",
            rusqlite::params![master_id, format!("Master{}", imagetyp)],
        ).unwrap();
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            rusqlite::params![master_id, raw_id],
        ).unwrap();
        for frame_id in frame_ids {
            let file_id = frame_id + 100_000;
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 0, '2025-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![
                    file_id,
                    format!("/test/scan_sup_{}.fits", frame_id),
                    format!("scan_sup_{}.fits", frame_id),
                ],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, ?3)",
                rusqlite::params![frame_id, file_id, imagetyp.to_uppercase()],
            ).unwrap();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![raw_id, frame_id],
            ).unwrap();
        }
        master_id
    }

    fn dark_group_for(frame_ids: Vec<i64>) -> DarkGroup {
        let t = chrono::DateTime::parse_from_rfc3339("2025-09-25T00:00:00+00:00").unwrap().with_timezone(&Utc);
        let frame_count = frame_ids.len();
        DarkGroup {
            frame_ids,
            start_time: t,
            end_time: t,
            avg_temp: Some(-10.0),
            temp_min: Some(-10.0),
            temp_max: Some(-10.0),
            frame_count,
            instrume: Some("TestCamera".to_string()),
            binning: Some("1x1".to_string()),
            gain: Some(56.0),
            offset: Some(50.0),
            exptime: Some(300.0),
            focal_length: None,
        }
    }

    fn set_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM calibration_set", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn scanner_dark_set_creation_reuses_superseding_master_instead_of_minting_duplicate() {
        // C1 regression, scanner Dark path. This function's own existence query
        // excludes superseded rows, so without the guard a re-scan of an already
        // superseded lineage falls through to the INSERT and mints a duplicate
        // raw set beside the master.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let master_id = seed_superseded_dark_lineage(&conn, 40, 41, "Dark", &[400, 401]);
        let before = set_count(&conn);

        let returned =
            create_dark_calibration_set_with_type(&conn, &dark_group_for(vec![400, 401]), "Dark").unwrap();

        assert_eq!(returned, master_id, "must return the superseding master's id");
        assert_eq!(set_count(&conn), before, "no new calibration_set row may be minted");
    }

    #[test]
    fn scanner_darkflat_set_creation_reuses_superseding_master_instead_of_minting_duplicate() {
        // Same path, DarkFlat imagetyp — the scanner routes both through here.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let master_id = seed_superseded_dark_lineage(&conn, 50, 51, "DarkFlat", &[500, 501]);
        let before = set_count(&conn);

        let returned =
            create_dark_calibration_set_with_type(&conn, &dark_group_for(vec![500, 501]), "DarkFlat").unwrap();

        assert_eq!(returned, master_id, "must return the superseding master's id");
        assert_eq!(set_count(&conn), before, "no new calibration_set row may be minted");
    }
}
