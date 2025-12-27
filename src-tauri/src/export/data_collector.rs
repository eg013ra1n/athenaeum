//! Data collector for export operations
//!
//! Collects light frames from a frame set and their linked calibrations
//! to prepare data for export.

use crate::export::models::{
    CalibrationSummary, ExportCalibrationSet, ExportData, ExportFrame, FilterExportGroup,
};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;

/// Collect all export data for a frame set
///
/// This traverses the frame set hierarchy to get all light frames,
/// groups them by filter, and retrieves their calibration links.
pub fn collect_export_data(conn: &Connection, frame_set_id: i64) -> Result<ExportData> {
    println!("📦 Collecting export data for frame set {}", frame_set_id);

    // Get frame set info
    let (frame_set_name, object_name) = get_frame_set_info(conn, frame_set_id)?;
    println!("  Frame set name: {}, object: {:?}", frame_set_name, object_name);

    // Get all light frames from the frame set
    let light_frames = get_light_frames_for_frame_set(conn, frame_set_id)?;
    println!("  Found {} light frames", light_frames.len());

    // Group frames by filter
    let mut filter_groups: HashMap<Option<String>, Vec<ExportFrame>> = HashMap::new();
    for frame in &light_frames {
        filter_groups
            .entry(frame.filter.clone())
            .or_default()
            .push(frame.clone());
    }

    // Build filter export groups with calibrations
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
                if sub.imagetyp == "DARKFLAT" {
                    total_dark_flat_count += sub.frames.len() as i32;
                } else if sub.imagetyp == "DARK" {
                    total_dark_count += sub.frames.len() as i32;
                } else if sub.imagetyp == "BIAS" {
                    total_bias_count += sub.frames.len() as i32;
                }
            }
            all_warnings.extend(flat.warnings.clone());
        }

        for dark in &dark_sets {
            total_dark_count += dark.frames.len() as i32;
            for sub in &dark.sub_calibrations {
                if sub.imagetyp == "BIAS" {
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
    for frame_id in frame_ids {
        if let Ok(frame) = get_export_frame_by_id(conn, frame_id) {
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
                f.binning, f.date_obs
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
            })
        },
    ).context("Failed to get frame by ID")
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
                f.binning, f.date_obs
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_sorting() {
        let mut filters = vec![
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
}
