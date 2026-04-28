// Equipment-related database operations

use crate::models::{CameraStats, CalibrationSetDetail, FileWithFrame, ImageType};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

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
    // Query calibration sets with extended frame metadata from first frame in each set
    let mut stmt = conn.prepare(
        "SELECT
            cs.id,
            cs.imagetyp,
            cs.exptime,
            cs.ccd_temp,
            cs.temp_min,
            cs.temp_max,
            cs.gain,
            cs.offset,
            cs.binning,
            cs.instrume,
            cs.filter,
            cs.date_start,
            cs.date_end,
            cs.date,
            cs.frame_count,
            f.naxis1,
            f.naxis2,
            f.bayerpat,
            f.swcreate,
            f.xpixsz,
            fi.format,
            cs.focallen
        FROM calibration_set cs
        LEFT JOIN calibration_set_frames csf ON csf.set_id = cs.id
        LEFT JOIN frames f ON f.id = csf.frame_id
        LEFT JOIN files fi ON fi.id = f.file_id
        WHERE cs.instrume = ?1
        AND cs.is_master_library = 0
        GROUP BY cs.id
        ORDER BY cs.imagetyp, cs.exptime, cs.ccd_temp"
    )?;

    let sets = stmt
        .query_map([instrume], |row| {
            let imagetyp_str: String = row.get(1)?;
            let imagetyp = ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            let date_start: String = row.get(11)?;
            let date_end: String = row.get(12)?;

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
                filter: row.get(10)?,
                date_start,
                date_end,
                date_display,
                frame_count: row.get::<_, Option<i64>>(14)?.unwrap_or(0),
                is_master: false,  // Regular calibration sets only
                // Extended fields from frame metadata
                naxis1: row.get(15)?,
                naxis2: row.get(16)?,
                bayerpat: row.get(17)?,
                swcreate: row.get(18)?,
                xpixsz: row.get(19)?,
                format: row.get(20)?,
                focallen: row.get(21)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sets)
}

/// Delete Dark/Bias/DarkFlat calibration sets for a camera (preserves Flat sets)
pub fn delete_camera_dark_library(conn: &Connection, instrume: &str) -> Result<()> {
    // First, get all Dark/Bias/DarkFlat set IDs for this camera (NOT Flat sets!)
    let mut stmt = conn.prepare(
        "SELECT id FROM calibration_set
         WHERE instrume = ?1 AND is_master_library = 0
         AND UPPER(imagetyp) IN ('DARK', 'BIAS', 'DARKFLAT')"
    )?;
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

    // Delete only Dark/Bias/DarkFlat calibration_set entries (preserves Flat sets)
    conn.execute(
        "DELETE FROM calibration_set
         WHERE instrume = ?1 AND is_master_library = 0
         AND UPPER(imagetyp) IN ('DARK', 'BIAS', 'DARKFLAT')",
        [instrume]
    )?;

    Ok(())
}

/// Check if dark library exists for camera
pub fn has_dark_library(conn: &Connection, instrume: &str) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM calibration_set WHERE instrume = ?1 AND is_master_library = 0")?;
    let count: i64 = stmt.query_row([instrume], |row| row.get(0))?;
    Ok(count > 0)
}

/// Get master dark/bias calibration sets for a specific camera (MasterDark, MasterBias, MasterDarkFlat)
pub fn get_camera_master_dark_library(
    conn: &Connection,
    instrume: &str,
) -> Result<Vec<CalibrationSetDetail>> {
    let mut stmt = conn.prepare(
        "SELECT
            cs.id,
            cs.imagetyp,
            cs.exptime,
            cs.ccd_temp,
            cs.temp_min,
            cs.temp_max,
            cs.gain,
            cs.offset,
            cs.binning,
            cs.instrume,
            cs.filter,
            cs.date_start,
            cs.date_end,
            cs.date,
            cs.frame_count,
            f.naxis1,
            f.naxis2,
            f.bayerpat,
            f.swcreate,
            f.xpixsz,
            fi.format,
            cs.focallen
        FROM calibration_set cs
        LEFT JOIN calibration_set_frames csf ON csf.set_id = cs.id
        LEFT JOIN frames f ON f.id = csf.frame_id
        LEFT JOIN files fi ON fi.id = f.file_id
        WHERE cs.instrume = ?1
        AND cs.is_master_library = 1
        AND cs.imagetyp IN ('MasterDark', 'MasterBias', 'MasterDarkFlat')
        GROUP BY cs.id
        ORDER BY cs.imagetyp, cs.exptime, cs.ccd_temp"
    )?;

    let sets = stmt
        .query_map([instrume], |row| {
            let imagetyp_str: String = row.get(1)?;
            let imagetyp = ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            let date_start: String = row.get(11)?;
            let date_end: String = row.get(12)?;

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
                filter: row.get(10)?,
                date_start,
                date_end,
                date_display,
                frame_count: row.get::<_, Option<i64>>(14)?.unwrap_or(0),
                is_master: true,  // Master calibration sets
                naxis1: row.get(15)?,
                naxis2: row.get(16)?,
                bayerpat: row.get(17)?,
                swcreate: row.get(18)?,
                xpixsz: row.get(19)?,
                format: row.get(20)?,
                focallen: row.get(21)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sets)
}

/// Check if master dark library exists for camera (MasterDark, MasterBias, MasterDarkFlat)
pub fn has_master_dark_library(conn: &Connection, instrume: &str) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT COUNT(*) FROM calibration_set
         WHERE instrume = ?1 AND is_master_library = 1
         AND imagetyp IN ('MasterDark', 'MasterBias', 'MasterDarkFlat')"
    )?;
    let count: i64 = stmt.query_row([instrume], |row| row.get(0))?;
    Ok(count > 0)
}

