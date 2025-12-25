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
    println!("🔧 Creating calibration sets from scan:");
    println!("   Flats: {}, Darks: {}, Bias: {}, DarkFlats: {}",
        flat_frame_ids.len(), dark_frame_ids.len(), bias_frame_ids.len(), darkflat_frame_ids.len());
    if !master_frame_ids.is_empty() {
        println!("   Masters: {} total (Dark: {}, Flat: {}, Bias: {}, DarkFlat: {})",
            master_frame_ids.total_count(),
            master_frame_ids.master_dark_ids.len(),
            master_frame_ids.master_flat_ids.len(),
            master_frame_ids.master_bias_ids.len(),
            master_frame_ids.master_darkflat_ids.len());
    }

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

    println!("   Clustering thresholds: flat={}min/{}°C, dark={}min/{}°C, bias={}min/{}°C, darkflat={}min/{}°C",
        flat_cluster_mins, flat_temp_threshold, dark_cluster_mins, dark_temp_threshold,
        bias_cluster_mins, bias_temp_threshold, darkflat_cluster_mins, darkflat_temp_threshold);

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
        result.flat_sets_created = create_flat_sets_from_frames(conn, &flat_frame_ids, flat_cluster_mins, flat_temp_threshold)?;
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

    println!("✅ Created {} total calibration sets", result.sets_created);

    Ok(result)
}

/// Query frame data for given frame IDs
fn query_frame_data(conn: &Connection, frame_ids: &[i64]) -> Result<Vec<CalibrationFrameData>> {
    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let query = format!(
        "SELECT id, instrume, filter, binning, gain, offset, exptime, focallen, ccd_temp, date_obs
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
        let date_obs_str: Option<String> = row.get(9)?;
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
            date_obs,
        });
    }

    Ok(frames)
}

/// Create flat calibration sets from frame IDs
fn create_flat_sets_from_frames(conn: &Connection, frame_ids: &[i64], time_cluster_minutes: i64, temp_threshold: f64) -> Result<i64> {
    let frames = query_frame_data(conn, frame_ids)?;
    if frames.is_empty() {
        return Ok(0);
    }

    println!("   📸 Processing {} flat frames", frames.len());

    // Group by exact match parameters
    let mut groups: HashMap<FlatGroupKey, Vec<CalibrationFrameData>> = HashMap::new();

    for frame in frames {
        let key = FlatGroupKey {
            instrume: frame.instrume.clone().unwrap_or_default(),
            filter: frame.filter.clone().unwrap_or_default(),
            binning: frame.binning.clone().unwrap_or_default(),
            gain: frame.gain.map(|g| format!("{:.2}", g)).unwrap_or_default(),
            offset: frame.offset.map(|o| format!("{:.2}", o)).unwrap_or_default(),
            focallen: frame.focallen.map(|f| format!("{:.1}", f)).unwrap_or_default(),
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
            match create_flat_calibration_set(conn, &flat_group) {
                Ok(_set_id) => {
                    sets_created += 1;
                }
                Err(e) => {
                    println!("   ⚠️  Failed to create flat set: {}", e);
                }
            }
        }
    }

    println!("   ✅ Created {} flat calibration sets", sets_created);
    Ok(sets_created)
}

/// Create dark calibration sets from frame IDs
fn create_dark_sets_from_frames(conn: &Connection, frame_ids: &[i64], imagetyp: &str, time_cluster_minutes: i64, temp_threshold: f64) -> Result<i64> {
    let frames = query_frame_data(conn, frame_ids)?;
    if frames.is_empty() {
        return Ok(0);
    }

    println!("   🌑 Processing {} {} frames", frames.len(), imagetyp.to_lowercase());

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
                    println!("   ⚠️  Failed to create {} set: {}", imagetyp.to_lowercase(), e);
                }
            }
        }
    }

    println!("   ✅ Created {} {} calibration sets", sets_created, imagetyp.to_lowercase());
    Ok(sets_created)
}

/// Create bias calibration sets from frame IDs
fn create_bias_sets_from_frames(conn: &Connection, frame_ids: &[i64], time_cluster_minutes: i64, temp_threshold: f64) -> Result<i64> {
    let frames = query_frame_data(conn, frame_ids)?;
    if frames.is_empty() {
        return Ok(0);
    }

    println!("   ⚡ Processing {} bias frames", frames.len());

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
            match create_bias_calibration_set(conn, &bias_group) {
                Ok(_set_id) => {
                    sets_created += 1;
                }
                Err(e) => {
                    println!("   ⚠️  Failed to create bias set: {}", e);
                }
            }
        }
    }

    println!("   ✅ Created {} bias calibration sets", sets_created);
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

    let temps: Vec<f64> = frames.iter().filter_map(|f| f.ccd_temp).collect();
    let avg_temp = if !temps.is_empty() {
        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    } else {
        None
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

    let temps: Vec<f64> = frames.iter().filter_map(|f| f.ccd_temp).collect();
    let avg_temp = if !temps.is_empty() {
        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    } else {
        None
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

    let temps: Vec<f64> = frames.iter().filter_map(|f| f.ccd_temp).collect();
    let avg_temp = if !temps.is_empty() {
        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    } else {
        None
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
        println!("    ♻️  Reusing existing {} calibration set ID: {}", imagetyp.to_lowercase(), set_id);

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
            dark_group.avg_temp,
            dark_group.avg_temp,
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
fn create_master_sets_from_frames(
    conn: &Connection,
    frame_ids: &[i64],
    imagetyp: &str,
) -> Result<i64> {
    if frame_ids.is_empty() {
        return Ok(0);
    }

    println!("   ⭐ Processing {} {} frames", frame_ids.len(), imagetyp);

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

    println!("   ✅ Created {} {} calibration sets", sets_created, imagetyp);
    Ok(sets_created)
}
