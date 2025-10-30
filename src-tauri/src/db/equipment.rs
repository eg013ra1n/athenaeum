// Equipment-related database operations

use crate::models::{CameraStats, CalibrationSetDetail, ImageType};
use anyhow::Result;
use chrono::DateTime;
use rusqlite::Connection;

/// Get all cameras with statistics
pub fn get_all_cameras(conn: &Connection) -> Result<Vec<CameraStats>> {
    let mut stmt = conn.prepare(
        "SELECT
            instrume,
            COUNT(*) as frame_count,
            SUM(exptime) / 3600.0 as total_hours,
            MIN(date_obs) as first_use,
            MAX(date_obs) as last_use
        FROM frames
        WHERE instrume IS NOT NULL
        GROUP BY instrume
        ORDER BY instrume"
    )?;

    let cameras = stmt
        .query_map([], |row| {
            Ok(CameraStats {
                instrume: row.get(0)?,
                frame_count: row.get(1)?,
                total_hours: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                first_use: row.get(3)?,
                last_use: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(cameras)
}

/// Get calibration sets for a specific camera
pub fn get_camera_dark_library(
    conn: &Connection,
    instrume: &str,
) -> Result<Vec<CalibrationSetDetail>> {
    let mut stmt = conn.prepare(
        "SELECT
            id,
            imagetyp,
            exptime,
            ccd_temp,
            temp_min,
            temp_max,
            gain,
            offset,
            binning,
            instrume,
            date_start,
            date_end,
            date,
            frame_count
        FROM calibration_set
        WHERE instrume = ?1
        ORDER BY imagetyp, exptime, ccd_temp"
    )?;

    let sets = stmt
        .query_map([instrume], |row| {
            let imagetyp_str: String = row.get(1)?;
            let imagetyp = ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            let date_start: String = row.get(10)?;
            let date_end: String = row.get(11)?;

            // Generate date_display from date_start (YYYY-MM format)
            let date_display = if let Ok(dt) = DateTime::parse_from_rfc3339(&date_start) {
                dt.format("%Y-%m").to_string()
            } else {
                // Fallback if parsing fails
                date_start.chars().take(7).collect()
            };

            Ok(CalibrationSetDetail {
                id: Some(row.get(0)?),
                imagetyp,
                exptime: row.get(2)?,
                ccd_temp: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                temp_min: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                temp_max: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                gain: row.get(6)?,
                offset: row.get(7)?,
                binning: row.get(8)?,
                instrume: row.get(9)?,
                date_start,
                date_end,
                date_display,
                frame_count: row.get::<_, Option<i64>>(13)?.unwrap_or(0),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sets)
}

/// Delete all calibration sets for a camera
pub fn delete_camera_dark_library(conn: &Connection, instrume: &str) -> Result<()> {
    // First, get all set IDs for this camera
    let mut stmt = conn.prepare("SELECT id FROM calibration_set WHERE instrume = ?1")?;
    let set_ids: Vec<i64> = stmt
        .query_map([instrume], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    // Delete all calibration_set_frames entries for these sets
    for set_id in &set_ids {
        conn.execute(
            "DELETE FROM calibration_set_frames WHERE set_id = ?1",
            [set_id],
        )?;
    }

    // Delete all calibration_set entries for this camera
    conn.execute("DELETE FROM calibration_set WHERE instrume = ?1", [instrume])?;

    Ok(())
}

/// Check if dark library exists for camera
pub fn has_dark_library(conn: &Connection, instrume: &str) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM calibration_set WHERE instrume = ?1")?;
    let count: i64 = stmt.query_row([instrume], |row| row.get(0))?;
    Ok(count > 0)
}