/// Get master flat calibration sets for a specific camera (MasterFlat)
pub fn get_camera_master_flat_library(
    conn: &Connection,
    instrume: &str,
) -> Result<Vec<CalibrationSetDetail>> {
    let mut stmt = conn.prepare(
        "SELECT
            cs.id,
            cs.imagetyp,
            cs.exptime,
            cs.ccd_temp,
            cs.temp_min,
            cs.temp_max,
            cs.gain,
            cs.offset,
            cs.binning,
            cs.instrume,
            cs.filter,
            cs.date_start,
            cs.date_end,
            cs.date,
            cs.frame_count,
            f.naxis1,
            f.naxis2,
            f.bayerpat,
            f.swcreate,
            f.xpixsz,
            fi.format,
            cs.focallen
        FROM calibration_set cs
        LEFT JOIN calibration_set_frames csf ON csf.set_id = cs.id
        LEFT JOIN frames f ON f.id = csf.frame_id
        LEFT JOIN files fi ON fi.id = f.file_id
        WHERE cs.instrume = ?1
        AND cs.is_master_library = 1
        AND cs.imagetyp = 'MasterFlat'
        GROUP BY cs.id
        ORDER BY cs.filter, cs.exptime, cs.ccd_temp"
    )?;

    let sets = stmt
        .query_map([instrume], |row| {
            let imagetyp_str: String = row.get(1)?;
            let imagetyp = ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            let date_start: String = row.get(11)?;
            let date_end: String = row.get(12)?;

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
                filter: row.get(10)?,
                date_start,
                date_end,
                date_display,
                frame_count: row.get::<_, Option<i64>>(14)?.unwrap_or(0),
                is_master: true,  // Master calibration sets
                naxis1: row.get(15)?,
                naxis2: row.get(16)?,
                bayerpat: row.get(17)?,
                swcreate: row.get(18)?,
                xpixsz: row.get(19)?,
                format: row.get(20)?,
                focallen: row.get(21)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sets)
}

/// Check if master flat library exists for camera
pub fn has_master_flat_library(conn: &Connection, instrume: &str) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT COUNT(*) FROM calibration_set
         WHERE instrume = ?1 AND is_master_library = 1
         AND imagetyp = 'MasterFlat'"
    )?;
    let count: i64 = stmt.query_row([instrume], |row| row.get(0))?;
    Ok(count > 0)
}

/// Get all distinct parent directory paths containing files for a given camera
pub fn get_camera_directories(conn: &Connection, instrume: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT
            SUBSTR(f.path, 1, LENGTH(f.path) - LENGTH(f.filename) - 1) as dir_path
        FROM files f
        JOIN frames fr ON f.id = fr.file_id
        WHERE fr.instrume = ?1
        ORDER BY dir_path"
    )?;

    let dirs = stmt
        .query_map([instrume], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(dirs)
}

/// Get frames for a specific calibration set
pub fn get_frames_for_calibration_set(
    conn: &Connection,
    set_id: i64,
) -> Result<Vec<FileWithFrame>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override
         FROM calibration_set_frames csf
         JOIN frames fr ON csf.frame_id = fr.id
         JOIN files f ON fr.file_id = f.id
         WHERE csf.set_id = ?1
         ORDER BY fr.date_obs ASC",
    )?;

    let frames = stmt.query_map(params![set_id], |row| {
        let file = crate::models::File {
            id: row.get(0)?,
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                .unwrap()
                .with_timezone(&Utc),
            format: if row.get::<_, String>(5)? == "FITS" {
                crate::models::FileFormat::FITS
            } else {
                crate::models::FileFormat::XISF
            },
            created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
                .unwrap()
                .with_timezone(&Utc),
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
        };

        let frame = crate::models::Frame {
            id: row.get(12)?,
            file_id: row.get(13)?,
            object: row.get(14)?,
            date_obs: row.get::<_, Option<String>>(15)?.and_then(|s| {
                DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
            }),
            telescop: row.get(16)?,
            instrume: row.get(17)?,
            exptime: row.get(18)?,
            filter: row.get(19)?,
            imagetyp: row.get::<_, Option<String>>(20)?.and_then(|s| crate::models::ImageType::from_str(&s)),
            is_master: row.get::<_, i32>(21)? == 1,
            gain: row.get(22)?,
            offset: row.get(23)?,
            binning: row.get(24)?,
            xbinning: row.get(25)?,
            ybinning: row.get(26)?,
            ccd_temp: row.get(27)?,
            set_temp: row.get(28)?,
            focallen: row.get(29)?,
            xpixsz: row.get(30)?,
            ypixsz: row.get(31)?,
            naxis1: row.get(32)?,
            naxis2: row.get(33)?,
            ra: row.get(34)?,
            dec: row.get(35)?,
            sitelat: row.get(36)?,
            lat_obs: row.get(37)?,
            sitelong: row.get(38)?,
            long_obs: row.get(39)?,
            objctra: row.get(40)?,
            objctdec: row.get(41)?,
            override_: row.get::<_, i32>(42)? == 1,
            swcreate: None,
            bayerpat: None,
            rotation: None,
        };

        Ok(FileWithFrame {
            file,
            frame: Some(frame),
        })
    })?;

    frames.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
}
