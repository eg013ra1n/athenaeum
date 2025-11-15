// Calibration library module
// Manages calibration frames and linking to days/setups

pub mod finder;
pub mod hierarchy;
pub mod auto_create;
pub mod processor;

use crate::models::{CalibrationSet, Frame};
use anyhow::Result;

/// Find matching calibration frames for a light frame
pub fn find_matching_calibrations(
    _frame: &Frame,
    _tolerance: &CalibrationTolerance,
) -> Result<MatchedCalibrations> {
    // TODO: Query for calibration frames matching:
    // - IMAGETYP (Dark, Flat, Bias, DarkFlat)
    // - EXPTIME (for Darks, within tolerance)
    // - FILTER (for Flats)
    // - INSTRUME
    // - GAIN/ISO (within tolerance)
    // - CCD-TEMP (within tolerance)
    // - Date proximity

    unimplemented!("Calibration matching not yet implemented")
}

/// Suggest calibration sets for a capture day
pub fn suggest_calibrations(_date: &str) -> Result<Vec<CalibrationSet>> {
    // TODO: Auto-suggest calibration frames for a given day
    // Group by parameters and suggest matches

    unimplemented!("Calibration suggestion not yet implemented")
}

pub struct CalibrationTolerance {
    pub temp_delta: f64,        // °C
    pub exptime_percent: f64,   // percentage
    pub gain_delta: f64,
}

pub struct MatchedCalibrations {
    pub darks: Vec<Frame>,
    pub flats: Vec<Frame>,
    pub bias: Vec<Frame>,
    pub dark_flats: Vec<Frame>,
}

// ========== Dark Library Clustering ==========

use crate::db::delete_camera_dark_library;
use crate::models::{DarkLibraryResult, ImageType};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::HashMap;

/// Frame data for clustering
#[derive(Debug, Clone)]
struct CalibrationFrame {
    id: i64,
    date_obs: DateTime<Utc>,
    exptime: Option<f64>,
    ccd_temp: Option<f64>,
    gain: Option<f64>,
    offset: Option<f64>,
    binning: Option<String>,
    imagetyp: ImageType,
}

/// Grouping key for exact-match parameters
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct GroupKey {
    exptime: String,  // Stored as string to handle float comparison
    gain: String,
    offset: String,
    binning: String,
    imagetyp: String,
}

/// Cluster of frames with similar dates and temperatures
#[derive(Debug, Clone)]
struct FrameCluster {
    frames: Vec<CalibrationFrame>,
}

impl FrameCluster {
    fn new() -> Self {
        Self { frames: Vec::new() }
    }

    fn add_frame(&mut self, frame: CalibrationFrame) {
        self.frames.push(frame);
    }

    /// Calculate average temperature
    fn avg_temp(&self) -> Option<f64> {
        let temps: Vec<f64> = self
            .frames
            .iter()
            .filter_map(|f| f.ccd_temp)
            .collect();

        if temps.is_empty() {
            return None;
        }

        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    }

    /// Get min and max temperatures
    fn temp_range(&self) -> (Option<f64>, Option<f64>) {
        let temps: Vec<f64> = self
            .frames
            .iter()
            .filter_map(|f| f.ccd_temp)
            .collect();

        if temps.is_empty() {
            return (None, None);
        }

        let min = temps.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

        (Some(min), Some(max))
    }

    /// Get date range
    fn date_range(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let dates: Vec<DateTime<Utc>> = self.frames.iter().map(|f| f.date_obs).collect();
        let min = dates.iter().min().unwrap().clone();
        let max = dates.iter().max().unwrap().clone();
        (min, max)
    }

    /// Get representative date for display (YYYY-MM from first frame)
    fn date_display(&self) -> String {
        if self.frames.is_empty() {
            return String::new();
        }
        self.frames[0].date_obs.format("%Y-%m").to_string()
    }

