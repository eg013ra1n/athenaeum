// Database CRUD operations

use crate::models::*;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result};

/// Insert a new file record
pub fn insert_file(conn: &Connection, file: &File) -> Result<i64> {
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            file.path,
            file.filename,
            file.size,
            file.modified_at.to_rfc3339(),
            format!("{:?}", file.format),
            file.created_at.to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert a new frame record
pub fn insert_frame(conn: &Connection, frame: &Frame) -> Result<i64> {
    let imagetyp_str = frame.imagetyp.as_ref().map(|t| format!("{:?}", t));
    let date_obs_str = frame.date_obs.as_ref().map(|d| d.to_rfc3339());
    let override_int = if frame.override_ { 1 } else { 0 };

    conn.execute(
        "INSERT INTO frames (file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp,
         gain, offset, binning, xbinning, ybinning, ccd_temp, set_temp, focallen, xpixsz, pixsz,
         ra, dec, sitelat, lat_obs, sitelong, long_obs, objctra, objctdec, override, calibration_set_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
         ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
        params![
            frame.file_id,
            frame.object,
            date_obs_str,
            frame.telescop,
            frame.instrume,
            frame.exptime,
            frame.filter,
            imagetyp_str,
            frame.gain,
            frame.offset,
            frame.binning,
            frame.xbinning,
            frame.ybinning,
            frame.ccd_temp,
            frame.set_temp,
            frame.focallen,
            frame.xpixsz,
            frame.pixsz,
            frame.ra,
            frame.dec,
            frame.sitelat,
            frame.lat_obs,
            frame.sitelong,
            frame.long_obs,
            frame.objctra,
            frame.objctdec,
            override_int,
            frame.calibration_set_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all scan roots
pub fn get_scan_roots(conn: &Connection) -> Result<Vec<ScanRoot>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, enabled, last_scan FROM scan_roots ORDER BY path"
    )?;

    let roots = stmt.query_map([], |row| {
        let last_scan_str: Option<String> = row.get(3)?;
        let last_scan = last_scan_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(ScanRoot {
            id: Some(row.get(0)?),
            path: row.get(1)?,
            enabled: row.get::<_, i32>(2)? == 1,
            last_scan,
        })
    })?;

    roots.collect()
}

/// Insert or update a scan root
pub fn upsert_scan_root(conn: &Connection, path: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO scan_roots (path, enabled) VALUES (?1, 1)
         ON CONFLICT(path) DO NOTHING",
        params![path],
    )?;

    let id: i64 = conn.query_row(
        "SELECT id FROM scan_roots WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;

    Ok(id)
}

/// Update scan root last_scan timestamp
pub fn update_scan_root_timestamp(conn: &Connection, id: i64) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE scan_roots SET last_scan = ?1 WHERE id = ?2",
        params![now, id],
    )?;
    Ok(())
}

