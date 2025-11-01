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

/// Insert FITS header
pub fn insert_fits_header(conn: &Connection, file_id: i64, header: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
        params![file_id, header],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert a new frame record
pub fn insert_frame(conn: &Connection, frame: &Frame) -> Result<i64> {
    let imagetyp_str = frame.imagetyp.as_ref().map(|t| format!("{:?}", t));
    let date_obs_str = frame.date_obs.as_ref().map(|d| d.to_rfc3339());
    let override_int = if frame.override_ { 1 } else { 0 };

    // Debug: Log what we're about to insert
    println!("insert_frame: file_id={}, object={:?}, date_obs={:?}",
        frame.file_id, frame.object, date_obs_str);

    let is_master_int = if frame.is_master { 1 } else { 0 };

    conn.execute(
        "INSERT INTO frames (file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp, is_master,
         gain, offset, binning, xbinning, ybinning, ccd_temp, set_temp, focallen, xpixsz, pixsz,
         ra, dec, sitelat, lat_obs, sitelong, long_obs, objctra, objctdec, override, calibration_set_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
         ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)",
        params![
            frame.file_id,
            frame.object,
            date_obs_str,
            frame.telescop,
            frame.instrume,
            frame.exptime,
            frame.filter,
            imagetyp_str,
            is_master_int,
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
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
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
                is_master: row.get::<_, i32>(15).ok().map(|v| v == 1).unwrap_or(false),
                gain: row.get(16).ok(),
                offset: row.get(17).ok(),
                binning: row.get(18).ok(),
                xbinning: row.get(19).ok(),
                ybinning: row.get(20).ok(),
                ccd_temp: row.get(21).ok(),
                set_temp: row.get(22).ok(),
                focallen: row.get(23).ok(),
                xpixsz: row.get(24).ok(),
                pixsz: row.get(25).ok(),
                ra: row.get(26).ok(),
                dec: row.get(27).ok(),
                sitelat: row.get(28).ok(),
                lat_obs: row.get(29).ok(),
                sitelong: row.get(30).ok(),
                long_obs: row.get(31).ok(),
                objctra: row.get(32).ok(),
                objctdec: row.get(33).ok(),
                override_: row.get::<_, i32>(34).ok().map(|v| v == 1).unwrap_or(false),
                calibration_set_id: row.get(35).ok(),
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

    // Find files that are directly in this directory (not in subdirectories)
    // path should start with directory_path/ but not contain additional slashes after that
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.calibration_set_id
         FROM files f
         LEFT JOIN frames fr ON f.id = fr.file_id
         WHERE f.path LIKE ?1 || '/%'
           AND (LENGTH(f.path) - LENGTH(REPLACE(f.path, '/', ''))) =
               (LENGTH(?1) - LENGTH(REPLACE(?1, '/', '')) + 1)
         ORDER BY f.filename
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
                is_master: row.get::<_, i32>(15).ok().map(|v| v == 1).unwrap_or(false),
                gain: row.get(16).ok(),
                offset: row.get(17).ok(),
                binning: row.get(18).ok(),
                xbinning: row.get(19).ok(),
                ybinning: row.get(20).ok(),
                ccd_temp: row.get(21).ok(),
                set_temp: row.get(22).ok(),
                focallen: row.get(23).ok(),
                xpixsz: row.get(24).ok(),
                pixsz: row.get(25).ok(),
                ra: row.get(26).ok(),
                dec: row.get(27).ok(),
                sitelat: row.get(28).ok(),
                lat_obs: row.get(29).ok(),
                sitelong: row.get(30).ok(),
                long_obs: row.get(31).ok(),
                objctra: row.get(32).ok(),
                objctdec: row.get(33).ok(),
                override_: row.get::<_, i32>(34).ok().map(|v| v == 1).unwrap_or(false),
                calibration_set_id: row.get(35).ok(),
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

// ============================================================================
// Settings Operations
// ============================================================================

/// Get a setting value by key
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let result: Result<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    );

    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Set a setting value (insert or update)
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        params![key, value, now],
    )?;
    Ok(())
}

/// Delete a setting by key
pub fn delete_setting(conn: &Connection, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        params![key],
    )?;
    Ok(())
}

