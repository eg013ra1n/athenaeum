//! Data collector for export operations
//!
//! Collects light frames from a frame set and their linked calibrations
//! to prepare data for export.

use crate::export::models::{
    CalibrationSetInfo, CalibrationSubgroup, CalibrationSummary, CameraType, ExportCalibrationSet,
    ExportData, ExportFrame, ExportGroup, FilterExportGroup, MasterCreationPlan, MasterInfo,
};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

/// Collect all export data for a frame set
///
/// This traverses the frame set hierarchy to get all light frames,
/// groups them by filter and camera type, and retrieves their calibration links.
pub fn collect_export_data(conn: &Connection, frame_set_id: i64) -> Result<ExportData> {
    println!("📦 Collecting export data for frame set {}", frame_set_id);

    // Get frame set info
    let (frame_set_name, object_name) = get_frame_set_info(conn, frame_set_id)?;
    println!("  Frame set name: {}, object: {:?}", frame_set_name, object_name);

    // Get all light frames from the frame set
    let light_frames = get_light_frames_for_frame_set(conn, frame_set_id)?;
    println!("  Found {} light frames", light_frames.len());

    // =========================================================================
    // Phase 3: Build new export groups with subgroups
    // =========================================================================
    let groups = build_export_groups(conn, &light_frames)?;
    let master_plan = build_master_creation_plan(conn, &groups)?;

    println!("  Built {} export groups with {} masters to create",
             groups.len(), master_plan.masters.len());

    // =========================================================================
    // Legacy: Build filter groups for backwards compatibility
    // =========================================================================
    let mut filter_groups: HashMap<Option<String>, Vec<ExportFrame>> = HashMap::new();
    for frame in &light_frames {
        filter_groups
            .entry(frame.filter.clone())
            .or_default()
            .push(frame.clone());
    }

    // Build filter export groups with calibrations (legacy format)
    let mut filters = Vec::new();
    let mut total_flat_count = 0;
    let mut total_dark_count = 0;
    let mut total_bias_count = 0;
    let mut total_dark_flat_count = 0;
    let mut all_warnings = Vec::new();
    let mut flats_complete = true;
    let mut darks_complete = true;
    let bias_complete = true;

    for (filter, frames) in filter_groups {
        // For each filter group, collect calibrations from the first frame
        // (calibrations are typically shared within a filter group)
        let (flat_sets, dark_sets, bias_sets) = if let Some(first_frame) = frames.first() {
            collect_calibrations_for_frame(conn, first_frame.frame_id)?
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

        // Update summary counts
        for flat in &flat_sets {
            total_flat_count += flat.frames.len() as i32;
            for sub in &flat.sub_calibrations {
                if sub.imagetyp == "DarkFlat" {
                    total_dark_flat_count += sub.frames.len() as i32;
                } else if sub.imagetyp == "Dark" {
                    total_dark_count += sub.frames.len() as i32;
                } else if sub.imagetyp == "Bias" {
                    total_bias_count += sub.frames.len() as i32;
                }
            }
            all_warnings.extend(flat.warnings.clone());
        }

        for dark in &dark_sets {
            total_dark_count += dark.frames.len() as i32;
            for sub in &dark.sub_calibrations {
                if sub.imagetyp == "Bias" {
                    total_bias_count += sub.frames.len() as i32;
                }
            }
            all_warnings.extend(dark.warnings.clone());
        }

        for bias in &bias_sets {
            total_bias_count += bias.frames.len() as i32;
            all_warnings.extend(bias.warnings.clone());
        }

        // Check completeness
        if flat_sets.is_empty() {
            flats_complete = false;
        }
        if dark_sets.is_empty() {
            darks_complete = false;
        }
        // Bias is optional, so we don't track completeness

        filters.push(FilterExportGroup {
            filter,
            light_frames: frames,
            flat_sets,
            dark_sets,
            bias_sets,
        });
    }

    // Sort filters alphabetically (None first, then by name)
    filters.sort_by(|a, b| match (&a.filter, &b.filter) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(b),
    });

    // Calculate totals
    let total_light_frames: i32 = filters.iter().map(|f| f.light_frames.len() as i32).sum();
    let total_exposure_seconds: f64 = filters
        .iter()
        .flat_map(|f| &f.light_frames)
        .filter_map(|f| f.exptime)
        .sum();

    Ok(ExportData {
        frame_set_id,
        frame_set_name,
        object_name,
        groups,
        master_plan,
        filters,
        calibration_summary: CalibrationSummary {
            flat_count: total_flat_count,
            dark_count: total_dark_count,
            bias_count: total_bias_count,
            dark_flat_count: total_dark_flat_count,
            flats_complete,
            darks_complete,
            bias_complete,
            warnings: all_warnings,
        },
        total_light_frames,
        total_exposure_seconds,
    })
}

