// Database CRUD operations

use crate::models::*;
use crate::fingerprint::compute_header_fingerprint;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result};

/// Insert a new file record
/// Uses prepare_cached() for better performance during bulk inserts
pub fn insert_file(conn: &Connection, file: &File) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at, metadata_hash, content_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    stmt.execute(params![
        file.path,
        file.filename,
        file.size,
        file.modified_at.to_rfc3339(),
        format!("{:?}", file.format),
        file.created_at.to_rfc3339(),
        file.metadata_hash,
        file.content_hash,
    ])?;
    Ok(conn.last_insert_rowid())
}

/// Insert FITS header with computed fingerprint
/// Uses prepare_cached() for better performance during bulk inserts
pub fn insert_fits_header(conn: &Connection, file_id: i64, header: &str) -> Result<i64> {
    let fingerprint = compute_header_fingerprint(header);
    let mut stmt = conn.prepare_cached(
        "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, ?3)",
    )?;
    stmt.execute(params![file_id, header, fingerprint])?;
    Ok(conn.last_insert_rowid())
}

/// Insert a new frame record
/// Uses prepare_cached() for better performance during bulk inserts
pub fn insert_frame(conn: &Connection, frame: &Frame) -> Result<i64> {
    let imagetyp_str = frame.imagetyp.as_ref().map(|t| format!("{:?}", t));
    let date_obs_str = frame.date_obs.as_ref().map(|d| d.to_rfc3339());
    let override_int = if frame.override_ { 1 } else { 0 };

    // Debug: Log what we're about to insert
    println!("insert_frame: file_id={}, object={:?}, date_obs={:?}",
        frame.file_id, frame.object, date_obs_str);

    let is_master_int = if frame.is_master { 1 } else { 0 };

    let mut stmt = conn.prepare_cached(
        "INSERT INTO frames (file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp, is_master,
         gain, offset, binning, xbinning, ybinning, ccd_temp, set_temp, focallen, xpixsz, ypixsz,
         naxis1, naxis2, ra, dec, sitelat, lat_obs, sitelong, long_obs, objctra, objctdec, override, swcreate, bayerpat, rotation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
         ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33)",
    )?;
    stmt.execute(params![
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
        frame.ypixsz,
        frame.naxis1,
        frame.naxis2,
        frame.ra,
        frame.dec,
        frame.sitelat,
        frame.lat_obs,
        frame.sitelong,
        frame.long_obs,
        frame.objctra,
        frame.objctdec,
        override_int,
        frame.swcreate,
        frame.bayerpat,
        frame.rotation,
    ])?;
    Ok(conn.last_insert_rowid())
}

/// Get all scan roots
pub fn get_scan_roots(conn: &Connection) -> Result<Vec<ScanRoot>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, enabled, find_duplicates, unique_camera, last_scan, last_scan_errors, monitor_enabled FROM scan_roots ORDER BY path"
    )?;

    let roots = stmt.query_map([], |row| {
        let last_scan_str: Option<String> = row.get(5)?;
        let last_scan = last_scan_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let last_scan_errors_str: Option<String> = row.get(6)?;
        let last_scan_errors = last_scan_errors_str
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok());

        Ok(ScanRoot {
            id: Some(row.get(0)?),
            path: row.get(1)?,
            enabled: row.get::<_, i32>(2)? == 1,
            find_duplicates: row.get::<_, i32>(3)? == 1,
            unique_camera: row.get::<_, i32>(4)? == 1,
            last_scan,
            last_scan_errors,
            monitor_enabled: row.get::<_, i32>(7)? == 1,
        })
    })?;

    roots.collect()}

/// Set monitor_enabled flag for a scan root
pub fn set_scan_root_monitor_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE scan_roots SET monitor_enabled = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, id],
    )?;
    Ok(())
}

/// Insert or update a scan root
pub fn upsert_scan_root(conn: &Connection, path: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO scan_roots (path, enabled, find_duplicates) VALUES (?1, 1, 1)
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

/// Update scan root find_duplicates flag
pub fn update_scan_root_duplicates_flag(conn: &Connection, id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE scan_roots SET find_duplicates = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, id],
    )?;
    Ok(())
}

/// Toggle unique_camera flag with cascade: updates frames.instrume, deletes affected
/// calibration sets, clears calibration links, and updates sessions in a single transaction.
/// Result of reconciling unique_camera instrume suffix state
#[derive(Debug)]
pub struct ReconcileResult {
    pub frames_renamed: usize,
    pub calibration_sets_deleted: usize,
    pub sessions_updated: usize,
}

/// Set unique_camera flag for a scan root (flag-only, no cascade)
pub fn set_unique_camera_flag(
    conn: &Connection,
    root_id: i64,
    enabled: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE scan_roots SET unique_camera = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, root_id],
    )?;
    Ok(())
}