/// Get all settings
pub fn get_all_settings(conn: &Connection) -> Result<Vec<Setting>> {
    let mut stmt = conn.prepare(
        "SELECT key, value, updated_at FROM settings ORDER BY key"
    )?;

    let settings = stmt.query_map([], |row| {
        let updated_at_str: Option<String> = row.get(2)?;
        let updated_at = updated_at_str.and_then(|s| {
            DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
        });

        Ok(Setting {
            key: row.get(0)?,
            value: row.get(1)?,
            updated_at,
        })
    })?;

    settings.collect()
}

// ============================================================================
// Frames Set Operations
// ============================================================================

/// Create a new frames_set and return its ID
pub fn create_frames_set(
    conn: &Connection,
    name: Option<&str>,
    is_custom: bool,
    date_obs: Option<&str>,
    objctra: Option<&str>,
    objctdec: Option<&str>,
    total_exp_time: Option<f64>,
    project_id: Option<i64>,
) -> Result<i64> {
    let is_custom_int = if is_custom { 1 } else { 0 };
    conn.execute(
        "INSERT INTO frames_set (name, is_custom, date_obs, objctra, objctdec, total_exp_time, project_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![name, is_custom_int, date_obs, objctra, objctdec, total_exp_time, project_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all frames sets for a project with member counts
pub fn get_frames_sets_by_project(
    conn: &Connection,
    project_id: i64,
) -> Result<Vec<(crate::models::FramesSet, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT fs.id, fs.name, fs.is_custom, fs.date_obs, fs.objctra, fs.objctdec, fs.total_exp_time, fs.project_id,
                COUNT(DISTINCT sm.frame_id) as member_count
         FROM frames_set fs
         LEFT JOIN imaging_nights in_tbl ON fs.id = in_tbl.frames_set_id
         LEFT JOIN sessions s ON in_tbl.id = s.imaging_night_id
         LEFT JOIN session_members sm ON s.id = sm.session_id
         WHERE fs.project_id = ?1 OR fs.project_id IS NULL
         GROUP BY fs.id
         ORDER BY fs.date_obs DESC, fs.name ASC"
    )?;

    let sets = stmt.query_map(params![project_id], |row| {
        let set = crate::models::FramesSet {
            id: Some(row.get(0)?),
            name: row.get(1)?,
            is_custom: row.get::<_, i32>(2)? == 1,
            date_obs: row.get(3)?,
            objctra: row.get(4)?,
            objctdec: row.get(5)?,
            total_exp_time: row.get(6)?,
            project_id: row.get(7)?,
        };
        let member_count: i32 = row.get(8)?;
        Ok((set, member_count as usize))
    })?;

    sets.collect()
}

/// Get all frame IDs that are already members of any frames_set
pub fn get_all_frames_set_member_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
         ORDER BY sm.frame_id"
    )?;

    let frame_ids = stmt.query_map(params![], |row| row.get(0))?;

    frame_ids.collect()
}

/// Delete a frames_set (cascade will delete members)
pub fn delete_frames_set(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM frames_set WHERE id = ?1", params![id])?;
    Ok(())
}