/// Get frame set name and object name
fn get_frame_set_info(conn: &Connection, frame_set_id: i64) -> Result<(String, Option<String>)> {
    let result = conn.query_row(
        "SELECT name FROM frames_set WHERE id = ?1",
        [frame_set_id],
        |row| {
            let name: Option<String> = row.get(0)?;
            Ok(name.unwrap_or_else(|| format!("Frame Set {}", frame_set_id)))
        },
    )?;

    // Get object name from first light frame in the set
    let object_name: Option<String> = conn
        .query_row(
            "SELECT f.object
             FROM frames f
             JOIN session_members sm ON f.id = sm.frame_id
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights i ON s.imaging_night_id = i.id
             WHERE i.frames_set_id = ?1
               AND f.imagetyp = 'Light'
               AND f.object IS NOT NULL
             LIMIT 1",
            [frame_set_id],
            |row| row.get(0),
        )
        .ok();

    Ok((result, object_name))
}

/// Get all light frames from a frame set via the hierarchy
fn get_light_frames_for_frame_set(conn: &Connection, frame_set_id: i64) -> Result<Vec<ExportFrame>> {
    // First get all frame IDs using a simpler query (proven pattern from calibration_links.rs)
    let mut id_stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         JOIN frames f ON sm.frame_id = f.id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'"
    )?;

    let frame_ids: Vec<i64> = id_stmt
        .query_map([frame_set_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    println!("  Found {} frame IDs via session_members", frame_ids.len());

    // Now get full frame info for each ID
    let mut frames = Vec::new();
    for frame_id in &frame_ids {
        if let Ok(frame) = get_export_frame_by_id(conn, *frame_id) {
            frames.push(frame);
        }
    }

    println!("  Loaded {} full frame records", frames.len());
    Ok(frames)
}

/// Get a single frame with file info by ID
fn get_export_frame_by_id(conn: &Connection, frame_id: i64) -> Result<ExportFrame> {
    conn.query_row(
        "SELECT f.id, f.file_id, fi.path, fi.filename,
                f.exptime, f.filter, f.ccd_temp, f.gain, f.offset,
                f.binning, f.date_obs, f.focallen, f.bayerpat, f.instrume
         FROM frames f
         JOIN files fi ON f.file_id = fi.id
         WHERE f.id = ?1",
        [frame_id],
        |row| {
            Ok(ExportFrame {
                frame_id: row.get(0)?,
                file_id: row.get(1)?,
                file_path: row.get(2)?,
                filename: row.get(3)?,
                exptime: row.get(4)?,
                filter: row.get(5)?,
                ccd_temp: row.get(6)?,
                gain: row.get(7)?,
                offset: row.get(8)?,
                binning: row.get(9)?,
                date_obs: row.get(10)?,
                focallen: row.get(11)?,
                bayerpat: row.get(12)?,
                instrume: row.get(13)?,
            })
        },
    ).context(format!("Failed to get frame by ID: {}", frame_id))
}

/// Collect calibrations for a single frame
/// Returns (flats, darks, bias)
fn collect_calibrations_for_frame(
    conn: &Connection,
    frame_id: i64,
) -> Result<(
    Vec<ExportCalibrationSet>,
    Vec<ExportCalibrationSet>,
    Vec<ExportCalibrationSet>,
)> {
    println!("  📋 Collecting calibrations for frame {}", frame_id);
    let mut flat_sets = Vec::new();
    let mut dark_sets = Vec::new();
    let mut bias_sets = Vec::new();

    // Get calibration links for this frame
    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score,
                date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'",
    )?;

    let links: Vec<(i64, String, Option<f64>, bool, bool)> = stmt
        .query_map([frame_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i32>(3)? != 0,
                row.get::<_, i32>(4)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    println!("    Found {} calibration links", links.len());
    for (set_id, cal_type, match_score, date_warning, temp_warning) in links {
        println!("    - {} set_id={} score={:?}", cal_type, set_id, match_score);
        let cal_set = build_calibration_set(conn, set_id, match_score, date_warning, temp_warning)?;

        match cal_type.as_str() {
            "Flat" => flat_sets.push(cal_set),
            "Dark" => dark_sets.push(cal_set),
            "Bias" => bias_sets.push(cal_set),
            _ => {}
        }
    }

    println!("    Result: {} flats, {} darks, {} bias", flat_sets.len(), dark_sets.len(), bias_sets.len());
    Ok((flat_sets, dark_sets, bias_sets))
}

/// Build an ExportCalibrationSet with its frames and sub-calibrations
fn build_calibration_set(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<ExportCalibrationSet> {
    // Get calibration set info
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    // Get frames in the calibration set
    let frames = get_calibration_set_frames(conn, set_id)?;

    // Build warnings list
    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    // Get sub-calibrations (recursively)
    let sub_calibrations = get_sub_calibrations(conn, set_id)?;

    Ok(ExportCalibrationSet {
        set_id,
        imagetyp,
        frames,
        sub_calibrations,
        match_score,
        warnings,
    })
}

/// Get frames belonging to a calibration set
fn get_calibration_set_frames(conn: &Connection, set_id: i64) -> Result<Vec<ExportFrame>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.file_id, fi.path, fi.filename,
                f.exptime, f.filter, f.ccd_temp, f.gain, f.offset,
                f.binning, f.date_obs, f.focallen, f.bayerpat, f.instrume
         FROM frames f
         JOIN files fi ON f.file_id = fi.id
         JOIN calibration_set_frames csf ON f.id = csf.frame_id
         WHERE csf.set_id = ?1
         ORDER BY f.date_obs ASC",
    )?;

    let frames = stmt
        .query_map([set_id], |row| {
            Ok(ExportFrame {
                frame_id: row.get(0)?,
                file_id: row.get(1)?,
                file_path: row.get(2)?,
                filename: row.get(3)?,
                exptime: row.get(4)?,
                filter: row.get(5)?,
                ccd_temp: row.get(6)?,
                gain: row.get(7)?,
                offset: row.get(8)?,
                binning: row.get(9)?,
                date_obs: row.get(10)?,
                focallen: row.get(11)?,
                bayerpat: row.get(12)?,
                instrume: row.get(13)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(frames)
}

/// Get sub-calibrations for a calibration set (e.g., Dark for Flat, Bias for Dark)
fn get_sub_calibrations(conn: &Connection, set_id: i64) -> Result<Vec<ExportCalibrationSet>> {
    let mut sub_calibrations = Vec::new();

    // Get calibration links where this set is the source
    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score,
                date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'",
    )?;

    let links: Vec<(i64, String, Option<f64>, bool, bool)> = stmt
        .query_map([set_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i32>(3)? != 0,
                row.get::<_, i32>(4)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for (sub_set_id, _cal_type, match_score, date_warning, temp_warning) in links {
        // Recursively build sub-calibration (without further recursion to avoid infinite loops)
        let sub_set = build_calibration_set_shallow(conn, sub_set_id, match_score, date_warning, temp_warning)?;
        sub_calibrations.push(sub_set);
    }

    Ok(sub_calibrations)
}

/// Build a calibration set without recursive sub-calibrations (to avoid deep recursion)
fn build_calibration_set_shallow(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<ExportCalibrationSet> {
    // Get calibration set info
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    // Get frames
    let frames = get_calibration_set_frames(conn, set_id)?;

    // Build warnings
    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    Ok(ExportCalibrationSet {
        set_id,
        imagetyp,
        frames,
        sub_calibrations: Vec::new(), // No recursive sub-calibrations
        match_score,
        warnings,
    })
}

// ============================================================================
// Phase 3: New Export Group Building Logic
// ============================================================================

/// Calibration links for a single frame
#[derive(Debug, Clone)]
struct FrameCalibrationLinks {
    #[allow(dead_code)]
    frame_id: i64,
    flat_id: Option<i64>,
    dark_id: Option<i64>,
    bias_id: Option<i64>,
}

impl FrameCalibrationLinks {
    /// Generate a subgroup key from calibration set IDs
    fn subgroup_key(&self) -> String {
        format!(
            "f{}_d{}_b{}",
            self.flat_id.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string()),
            self.dark_id.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string()),
            self.bias_id.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string()),
        )
    }
}

/// Get calibration links for a single frame
fn get_frame_calibration_links(conn: &Connection, frame_id: i64) -> Result<FrameCalibrationLinks> {
    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'",
    )?;

    let links: Vec<(i64, String)> = stmt
        .query_map([frame_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut flat_id = None;
    let mut dark_id = None;
    let mut bias_id = None;

    for (set_id, cal_type) in links {
        match cal_type.as_str() {
            "Flat" => flat_id = Some(set_id),
            "Dark" => dark_id = Some(set_id),
            "Bias" => bias_id = Some(set_id),
            _ => {}
        }
    }

    Ok(FrameCalibrationLinks {
        frame_id,
        flat_id,
        dark_id,
        bias_id,
    })
}

/// Build CalibrationSetInfo with recursive sub-calibrations
fn build_calibration_set_info(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<CalibrationSetInfo> {
    // Get calibration set info
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    // Get frames in the calibration set
    let frames = get_calibration_set_frames(conn, set_id)?;
    let frame_count = frames.len() as i32;

    // Build warnings list
    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    // Get sub-calibrations for this set
    let mut dark_flat: Option<Box<CalibrationSetInfo>> = None;
    let mut dark: Option<Box<CalibrationSetInfo>> = None;
    let mut bias: Option<Box<CalibrationSetInfo>> = None;

    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score, date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'",
    )?;

    let sub_links: Vec<(i64, String, Option<f64>, bool, bool)> = stmt
        .query_map([set_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i32>(3)? != 0,
                row.get::<_, i32>(4)? != 0,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    for (sub_set_id, cal_type, sub_score, sub_date_warn, sub_temp_warn) in sub_links {
        // Build sub-calibration (one level deep to avoid infinite recursion)
        let sub_info = build_calibration_set_info_shallow(conn, sub_set_id, sub_score, sub_date_warn, sub_temp_warn)?;

        match cal_type.as_str() {
            "DarkFlat" => dark_flat = Some(Box::new(sub_info)),
            "Dark" => dark = Some(Box::new(sub_info)),
            "Bias" => bias = Some(Box::new(sub_info)),
            _ => {}
        }
    }

    Ok(CalibrationSetInfo {
        set_id,
        imagetyp,
        frames,
        frame_count,
        dark_flat,
        dark,
        bias,
        match_score,
        warnings,
    })
}

/// Build CalibrationSetInfo without deep recursion (for sub-calibrations)
fn build_calibration_set_info_shallow(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<CalibrationSetInfo> {
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    let frames = get_calibration_set_frames(conn, set_id)?;
    let frame_count = frames.len() as i32;

    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    // Get sub-calibrations (bias for darks)
    let mut bias: Option<Box<CalibrationSetInfo>> = None;

    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score, date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set' AND calibration_type = 'Bias'",
    )?;

    if let Ok((bias_id, _, bias_score, bias_date, bias_temp)) = stmt.query_row([set_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, i32>(3)? != 0,
            row.get::<_, i32>(4)? != 0,
        ))
    }) {
        // Build bias (terminal node, no further sub-calibrations)
        let bias_info = build_calibration_set_info_terminal(conn, bias_id, bias_score, bias_date, bias_temp)?;
        bias = Some(Box::new(bias_info));
    }

    Ok(CalibrationSetInfo {
        set_id,
        imagetyp,
        frames,
        frame_count,
        dark_flat: None,
        dark: None,
        bias,
        match_score,
        warnings,
    })
}

/// Build CalibrationSetInfo as terminal node (no sub-calibrations)
fn build_calibration_set_info_terminal(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<CalibrationSetInfo> {
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    let frames = get_calibration_set_frames(conn, set_id)?;
    let frame_count = frames.len() as i32;

    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    Ok(CalibrationSetInfo {
        set_id,
        imagetyp,
        frames,
        frame_count,
        dark_flat: None,
        dark: None,
        bias: None,
        match_score,
        warnings,
    })
}

/// Get CalibrationSetInfo for a frame's calibration link
fn get_calibration_set_info_for_frame(
    conn: &Connection,
    frame_id: i64,
    cal_type: &str,
) -> Result<Option<CalibrationSetInfo>> {
    let result: Option<(i64, Option<f64>, bool, bool)> = conn
        .query_row(
            "SELECT calibration_set_id, match_score, date_warning, temp_warning
             FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = 'frame' AND calibration_type = ?2",
            rusqlite::params![frame_id, cal_type],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, i32>(2)? != 0,
                    row.get::<_, i32>(3)? != 0,
                ))
            },
        )
        .ok();

    match result {
        Some((set_id, match_score, date_warning, temp_warning)) => {
            let info = build_calibration_set_info(conn, set_id, match_score, date_warning, temp_warning)?;
            Ok(Some(info))
        }
        None => Ok(None),
    }
}

/// Build export groups from light frames
/// Groups by (filter, camera_type) and creates subgroups by calibration links
fn build_export_groups(conn: &Connection, light_frames: &[ExportFrame]) -> Result<Vec<ExportGroup>> {
    println!("  📊 Building export groups from {} light frames", light_frames.len());

    // Group key: (filter, camera_type)
    type GroupKey = (Option<String>, CameraType);
    let mut groups_map: HashMap<GroupKey, Vec<&ExportFrame>> = HashMap::new();

    // Group frames by filter and camera type
    for frame in light_frames {
        let camera_type = CameraType::from_bayerpat(frame.bayerpat.as_deref());
        let key = (frame.filter.clone(), camera_type);
        groups_map.entry(key).or_default().push(frame);
    }

    println!("  Found {} distinct (filter, camera_type) groups", groups_map.len());

    let mut export_groups = Vec::new();

    for ((filter, camera_type), frames) in groups_map {
        let group_key = ExportGroup::make_group_key(filter.as_deref(), &camera_type);
        let display_name = ExportGroup::make_display_name(filter.as_deref(), &camera_type);

        println!("  Building group: {} with {} frames", display_name, frames.len());

        // Build subgroups within this group
        let subgroups = build_calibration_subgroups(conn, &frames)?;

        // Calculate totals
        let total_frames = frames.len() as i32;
        let total_exposure: f64 = frames.iter().filter_map(|f| f.exptime).sum();

        // Collect warnings
        let mut warnings = Vec::new();
        for subgroup in &subgroups {
            warnings.extend(subgroup.warnings.clone());
        }

        export_groups.push(ExportGroup {
            group_key,
            filter,
            camera_type,
            display_name,
            subgroups,
            total_frames,
            total_exposure,
            warnings,
        });
    }

    // Sort groups by display name for consistent ordering
    export_groups.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    Ok(export_groups)
}

/// Build calibration subgroups within an export group
/// Groups frames by their linked calibration set IDs
fn build_calibration_subgroups(
    conn: &Connection,
    frames: &[&ExportFrame],
) -> Result<Vec<CalibrationSubgroup>> {
    // Get calibration links for each frame
    let mut frame_links: HashMap<i64, FrameCalibrationLinks> = HashMap::new();
    for frame in frames {
        let links = get_frame_calibration_links(conn, frame.frame_id)?;
        frame_links.insert(frame.frame_id, links);
    }

    // Group frames by subgroup key (combination of calibration set IDs)
    let mut subgroup_map: HashMap<String, Vec<&ExportFrame>> = HashMap::new();
    for frame in frames {
        if let Some(links) = frame_links.get(&frame.frame_id) {
            let key = links.subgroup_key();
            subgroup_map.entry(key).or_default().push(*frame);
        }
    }

    let subgroup_count = subgroup_map.len();
    println!("    Found {} calibration subgroups", subgroup_count);

    let mut subgroups = Vec::new();
    let mut subgroup_index = 1;

    for (subgroup_key, subgroup_frames) in subgroup_map {
        // Get the calibration links from the first frame (all frames in subgroup share the same links)
        let first_frame = subgroup_frames.first().unwrap();
        let links = frame_links.get(&first_frame.frame_id).unwrap();

        // Build CalibrationSetInfo for each calibration type
        let flat = if links.flat_id.is_some() {
            get_calibration_set_info_for_frame(conn, first_frame.frame_id, "Flat")?
        } else {
            None
        };

        let dark = if links.dark_id.is_some() {
            get_calibration_set_info_for_frame(conn, first_frame.frame_id, "Dark")?
        } else {
            None
        };

        let bias = if links.bias_id.is_some() {
            get_calibration_set_info_for_frame(conn, first_frame.frame_id, "Bias")?
        } else {
            None
        };

        // Generate display name
        let display_name = if subgroup_count == 1 {
            "Default".to_string()
        } else {
            format!("Subgroup {}", subgroup_index)
        };

        // Collect warnings
        let mut warnings = Vec::new();
        if let Some(ref f) = flat {
            warnings.extend(f.warnings.clone());
        }
        if let Some(ref d) = dark {
            warnings.extend(d.warnings.clone());
        }
        if let Some(ref b) = bias {
            warnings.extend(b.warnings.clone());
        }

        subgroups.push(CalibrationSubgroup {
            subgroup_key,
            display_name,
            frames: subgroup_frames.iter().map(|f| (*f).clone()).collect(),
            flat,
            dark,
            bias,
            warnings,
        });

        subgroup_index += 1;
    }

    // Sort subgroups by display name
    subgroups.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    Ok(subgroups)
}

/// Build the master creation plan from export groups
/// Collects all unique calibration sets and topologically sorts by dependencies
fn build_master_creation_plan(
    conn: &Connection,
    groups: &[ExportGroup],
) -> Result<MasterCreationPlan> {
    println!("  🔧 Building master creation plan");

    // Collect all unique calibration set IDs with their types
    let mut all_sets: HashMap<i64, String> = HashMap::new(); // set_id -> imagetyp
    let mut dependencies: HashMap<i64, Vec<i64>> = HashMap::new(); // set_id -> depends on set_ids

    // Traverse all groups and subgroups to collect calibration sets
    for group in groups {
        for subgroup in &group.subgroups {
            // Collect from flat
            if let Some(ref flat) = subgroup.flat {
                collect_calibration_sets_recursive(flat, &mut all_sets, &mut dependencies);
            }
            // Collect from dark
            if let Some(ref dark) = subgroup.dark {
                collect_calibration_sets_recursive(dark, &mut all_sets, &mut dependencies);
            }
            // Collect from bias
            if let Some(ref bias) = subgroup.bias {
                collect_calibration_sets_recursive(bias, &mut all_sets, &mut dependencies);
            }
        }
    }

    println!("    Found {} unique calibration sets", all_sets.len());

    // Topological sort to determine creation order
    let sorted_ids = topological_sort(&all_sets, &dependencies);

    println!("    Topological sort complete, {} masters to create", sorted_ids.len());

    // Build MasterInfo for each set
    let mut masters = Vec::new();
    let mut master_paths: HashMap<i64, String> = HashMap::new();

    for set_id in sorted_ids {
        if let Some(imagetyp) = all_sets.get(&set_id) {
            // Get frames for this set
            let frames = get_calibration_set_frames(conn, set_id)?;

            // Determine dependencies
            let deps = dependencies.get(&set_id).cloned().unwrap_or_default();

            // Determine which calibrations to apply
            let (apply_bias, apply_dark) = get_calibration_applications(conn, set_id)?;

            // Generate output filename
            let output_name = format!("master_{}_{}.fit", imagetyp.to_lowercase(), set_id);
            master_paths.insert(set_id, output_name.clone());

            masters.push(MasterInfo {
                set_id,
                master_type: imagetyp.clone(),
                output_name,
                source_frames: frames,
                depends_on: deps,
                apply_bias,
                apply_dark,
            });
        }
    }

    Ok(MasterCreationPlan {
        masters,
        master_paths,
    })
}

/// Recursively collect calibration sets from a CalibrationSetInfo hierarchy
fn collect_calibration_sets_recursive(
    info: &CalibrationSetInfo,
    all_sets: &mut HashMap<i64, String>,
    dependencies: &mut HashMap<i64, Vec<i64>>,
) {
    // Add this set
    all_sets.insert(info.set_id, info.imagetyp.clone());

    // Track dependencies
    let mut deps = Vec::new();

    // Recurse into sub-calibrations
    if let Some(ref dark_flat) = info.dark_flat {
        deps.push(dark_flat.set_id);
        collect_calibration_sets_recursive(dark_flat, all_sets, dependencies);
    }
    if let Some(ref dark) = info.dark {
        deps.push(dark.set_id);
        collect_calibration_sets_recursive(dark, all_sets, dependencies);
    }
    if let Some(ref bias) = info.bias {
        deps.push(bias.set_id);
        collect_calibration_sets_recursive(bias, all_sets, dependencies);
    }

    if !deps.is_empty() {
        dependencies.insert(info.set_id, deps);
    }
}

/// Topological sort of calibration sets by dependencies
/// Returns set IDs in order they should be created (dependencies first)
fn topological_sort(
    all_sets: &HashMap<i64, String>,
    dependencies: &HashMap<i64, Vec<i64>>,
) -> Vec<i64> {
    let mut result = Vec::new();
    let mut visited: HashSet<i64> = HashSet::new();
    let mut temp_mark: HashSet<i64> = HashSet::new();

    fn visit(
        node: i64,
        dependencies: &HashMap<i64, Vec<i64>>,
        visited: &mut HashSet<i64>,
        temp_mark: &mut HashSet<i64>,
        result: &mut Vec<i64>,
    ) {
        if visited.contains(&node) {
            return;
        }
        if temp_mark.contains(&node) {
            // Cycle detected, skip (shouldn't happen with proper calibration hierarchy)
            return;
        }

        temp_mark.insert(node);

        // Visit dependencies first
        if let Some(deps) = dependencies.get(&node) {
            for dep in deps {
                visit(*dep, dependencies, visited, temp_mark, result);
            }
        }

        temp_mark.remove(&node);
        visited.insert(node);
        result.push(node);
    }

    for &set_id in all_sets.keys() {
        visit(set_id, dependencies, &mut visited, &mut temp_mark, &mut result);
    }

    result
}

/// Get which calibrations should be applied when creating a master
/// Returns (apply_bias, apply_dark) set IDs
fn get_calibration_applications(conn: &Connection, set_id: i64) -> Result<(Option<i64>, Option<i64>)> {
    let mut apply_bias = None;
    let mut apply_dark = None;

    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'",
    )?;

    let links: Vec<(i64, String)> = stmt
        .query_map([set_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (cal_id, cal_type) in links {
        match cal_type.as_str() {
            "Bias" => apply_bias = Some(cal_id),
            "Dark" | "DarkFlat" => apply_dark = Some(cal_id),
            _ => {}
        }
    }

    Ok((apply_bias, apply_dark))
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_filter_sorting() {
        let mut filters: Vec<(Option<String>, Vec<i64>)> = vec![
            (Some("R".to_string()), vec![]),
            (None, vec![]),
            (Some("Ha".to_string()), vec![]),
            (Some("G".to_string()), vec![]),
        ];

        filters.sort_by(|a, b| match (&a.0, &b.0) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        });

        assert_eq!(filters[0].0, None);
        assert_eq!(filters[1].0, Some("G".to_string()));
        assert_eq!(filters[2].0, Some("Ha".to_string()));
        assert_eq!(filters[3].0, Some("R".to_string()));
    }

    #[test]
    fn test_subgroup_key_generation() {
        use super::FrameCalibrationLinks;

        let links = FrameCalibrationLinks {
            frame_id: 1,
            flat_id: Some(5),
            dark_id: Some(3),
            bias_id: Some(1),
        };
        assert_eq!(links.subgroup_key(), "f5_d3_b1");

        let links_partial = FrameCalibrationLinks {
            frame_id: 2,
            flat_id: Some(5),
            dark_id: None,
            bias_id: Some(1),
        };
        assert_eq!(links_partial.subgroup_key(), "f5_dnone_b1");

        let links_none = FrameCalibrationLinks {
            frame_id: 3,
            flat_id: None,
            dark_id: None,
            bias_id: None,
        };
        assert_eq!(links_none.subgroup_key(), "fnone_dnone_bnone");
    }
}