    /// Get common metadata
    fn get_metadata(&self) -> (Option<f64>, Option<f64>, Option<f64>, Option<String>, ImageType) {
        if self.frames.is_empty() {
            return (None, None, None, None, ImageType::Dark);
        }

        let first = &self.frames[0];
        (
            first.exptime,
            first.gain,
            first.offset,
            first.binning.clone(),
            first.imagetyp.clone(),
        )
    }
}

/// Groups calibration frames by date and temperature thresholds
fn cluster_frames(
    frames: Vec<CalibrationFrame>,
    date_threshold_days: i64,
    temp_threshold: f64,
) -> Vec<FrameCluster> {
    if frames.is_empty() {
        return Vec::new();
    }

    let mut clusters = Vec::new();
    let mut sorted_frames = frames.clone();

    // Sort by date
    sorted_frames.sort_by_key(|f| f.date_obs);

    let mut current_date_cluster = Vec::new();
    let mut last_date: Option<DateTime<Utc>> = None;

    // First pass: cluster by date
    for frame in sorted_frames {
        if let Some(prev_date) = last_date {
            let days_diff = (frame.date_obs - prev_date).num_days();

            if days_diff > date_threshold_days {
                // Start new date cluster
                if !current_date_cluster.is_empty() {
                    let temp_clusters = cluster_by_temperature(current_date_cluster, temp_threshold);
                    clusters.extend(temp_clusters);
                }
                current_date_cluster = Vec::new();
            }
        }

        current_date_cluster.push(frame.clone());
        last_date = Some(frame.date_obs);
    }

    // Process final date cluster
    if !current_date_cluster.is_empty() {
        let temp_clusters = cluster_by_temperature(current_date_cluster, temp_threshold);
        clusters.extend(temp_clusters);
    }

    clusters
}

/// Sub-cluster frames by temperature within a date cluster
fn cluster_by_temperature(
    frames: Vec<CalibrationFrame>,
    temp_threshold: f64,
) -> Vec<FrameCluster> {
    let mut clusters = Vec::new();

    // Separate frames with and without temperature data
    let mut frames_with_temp: Vec<_> = frames
        .iter()
        .filter(|f| f.ccd_temp.is_some())
        .cloned()
        .collect();

    let frames_without_temp: Vec<_> = frames
        .iter()
        .filter(|f| f.ccd_temp.is_none())
        .cloned()
        .collect();

    // Sort frames with temp by temperature
    frames_with_temp.sort_by(|a, b| {
        a.ccd_temp
            .unwrap()
            .partial_cmp(&b.ccd_temp.unwrap())
            .unwrap()
    });

    // Cluster by temperature
    let mut current_cluster = FrameCluster::new();
    let mut last_temp: Option<f64> = None;

    for frame in frames_with_temp {
        if let Some(prev_temp) = last_temp {
            let temp_diff = (frame.ccd_temp.unwrap() - prev_temp).abs();

            if temp_diff > temp_threshold {
                // Start new temperature cluster
                if !current_cluster.frames.is_empty() {
                    clusters.push(current_cluster);
                }
                current_cluster = FrameCluster::new();
            }
        }

        current_cluster.add_frame(frame.clone());
        last_temp = frame.ccd_temp;
    }

    // Add final cluster
    if !current_cluster.frames.is_empty() {
        clusters.push(current_cluster);
    }

    // Create separate cluster for frames without temperature
    if !frames_without_temp.is_empty() {
        let mut no_temp_cluster = FrameCluster::new();
        for frame in frames_without_temp {
            no_temp_cluster.add_frame(frame);
        }
        clusters.push(no_temp_cluster);
    }

    clusters
}