/// Delete calibration sets for frames under a scan root
pub fn delete_calibration_sets_for_root(
    conn: &Connection,
    root_id: i64,
) -> Result<usize> {
    // Get root path
    let root_path: String = conn.query_row(
        "SELECT path FROM scan_roots WHERE id = ?1",
        params![root_id],
        |row| row.get(0),
    )?;

    let like_pattern = format!("{}%", root_path);

    // Find affected calibration set IDs (sets containing frames under this root)
    let affected_set_ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT csf.set_id
             FROM calibration_set_frames csf
             JOIN frames fr ON csf.frame_id = fr.id
             JOIN files f ON fr.file_id = f.id
             WHERE f.path LIKE ?1"
        )?;
        let rows = stmt.query_map(params![like_pattern], |row| row.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let calibration_sets_deleted = affected_set_ids.len();

    // Explicit cascade delete for affected calibration sets
    if !affected_set_ids.is_empty() {
        let placeholders: String = affected_set_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let set_values: Vec<rusqlite::types::Value> = affected_set_ids
            .iter()
            .map(|id| rusqlite::types::Value::Integer(*id))
            .collect();

        // calibration_set_frames
        let sql = format!("DELETE FROM calibration_set_frames WHERE set_id IN ({})", placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set_to_frames — as target (calibration_set_id)
        let sql = format!("DELETE FROM calibration_set_to_frames WHERE calibration_set_id IN ({})", placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set_to_frames — as source (source_type='calibration_set')
        let sql = format!(
            "DELETE FROM calibration_set_to_frames WHERE source_type = 'calibration_set' AND source_id IN ({})",
            placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set_originals
        let sql = format!("DELETE FROM calibration_set_originals WHERE set_id IN ({})", placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set
        let sql = format!("DELETE FROM calibration_set WHERE id IN ({})", placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;
    }

    // Clear calibration links for affected light frames (source_type='frame')
    conn.execute(
        "DELETE FROM calibration_set_to_frames
         WHERE source_type = 'frame'
           AND source_id IN (
             SELECT fr.id FROM frames fr
             JOIN files f ON fr.file_id = f.id
             WHERE f.path LIKE ?1
           )",
        params![like_pattern],
    )?;

    Ok(calibration_sets_deleted)
}

/// Reconcile unique_camera instrume suffix state for a scan root
/// Idempotent: if frames already match the flag, returns all zeros
pub fn reconcile_unique_camera_instrume(
    conn: &Connection,
    root_id: i64,
) -> Result<ReconcileResult> {
    // Read current flag and path
    let (unique_camera, root_path): (bool, String) = conn.query_row(
        "SELECT unique_camera, path FROM scan_roots WHERE id = ?1",
        params![root_id],
        |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)),
    )?;

    let suffix = format!(" N{}", root_id);
    let like_pattern = format!("{}%", root_path);

    conn.execute("BEGIN TRANSACTION", [])?;

    // Update frames.instrume based on flag state
    let frames_renamed = if unique_camera {
        // Add suffix — skip NULL instrume and already-suffixed
        conn.execute(
            "UPDATE frames SET instrume = instrume || ?1
             WHERE instrume IS NOT NULL
               AND instrume NOT LIKE '%' || ?1
               AND id IN (
                 SELECT fr.id FROM frames fr
                 JOIN files f ON fr.file_id = f.id
                 WHERE f.path LIKE ?2
               )",
            params![suffix, like_pattern],
        )?
    } else {
        // Strip suffix — only from frames that have it
        let suffix_len = suffix.len() as i64;
        conn.execute(
            "UPDATE frames SET instrume = SUBSTR(instrume, 1, LENGTH(instrume) - ?1)
             WHERE instrume IS NOT NULL
               AND instrume LIKE '%' || ?2
               AND id IN (
                 SELECT fr.id FROM frames fr
                 JOIN files f ON fr.file_id = f.id
                 WHERE f.path LIKE ?3
               )",
            params![suffix_len, suffix, like_pattern],
        )?
    };

    let mut calibration_sets_deleted = 0;
    let mut sessions_updated = 0;

    // Only cascade if frames were actually changed
    if frames_renamed > 0 {
        // Delete affected calibration sets
        calibration_sets_deleted = delete_calibration_sets_for_root(conn, root_id)?;

        // Update sessions.instrume to match updated frame values
        sessions_updated = conn.execute(
            "UPDATE sessions SET instrume = (
               SELECT fr.instrume FROM session_members sm
               JOIN frames fr ON sm.frame_id = fr.id
               WHERE sm.session_id = sessions.id AND fr.instrume IS NOT NULL
               LIMIT 1
             )
             WHERE id IN (
               SELECT DISTINCT sm.session_id FROM session_members sm
               JOIN frames fr ON sm.frame_id = fr.id
               JOIN files f ON fr.file_id = f.id
               WHERE f.path LIKE ?1
             )",
            params![like_pattern],
        )?;
    }

    conn.execute("COMMIT", [])?;

    Ok(ReconcileResult {
        frames_renamed,
        calibration_sets_deleted,
        sessions_updated,
    })
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

/// Persist scan errors for a scan root (stored as JSON, cleared if empty)
pub fn update_scan_root_errors(conn: &Connection, id: i64, errors: &[String]) -> anyhow::Result<()> {
    let json_val: Option<String> = if errors.is_empty() {
        None
    } else {
        Some(serde_json::to_string(errors).map_err(|e| anyhow::anyhow!(e))?)
    };
    conn.execute(
        "UPDATE scan_roots SET last_scan_errors = ?1 WHERE id = ?2",
        params![json_val, id],
    )?;
    Ok(())
}

/// Delete a scan root and all associated files
/// Optimized: uses transaction, pre-computes IDs, deletes child tables explicitly
pub fn delete_scan_root(conn: &Connection, id: i64) -> Result<()> {
    // Get the path of the scan root
    let path: String = conn.query_row(
        "SELECT path FROM scan_roots WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;

    // Start transaction for atomicity and performance
    conn.execute("BEGIN TRANSACTION", [])?;

    // Pre-compute file IDs to delete (single LIKE query)
    let file_ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM files WHERE path LIKE ?1 || '%'")?;
        let rows = stmt.query_map(params![path], |row| row.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if !file_ids.is_empty() {
        // Pre-compute frame IDs for these files
        let frame_ids: Vec<i64> = {
            let placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT id FROM frames WHERE file_id IN ({})", placeholders);
            let mut stmt = conn.prepare(&sql)?;
            let params_vec: Vec<rusqlite::types::Value> = file_ids.iter().map(|id| rusqlite::types::Value::Integer(*id)).collect();
            let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| row.get(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };

        // Delete child tables explicitly (avoiding cascade overhead)
        if !frame_ids.is_empty() {
            let frame_placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let frame_values: Vec<rusqlite::types::Value> = frame_ids.iter().map(|id| rusqlite::types::Value::Integer(*id)).collect();

            // 1. session_members
            let sql = format!("DELETE FROM session_members WHERE frame_id IN ({})", frame_placeholders);
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;

            // 2. calibration_set_frames
            let sql = format!("DELETE FROM calibration_set_frames WHERE frame_id IN ({})", frame_placeholders);
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;

            // 3. frame_tags
            let sql = format!("DELETE FROM frame_tags WHERE frame_id IN ({})", frame_placeholders);
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;

            // 4. calibration_set_to_frames (source_id refers to frame_id when source_type='frame')
            let sql = format!("DELETE FROM calibration_set_to_frames WHERE source_id IN ({}) AND source_type = 'frame'", frame_placeholders);
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;
        }

        // Delete by file_id
        let file_placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let file_values: Vec<rusqlite::types::Value> = file_ids.iter().map(|id| rusqlite::types::Value::Integer(*id)).collect();

        // 5. fits_header
        let sql = format!("DELETE FROM fits_header WHERE file_id IN ({})", file_placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;

        // 6. black_hole
        let sql = format!("DELETE FROM black_hole WHERE file_id IN ({})", file_placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;

        // 7. frames
        let sql = format!("DELETE FROM frames WHERE file_id IN ({})", file_placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;

        // 8. files
        let sql = format!("DELETE FROM files WHERE id IN ({})", file_placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;
    }

    // 9. Delete orphaned calibration sets
    conn.execute(
        "DELETE FROM calibration_set WHERE id NOT IN (
            SELECT DISTINCT set_id FROM calibration_set_frames
        )",
        [],
    )?;

    // 10. Delete orphaned sessions and imaging_nights first
    conn.execute(
        "DELETE FROM sessions WHERE id NOT IN (
            SELECT DISTINCT session_id FROM session_members
        )",
        [],
    )?;

    conn.execute(
        "DELETE FROM imaging_nights WHERE id NOT IN (
            SELECT DISTINCT imaging_night_id FROM sessions
        )",
        [],
    )?;

    // 11. Delete orphaned frame sets
    conn.execute(
        "DELETE FROM frames_set WHERE id NOT IN (
            SELECT DISTINCT frames_set_id FROM imaging_nights
        )",
        [],
    )?;

    // 12. Delete the scan root
    conn.execute("DELETE FROM scan_roots WHERE id = ?1", params![id])?;

    // Commit transaction
    conn.execute("COMMIT", [])?;

    Ok(())
}

/// Get a file by its path
pub fn get_file_by_path(conn: &Connection, path: &str) -> Result<File> {
    conn.query_row(
        "SELECT id, path, filename, size, modified_at, format, created_at, metadata_hash, content_hash,
                archived_in_operation, archive_zip_path, archive_path_in_zip
         FROM files WHERE path = ?1",
        params![path],
        |row| {
            Ok(File {
                id: row.get(0)?,
                path: row.get(1)?,
                filename: row.get(2)?,
                size: row.get(3)?,
                modified_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&Utc),
                format: match row.get::<_, String>(5)?.as_str() {
                    "FITS" => FileFormat::FITS,
                    "XISF" => FileFormat::XISF,
                    _ => FileFormat::FITS,
                },
                created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
                    .unwrap()
                    .with_timezone(&Utc),
                metadata_hash: row.get(7)?,
                content_hash: row.get(8)?,
                archived_in_operation: row.get(9)?,
                archive_zip_path: row.get(10)?,
                archive_path_in_zip: row.get(11)?,
            })
        },
    )
}

/// Get all files with optional filters
pub fn get_files(conn: &Connection, limit: Option<usize>) -> Result<Vec<(File, Option<Frame>)>> {
    let limit_clause = match limit {
        Some(n) => format!("LIMIT {}", n),
        None => String::new(),
    };

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation
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
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
        };

        let frame = if let Ok(frame_id) = row.get::<_, Option<i64>>(12) {
            frame_id.map(|fid| Frame {
                id: Some(fid),
                file_id: file.id.unwrap(),
                object: row.get(13).ok(),
                date_obs: row.get::<_, Option<String>>(14).ok().flatten().and_then(|s| {
                    DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                }),
                telescop: row.get(15).ok(),
                instrume: row.get(16).ok(),
                exptime: row.get(17).ok(),
                filter: row.get(18).ok(),
                imagetyp: row.get::<_, Option<String>>(19).ok().flatten().and_then(|s| ImageType::from_str(&s)),
                is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
                gain: row.get(21).ok(),
                offset: row.get(22).ok(),
                binning: row.get(23).ok(),
                xbinning: row.get(24).ok(),
                ybinning: row.get(25).ok(),
                ccd_temp: row.get(26).ok(),
                set_temp: row.get(27).ok(),
                focallen: row.get(28).ok(),
                xpixsz: row.get(29).ok(),
                ypixsz: row.get(30).ok(),
                naxis1: row.get(31).ok(),
                naxis2: row.get(32).ok(),
                ra: row.get(33).ok(),
                dec: row.get(34).ok(),
                sitelat: row.get(35).ok(),
                lat_obs: row.get(36).ok(),
                sitelong: row.get(37).ok(),
                long_obs: row.get(38).ok(),
                objctra: row.get(39).ok(),
                objctdec: row.get(40).ok(),
                override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
                swcreate: row.get(42).ok(),
                bayerpat: row.get(43).ok(),
                rotation: row.get(44).ok(),
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
    // Use OS path separator for cross-platform compatibility (/ on macOS/Linux, \ on Windows)
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let like_pattern = format!("{}{}%", directory_path, sep);
    let expected_depth = directory_path.matches(sep.as_str()).count() as i64 + 1;

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation
         FROM files f
         LEFT JOIN frames fr ON f.id = fr.file_id
         WHERE f.path LIKE ?1
           AND (LENGTH(f.path) - LENGTH(REPLACE(f.path, ?2, ''))) = ?3
         ORDER BY f.filename
         {}",
        limit_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let results = stmt.query_map(params![like_pattern, sep, expected_depth], |row| {
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
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
        };

        let frame = if let Ok(frame_id) = row.get::<_, Option<i64>>(12) {
            frame_id.map(|fid| Frame {
                id: Some(fid),
                file_id: file.id.unwrap(),
                object: row.get(13).ok(),
                date_obs: row.get::<_, Option<String>>(14).ok().flatten().and_then(|s| {
                    DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                }),
                telescop: row.get(15).ok(),
                instrume: row.get(16).ok(),
                exptime: row.get(17).ok(),
                filter: row.get(18).ok(),
                imagetyp: row.get::<_, Option<String>>(19).ok().flatten().and_then(|s| ImageType::from_str(&s)),
                is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
                gain: row.get(21).ok(),
                offset: row.get(22).ok(),
                binning: row.get(23).ok(),
                xbinning: row.get(24).ok(),
                ybinning: row.get(25).ok(),
                ccd_temp: row.get(26).ok(),
                set_temp: row.get(27).ok(),
                focallen: row.get(28).ok(),
                xpixsz: row.get(29).ok(),
                ypixsz: row.get(30).ok(),
                naxis1: row.get(31).ok(),
                naxis2: row.get(32).ok(),
                ra: row.get(33).ok(),
                dec: row.get(34).ok(),
                sitelat: row.get(35).ok(),
                lat_obs: row.get(36).ok(),
                sitelong: row.get(37).ok(),
                long_obs: row.get(38).ok(),
                objctra: row.get(39).ok(),
                objctdec: row.get(40).ok(),
                override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
                swcreate: row.get(42).ok(),
                bayerpat: row.get(43).ok(),
                rotation: row.get(44).ok(),
            })
        } else {
            None
        };

        Ok((file, frame))
    })?;

    results.collect()
}

/// Get files in a specific directory filtered by camera (instrume)
pub fn get_files_by_directory_for_camera(
    conn: &Connection,
    directory_path: &str,
    instrume: &str,
    limit: Option<usize>
) -> Result<Vec<(File, Option<Frame>)>> {
    let limit_clause = match limit {
        Some(n) => format!("LIMIT {}", n),
        None => String::new(),
    };

    let sep = std::path::MAIN_SEPARATOR.to_string();
    let like_pattern = format!("{}{}%", directory_path, sep);
    let expected_depth = directory_path.matches(sep.as_str()).count() as i64 + 1;

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation
         FROM files f
         JOIN frames fr ON f.id = fr.file_id
         WHERE f.path LIKE ?1
           AND (LENGTH(f.path) - LENGTH(REPLACE(f.path, ?2, ''))) = ?3
           AND fr.instrume = ?4
         ORDER BY f.filename
         {}",
        limit_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let results = stmt.query_map(params![like_pattern, sep, expected_depth, instrume], |row| {
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
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
        };

        let frame = Some(Frame {
            id: Some(row.get(12)?),
            file_id: file.id.unwrap(),
            object: row.get(13).ok(),
            date_obs: row.get::<_, Option<String>>(14).ok().flatten().and_then(|s| {
                DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
            }),
            telescop: row.get(15).ok(),
            instrume: row.get(16).ok(),
            exptime: row.get(17).ok(),
            filter: row.get(18).ok(),
            imagetyp: row.get::<_, Option<String>>(19).ok().flatten().and_then(|s| ImageType::from_str(&s)),
            is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
            gain: row.get(21).ok(),
            offset: row.get(22).ok(),
            binning: row.get(23).ok(),
            xbinning: row.get(24).ok(),
            ybinning: row.get(25).ok(),
            ccd_temp: row.get(26).ok(),
            set_temp: row.get(27).ok(),
            focallen: row.get(28).ok(),
            xpixsz: row.get(29).ok(),
            ypixsz: row.get(30).ok(),
            naxis1: row.get(31).ok(),
            naxis2: row.get(32).ok(),
            ra: row.get(33).ok(),
            dec: row.get(34).ok(),
            sitelat: row.get(35).ok(),
            lat_obs: row.get(36).ok(),
            sitelong: row.get(37).ok(),
            long_obs: row.get(38).ok(),
            objctra: row.get(39).ok(),
            objctdec: row.get(40).ok(),
            override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
            swcreate: row.get(42).ok(),
            bayerpat: row.get(43).ok(),
            rotation: row.get(44).ok(),
        });

        Ok((file, frame))
    })?;

    results.collect()
}

/// Get frames with missing metadata
/// category: "all", "coordinates", "object", "datetime", "instrument", "frametype"
///
/// Each returned row includes a `has_duplicate` flag that is true when another
/// file in the catalog shares either the same `content_hash` or the same
/// `metadata_hash`. That covers both exact-byte duplicates (content hash) and
/// "same header/size/filename" near-duplicates (metadata hash), matching how
/// the rest of the app treats duplicates.
pub fn get_frames_with_missing_metadata(
    conn: &Connection,
    category: &str,
) -> Result<Vec<MissingMetadataRow>> {
    // "Missing coordinates" catches three sentinel patterns that FITS
    // pipelines write instead of leaving fields NULL:
    //   1. numeric ra/dec = NULL
    //   2. numeric (ra, dec) = (0, 0) — literal zero, no one images at 0,0
    //   3. sexagesimal objctra/objctdec that stringify to all-zero after
    //      stripping whitespace, signs, and colons — e.g. "00 00 00",
    //      "+00 00 00", "00:00:00". Thousands of LIGHT frames in typical
    //      libraries have these because the acquisition software wrote a
    //      placeholder rather than the real RA/Dec.
    //
    // A frame counts as "missing coordinates" when BOTH the numeric side
    // and the sexagesimal side are in one of the above states. The same
    // clause is inlined into the "all" branch below.
    let coords_missing_clause =
        "((fr.ra IS NULL AND fr.dec IS NULL) OR (fr.ra = 0 AND fr.dec = 0)) \
         AND (\
             fr.objctra IS NULL OR fr.objctdec IS NULL \
             OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctra, ' ', ''), '+', ''), '-', ''), ':', '') = '000000' \
             OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctdec, ' ', ''), '+', ''), '-', ''), ':', '') = '000000'\
         )";
    // LIGHT-only categories: coordinates and object. Darks/flats/bias legitimately
    // have no RA/Dec and no target OBJECT — surfacing them under those chips would
    // be noise. For instrument/datetime/frametype, all frame types qualify because
    // the user may need to fix a missing camera/date/type on any kind of frame.
    let (missing_clause, imagetyp_filter) = match category {
        "coordinates" => (coords_missing_clause, "UPPER(fr.imagetyp) = 'LIGHT' AND"),
        "object" => ("fr.object IS NULL OR fr.object = ''", "UPPER(fr.imagetyp) = 'LIGHT' AND"),
        "datetime" => ("fr.date_obs IS NULL", ""),
        "instrument" => ("fr.instrume IS NULL OR fr.instrume = ''", ""),
        "frametype" => ("(fr.imagetyp IS NULL OR fr.imagetyp = '')", ""),
        _ => (
            "(UPPER(fr.imagetyp) = 'LIGHT' AND ((((fr.ra IS NULL AND fr.dec IS NULL) OR (fr.ra = 0 AND fr.dec = 0)) AND (fr.objctra IS NULL OR fr.objctdec IS NULL OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctra, ' ', ''), '+', ''), '-', ''), ':', '') = '000000' OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctdec, ' ', ''), '+', ''), '-', ''), ':', '') = '000000')) OR (fr.object IS NULL OR fr.object = ''))) OR fr.date_obs IS NULL OR (fr.instrume IS NULL OR fr.instrume = '') OR (fr.imagetyp IS NULL OR fr.imagetyp = '')",
            ""
        ),
    };

    // `has_duplicate` is a LEFT JOIN against the cached `duplicate_group_files`
    // table (populated by the existing duplicate-detection pipeline). No row
    // means the file isn't in any detected duplicate group; any match means it
    // is. Uses the `idx_dup_group_files_file` index for O(1) lookups.
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation,
                CASE WHEN dgf.file_id IS NOT NULL THEN 1 ELSE 0 END AS has_duplicate
         FROM files f
         INNER JOIN frames fr ON f.id = fr.file_id
         LEFT JOIN duplicate_group_files dgf ON dgf.file_id = f.id
         WHERE {} ({})
         GROUP BY f.id
         ORDER BY f.modified_at DESC",
        imagetyp_filter, missing_clause
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
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
        };

        let frame = Frame {
            id: Some(row.get(12)?),
            file_id: file.id.unwrap(),
            object: row.get(13).ok(),
            date_obs: row.get::<_, Option<String>>(14).ok().flatten().and_then(|s| {
                DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
            }),
            telescop: row.get(15).ok(),
            instrume: row.get(16).ok(),
            exptime: row.get(17).ok(),
            filter: row.get(18).ok(),
            imagetyp: row.get::<_, Option<String>>(19).ok().flatten().and_then(|s| ImageType::from_str(&s)),
            is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
            gain: row.get(21).ok(),
            offset: row.get(22).ok(),
            binning: row.get(23).ok(),
            xbinning: row.get(24).ok(),
            ybinning: row.get(25).ok(),
            ccd_temp: row.get(26).ok(),
            set_temp: row.get(27).ok(),
            focallen: row.get(28).ok(),
            xpixsz: row.get(29).ok(),
            ypixsz: row.get(30).ok(),
            naxis1: row.get(31).ok(),
            naxis2: row.get(32).ok(),
            ra: row.get(33).ok(),
            dec: row.get(34).ok(),
            sitelat: row.get(35).ok(),
            lat_obs: row.get(36).ok(),
            sitelong: row.get(37).ok(),
            long_obs: row.get(38).ok(),
            objctra: row.get(39).ok(),
            objctdec: row.get(40).ok(),
            override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
            swcreate: row.get(42).ok(),
            bayerpat: row.get(43).ok(),
            rotation: row.get(44).ok(),
        };

        let has_duplicate: i32 = row.get(45)?;

        Ok(MissingMetadataRow {
            file,
            frame,
            has_duplicate: has_duplicate != 0,
        })
    })?;

    results.collect()
}

/// Bulk-update selected metadata fields on the given frames.
///
/// Used by the Missing Metadata page's Set Camera / Set Date / Set Frame Type
/// actions. DB-only update — the underlying FITS files on disk are NOT touched.
/// Every row that gets any field updated also has `override = 1` set, matching
/// the existing pattern so re-scanners know the DB row beats the on-disk header.
///
/// Only fields that are `Some` in `edits` are written. If `edits` has no fields,
/// the call is a no-op and returns 0.
///
/// `imagetyp` is normalized via `ImageType::from_str` when recognized so the
/// stored string matches what `insert_frame` produces (the Debug form of the
/// enum variant, e.g. `"Dark"`, `"MasterDark"`). Unknown strings are stored
/// as-given.
pub fn bulk_update_frame_metadata(
    conn: &Connection,
    frame_ids: &[i64],
    edits: &FrameMetadataEdits,
) -> Result<usize> {
    if frame_ids.is_empty() {
        return Ok(0);
    }
    if edits.instrume.is_none()
        && edits.date_obs.is_none()
        && edits.imagetyp.is_none()
        && edits.is_master.is_none()
        && edits.object.is_none()
        && edits.filter.is_none()
        && edits.telescop.is_none()
        && edits.focallen.is_none()
        && edits.gain.is_none()
        && edits.offset.is_none()
        && edits.binning.is_none()
        && edits.exptime.is_none()
        && edits.ccd_temp.is_none()
    {
        return Ok(0);
    }

    // Build the SET clauses and collect parameters in order.
    use rusqlite::types::Value;
    let mut set_clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Value> = Vec::new();

    if let Some(instrume) = &edits.instrume {
        set_clauses.push("instrume = ?");
        values.push(Value::Text(instrume.clone()));
    }
    if let Some(date_obs) = &edits.date_obs {
        set_clauses.push("date_obs = ?");
        values.push(Value::Text(date_obs.clone()));
    }
    // imagetyp + is_master are paired: setting imagetyp also writes the
    // matching is_master bool (true for any Master* variant, false for the
    // base variants). This keeps Equipment's master detection (which filters
    // on the imagetyp string) and any code that reads frames.is_master in
    // sync — no UI checkbox is needed for the toggle.
    let mut imagetyp_set_master_flag: Option<bool> = None;
    if let Some(imagetyp) = &edits.imagetyp {
        let parsed = ImageType::from_str(imagetyp);
        let normalized = parsed
            .as_ref()
            .map(|t| format!("{:?}", t))
            .unwrap_or_else(|| imagetyp.clone());
        set_clauses.push("imagetyp = ?");
        values.push(Value::Text(normalized));
        if let Some(t) = parsed {
            imagetyp_set_master_flag = Some(t.is_master());
        }
    }
    // Derived is_master from imagetyp wins over an explicit edits.is_master
    // when both are present (older callers pass them separately; the dropdown
    // never does). Otherwise honour edits.is_master, otherwise leave alone.
    let final_is_master: Option<bool> = imagetyp_set_master_flag.or(edits.is_master);
    if let Some(is_master) = final_is_master {
        set_clauses.push("is_master = ?");
        values.push(Value::Integer(if is_master { 1 } else { 0 }));
    }
    if let Some(object) = &edits.object {
        set_clauses.push("object = ?");
        values.push(Value::Text(object.clone()));
    }
    if let Some(filter) = &edits.filter {
        set_clauses.push("filter = ?");
        values.push(Value::Text(filter.clone()));
    }
    if let Some(telescop) = &edits.telescop {
        set_clauses.push("telescop = ?");
        values.push(Value::Text(telescop.clone()));
    }
    if let Some(focallen) = edits.focallen {
        set_clauses.push("focallen = ?");
        values.push(Value::Real(focallen));
    }
    if let Some(gain) = edits.gain {
        set_clauses.push("gain = ?");
        values.push(Value::Real(gain));
    }
    if let Some(offset) = edits.offset {
        set_clauses.push("offset = ?");
        values.push(Value::Real(offset));
    }
    if let Some(binning) = &edits.binning {
        // Validate "AxB" format with positive integers and store xbinning /
        // ybinning in lockstep so derived queries (calibration matching, etc.)
        // see consistent values.
        let parts: Vec<&str> = binning.split('x').collect();
        if parts.len() != 2 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                format!("binning must be in 'AxB' format (e.g. '1x1'), got '{}'", binning).into(),
            ));
        }
        let xb: i64 = parts[0].parse().map_err(|_| {
            rusqlite::Error::ToSqlConversionFailure(
                format!("binning x component must be an integer, got '{}'", parts[0]).into(),
            )
        })?;
        let yb: i64 = parts[1].parse().map_err(|_| {
            rusqlite::Error::ToSqlConversionFailure(
                format!("binning y component must be an integer, got '{}'", parts[1]).into(),
            )
        })?;
        if xb < 1 || yb < 1 || xb > 16 || yb > 16 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                format!("binning components must be 1-16, got '{}'", binning).into(),
            ));
        }
        set_clauses.push("binning = ?");
        values.push(Value::Text(binning.clone()));
        set_clauses.push("xbinning = ?");
        values.push(Value::Integer(xb));
        set_clauses.push("ybinning = ?");
        values.push(Value::Integer(yb));
    }
    if let Some(exptime) = edits.exptime {
        if !exptime.is_finite() || exptime <= 0.0 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                format!("exptime must be > 0 seconds, got {}", exptime).into(),
            ));
        }
        set_clauses.push("exptime = ?");
        values.push(Value::Real(exptime));
    }
    if let Some(ccd_temp) = edits.ccd_temp {
        if !ccd_temp.is_finite() {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                format!("ccd_temp must be a finite number, got {}", ccd_temp).into(),
            ));
        }
        set_clauses.push("ccd_temp = ?");
        values.push(Value::Real(ccd_temp));
    }
    // Always flag touched rows as override so rescans preserve the edit.
    set_clauses.push("override = 1");

    let id_placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    for id in frame_ids {
        values.push(Value::Integer(*id));
    }

    let sql = format!(
        "UPDATE frames SET {} WHERE id IN ({})",
        set_clauses.join(", "),
        id_placeholders
    );

    let count = conn
        .execute(&sql, rusqlite::params_from_iter(values.iter()))
        .map_err(|e| {
            eprintln!("bulk_update_frame_metadata failed: {} (sql={})", e, sql);
            e
        })?;

    // ANY metadata edit invalidates the frame's existing relations to
    // calibration sets (as a member or as a consumer) and to sessions /
    // imaging nights / frame sets. The user has chosen to be conservative:
    // editing CCD-TEMP, FILTER, OBJECT, IMAGETYP — anything — unlinks. The
    // UI must warn before calling this (see CountFrameMetadataRelations).
    //
    // Calibration sets that lose their last member are pruned (otherwise a
    // single-frame "master" set would survive as a zombie pointing nowhere).
    // Sessions / imaging_nights / frames_sets are intentionally left in
    // place even when empty — same policy as elsewhere.
    {
        let id_placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let frame_id_params: Vec<rusqlite::types::Value> = frame_ids
            .iter()
            .map(|id| rusqlite::types::Value::Integer(*id))
            .collect();

        // Capture which calibration sets had any of these frames as members,
        // BEFORE we delete the junction rows. We'll re-check each set after
        // the delete and prune any that became empty.
        let sql_affected = format!(
            "SELECT DISTINCT set_id FROM calibration_set_frames WHERE frame_id IN ({})",
            id_placeholders
        );
        let mut stmt_affected = conn.prepare(&sql_affected)?;
        let affected_set_ids: Vec<i64> = stmt_affected
            .query_map(
                rusqlite::params_from_iter(frame_id_params.iter()),
                |r| r.get::<_, i64>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt_affected);

        // calibration_set_frames: was a member of a calibration set.
        let sql_csf = format!(
            "DELETE FROM calibration_set_frames WHERE frame_id IN ({})",
            id_placeholders
        );
        conn.execute(&sql_csf, rusqlite::params_from_iter(frame_id_params.iter()))
            .map_err(|e| {
                eprintln!(
                    "bulk_update_frame_metadata cascade calibration_set_frames failed: {}",
                    e
                );
                e
            })?;

        // calibration_set_to_frames: consumer link from frame → calibration set.
        let sql_cstf = format!(
            "DELETE FROM calibration_set_to_frames WHERE source_type = 'frame' AND source_id IN ({})",
            id_placeholders
        );
        conn.execute(&sql_cstf, rusqlite::params_from_iter(frame_id_params.iter()))
            .map_err(|e| {
                eprintln!(
                    "bulk_update_frame_metadata cascade calibration_set_to_frames failed: {}",
                    e
                );
                e
            })?;

        // session_members: frame's seat in a session/imaging-night/frame-set.
        let sql_sm = format!(
            "DELETE FROM session_members WHERE frame_id IN ({})",
            id_placeholders
        );
        conn.execute(&sql_sm, rusqlite::params_from_iter(frame_id_params.iter()))
            .map_err(|e| {
                eprintln!(
                    "bulk_update_frame_metadata cascade session_members failed: {}",
                    e
                );
                e
            })?;

        // Prune calibration sets that now have zero member frames. Includes
        // master sets (single-frame) — when their only frame is reclassified
        // to a non-calibration type, the set is dead. Multi-frame regular
        // sets only get pruned if EVERY member was just unlinked, which is
        // the correct semantic. Consumer links (`calibration_set_to_frames`)
        // are removed automatically by the FK CASCADE on
        // calibration_set_to_frames.calibration_set_id.
        for set_id in affected_set_ids {
            let remaining: i64 = conn.query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1",
                [set_id],
                |r| r.get(0),
            )?;
            if remaining == 0 {
                conn.execute("DELETE FROM calibration_set WHERE id = ?1", [set_id])
                    .map_err(|e| {
                        eprintln!(
                            "bulk_update_frame_metadata prune empty calibration_set #{} failed: {}",
                            set_id, e
                        );
                        e
                    })?;
            }
        }
    }

    // After writing the edits the override flag is on for every touched
    // frame. If the user just reverted everything back to its original
    // file-header values, drop the flag so the row stops looking edited.
    // Errors here aren't fatal — the edits themselves succeeded already.
    if let Err(e) = recompute_override_flag_for_frames(conn, frame_ids) {
        eprintln!(
            "recompute_override_flag_for_frames after bulk_update failed: {}",
            e
        );
    }

    Ok(count)
}