/// Delete a scan root and all associated files
pub fn delete_scan_root(conn: &Connection, id: i64) -> Result<()> {
    // First, get the path of the scan root
    let path: String = conn.query_row(
        "SELECT path FROM scan_roots WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;

    // Delete all files under this path (frames will be cascade deleted due to foreign key)
    conn.execute(
        "DELETE FROM files WHERE path LIKE ?1 || '%'",
        params![path],
    )?;

    // Delete the scan root
    conn.execute("DELETE FROM scan_roots WHERE id = ?1", params![id])?;

    Ok(())
}

/// Get all files with optional filters
pub fn get_files(conn: &Connection, limit: Option<usize>) -> Result<Vec<(File, Option<Frame>)>> {
    let limit_clause = match limit {
        Some(n) => format!("LIMIT {}", n),
        None => String::new(),
    };

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.calibration_set_id
         FROM files f
         LEFT JOIN frames fr ON f.id = fr.file_id
         ORDER BY f.created_at DESC
         {}",
        limit_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let results = stmt.query_map([], |row| {
        let file = File {
            id: Some(row.get(0)?),
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                .unwrap()
                .with_timezone(&Utc),
            format: if row.get::<_, String>(5)? == "FITS" {
                FileFormat::FITS
            } else {
                FileFormat::XISF
            },
            created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
                .unwrap()
                .with_timezone(&Utc),
        };

        let frame = if let Ok(frame_id) = row.get::<_, Option<i64>>(7) {
            frame_id.map(|fid| Frame {
                id: Some(fid),
                file_id: file.id.unwrap(),
                object: row.get(8).ok(),
                date_obs: row.get::<_, Option<String>>(9).ok().flatten().and_then(|s| {
                    DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                }),
                telescop: row.get(10).ok(),
                instrume: row.get(11).ok(),
                exptime: row.get(12).ok(),
                filter: row.get(13).ok(),
                imagetyp: row.get::<_, Option<String>>(14).ok().flatten().and_then(|s| ImageType::from_str(&s)),
                gain: row.get(15).ok(),
                offset: row.get(16).ok(),
                binning: row.get(17).ok(),
                xbinning: row.get(18).ok(),
                ybinning: row.get(19).ok(),
                ccd_temp: row.get(20).ok(),
                set_temp: row.get(21).ok(),
                focallen: row.get(22).ok(),
                xpixsz: row.get(23).ok(),
                pixsz: row.get(24).ok(),
                ra: row.get(25).ok(),
                dec: row.get(26).ok(),
                sitelat: row.get(27).ok(),
                lat_obs: row.get(28).ok(),
                sitelong: row.get(29).ok(),
                long_obs: row.get(30).ok(),
                objctra: row.get(31).ok(),
                objctdec: row.get(32).ok(),
                override_: row.get::<_, i32>(33).ok().map(|v| v == 1).unwrap_or(false),
                calibration_set_id: row.get(34).ok(),
            })
        } else {
            None
        };

        Ok((file, frame))
    })?;

    results.collect()
}

/// Get files in a specific directory
pub fn get_files_by_directory(
    conn: &Connection,
    directory_path: &str,
    limit: Option<usize>
) -> Result<Vec<(File, Option<Frame>)>> {
    let limit_clause = match limit {
        Some(n) => format!("LIMIT {}", n),
        None => String::new(),
    };

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.calibration_set_id
         FROM files f
         LEFT JOIN frames fr ON f.id = fr.file_id
         WHERE f.path LIKE ?1 || '/%'
         ORDER BY f.path
         {}",
        limit_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let results = stmt.query_map(params![directory_path], |row| {
        let file = File {
            id: Some(row.get(0)?),
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                .unwrap()
                .with_timezone(&Utc),
            format: if row.get::<_, String>(5)? == "FITS" {
                FileFormat::FITS
            } else {
                FileFormat::XISF
            },
            created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
                .unwrap()
                .with_timezone(&Utc),
        };

        let frame = if let Ok(frame_id) = row.get::<_, Option<i64>>(7) {
            frame_id.map(|fid| Frame {
                id: Some(fid),
                file_id: file.id.unwrap(),
                object: row.get(8).ok(),
                date_obs: row.get::<_, Option<String>>(9).ok().flatten().and_then(|s| {
                    DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                }),
                telescop: row.get(10).ok(),
                instrume: row.get(11).ok(),
                exptime: row.get(12).ok(),
                filter: row.get(13).ok(),
                imagetyp: row.get::<_, Option<String>>(14).ok().flatten().and_then(|s| ImageType::from_str(&s)),
                gain: row.get(15).ok(),
                offset: row.get(16).ok(),
                binning: row.get(17).ok(),
                xbinning: row.get(18).ok(),
                ybinning: row.get(19).ok(),
                ccd_temp: row.get(20).ok(),
                set_temp: row.get(21).ok(),
                focallen: row.get(22).ok(),
                xpixsz: row.get(23).ok(),
                pixsz: row.get(24).ok(),
                ra: row.get(25).ok(),
                dec: row.get(26).ok(),
                sitelat: row.get(27).ok(),
                lat_obs: row.get(28).ok(),
                sitelong: row.get(29).ok(),
                long_obs: row.get(30).ok(),
                objctra: row.get(31).ok(),
                objctdec: row.get(32).ok(),
                override_: row.get::<_, i32>(33).ok().map(|v| v == 1).unwrap_or(false),
                calibration_set_id: row.get(34).ok(),
            })
        } else {
            None
        };

        Ok((file, frame))
    })?;

    results.collect()
}

/// Find duplicates by filename and metadata
pub fn find_duplicate_groups(conn: &Connection) -> Result<Vec<DuplicateGroup>> {
    let mut stmt = conn.prepare(
        "SELECT f.filename, f.size, COUNT(*) as count, GROUP_CONCAT(f.path, '|') as paths
         FROM files f
         LEFT JOIN frames fr ON f.id = fr.file_id
         GROUP BY f.filename, fr.object, fr.telescop, fr.instrume, fr.filter, fr.exptime
         HAVING count > 1
         ORDER BY count DESC, f.size DESC"
    )?;

    let groups = stmt.query_map([], |row| {
        let paths_str: String = row.get(3)?;
        let file_paths: Vec<String> = paths_str.split('|').map(|s| s.to_string()).collect();

        Ok(DuplicateGroup {
            id: None,
            size: row.get(1)?,
            content_hash: row.get(0)?, // Using filename as identifier
            file_count: row.get(2)?,
            file_paths,
        })
    })?;

    groups.collect()
}

/// Check if file already exists in database
pub fn file_exists(conn: &Connection, path: &str) -> Result<bool> {
    let count: i32 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}