/// Creates dark library for a specific camera
pub fn create_dark_library(
    conn: &Connection,
    instrume: &str,
    date_threshold_days: i64,
    temp_threshold: f64,
) -> Result<DarkLibraryResult> {
    // Delete existing library first
    delete_camera_dark_library(conn, instrume)?;

    // Query all DARK, BIAS, and DARKFLAT frames for this camera
    let mut stmt = conn.prepare(
        "SELECT
            f.id,
            f.date_obs,
            f.exptime,
            f.ccd_temp,
            f.gain,
            f.offset,
            f.binning,
            f.imagetyp
        FROM frames f
        WHERE f.instrume = ?1
        AND f.imagetyp IN ('Dark', 'Bias', 'DarkFlat')
        AND f.date_obs IS NOT NULL
        ORDER BY f.date_obs"
    )?;

    let frames: Vec<CalibrationFrame> = stmt
        .query_map([instrume], |row| {
            let date_obs_str: String = row.get(1)?;
            let date_obs = DateTime::parse_from_rfc3339(&date_obs_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;

            let imagetyp_str: String = row.get(7)?;
            let imagetyp = ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            Ok(CalibrationFrame {
                id: row.get(0)?,
                date_obs,
                exptime: row.get(2)?,
                ccd_temp: row.get(3)?,
                gain: row.get(4)?,
                offset: row.get(5)?,
                binning: row.get(6)?,
                imagetyp,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let total_frames = frames.len() as i64;
    let mut frames_grouped = 0i64;
    let mut sets_created = 0i64;

    // Group frames by exact-match parameters
    let mut groups: HashMap<GroupKey, Vec<CalibrationFrame>> = HashMap::new();

    for frame in frames {
        let key = GroupKey {
            exptime: frame.exptime.map(|e| format!("{:.2}", e)).unwrap_or_default(),
            gain: frame.gain.map(|g| format!("{:.2}", g)).unwrap_or_default(),
            offset: frame.offset.map(|o| format!("{:.2}", o)).unwrap_or_default(),
            binning: frame.binning.clone().unwrap_or_default(),
            imagetyp: format!("{:?}", frame.imagetyp),
        };

        groups.entry(key).or_insert_with(Vec::new).push(frame);
    }

    // Process each group
    for (_key, group_frames) in groups {
        // Cluster by date and temperature
        let clusters = cluster_frames(group_frames, date_threshold_days, temp_threshold);

        // Create calibration sets for each cluster
        for cluster in clusters {
            let (date_start, date_end) = cluster.date_range();
            let avg_temp = cluster.avg_temp().unwrap_or(0.0);
            let (temp_min, temp_max) = cluster.temp_range();
            let (exptime, gain, offset, binning, imagetyp) = cluster.get_metadata();
            let date_display = cluster.date_display();
            let frame_count = cluster.frames.len() as i64;

            // Insert calibration set
            conn.execute(
                "INSERT INTO calibration_set (
                    imagetyp, exptime, ccd_temp, temp_min, temp_max,
                    gain, offset, binning, instrume, date,
                    date_start, date_end, frame_count
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    format!("{:?}", imagetyp),
                    exptime,
                    avg_temp,
                    temp_min,
                    temp_max,
                    gain,
                    offset,
                    binning,
                    instrume,
                    date_display,
                    date_start.to_rfc3339(),
                    date_end.to_rfc3339(),
                    frame_count,
                ],
            )?;

            let set_id = conn.last_insert_rowid();

            // Link frames to set
            for frame in &cluster.frames {
                conn.execute(
                    "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                    rusqlite::params![set_id, frame.id],
                )?;
            }

            frames_grouped += cluster.frames.len() as i64;
            sets_created += 1;
        }
    }

    let frames_excluded = total_frames - frames_grouped;

    Ok(DarkLibraryResult {
        sets_created,
        frames_grouped,
        frames_excluded,
    })
}

/// Creates master dark library for a specific camera
/// This handles master calibration frames (MasterDark, MasterBias, MasterDarkFlat)
pub fn create_master_dark_library(
    conn: &Connection,
    instrume: &str,
    date_threshold_days: i64,
    temp_threshold: f64,
) -> Result<DarkLibraryResult> {
    // Delete existing master library first
    delete_camera_master_dark_library(conn, instrume)?;

    // Query all MASTER DARK and MASTER BIAS frames for this camera
    let mut stmt = conn.prepare(
        "SELECT
            f.id,
            f.date_obs,
            f.exptime,
            f.ccd_temp,
            f.gain,
            f.offset,
            f.binning,
            f.imagetyp
        FROM frames f
        WHERE f.instrume = ?1
        AND f.imagetyp IN ('MasterDark', 'MasterBias', 'MasterDarkFlat')
        AND f.date_obs IS NOT NULL
        ORDER BY f.date_obs"
    )?;

    let frames: Vec<CalibrationFrame> = stmt
        .query_map([instrume], |row| {
            let date_obs_str: String = row.get(1)?;
            let date_obs = DateTime::parse_from_rfc3339(&date_obs_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;

            let imagetyp_str: String = row.get(7)?;
            let imagetyp = ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            Ok(CalibrationFrame {
                id: row.get(0)?,
                date_obs,
                exptime: row.get(2)?,
                ccd_temp: row.get(3)?,
                gain: row.get(4)?,
                offset: row.get(5)?,
                binning: row.get(6)?,
                imagetyp,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let total_frames = frames.len() as i64;
    let mut frames_grouped = 0i64;
    let mut sets_created = 0i64;

    // Group frames by exact-match parameters
    let mut groups: HashMap<GroupKey, Vec<CalibrationFrame>> = HashMap::new();

    for frame in frames {
        let key = GroupKey {
            exptime: frame.exptime.map(|e| format!("{:.2}", e)).unwrap_or_default(),
            gain: frame.gain.map(|g| format!("{:.2}", g)).unwrap_or_default(),
            offset: frame.offset.map(|o| format!("{:.2}", o)).unwrap_or_default(),
            binning: frame.binning.clone().unwrap_or_default(),
            imagetyp: format!("{:?}", frame.imagetyp),
        };

        groups.entry(key).or_insert_with(Vec::new).push(frame);
    }

    // Process each group
    for (_key, group_frames) in groups {
        // Cluster by date and temperature
        let clusters = cluster_frames(group_frames, date_threshold_days, temp_threshold);

        // Create calibration sets for each cluster
        for cluster in clusters {
            let (date_start, date_end) = cluster.date_range();
            let avg_temp = cluster.avg_temp().unwrap_or(0.0);
            let (temp_min, temp_max) = cluster.temp_range();
            let (exptime, gain, offset, binning, imagetyp) = cluster.get_metadata();
            let date_display = cluster.date_display();
            let frame_count = cluster.frames.len() as i64;

            // Insert calibration set with is_master_library = 1
            conn.execute(
                "INSERT INTO calibration_set (
                    imagetyp, exptime, ccd_temp, temp_min, temp_max,
                    gain, offset, binning, instrume, date,
                    date_start, date_end, frame_count, is_master_library
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1)",
                rusqlite::params![
                    format!("{:?}", imagetyp),
                    exptime,
                    avg_temp,
                    temp_min,
                    temp_max,
                    gain,
                    offset,
                    binning,
                    instrume,
                    date_display,
                    date_start.to_rfc3339(),
                    date_end.to_rfc3339(),
                    frame_count,
                ],
            )?;

            let set_id = conn.last_insert_rowid();

            // Link frames to set
            for frame in &cluster.frames {
                conn.execute(
                    "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                    rusqlite::params![set_id, frame.id],
                )?;
            }

            frames_grouped += cluster.frames.len() as i64;
            sets_created += 1;
        }
    }

    let frames_excluded = total_frames - frames_grouped;

    Ok(DarkLibraryResult {
        sets_created,
        frames_grouped,
        frames_excluded,
    })
}

/// Delete camera's master dark library
fn delete_camera_master_dark_library(conn: &Connection, instrume: &str) -> Result<()> {
    // Delete all calibration sets for this camera that are master library sets
    conn.execute(
        "DELETE FROM calibration_set WHERE instrume = ?1 AND is_master_library = 1",
        rusqlite::params![instrume],
    )?;
    Ok(())
}