/// Return the distinct non-empty INSTRUME values seen in the `frames` table,
/// alphabetically sorted. Used by the Set Camera modal to populate its dropdown.
pub fn get_distinct_instrumes(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT instrume FROM frames \
         WHERE instrume IS NOT NULL AND TRIM(instrume) != '' \
         ORDER BY instrume COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Find duplicates by filename and metadata, or by content hash
/// Only includes files from scan roots where find_duplicates = 1
///
/// If use_content_hash is true, groups by content_hash (xxhash).
/// If use_content_hash is false, groups by metadata_hash (size + modified + filename).
pub fn find_duplicate_groups(conn: &Connection, use_content_hash: bool) -> Result<Vec<DuplicateGroup>> {
    let hash_column = if use_content_hash { "content_hash" } else { "metadata_hash" };

    let query = format!(
        "SELECT f.{}, f.size, COUNT(*) as count, GROUP_CONCAT(f.path, '|') as paths, GROUP_CONCAT(f.id, '|') as ids
         FROM files f
         WHERE f.{} IS NOT NULL
         AND NOT EXISTS (
             SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id
         )
         AND EXISTS (
             SELECT 1 FROM scan_roots sr
             WHERE sr.find_duplicates = 1
             AND (f.path LIKE sr.path || '%' OR f.path LIKE sr.path || '/%')
         )
         GROUP BY f.{}, f.size
         HAVING count > 1
         ORDER BY count DESC, f.size DESC",
        hash_column, hash_column, hash_column
    );

    let mut stmt = conn.prepare(&query)?;

    let mut groups: Vec<DuplicateGroup> = stmt.query_map([], |row| {
        let paths_str: String = row.get(3)?;
        let file_paths: Vec<String> = paths_str.split('|').map(|s| s.to_string()).collect();

        let ids_str: String = row.get(4)?;
        let file_ids: Vec<i64> = ids_str.split('|')
            .filter_map(|s| s.parse::<i64>().ok())
            .collect();

        Ok(DuplicateGroup {
            id: None,
            size: row.get(1)?,
            content_hash: row.get(0)?,
            file_count: row.get(2)?,
            file_paths,
            file_ids,
            files: Vec::new(),
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?;

    enrich_duplicate_groups(conn, &mut groups)?;
    Ok(groups)
}

/// Fetch per-file metadata (modified_at, scan-root assignment) for every file
/// in `groups` and fill in each group's `files` field. Scan-root assignment is
/// a longest-prefix match against `scan_roots.path` computed in Rust so the
/// caller doesn't need to add a join per row.
fn enrich_duplicate_groups(conn: &Connection, groups: &mut [DuplicateGroup]) -> Result<()> {
    if groups.is_empty() {
        return Ok(());
    }

    // Load scan roots once. Small table; a fresh read every call is cheaper
    // than caching and getting stale.
    let mut sr_stmt = conn.prepare("SELECT id, path FROM scan_roots ORDER BY length(path) DESC")?;
    let scan_roots: Vec<(i64, String)> = sr_stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    // One SELECT covering every file id across every group. Pulls the frame
    // row (imagetyp, date_obs, filename) via LEFT JOIN so files without an
    // extracted frame still come through with None-y frame fields.
    let mut all_ids: Vec<i64> = groups.iter().flat_map(|g| g.file_ids.iter().copied()).collect();
    all_ids.sort_unstable();
    all_ids.dedup();
    if all_ids.is_empty() {
        return Ok(());
    }
    let placeholders: String = std::iter::repeat("?").take(all_ids.len()).collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.modified_at, fr.imagetyp, fr.date_obs
         FROM files f
         LEFT JOIN frames fr ON fr.file_id = f.id
         WHERE f.id IN ({})",
        placeholders
    );
    let params_vec: Vec<rusqlite::types::Value> = all_ids.iter().map(|i| rusqlite::types::Value::Integer(*i)).collect();
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let filename: String = row.get(2)?;
        let modified_at_str: String = row.get(3)?;
        let imagetyp: Option<String> = row.get(4)?;
        let date_obs: Option<String> = row.get(5)?;
        Ok((id, path, filename, modified_at_str, imagetyp, date_obs))
    })?;

    struct Row {
        path: String,
        filename: String,
        modified_at: chrono::DateTime<chrono::Utc>,
        imagetyp: Option<String>,
        date_obs: Option<String>,
    }
    let mut by_id: std::collections::HashMap<i64, Row> = std::collections::HashMap::new();
    for row in rows {
        let (id, path, filename, modified_at_str, imagetyp, date_obs) = row?;
        let modified_at = chrono::DateTime::parse_from_rfc3339(&modified_at_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        by_id.insert(id, Row { path, filename, modified_at, imagetyp, date_obs });
    }

    for group in groups.iter_mut() {
        let mut files: Vec<crate::models::DuplicateFile> = Vec::with_capacity(group.file_ids.len());
        for file_id in &group.file_ids {
            let Some(r) = by_id.get(file_id) else { continue };
            let (scan_root_id, scan_root_path) = scan_roots
                .iter()
                .find(|(_, sr_path)| r.path.starts_with(sr_path.as_str()))
                .map(|(id, sr_path)| (Some(*id), Some(sr_path.clone())))
                .unwrap_or((None, None));
            files.push(crate::models::DuplicateFile {
                file_id: *file_id,
                path: r.path.clone(),
                filename: r.filename.clone(),
                modified_at: r.modified_at,
                scan_root_id,
                scan_root_path,
                imagetyp: r.imagetyp.clone(),
                date_obs: r.date_obs.clone(),
            });
        }
        group.files = files;
    }

    Ok(())
}

/// Rebuild the duplicate groups cache tables
/// This clears existing cache and recomputes all duplicate groups
pub fn rebuild_duplicate_groups_cache(conn: &Connection, use_content_hash: bool) -> Result<usize> {
    let hash_type = if use_content_hash { "content" } else { "metadata" };
    let hash_column = if use_content_hash { "content_hash" } else { "metadata_hash" };

    // Start transaction
    conn.execute("BEGIN TRANSACTION", [])?;

    // Clear existing cache for this hash type
    conn.execute(
        "DELETE FROM duplicate_group_files WHERE group_id IN (SELECT id FROM duplicate_groups WHERE hash_type = ?1)",
        params![hash_type],
    )?;
    conn.execute(
        "DELETE FROM duplicate_groups WHERE hash_type = ?1",
        params![hash_type],
    )?;

    // Find duplicate files (groups with count > 1)
    let query = format!(
        "SELECT f.{}, f.size, COUNT(*) as count
         FROM files f
         WHERE f.{} IS NOT NULL
         AND NOT EXISTS (
             SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id
         )
         AND EXISTS (
             SELECT 1 FROM scan_roots sr
             WHERE sr.find_duplicates = 1
             AND f.path LIKE sr.path || '%'
         )
         GROUP BY f.{}, f.size
         HAVING count > 1",
        hash_column, hash_column, hash_column
    );

    let mut stmt = conn.prepare(&query)?;
    let rows: Vec<(String, i64, i64)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let now = chrono::Utc::now().to_rfc3339();
    let mut groups_created = 0;

    for (hash, size, file_count) in rows {
        // Insert the group
        conn.execute(
            "INSERT INTO duplicate_groups (hash, hash_type, size, file_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![hash, hash_type, size, file_count, now],
        )?;
        let group_id = conn.last_insert_rowid();

        // Find and insert all files in this group
        let files_query = format!(
            "SELECT id FROM files WHERE {} = ?1 AND size = ?2
             AND NOT EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = files.id)
             AND EXISTS (
                 SELECT 1 FROM scan_roots sr
                 WHERE sr.find_duplicates = 1
                 AND files.path LIKE sr.path || '%'
             )",
            hash_column
        );
        let mut files_stmt = conn.prepare(&files_query)?;
        let file_ids: Vec<i64> = files_stmt
            .query_map(params![hash, size], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        for file_id in file_ids {
            conn.execute(
                "INSERT OR IGNORE INTO duplicate_group_files (group_id, file_id) VALUES (?1, ?2)",
                params![group_id, file_id],
            )?;
        }

        groups_created += 1;
    }

    conn.execute("COMMIT", [])?;
    Ok(groups_created)
}

/// Get duplicate groups from cache
/// Returns cached duplicate groups with file paths and IDs
pub fn get_cached_duplicates(conn: &Connection, use_content_hash: bool) -> Result<Vec<DuplicateGroup>> {
    let hash_type = if use_content_hash { "content" } else { "metadata" };

    let mut stmt = conn.prepare(
        "SELECT dg.id, dg.hash, dg.size, dg.file_count
         FROM duplicate_groups dg
         WHERE dg.hash_type = ?1
         ORDER BY dg.file_count DESC, dg.size DESC"
    )?;

    let groups: Vec<(i64, String, i64, i64)> = stmt
        .query_map(params![hash_type], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut result = Vec::with_capacity(groups.len());

    for (group_id, hash, size, _file_count) in groups {
        // Get files for this group, excluding files in black_hole
        let mut files_stmt = conn.prepare(
            "SELECT f.id, f.path
             FROM duplicate_group_files dgf
             JOIN files f ON f.id = dgf.file_id
             WHERE dgf.group_id = ?1
             AND NOT EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id)
             ORDER BY f.path"
        )?;

        let files: Vec<(i64, String)> = files_stmt
            .query_map(params![group_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // Skip if fewer than 2 files (no longer duplicates after black_hole filtering)
        if files.len() < 2 {
            continue;
        }

        let file_ids: Vec<i64> = files.iter().map(|(id, _)| *id).collect();
        let file_paths: Vec<String> = files.iter().map(|(_, path)| path.clone()).collect();

        result.push(DuplicateGroup {
            id: Some(group_id),
            size,
            content_hash: hash,
            file_count: files.len() as i32,  // Use actual count after filtering
            file_paths,
            file_ids,
            files: Vec::new(),
        });
    }

    enrich_duplicate_groups(conn, &mut result)?;
    Ok(result)
}

/// Check if duplicate cache exists and has data
pub fn has_duplicate_cache(conn: &Connection, use_content_hash: bool) -> Result<bool> {
    let hash_type = if use_content_hash { "content" } else { "metadata" };
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM duplicate_groups WHERE hash_type = ?1",
        params![hash_type],
        |row| row.get(0),
    )?;
    Ok(count > 0)
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
    date_obs_start: Option<&str>,
    date_obs_end: Option<&str>,
    objctra: Option<&str>,
    objctdec: Option<&str>,
    total_exp_time: Option<f64>,
    avg_rotation: Option<f64>,
    min_rotation: Option<f64>,
    max_rotation: Option<f64>,
) -> Result<i64> {
    let is_custom_int = if is_custom { 1 } else { 0 };
    conn.execute(
        "INSERT INTO frames_set (name, is_custom, date_obs_start, date_obs_end, objctra, objctdec, total_exp_time, avg_rotation, min_rotation, max_rotation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![name, is_custom_int, date_obs_start, date_obs_end, objctra, objctdec, total_exp_time, avg_rotation, min_rotation, max_rotation],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all frames sets with member counts
pub fn get_frames_sets_by_project(
    conn: &Connection,
    _project_id: i64,  // Kept for backwards compatibility, but ignored
) -> Result<Vec<(crate::models::FramesSet, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT fs.id, fs.name, fs.is_custom, fs.date_obs_start, fs.date_obs_end, fs.objctra, fs.objctdec, fs.total_exp_time, fs.flat_pattern,
                COUNT(DISTINCT sm.frame_id) as member_count, fs.avg_rotation, fs.min_rotation, fs.max_rotation, fs.is_archived,
                fs.archived_at, fs.archive_operation_id
         FROM frames_set fs
         LEFT JOIN imaging_nights in_tbl ON fs.id = in_tbl.frames_set_id
         LEFT JOIN sessions s ON in_tbl.id = s.imaging_night_id
         LEFT JOIN session_members sm ON s.id = sm.session_id
         GROUP BY fs.id
         ORDER BY fs.date_obs_start DESC, fs.name ASC"
    )?;

    let sets = stmt.query_map(params![], |row| {
        let set = crate::models::FramesSet {
            id: Some(row.get(0)?),
            name: row.get(1)?,
            is_custom: row.get::<_, i32>(2)? == 1,
            date_obs_start: row.get(3)?,
            date_obs_end: row.get(4)?,
            objctra: row.get(5)?,
            objctdec: row.get(6)?,
            total_exp_time: row.get(7)?,
            flat_pattern: row.get(8)?,
            avg_rotation: row.get(10)?,
            min_rotation: row.get(11)?,
            max_rotation: row.get(12)?,
            is_archived: row.get::<_, i32>(13).unwrap_or(0) == 1,
            archived_at: row.get(14)?,
            archive_operation_id: row.get(15)?,
        };
        let member_count: i32 = row.get(9)?;
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

/// Delete all auto-generated frames_sets (where is_custom = 0)
pub fn delete_auto_generated_frame_sets(conn: &Connection) -> Result<usize> {
    let count = conn.execute("DELETE FROM frames_set WHERE is_custom = 0", params![])?;
    Ok(count)
}

/// Update frames_set name
pub fn update_frames_set_name(conn: &Connection, id: i64, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE frames_set SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

/// Update frames_set metadata
pub fn update_frames_set_metadata(
    conn: &Connection,
    id: i64,
    date_obs_start: Option<&str>,
    date_obs_end: Option<&str>,
    objctra: Option<&str>,
    objctdec: Option<&str>,
    total_exp_time: Option<f64>,
    is_custom: bool,
    avg_rotation: Option<f64>,
    min_rotation: Option<f64>,
    max_rotation: Option<f64>,
) -> Result<()> {
    let is_custom_int = if is_custom { 1 } else { 0 };
    conn.execute(
        "UPDATE frames_set
         SET date_obs_start = ?1, date_obs_end = ?2, objctra = ?3, objctdec = ?4,
             total_exp_time = ?5, is_custom = ?6, avg_rotation = ?7, min_rotation = ?8, max_rotation = ?9
         WHERE id = ?10",
        params![date_obs_start, date_obs_end, objctra, objctdec, total_exp_time, is_custom_int, avg_rotation, min_rotation, max_rotation, id],
    )?;
    Ok(())
}

pub fn update_frames_set_flat_pattern(
    conn: &Connection,
    id: i64,
    flat_pattern: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE frames_set SET flat_pattern = ?1 WHERE id = ?2",
        params![flat_pattern, id],
    )?;
    Ok(())
}

/// Set the archived status of a frames_set
pub fn set_frame_set_archived(conn: &Connection, id: i64, archived: bool) -> Result<()> {
    let archived_int = if archived { 1 } else { 0 };
    conn.execute(
        "UPDATE frames_set SET is_archived = ?1 WHERE id = ?2",
        params![archived_int, id],
    )?;
    Ok(())
}

/// Get all LIGHT frames for a project (for clustering)
pub fn get_light_frames_for_project(
    conn: &Connection,
    _project_id: i64,
) -> Result<Vec<(i64, crate::models::Frame)>> {
    // For now, we'll get all LIGHT frames regardless of project
    // In the future, we can add project filtering at the frame level
    let mut stmt = conn.prepare(
        "SELECT f.id, fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override
         FROM files f
         INNER JOIN frames fr ON f.id = fr.file_id
         WHERE fr.imagetyp = 'Light' OR fr.imagetyp IS NULL
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
            ypixsz: row.get(20)?,
            naxis1: row.get(21)?,
            naxis2: row.get(22)?,
            ra: row.get(23)?,
            dec: row.get(24)?,
            sitelat: row.get(25)?,
            lat_obs: row.get(26)?,
            sitelong: row.get(27)?,
            long_obs: row.get(28)?,
            objctra: row.get(29)?,
            objctdec: row.get(30)?,
            override_: row.get::<_, i32>(31)? == 1,
            swcreate: None,
            bayerpat: None,
            rotation: None,
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

/// Insert session members (bulk insert).
///
/// Tolerates being called either standalone (caller passes a fresh `&conn`)
/// or from inside an outer transaction (caller passes `&tx`, which derefs to
/// `&Connection`). Standalone callers get a local batch transaction for
/// fsync amortization; in-transaction callers join the existing tx so we
/// don't trigger SQLite's "cannot start a transaction within a transaction".
pub fn insert_session_members(
    conn: &Connection,
    session_id: i64,
    frame_ids: &[i64],
) -> Result<()> {
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        for frame_id in frame_ids {
            tx.execute(
                "INSERT OR IGNORE INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                params![session_id, frame_id],
            )?;
        }
        tx.commit()?;
    } else {
        for frame_id in frame_ids {
            conn.execute(
                "INSERT OR IGNORE INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                params![session_id, frame_id],
            )?;
        }
    }
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

/// Reassign an imaging night to a different frame set
pub fn reassign_imaging_night_to_frame_set(
    conn: &Connection,
    night_id: i64,
    new_frames_set_id: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE imaging_nights SET frames_set_id = ?1 WHERE id = ?2",
        params![new_frames_set_id, night_id],
    )?;
    Ok(())
}

/// Move sessions from one night to another.
///
/// Tolerates outer-transaction callers (see `insert_session_members` above
/// for the same pattern and rationale).
pub fn move_sessions_to_night(
    conn: &Connection,
    session_ids: &[i64],
    target_night_id: i64,
) -> Result<()> {
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        for session_id in session_ids {
            tx.execute(
                "UPDATE sessions SET imaging_night_id = ?1 WHERE id = ?2",
                params![target_night_id, session_id],
            )?;
        }
        tx.commit()?;
    } else {
        for session_id in session_ids {
            conn.execute(
                "UPDATE sessions SET imaging_night_id = ?1 WHERE id = ?2",
                params![target_night_id, session_id],
            )?;
        }
    }
    Ok(())
}

/// Update imaging night time range
pub fn update_imaging_night_time_range(
    conn: &Connection,
    night_id: i64,
    start_time: &str,
    end_time: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE imaging_nights SET start_time = ?1, end_time = ?2 WHERE id = ?3",
        params![start_time, end_time, night_id],
    )?;
    Ok(())
}

/// Deduplicate session members within a frame set
/// Three phases:
/// 1. Per-session: remove identical frame_id duplicates within a single session
/// 2. Cross-session: remove different frame_ids that reference the same physical file (same files.path)
/// 3. Session consolidation: merge sessions with the same instrume within the same imaging night
pub fn deduplicate_session_members_in_set(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<usize> {
    let mut total_removed = 0;

    // --- Phase 1: Per-session frame_id dedup (original logic) ---
    let session_ids: Vec<i64> = conn
        .prepare(
            "SELECT s.id
             FROM sessions s
             JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
             WHERE in_tbl.frames_set_id = ?1"
        )?
        .query_map(params![frames_set_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for session_id in &session_ids {
        let duplicates: Vec<(i64, i32)> = conn
            .prepare(
                "SELECT frame_id, COUNT(*) as count
                 FROM session_members
                 WHERE session_id = ?1
                 GROUP BY frame_id
                 HAVING count > 1"
            )?
            .query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        for (frame_id, count) in duplicates {
            conn.execute(
                "DELETE FROM session_members WHERE session_id = ?1 AND frame_id = ?2",
                params![session_id, frame_id],
            )?;
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                params![session_id, frame_id],
            )?;
            total_removed += (count - 1) as usize;
        }
    }

    // --- Phase 2: Cross-session dedup by file path ---
    // Different frame_ids can reference the same physical file (same files.path)
    // after a scan root is removed and re-added. Keep the lowest frame_id, remove others.
    let path_members: Vec<(i64, i64, String)> = conn
        .prepare(
            "SELECT sm.frame_id, sm.session_id, f.path
             FROM session_members sm
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             JOIN frames fr ON sm.frame_id = fr.id
             JOIN files f ON fr.file_id = f.id
             WHERE n.frames_set_id = ?1
             ORDER BY f.path, sm.frame_id"
        )?
        .query_map(params![frames_set_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut seen_paths: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for (frame_id, session_id, path) in &path_members {
        if let Some(&kept_id) = seen_paths.get(path) {
            if *frame_id != kept_id {
                conn.execute(
                    "DELETE FROM session_members WHERE session_id = ?1 AND frame_id = ?2",
                    params![session_id, frame_id],
                )?;
                total_removed += 1;
            }
        } else {
            seen_paths.insert(path.clone(), *frame_id);
        }
    }

    // --- Phase 3: Session consolidation ---
    // Merge sessions with the same instrume within the same imaging night.
    let night_ids: Vec<i64> = conn
        .prepare(
            "SELECT id FROM imaging_nights WHERE frames_set_id = ?1"
        )?
        .query_map(params![frames_set_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for night_id in &night_ids {
        // Get sessions ordered by instrume then by member count descending (keep largest)
        let sessions: Vec<(i64, String, i64)> = conn
            .prepare(
                "SELECT s.id, COALESCE(s.instrume, ''), COUNT(sm.frame_id) as cnt
                 FROM sessions s
                 LEFT JOIN session_members sm ON s.id = sm.session_id
                 WHERE s.imaging_night_id = ?1
                 GROUP BY s.id, s.instrume
                 ORDER BY COALESCE(s.instrume, ''), cnt DESC"
            )?
            .query_map(params![night_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut instrume_primary: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for (session_id, instrume, _count) in &sessions {
            if let Some(&primary_id) = instrume_primary.get(instrume) {
                // Move members from duplicate session to primary (ignore conflicts from PK)
                conn.execute(
                    "UPDATE OR IGNORE session_members SET session_id = ?1 WHERE session_id = ?2",
                    params![primary_id, session_id],
                )?;
                // Delete any that couldn't be moved (already exist in primary)
                conn.execute(
                    "DELETE FROM session_members WHERE session_id = ?1",
                    params![session_id],
                )?;
                // Delete the now-empty session
                conn.execute(
                    "DELETE FROM sessions WHERE id = ?1",
                    params![session_id],
                )?;
            } else {
                instrume_primary.insert(instrume.clone(), *session_id);
            }
        }
    }

    // --- Update frame_count and total_exp_time on surviving sessions ---
    let surviving_sessions: Vec<i64> = conn
        .prepare(
            "SELECT s.id
             FROM sessions s
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             WHERE n.frames_set_id = ?1"
        )?
        .query_map(params![frames_set_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for session_id in &surviving_sessions {
        conn.execute(
            "UPDATE sessions SET
                frame_count = (SELECT COUNT(*) FROM session_members WHERE session_id = ?1),
                total_exp_time = COALESCE(
                    (SELECT SUM(fr.exptime) FROM session_members sm
                     JOIN frames fr ON sm.frame_id = fr.id
                     WHERE sm.session_id = ?1), 0.0)
             WHERE id = ?1",
            params![session_id],
        )?;
    }

    Ok(total_removed)
}

/// Get all imaging nights for a frame set (simplified, just nights without sessions)
pub fn get_imaging_nights_for_set(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<crate::models::ImagingNight>> {
    let mut stmt = conn.prepare(
        "SELECT id, frames_set_id, start_time, end_time, created_at
         FROM imaging_nights
         WHERE frames_set_id = ?1
         ORDER BY start_time ASC"
    )?;

    let nights = stmt.query_map(params![frames_set_id], |row| {
        let created_at_str: Option<String> = row.get(4)?;
        let created_at = created_at_str
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        Ok(crate::models::ImagingNight {
            id: Some(row.get(0)?),
            frames_set_id: row.get(1)?,
            start_time: row.get(2)?,
            end_time: row.get(3)?,
            created_at,
        })
    })?;

    nights.collect()
}

/// Get all sessions for an imaging night
pub fn get_sessions_for_night(
    conn: &Connection,
    night_id: i64,
) -> Result<Vec<crate::models::Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, imaging_night_id, instrume, frame_count, total_exp_time, created_at
         FROM sessions
         WHERE imaging_night_id = ?1
         ORDER BY created_at ASC"
    )?;

    let sessions = stmt.query_map(params![night_id], |row| {
        let created_at_str: Option<String> = row.get(5)?;
        let created_at = created_at_str
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        Ok(crate::models::Session {
            id: Some(row.get(0)?),
            imaging_night_id: row.get(1)?,
            instrume: row.get(2)?,
            frame_count: row.get(3)?,
            total_exp_time: row.get(4)?,
            created_at,
        })
    })?;

    sessions.collect()
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
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override
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
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
        };

        let date_obs_raw: Option<String> = row.get(15)?;

        let frame = crate::models::Frame {
            id: row.get(12)?,
            file_id: row.get(13)?,
            object: row.get(14)?,
            date_obs: date_obs_raw.and_then(|s| {
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
                "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                        f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                        fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                        fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                        fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                        fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                        fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override
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

// ============================================================================
// Excluded Frames Operations
// ============================================================================

/// Clear all excluded frames
pub fn clear_excluded_frames(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM excluded_frames", [])?;
    Ok(())
}

/// Insert excluded frames in batch
pub fn insert_excluded_frames(conn: &Connection, entries: &[(i64, String)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO excluded_frames (file_id, reason) VALUES (?1, ?2)",
    )?;
    for (file_id, reason) in entries {
        stmt.execute(params![file_id, reason])?;
    }
    tx.commit()?;
    Ok(())
}

/// Get count of excluded frames
pub fn get_excluded_frames_count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM excluded_frames", [], |row| row.get(0))
}

/// Reclassify excluded frames: update imagetyp, remove from excluded_frames,
/// return distinct instrume values for calibration refresh.
pub fn reclassify_excluded_frames(
    conn: &Connection,
    file_ids: &[i64],
    new_imagetyp: &str,
) -> Result<(usize, Vec<String>)> {
    if file_ids.is_empty() {
        return Ok((0, Vec::new()));
    }

    let placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let values: Vec<rusqlite::types::Value> = file_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    // Update imagetyp for frames matching these file_ids
    let sql = format!(
        "UPDATE frames SET imagetyp = ?1 WHERE file_id IN ({})",
        placeholders
    );
    let mut update_params: Vec<rusqlite::types::Value> = vec![rusqlite::types::Value::Text(new_imagetyp.to_string())];
    update_params.extend(values.iter().cloned());
    let frames_updated = conn.execute(&sql, rusqlite::params_from_iter(update_params.iter()))?;

    // Remove from excluded_frames
    let sql = format!(
        "DELETE FROM excluded_frames WHERE file_id IN ({})",
        placeholders
    );
    conn.execute(&sql, rusqlite::params_from_iter(values.iter()))?;

    // Get distinct instrume values for affected frames
    let sql = format!(
        "SELECT DISTINCT instrume FROM frames WHERE file_id IN ({}) AND instrume IS NOT NULL",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let cameras: Vec<String> = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    Ok((frames_updated, cameras))
}

/// Get frame IDs (frames.id) for a list of file IDs (files.id)
pub fn get_frame_ids_for_file_ids(conn: &Connection, file_ids: &[i64]) -> Result<Vec<i64>> {
    if file_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let values: Vec<rusqlite::types::Value> = file_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();
    let sql = format!("SELECT id FROM frames WHERE file_id IN ({})", placeholders);
    let mut stmt = conn.prepare(&sql)?;
    let ids: Vec<i64> = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(ids)
}

/// Delete excluded frames by file IDs
pub fn delete_excluded_frames_by_file_ids(conn: &Connection, file_ids: &[i64]) -> Result<usize> {
    if file_ids.is_empty() {
        return Ok(0);
    }
    let placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let values: Vec<rusqlite::types::Value> = file_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();
    let sql = format!(
        "DELETE FROM excluded_frames WHERE file_id IN ({})",
        placeholders
    );
    let deleted = conn.execute(&sql, rusqlite::params_from_iter(values.iter()))?;
    Ok(deleted)
}

/// Per-frame-set entry in a memberships summary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetMembership {
    pub frame_set_id: i64,
    pub name: Option<String>,
    /// How many of the queried `frame_ids` are members of this frame set.
    pub selected_count: i64,
}

/// Per-calibration-set entry in a memberships summary, used for both
/// "member of" (the frame is a Dark/Flat/Bias/DarkFlat in this set) and
/// "consumer of" (the frame is a LIGHT/etc that *uses* this set's
/// calibrations) listings. `calibration_type` is set only for the
/// consumer case.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSetMembership {
    pub calibration_set_id: i64,
    pub imagetyp: String,
    pub calibration_type: Option<String>,
    pub instrume: Option<String>,
    pub gain: Option<f64>,
    pub binning: Option<String>,
    pub exptime: Option<f64>,
    pub filter: Option<String>,
    pub ccd_temp: Option<f64>,
    pub date: String,
    pub selected_count: i64,
}

/// "Used in frame set" — for the calibration sets the queried frames are
/// MEMBERS of, this resolves the chain calibration_set_to_frames →
/// session_members → sessions → imaging_nights → frames_set so the user
/// sees which Objects ultimately get calibrated by these frames.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsedInFrameSetMembership {
    pub frame_set_id: i64,
    pub name: Option<String>,
    /// Calibration role the selection's calibration set fills for this
    /// frame_set (Dark / Flat / Bias / DarkFlat).
    pub calibration_type: String,
    /// Distinct consumer frames (lights/etc.) in `frame_set_id` that
    /// reference one of the queried frames' calibration sets.
    pub used_for_frame_count: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMembershipsSummary {
    pub frame_sets: Vec<FrameSetMembership>,
    pub calibration_set_member: Vec<CalibrationSetMembership>,
    pub calibration_set_consumer: Vec<CalibrationSetMembership>,
    pub used_in_frame_sets: Vec<UsedInFrameSetMembership>,
    pub frame_count_total: i64,
    pub frame_count_unaffiliated: i64,
}

/// Aggregate the framesets a list of frames is a part of: the top-level
/// frame_set ("Object") via session_members, the calibration sets the
/// frames are members OF, and the calibration sets the frames CONSUME.
/// Counts are deduped per (frame_id, set_id) so a frame that appears in
/// multiple sessions of the same frame_set still counts once.
pub fn get_frame_memberships_summary(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<FrameMembershipsSummary> {
    let mut summary = FrameMembershipsSummary {
        frame_count_total: frame_ids.len() as i64,
        ..Default::default()
    };
    if frame_ids.is_empty() {
        return Ok(summary);
    }

    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    // Frame sets (Objects) — distinct frame_id within each frame_set.
    let sql = format!(
        "SELECT fs.id, fs.name, COUNT(DISTINCT sm.frame_id) AS selected_count
         FROM frames_set fs
         JOIN imaging_nights inight ON inight.frames_set_id = fs.id
         JOIN sessions s ON s.imaging_night_id = inight.id
         JOIN session_members sm ON sm.session_id = s.id
         WHERE sm.frame_id IN ({})
         GROUP BY fs.id, fs.name
         ORDER BY selected_count DESC, fs.name",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(FrameSetMembership {
            frame_set_id: row.get(0)?,
            name: row.get(1)?,
            selected_count: row.get(2)?,
        })
    })?;
    for r in rows {
        summary.frame_sets.push(r?);
    }

    // Calibration set MEMBER (frame is a calibration frame in this set).
    let sql_member = format!(
        "SELECT cs.id, cs.imagetyp, cs.instrume, cs.gain, cs.binning, cs.exptime,
                cs.filter, cs.ccd_temp, cs.date,
                COUNT(DISTINCT csf.frame_id) AS selected_count
         FROM calibration_set cs
         JOIN calibration_set_frames csf ON csf.set_id = cs.id
         WHERE csf.frame_id IN ({})
         GROUP BY cs.id
         ORDER BY selected_count DESC, cs.date DESC",
        placeholders
    );
    let mut stmt = conn.prepare(&sql_member)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(CalibrationSetMembership {
            calibration_set_id: row.get(0)?,
            imagetyp: row.get(1)?,
            calibration_type: None,
            instrume: row.get(2)?,
            gain: row.get(3)?,
            binning: row.get(4)?,
            exptime: row.get(5)?,
            filter: row.get(6)?,
            ccd_temp: row.get(7)?,
            date: row.get(8)?,
            selected_count: row.get(9)?,
        })
    })?;
    for r in rows {
        summary.calibration_set_member.push(r?);
    }

    // Calibration set CONSUMER (frame uses this set as Dark/Flat/Bias/DarkFlat).
    let sql_consumer = format!(
        "SELECT cs.id, cs.imagetyp, cstf.calibration_type, cs.instrume, cs.gain,
                cs.binning, cs.exptime, cs.filter, cs.ccd_temp, cs.date,
                COUNT(DISTINCT cstf.source_id) AS selected_count
         FROM calibration_set_to_frames cstf
         JOIN calibration_set cs ON cs.id = cstf.calibration_set_id
         WHERE cstf.source_type = 'frame' AND cstf.source_id IN ({})
         GROUP BY cs.id, cstf.calibration_type
         ORDER BY cstf.calibration_type, selected_count DESC",
        placeholders
    );
    let mut stmt = conn.prepare(&sql_consumer)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(CalibrationSetMembership {
            calibration_set_id: row.get(0)?,
            imagetyp: row.get(1)?,
            calibration_type: row.get(2)?,
            instrume: row.get(3)?,
            gain: row.get(4)?,
            binning: row.get(5)?,
            exptime: row.get(6)?,
            filter: row.get(7)?,
            ccd_temp: row.get(8)?,
            date: row.get(9)?,
            selected_count: row.get(10)?,
        })
    })?;
    for r in rows {
        summary.calibration_set_consumer.push(r?);
    }

    // "Used in frame set" — for each calibration set the queried frames are
    // members of, find the LIGHTs that consume that cal set and roll them up
    // to their frame_set ("Object"). Group by (frame_set, calibration_type)
    // so a Dark whose set is shared between two Objects shows two rows.
    let sql_used_in = format!(
        "SELECT fs.id, fs.name, cstf.calibration_type,
                COUNT(DISTINCT cstf.source_id) AS used_count
         FROM calibration_set_frames csf
         JOIN calibration_set_to_frames cstf
              ON cstf.calibration_set_id = csf.set_id AND cstf.source_type = 'frame'
         JOIN session_members sm   ON sm.frame_id = cstf.source_id
         JOIN sessions s           ON s.id = sm.session_id
         JOIN imaging_nights inight ON inight.id = s.imaging_night_id
         JOIN frames_set fs        ON fs.id = inight.frames_set_id
         WHERE csf.frame_id IN ({})
         GROUP BY fs.id, fs.name, cstf.calibration_type
         ORDER BY used_count DESC, fs.name",
        placeholders
    );
    let mut stmt = conn.prepare(&sql_used_in)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(UsedInFrameSetMembership {
            frame_set_id: row.get(0)?,
            name: row.get(1)?,
            calibration_type: row.get(2)?,
            used_for_frame_count: row.get(3)?,
        })
    })?;
    for r in rows {
        summary.used_in_frame_sets.push(r?);
    }

    // Frames that are not in ANY frame set (orphan lights, calibration frames, etc).
    let sql_unaffiliated = format!(
        "SELECT COUNT(*) FROM frames f
         WHERE f.id IN ({}) AND NOT EXISTS (
             SELECT 1 FROM session_members sm WHERE sm.frame_id = f.id
         )",
        placeholders
    );
    summary.frame_count_unaffiliated = conn.query_row(
        &sql_unaffiliated,
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;

    Ok(summary)
}

/// Re-evaluate the `override` flag for each given frame: if every editable
/// field on the frame now equals what the original FITS header said, drop
/// the flag back to 0. Otherwise leave it alone (`bulk_update_frame_metadata`
/// already sets it to 1 when it writes anything, so this is the inverse).
///
/// Used to make per-field revert "stick" — once a user has reverted every
/// field that was ever changed, the frame visually returns to its
/// "untouched" appearance (no pencil indicator in the file browser).
///
/// Only frames currently flagged `override = 1` are inspected; clean rows
/// are skipped so the cost is proportional to actual edits.
pub fn recompute_override_flag_for_frames(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<usize> {
    use crate::fits_parser::stored_header::{parse_stored_header_keys, snapshot_from_keys};
    use crate::models::FileFormat;

    if frame_ids.is_empty() {
        return Ok(0);
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    let sql = format!(
        "SELECT fr.id, fr.object, fr.filter, fr.imagetyp, fr.instrume, fr.telescop,
                fr.focallen, fr.gain, fr.offset, fr.binning, fr.exptime, fr.ccd_temp,
                fr.date_obs, files.format, fh.header
         FROM frames fr
         JOIN files ON files.id = fr.file_id
         LEFT JOIN fits_header fh ON fh.file_id = fr.file_id
         WHERE fr.id IN ({}) AND fr.override = 1",
        placeholders
    );

    type FrameRow = (
        i64,
        Option<String>, // object
        Option<String>, // filter
        Option<String>, // imagetyp
        Option<String>, // instrume
        Option<String>, // telescop
        Option<f64>,    // focallen
        Option<f64>,    // gain
        Option<f64>,    // offset
        Option<String>, // binning
        Option<f64>,    // exptime
        Option<f64>,    // ccd_temp
        Option<String>, // date_obs
        String,         // files.format
        Option<String>, // header
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<FrameRow> = stmt
        .query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut cleared = 0usize;
    for r in rows {
        let (
            frame_id,
            object,
            filter,
            imagetyp,
            instrume,
            telescop,
            focallen,
            gain,
            offset,
            binning,
            exptime,
            ccd_temp,
            date_obs,
            format_str,
            header,
        ) = r;

        let format = match format_str.as_str() {
            "FITS" => FileFormat::FITS,
            "XISF" => FileFormat::XISF,
            _ => continue,
        };
        let header_text = match header {
            Some(h) if !h.is_empty() => h,
            // No stored header → can't safely conclude "matches original",
            // so leave override = 1.
            _ => continue,
        };
        let keys = parse_stored_header_keys(format, &header_text);
        let snap = snapshot_from_keys(frame_id, &keys);

        let all_match = opt_str_eq(&object, &snap.object)
            && opt_str_eq(&filter, &snap.filter)
            && opt_str_eq(&imagetyp, &snap.imagetyp)
            && opt_str_eq(&instrume, &snap.instrume)
            && opt_str_eq(&telescop, &snap.telescop)
            && opt_f64_eq(&focallen, &snap.focallen)
            && opt_f64_eq(&gain, &snap.gain)
            && opt_f64_eq(&offset, &snap.offset)
            && opt_str_eq(&binning, &snap.binning)
            && opt_f64_eq(&exptime, &snap.exptime)
            && opt_f64_eq(&ccd_temp, &snap.ccd_temp)
            && opt_date_eq(&date_obs, &snap.date_obs);

        if all_match {
            conn.execute(
                "UPDATE frames SET override = 0 WHERE id = ?1",
                [frame_id],
            )?;
            cleared += 1;
        }
    }
    Ok(cleared)
}

fn opt_str_eq(a: &Option<String>, b: &Option<String>) -> bool {
    let an = a.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let bn = b.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
    match (an, bn) {
        (None, None) => true,
        (Some(av), Some(bv)) => av == bv,
        _ => false,
    }
}

fn opt_f64_eq(a: &Option<f64>, b: &Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(av), Some(bv)) => {
            (!av.is_finite() && !bv.is_finite()) || (av - bv).abs() <= 1e-6
        }
        _ => false,
    }
}

/// Compare two ISO-ish datetime strings as instants. Handles the "FITS
/// header has no timezone marker, catalog stores RFC3339 with Z" mismatch
/// by treating naive timestamps as UTC (FITS DATE-OBS convention).
fn opt_date_eq(a: &Option<String>, b: &Option<String>) -> bool {
    use chrono::{DateTime, NaiveDateTime, Utc};
    fn parse_to_utc(s: &str) -> Option<DateTime<Utc>> {
        let s = s.trim();
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.with_timezone(&Utc));
        }
        for fmt in &["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
            if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
                return Some(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
            }
        }
        None
    }
    match (a, b) {
        (None, None) => true,
        (Some(av), Some(bv)) => match (parse_to_utc(av), parse_to_utc(bv)) {
            (Some(da), Some(db)) => da == db,
            _ => av.trim() == bv.trim(),
        },
        _ => false,
    }
}

/// Pull the original (last-scanned) header values for the given frames out
/// of `fits_header` and decode them into canonical fields. Used by the
/// metadata pane to show "what the file said before you edited it" and
/// to support per-field revert.
pub fn get_frame_metadata_originals(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<Vec<crate::fits_parser::stored_header::FrameOriginalSnapshot>> {
    use crate::fits_parser::stored_header::{
        parse_stored_header_keys, snapshot_from_keys, FrameOriginalSnapshot,
    };
    use crate::models::FileFormat;

    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    let sql = format!(
        "SELECT fr.id, files.format, fh.header
         FROM frames fr
         JOIN files ON files.id = fr.file_id
         LEFT JOIN fits_header fh ON fh.file_id = fr.file_id
         WHERE fr.id IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        let frame_id: i64 = row.get(0)?;
        let format_str: String = row.get(1)?;
        let header: Option<String> = row.get(2)?;
        Ok((frame_id, format_str, header))
    })?;

    let mut out: Vec<FrameOriginalSnapshot> = Vec::new();
    for r in rows {
        let (frame_id, format_str, header) = r?;
        let format = match format_str.as_str() {
            "FITS" => FileFormat::FITS,
            "XISF" => FileFormat::XISF,
            _ => continue, // Unknown format — skip rather than guess.
        };
        match header {
            Some(text) if !text.is_empty() => {
                let keys = parse_stored_header_keys(format, &text);
                out.push(snapshot_from_keys(frame_id, &keys));
            }
            // No stored header (rare — may pre-date the fits_header table or
            // a DB without re-scan after the migration). Emit an empty snapshot
            // so the UI can still show "no original recorded".
            _ => out.push(FrameOriginalSnapshot {
                frame_id,
                ..Default::default()
            }),
        }
    }
    Ok(out)
}

/// Counts of the relation rows that would be removed if `bulk_update_frame_metadata`
/// were called for the given frame_ids. Drives the pre-save unlink warning.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMetadataRelations {
    /// Frames that are *members* of a calibration set.
    pub calibration_set_member_count: i64,
    /// Frames that *consume* a calibration set (linked as a calibration consumer).
    pub calibration_set_consumer_count: i64,
    /// Frames that are in a session (and therefore in an imaging night / frame set).
    pub session_member_count: i64,
    /// Total distinct frames affected (deduped across the three buckets).
    pub affected_frame_count: i64,
}

/// Count how many of `frame_ids` have rows in `calibration_set_frames`,
/// `calibration_set_to_frames`, and `session_members`. Used by the dual-pane
/// metadata editor to warn the user before applying edits that will unlink
/// the affected frames.
pub fn count_frame_metadata_relations(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<FrameMetadataRelations> {
    if frame_ids.is_empty() {
        return Ok(FrameMetadataRelations::default());
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    let csf_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT frame_id) FROM calibration_set_frames WHERE frame_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;
    let cstf_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT source_id) FROM calibration_set_to_frames
             WHERE source_type = 'frame' AND source_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;
    let sm_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT frame_id) FROM session_members WHERE frame_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;

    // Deduped affected-frame count across all three junctions.
    let affected: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM (
                SELECT frame_id AS fid FROM calibration_set_frames WHERE frame_id IN ({p})
                UNION
                SELECT source_id AS fid FROM calibration_set_to_frames
                  WHERE source_type = 'frame' AND source_id IN ({p})
                UNION
                SELECT frame_id AS fid FROM session_members WHERE frame_id IN ({p})
            )",
            p = placeholders
        ),
        rusqlite::params_from_iter(
            id_params.iter().chain(id_params.iter()).chain(id_params.iter()),
        ),
        |r| r.get(0),
    )?;

    Ok(FrameMetadataRelations {
        calibration_set_member_count: csf_count,
        calibration_set_consumer_count: cstf_count,
        session_member_count: sm_count,
        affected_frame_count: affected,
    })
}

/// Rewire `files.path` for every catalog row whose path begins with
/// `old_prefix`, replacing only the leading prefix with `new_prefix`.
/// Returns the number of rows updated.
///
/// The naive approach `REPLACE(path, old, new)` is wrong: it substitutes
/// EVERY occurrence of the prefix in the path. If a path happens to repeat
/// the old segment elsewhere (e.g. user has `/data/2025/data/foo.fits` and
/// renames `/data/2025`), REPLACE mangles the inner segments. SUBSTR-based
/// prefix swap only touches the leading prefix and is safe regardless of
/// repetition.
///
/// Both `old_prefix` and `new_prefix` MUST end with a path separator so
/// `WHERE path LIKE old_prefix || '%'` only matches descendants and not
/// sibling folders sharing the same name root.
pub fn rename_files_path_prefix(
    conn: &Connection,
    old_prefix: &str,
    new_prefix: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE files
         SET path = ?1 || SUBSTR(path, LENGTH(?2) + 1)
         WHERE path LIKE ?2 || '%'",
        rusqlite::params![new_prefix, old_prefix],
    )?;
    Ok(n)
}

/// Get all excluded frames joined with file paths
pub fn get_excluded_frames(conn: &Connection) -> Result<Vec<crate::models::ExcludedFrameEntry>> {
    let mut stmt = conn.prepare(
        "SELECT ef.file_id, f.path, f.filename, ef.reason, ef.excluded_at
         FROM excluded_frames ef
         JOIN files f ON ef.file_id = f.id
         ORDER BY f.path ASC"
    )?;

    let entries = stmt.query_map([], |row| {
        Ok(crate::models::ExcludedFrameEntry {
            file_id: row.get(0)?,
            path: row.get(1)?,
            filename: row.get(2)?,
            reason: row.get(3)?,
            excluded_at: row.get(4)?,
        })
    })?;

    entries.collect()
}

#[cfg(test)]
mod bulk_metadata_tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::models::FrameMetadataEdits;
    use rusqlite::Connection;

    /// Insert a `files` + `frames` row pair and return the frame_id.
    fn insert_frame(conn: &Connection, path: &str, imagetyp: Option<&str>) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, 'a.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [path],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
            rusqlite::params![file_id, imagetyp],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM frames WHERE file_id = ?1",
            [file_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Build a frames_set + imaging_night + session chain so a session row
    /// can satisfy its NOT NULL imaging_night_id FK. Returns the session_id.
    fn make_session(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (name, date_obs_start, date_obs_end)
             VALUES ('test', '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z')",
            [],
        ).unwrap();
        let frames_set_id: i64 = conn
            .query_row("SELECT id FROM frames_set ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z')",
            [frames_set_id],
        ).unwrap();
        let imaging_night_id: i64 = conn
            .query_row("SELECT id FROM imaging_nights ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'CamA')",
            [imaging_night_id],
        ).unwrap();
        conn.query_row("SELECT id FROM sessions ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn imagetyp_change_unlinks_calibration_set_membership_and_sessions() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dark_id = insert_frame(&conn, "/x/dark1.fits", Some("Dark"));

        // Create a calibration set and put the frame in it as a member.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')",
            [],
        ).unwrap();
        let set_id: i64 = conn
            .query_row("SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, dark_id],
        ).unwrap();

        // Create a session (with parent imaging_night + frames_set) and put
        // the frame in it.
        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, dark_id],
        ).unwrap();

        // Also link as a consumer to a calibration set.
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            rusqlite::params![dark_id, set_id],
        ).unwrap();

        // Sanity: links exist before edit.
        let csf_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id = ?1", [dark_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(csf_count, 1);

        // Reclassify the frame: Dark → Light.
        let edits = FrameMetadataEdits {
            imagetyp: Some("LIGHT".into()),
            ..Default::default()
        };
        let changed = bulk_update_frame_metadata(&conn, &[dark_id], &edits).unwrap();
        assert_eq!(changed, 1);

        // imagetyp updated.
        let new_type: String = conn
            .query_row("SELECT imagetyp FROM frames WHERE id = ?1", [dark_id], |r| r.get(0))
            .unwrap();
        assert_eq!(new_type, "Light");

        // All three junction tables should no longer reference this frame.
        let csf_after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id = ?1", [dark_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(csf_after, 0, "calibration_set_frames should be cleared");

        let cstf_after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_to_frames WHERE source_type='frame' AND source_id = ?1",
            [dark_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(cstf_after, 0, "calibration_set_to_frames should be cleared");

        let sm_after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1", [dark_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(sm_after, 0, "session_members should be cleared");

        // calibration_set with no surviving members IS pruned (master sets
        // and last-frame regular sets — see
        // `changing_imagetyp_prunes_now_empty_master_calibration_set`).
        let set_still_there: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = ?1", [set_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(set_still_there, 0, "now-empty calibration_set should be pruned");
        // Sessions / imaging_nights / frames_set are still left intact —
        // their pruning policy is unchanged.
        let session_still_there: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1", [session_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(session_still_there, 1);
    }

    #[test]
    fn any_edit_unlinks_relations_not_just_imagetyp() {
        // Per design decision: any metadata edit invalidates the frame's
        // relations. Even editing OBJECT alone should unlink.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/light1.fits", Some("Light"));

        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, frame_id],
        ).unwrap();

        let edits = FrameMetadataEdits {
            object: Some("M31".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let sm_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1", [frame_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(sm_count, 0, "OBJECT edit must unlink session membership");
    }

    #[test]
    fn empty_edits_do_not_cascade() {
        // If no fields are set in the edits payload, the function early-returns
        // 0 and must not touch junction tables.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));
        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, frame_id],
        ).unwrap();

        let edits = FrameMetadataEdits::default();
        let n = bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();
        assert_eq!(n, 0);

        let sm_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1", [frame_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(sm_count, 1, "empty-edit calls must not cascade");
    }

    #[test]
    fn binning_edit_updates_x_and_y_components() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));

        let edits = FrameMetadataEdits {
            binning: Some("2x2".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let (b, xb, yb): (String, i64, i64) = conn
            .query_row(
                "SELECT binning, xbinning, ybinning FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(b, "2x2");
        assert_eq!(xb, 2);
        assert_eq!(yb, 2);
    }

    #[test]
    fn binning_invalid_format_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));
        for bad in ["foo", "1", "x1", "0x1", "20x20", ""] {
            let edits = FrameMetadataEdits {
                binning: Some(bad.into()),
                ..Default::default()
            };
            let r = bulk_update_frame_metadata(&conn, &[frame_id], &edits);
            assert!(r.is_err(), "binning '{}' should have been rejected", bad);
        }
    }

    #[test]
    fn exptime_zero_or_negative_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let edits = FrameMetadataEdits {
                exptime: Some(bad),
                ..Default::default()
            };
            let r = bulk_update_frame_metadata(&conn, &[frame_id], &edits);
            assert!(r.is_err(), "exptime {} should have been rejected", bad);
        }
    }

    #[test]
    fn rename_files_path_prefix_only_swaps_leading_segment() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Insert a few rows under /old/A/, including one whose path repeats
        // the old prefix in a non-leading segment — this is the case where
        // SQL REPLACE() would mangle paths.
        let rows = [
            "/old/A/foo.fits",
            "/old/A/sub/bar.fits",
            "/old/A/sub/old/A/baz.fits", // ← repeated segment inside
            "/old/Acme/leave_me_alone.fits", // ← prefix-of-prefix, must not match
            "/other/quux.fits",          // ← unrelated
        ];
        for p in rows {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                 VALUES (?1, 'x.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
                [p],
            ).unwrap();
        }

        let n = rename_files_path_prefix(&conn, "/old/A/", "/new/A/").unwrap();
        assert_eq!(n, 3, "should update exactly the three rows under /old/A/");

        let collect = |sql: &str| -> Vec<String> {
            let mut stmt = conn.prepare(sql).unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().filter_map(|r| r.ok()).collect()
        };
        let paths = collect("SELECT path FROM files ORDER BY path");
        assert_eq!(paths, vec![
            "/new/A/foo.fits".to_string(),
            "/new/A/sub/bar.fits".to_string(),
            // The inner '/old/A/' segment is preserved — only the leading
            // prefix was swapped. (REPLACE would have produced /new/A/sub/new/A/baz.fits.)
            "/new/A/sub/old/A/baz.fits".to_string(),
            "/old/Acme/leave_me_alone.fits".to_string(),
            "/other/quux.fits".to_string(),
        ]);
    }

    /// Stash a believable FITS header text for a frame so the override
    /// recomputation has something to compare against.
    fn insert_fits_header_for_file(conn: &Connection, file_id: i64, header_text: &str) {
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            rusqlite::params![file_id, header_text],
        )
        .unwrap();
    }

    #[test]
    fn override_clears_after_revert_to_original_values() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Insert a file + frame with object="M42" (matches the FITS header
        // we'll attach below).
        let frame_id = insert_frame(&conn, "/x/m42.fits", Some("Light"));
        let file_id: i64 = conn
            .query_row(
                "SELECT file_id FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE frames SET object = 'M42' WHERE id = ?1",
            [frame_id],
        )
        .unwrap();

        // Pretend the FITS header was scanned with OBJECT="M42" and
        // IMAGETYP="Light" — matching the frame's stored fields, so any
        // post-revert mismatch isolates to OBJECT.
        let cards = format!(
            "{}\n{}",
            format!("{:<80}", "OBJECT  = 'M42                 '"),
            format!("{:<80}", "IMAGETYP= 'Light               '"),
        );
        insert_fits_header_for_file(&conn, file_id, &cards);

        // Step 1: user edits OBJECT to "M 42" — bulk_update sets override = 1.
        let edits = FrameMetadataEdits {
            object: Some("M 42".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();
        let override_flag: i64 = conn
            .query_row(
                "SELECT override FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(override_flag, 1, "edit should set override = 1");

        // Step 2: user reverts OBJECT to the FITS-header original ("M42").
        let revert = FrameMetadataEdits {
            object: Some("M42".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &revert).unwrap();
        let override_flag_after: i64 = conn
            .query_row(
                "SELECT override FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            override_flag_after, 0,
            "reverting all changes back to the FITS header should clear override"
        );
    }

    #[test]
    fn override_persists_when_some_field_still_differs_from_original() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let frame_id = insert_frame(&conn, "/x/m42.fits", Some("Light"));
        let file_id: i64 = conn
            .query_row(
                "SELECT file_id FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE frames SET object = 'M42', filter = 'L' WHERE id = ?1",
            [frame_id],
        )
        .unwrap();

        let cards = format!(
            "{}\n{}\n{}",
            format!("{:<80}", "OBJECT  = 'M42                 '"),
            format!("{:<80}", "FILTER  = 'L                   '"),
            format!("{:<80}", "IMAGETYP= 'Light               '"),
        );
        insert_fits_header_for_file(&conn, file_id, &cards);

        // Edit OBJECT and FILTER both. Then revert OBJECT only.
        let edits1 = FrameMetadataEdits {
            object: Some("OTHER".into()),
            filter: Some("R".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits1).unwrap();

        let edits2 = FrameMetadataEdits {
            object: Some("M42".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits2).unwrap();

        let override_flag: i64 = conn
            .query_row(
                "SELECT override FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            override_flag, 1,
            "FILTER still differs from original ('R' vs 'L'), override must stay"
        );
    }

    #[test]
    fn changing_imagetyp_prunes_now_empty_master_calibration_set() {
        // A MasterFlat is a calibration_set with exactly one member frame.
        // When the frame is reclassified to LIGHT, that set is meaningless
        // and must be deleted (along with any consumer references via FK
        // cascade), not left as a zombie.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let masterflat_id = insert_frame(&conn, "/x/master.fits", Some("MasterFlat"));

        // Build the master calibration set + member link + a consumer link.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, instrume, gain, binning, date)
             VALUES ('MasterFlat', 'CamA', 100.0, '1x1', '2026-01-01')",
            [],
        ).unwrap();
        let cal_set_id: i64 = conn
            .query_row("SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![cal_set_id, masterflat_id],
        ).unwrap();

        // A LIGHT in another frame consumes this MasterFlat — the FK cascade
        // on calibration_set_to_frames.calibration_set_id should clean this
        // up automatically when the calibration_set itself is deleted.
        let consumer_light = insert_frame(&conn, "/x/light.fits", Some("Light"));
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Flat')",
            rusqlite::params![consumer_light, cal_set_id],
        ).unwrap();

        // Reclassify the MasterFlat → LIGHT.
        let edits = FrameMetadataEdits {
            imagetyp: Some("LIGHT".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[masterflat_id], &edits).unwrap();

        // Calibration set is gone.
        let set_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM calibration_set WHERE id = ?1", [cal_set_id], |r| r.get(0))
            .unwrap();
        assert_eq!(set_count, 0, "the empty MasterFlat calibration_set should have been pruned");

        // Membership row gone.
        let member_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1", [cal_set_id], |r| r.get(0))
            .unwrap();
        assert_eq!(member_count, 0);

        // Consumer link gone too (via FK cascade).
        let consumer_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_to_frames WHERE calibration_set_id = ?1",
                [cal_set_id], |r| r.get(0),
            )
            .unwrap();
        assert_eq!(consumer_count, 0, "consumer references should cascade-delete with the set");
    }

    #[test]
    fn changing_one_frame_in_multi_member_set_keeps_set_alive() {
        // Non-master calibration set with two darks. Reclassifying ONE of
        // them should remove the membership row but leave the set (and its
        // other member) intact.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dark_a = insert_frame(&conn, "/x/dark_a.fits", Some("Dark"));
        let dark_b = insert_frame(&conn, "/x/dark_b.fits", Some("Dark"));

        conn.execute(
            "INSERT INTO calibration_set (imagetyp, instrume, gain, binning, exptime, ccd_temp, date)
             VALUES ('Dark', 'CamA', 100.0, '1x1', 60.0, -10.0, '2026-01-01')",
            [],
        ).unwrap();
        let cal_set_id: i64 = conn
            .query_row("SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        for fid in [dark_a, dark_b] {
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![cal_set_id, fid],
            ).unwrap();
        }

        // Reclassify dark_a → LIGHT.
        let edits = FrameMetadataEdits {
            imagetyp: Some("LIGHT".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[dark_a], &edits).unwrap();

        // Set still exists.
        let set_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM calibration_set WHERE id = ?1", [cal_set_id], |r| r.get(0))
            .unwrap();
        assert_eq!(set_count, 1, "set with surviving members must NOT be pruned");

        // dark_b membership intact.
        let surviving_member: i64 = conn
            .query_row(
                "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1",
                [cal_set_id], |r| r.get(0),
            )
            .unwrap();
        assert_eq!(surviving_member, dark_b);
    }

    #[test]
    fn imagetyp_master_variant_sets_is_master_flag() {
        // Picking "MasterDark" (or any of its case-insensitive aliases) in
        // the dropdown must store imagetyp='MasterDark' AND is_master=1
        // without the caller having to set is_master separately.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/d.fits", Some("Dark"));

        for input in ["MASTERDARK", "Master Dark", "MasterDark"] {
            // Clear flag between attempts so each iteration has a meaningful
            // before/after.
            conn.execute("UPDATE frames SET is_master = 0 WHERE id = ?1", [frame_id])
                .unwrap();
            let edits = FrameMetadataEdits {
                imagetyp: Some(input.into()),
                ..Default::default()
            };
            bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

            let (typ, is_master): (String, i64) = conn
                .query_row(
                    "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
                    [frame_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(typ, "MasterDark", "input '{}' should normalize to MasterDark", input);
            assert_eq!(is_master, 1, "input '{}' should set is_master=1", input);
        }
    }

    #[test]
    fn imagetyp_base_variant_clears_is_master_flag() {
        // Picking "Dark" must store is_master=0 even if the row was a master
        // before (demotion via dropdown).
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/d.fits", Some("MasterDark"));
        conn.execute("UPDATE frames SET is_master = 1 WHERE id = ?1", [frame_id])
            .unwrap();

        let edits = FrameMetadataEdits {
            imagetyp: Some("DARK".into()),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let (typ, is_master): (String, i64) = conn
            .query_row(
                "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(typ, "Dark");
        assert_eq!(is_master, 0, "is_master must be demoted to 0");
    }

    #[test]
    fn imagetyp_derived_master_flag_overrides_explicit_edit() {
        // If a caller passes both imagetyp=MASTERDARK and is_master=false,
        // the imagetyp-derived flag wins so the row stays consistent.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/d.fits", Some("Dark"));

        let edits = FrameMetadataEdits {
            imagetyp: Some("MasterDark".into()),
            is_master: Some(false),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let (typ, is_master): (String, i64) = conn
            .query_row(
                "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(typ, "MasterDark");
        assert_eq!(is_master, 1, "imagetyp=MasterDark must force is_master=1");
    }

    #[test]
    fn frame_memberships_summary_aggregates_all_three_relations() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let light = insert_frame(&conn, "/x/l.fits", Some("Light"));
        let dark = insert_frame(&conn, "/x/d.fits", Some("Dark"));
        let lonely = insert_frame(&conn, "/x/o.fits", Some("Light"));

        // light → frame_set "M42"
        let session_id = make_session(&conn);
        conn.execute(
            "UPDATE frames_set SET name = 'M42' WHERE id = (SELECT MIN(id) FROM frames_set)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, light],
        ).unwrap();

        // dark → calibration_set member
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, instrume, gain, binning, exptime, ccd_temp, date)
             VALUES ('Dark', 'CamA', 100.0, '1x1', 60.0, -10.0, '2026-01-01')",
            [],
        ).unwrap();
        let cal_set_id: i64 = conn
            .query_row("SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![cal_set_id, dark],
        ).unwrap();

        // light → consumer of cal_set_id (as Dark)
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            rusqlite::params![light, cal_set_id],
        ).unwrap();

        let summary = get_frame_memberships_summary(&conn, &[light, dark, lonely]).unwrap();

        assert_eq!(summary.frame_count_total, 3);
        // 2 of 3 frames are not in any frame_set (dark, lonely).
        assert_eq!(summary.frame_count_unaffiliated, 2);

        assert_eq!(summary.frame_sets.len(), 1);
        assert_eq!(summary.frame_sets[0].name.as_deref(), Some("M42"));
        assert_eq!(summary.frame_sets[0].selected_count, 1);

        assert_eq!(summary.calibration_set_member.len(), 1);
        assert_eq!(summary.calibration_set_member[0].imagetyp, "Dark");
        assert_eq!(summary.calibration_set_member[0].selected_count, 1);

        assert_eq!(summary.calibration_set_consumer.len(), 1);
        assert_eq!(
            summary.calibration_set_consumer[0].calibration_type.as_deref(),
            Some("Dark")
        );
        assert_eq!(summary.calibration_set_consumer[0].selected_count, 1);

        // The dark is a member of the cal set, the light consumes it, and
        // the light belongs to frame_set "M42". So selecting the dark
        // should report "Used in M42 (Dark)".
        assert_eq!(summary.used_in_frame_sets.len(), 1);
        assert_eq!(summary.used_in_frame_sets[0].name.as_deref(), Some("M42"));
        assert_eq!(summary.used_in_frame_sets[0].calibration_type, "Dark");
        assert_eq!(summary.used_in_frame_sets[0].used_for_frame_count, 1);
    }

    #[test]
    fn count_frame_metadata_relations_reports_each_bucket() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let f1 = insert_frame(&conn, "/x/f1.fits", Some("Light"));
        let f2 = insert_frame(&conn, "/x/f2.fits", Some("Dark"));

        // f1: in a session.
        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, f1],
        ).unwrap();

        // f2: member of a calibration set + consumer of one.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')",
            [],
        ).unwrap();
        let set_id: i64 = conn
            .query_row("SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, f2],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            rusqlite::params![f2, set_id],
        ).unwrap();

        let rel = count_frame_metadata_relations(&conn, &[f1, f2]).unwrap();
        assert_eq!(rel.calibration_set_member_count, 1);
        assert_eq!(rel.calibration_set_consumer_count, 1);
        assert_eq!(rel.session_member_count, 1);
        assert_eq!(rel.affected_frame_count, 2, "f1 and f2 are both linked somewhere");
    }
}