/// Update frames_set name
pub fn update_frames_set_name(conn: &Connection, id: i64, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE frames_set SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

/// Get all LIGHT frames for a project (for clustering)
pub fn get_light_frames_for_project(
    conn: &Connection,
    project_id: i64,
) -> Result<Vec<(i64, crate::models::Frame)>> {
    // For now, we'll get all LIGHT frames regardless of project
    // In the future, we can add project filtering at the frame level
    let mut stmt = conn.prepare(
        "SELECT f.id, fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override,
                fr.calibration_set_id
         FROM files f
         INNER JOIN frames fr ON f.id = fr.file_id
         WHERE fr.imagetyp = 'Light'
         ORDER BY f.id"
    )?;

    let results = stmt.query_map(params![], |row| {
        let file_id: i64 = row.get(0)?;

        let date_obs_str: Option<String> = row.get(4)?;
        let date_obs = date_obs_str.and_then(|s| {
            DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
        });

        let imagetyp_str: Option<String> = row.get(9)?;
        let imagetyp = imagetyp_str.and_then(|s| crate::models::ImageType::from_str(&s));

        let frame = crate::models::Frame {
            id: Some(row.get(1)?),
            file_id: row.get(2)?,
            object: row.get(3)?,
            date_obs,
            telescop: row.get(5)?,
            instrume: row.get(6)?,
            exptime: row.get(7)?,
            filter: row.get(8)?,
            imagetyp,
            is_master: row.get::<_, i32>(10)? == 1,
            gain: row.get(11)?,
            offset: row.get(12)?,
            binning: row.get(13)?,
            xbinning: row.get(14)?,
            ybinning: row.get(15)?,
            ccd_temp: row.get(16)?,
            set_temp: row.get(17)?,
            focallen: row.get(18)?,
            xpixsz: row.get(19)?,
            pixsz: row.get(20)?,
            ra: row.get(21)?,
            dec: row.get(22)?,
            sitelat: row.get(23)?,
            lat_obs: row.get(24)?,
            sitelong: row.get(25)?,
            long_obs: row.get(26)?,
            objctra: row.get(27)?,
            objctdec: row.get(28)?,
            override_: row.get::<_, i32>(29)? == 1,
            calibration_set_id: row.get(30)?,
        };

        Ok((file_id, frame))
    })?;

    results.collect()
}

// ============================================================================
// Imaging Nights and Sessions Operations
// ============================================================================

/// Create a new imaging night
pub fn create_imaging_night(
    conn: &Connection,
    frames_set_id: i64,
    start_time: &str,
    end_time: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
         VALUES (?1, ?2, ?3)",
        params![frames_set_id, start_time, end_time],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Create a new session within an imaging night
pub fn create_session(
    conn: &Connection,
    imaging_night_id: i64,
    instrume: &str,
    frame_count: i32,
    total_exp_time: Option<f64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO sessions (imaging_night_id, instrume, frame_count, total_exp_time)
         VALUES (?1, ?2, ?3, ?4)",
        params![imaging_night_id, instrume, frame_count, total_exp_time],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert session members (bulk insert)
pub fn insert_session_members(
    conn: &Connection,
    session_id: i64,
    frame_ids: &[i64],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    for frame_id in frame_ids {
        conn.execute(
            "INSERT OR IGNORE INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Check if sessions exist for a frame set
pub fn sessions_exist_for_frame_set(conn: &Connection, frames_set_id: i64) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = ?1",
        params![frames_set_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Delete all sessions for a frame set
pub fn delete_sessions_for_frame_set(conn: &Connection, frames_set_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM imaging_nights WHERE frames_set_id = ?1",
        params![frames_set_id],
    )?;
    Ok(())
}

/// Get frames for a specific frames_set with file info (for session detection)
pub fn get_frames_with_files_for_set(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<(i64, crate::models::File, crate::models::Frame)>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at,
                fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override,
                fr.calibration_set_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
         JOIN frames fr ON sm.frame_id = fr.id
         JOIN files f ON fr.file_id = f.id
         WHERE in_tbl.frames_set_id = ?1
         ORDER BY fr.date_obs ASC",
    )?;

    let results = stmt.query_map(params![frames_set_id], |row| {
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
        };

        // Debug: Check date_obs in database
        let date_obs_raw: Option<String> = row.get(10)?;
        if date_obs_raw.is_none() {
            let frame_id: i64 = row.get(7)?;
            let filename: String = row.get(2)?;
            println!("Frame {} ({}) has NULL date_obs in database", frame_id, filename);
        } else {
            println!("Frame has date_obs: {:?}", date_obs_raw);
        }

        let frame = crate::models::Frame {
            id: row.get(7)?,
            file_id: row.get(8)?,
            object: row.get(9)?,
            date_obs: date_obs_raw.and_then(|s| {
                DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
            }),
            telescop: row.get(11)?,
            instrume: row.get(12)?,
            exptime: row.get(13)?,
            filter: row.get(14)?,
            imagetyp: row.get::<_, Option<String>>(15)?.and_then(|s| crate::models::ImageType::from_str(&s)),
            is_master: row.get::<_, i32>(16)? == 1,
            gain: row.get(17)?,
            offset: row.get(18)?,
            binning: row.get(19)?,
            xbinning: row.get(20)?,
            ybinning: row.get(21)?,
            ccd_temp: row.get(22)?,
            set_temp: row.get(23)?,
            focallen: row.get(24)?,
            xpixsz: row.get(25)?,
            pixsz: row.get(26)?,
            ra: row.get(27)?,
            dec: row.get(28)?,
            sitelat: row.get(29)?,
            lat_obs: row.get(30)?,
            sitelong: row.get(31)?,
            long_obs: row.get(32)?,
            objctra: row.get(33)?,
            objctdec: row.get(34)?,
            override_: row.get::<_, i32>(35)? == 1,
            calibration_set_id: row.get(36)?,
        };

        let file_id: i64 = row.get(0)?;
        Ok((file_id, file, frame))
    })?;

    results.collect()
}

/// Get frames with their files by frame IDs (for session generation during auto-clustering)
pub fn get_frames_with_files_by_ids(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<Vec<(i64, crate::models::File, crate::models::Frame)>> {
    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Build the query with placeholders for each ID
    let placeholders = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at,
                fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override,
                fr.calibration_set_id
         FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE fr.id IN ({})
         ORDER BY fr.date_obs ASC",
        placeholders
    );

    let mut stmt = conn.prepare(&query)?;

    // Convert frame_ids to params
    let params_vec: Vec<&dyn rusqlite::ToSql> = frame_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let results = stmt.query_map(params_vec.as_slice(), |row| {
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
        };

        let date_obs_raw: Option<String> = row.get(10)?;

        let frame = crate::models::Frame {
            id: row.get(7)?,
            file_id: row.get(8)?,
            object: row.get(9)?,
            date_obs: date_obs_raw.and_then(|s| {
                DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
            }),
            telescop: row.get(11)?,
            instrume: row.get(12)?,
            exptime: row.get(13)?,
            filter: row.get(14)?,
            imagetyp: row.get::<_, Option<String>>(15)?.and_then(|s| crate::models::ImageType::from_str(&s)),
            is_master: row.get::<_, i32>(16)? == 1,
            gain: row.get(17)?,
            offset: row.get(18)?,
            binning: row.get(19)?,
            xbinning: row.get(20)?,
            ybinning: row.get(21)?,
            ccd_temp: row.get(22)?,
            set_temp: row.get(23)?,
            focallen: row.get(24)?,
            xpixsz: row.get(25)?,
            pixsz: row.get(26)?,
            ra: row.get(27)?,
            dec: row.get(28)?,
            sitelat: row.get(29)?,
            lat_obs: row.get(30)?,
            sitelong: row.get(31)?,
            long_obs: row.get(32)?,
            objctra: row.get(33)?,
            objctdec: row.get(34)?,
            override_: row.get::<_, i32>(35)? == 1,
            calibration_set_id: row.get(36)?,
        };

        let file_id: i64 = row.get(0)?;
        Ok((file_id, file, frame))
    })?;

    results.collect()
}

/// Get imaging nights with sessions for a frame set
pub fn get_imaging_nights_with_sessions(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<crate::models::ImagingNightWithSessions>> {
    // Get all imaging nights for this frame set
    let mut nights_stmt = conn.prepare(
        "SELECT id, frames_set_id, start_time, end_time, created_at
         FROM imaging_nights
         WHERE frames_set_id = ?1
         ORDER BY start_time ASC",
    )?;

    let nights = nights_stmt.query_map(params![frames_set_id], |row| {
        Ok(crate::models::ImagingNight {
            id: row.get(0)?,
            frames_set_id: row.get(1)?,
            start_time: row.get(2)?,
            end_time: row.get(3)?,
            created_at: row.get::<_, Option<String>>(4)?.and_then(|s| {
                DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
            }),
        })
    })?;

    let mut result = Vec::new();

    for night in nights {
        let night = night?;
        let night_id = night.id.unwrap();

        // Get sessions for this night
        let mut sessions_stmt = conn.prepare(
            "SELECT id, imaging_night_id, instrume, frame_count, total_exp_time, created_at
             FROM sessions
             WHERE imaging_night_id = ?1
             ORDER BY instrume ASC",
        )?;

        let sessions = sessions_stmt.query_map(params![night_id], |row| {
            Ok(crate::models::Session {
                id: row.get(0)?,
                imaging_night_id: row.get(1)?,
                instrume: row.get(2)?,
                frame_count: row.get(3)?,
                total_exp_time: row.get(4)?,
                created_at: row.get::<_, Option<String>>(5)?.and_then(|s| {
                    DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                }),
            })
        })?;

        let mut sessions_with_frames = Vec::new();

        for session in sessions {
            let session = session?;
            let session_id = session.id.unwrap();

            // Get frames for this session
            let mut frames_stmt = conn.prepare(
                "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at,
                        fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                        fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                        fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                        fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                        fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override,
                        fr.calibration_set_id
                 FROM session_members sm
                 JOIN frames fr ON sm.frame_id = fr.id
                 JOIN files f ON fr.file_id = f.id
                 WHERE sm.session_id = ?1
                 ORDER BY fr.date_obs ASC",
            )?;

            let frames = frames_stmt.query_map(params![session_id], |row| {
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
                };

                let frame = crate::models::Frame {
                    id: row.get(7)?,
                    file_id: row.get(8)?,
                    object: row.get(9)?,
                    date_obs: row.get::<_, Option<String>>(10)?.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                    }),
                    telescop: row.get(11)?,
                    instrume: row.get(12)?,
                    exptime: row.get(13)?,
                    filter: row.get(14)?,
                    imagetyp: row.get::<_, Option<String>>(15)?.and_then(|s| crate::models::ImageType::from_str(&s)),
                    is_master: row.get::<_, i32>(16)? == 1,
                    gain: row.get(17)?,
                    offset: row.get(18)?,
                    binning: row.get(19)?,
                    xbinning: row.get(20)?,
                    ybinning: row.get(21)?,
                    ccd_temp: row.get(22)?,
                    set_temp: row.get(23)?,
                    focallen: row.get(24)?,
                    xpixsz: row.get(25)?,
                    pixsz: row.get(26)?,
                    ra: row.get(27)?,
                    dec: row.get(28)?,
                    sitelat: row.get(29)?,
                    lat_obs: row.get(30)?,
                    sitelong: row.get(31)?,
                    long_obs: row.get(32)?,
                    objctra: row.get(33)?,
                    objctdec: row.get(34)?,
                    override_: row.get::<_, i32>(35)? == 1,
                    calibration_set_id: row.get(36)?,
                };

                Ok(crate::models::FileWithFrame {
                    file,
                    frame: Some(frame),
                })
            })?;

            let frames_vec: Result<Vec<_>> = frames.collect();
            sessions_with_frames.push(crate::models::SessionWithFrames {
                session,
                frames: frames_vec?,
            });
        }

        result.push(crate::models::ImagingNightWithSessions {
            imaging_night: night,
            sessions: sessions_with_frames,
        });
    }

    Ok(result)
}

// ============================================================================
// Custom Frames Set Operations
// ============================================================================

/// Get frame IDs for a given session
pub fn get_frame_ids_for_session(conn: &Connection, session_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT frame_id FROM session_members WHERE session_id = ?1"
    )?;

    let frame_ids = stmt.query_map(params![session_id], |row| row.get(0))?;
    frame_ids.collect()
}

/// Clone a session (create a new session with the same data but different imaging_night_id)
/// Returns the new session_id
pub fn clone_session(
    conn: &Connection,
    original_session_id: i64,
    new_imaging_night_id: i64,
) -> Result<i64> {
    // Get original session data
    let session: crate::models::Session = conn.query_row(
        "SELECT id, imaging_night_id, instrume, frame_count, total_exp_time, created_at
         FROM sessions WHERE id = ?1",
        params![original_session_id],
        |row| {
            Ok(crate::models::Session {
                id: row.get(0)?,
                imaging_night_id: row.get(1)?,
                instrume: row.get(2)?,
                frame_count: row.get(3)?,
                total_exp_time: row.get(4)?,
                created_at: row.get::<_, Option<String>>(5)?.and_then(|s| {
                    DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                }),
            })
        },
    )?;

    // Create new session with new imaging_night_id
    let new_session_id = create_session(
        conn,
        new_imaging_night_id,
        &session.instrume,
        session.frame_count,
        session.total_exp_time,
    )?;

    // Copy session_members
    let frame_ids = get_frame_ids_for_session(conn, original_session_id)?;
    insert_session_members(conn, new_session_id, &frame_ids)?;

    Ok(new_session_id)
}

