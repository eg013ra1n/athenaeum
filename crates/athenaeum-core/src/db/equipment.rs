// Equipment-related database operations

use crate::models::{CameraStats, CalibrationSetDetail, FileWithFrame, ImageType};
use anyhow::Result;
use chrono::DateTime;
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

/// Shared SELECT + row mapper behind the three camera calibration-library
/// accessors (audit I7 → DB-hygiene Task 16). All three query `calibration_set`
/// joined to its first member frame (lowest frame id — deterministic, see the
/// comment on [`first_member_tests`] above) and differ only in the master
/// flag, an optional `imagetyp` allow-list, and the sort order:
///
/// - `get_camera_dark_library`: `is_master = false`, no `imagetyp` filter (the
///   raw calibration library, despite the "dark" name).
/// - `get_camera_master_dark_library`: `is_master = true`,
///   `imagetyp IN ('MasterDark','MasterBias','MasterDarkFlat')`.
/// - `get_camera_master_flat_library`: `is_master = true`,
///   `imagetyp = 'MasterFlat'`.
///
/// These divergences are real and pinned by
/// `camera_library_accessor_tests::each_accessor_returns_only_its_own_set` —
/// never unify them.
fn query_camera_calibration_sets(
    conn: &Connection,
    instrume: &str,
    is_master: bool,
    imagetyp_filter: Option<&[&str]>,
    order_by: &str,
) -> Result<Vec<CalibrationSetDetail>> {
    let imagetyp_clause = match imagetyp_filter {
        Some(types) => {
            let quoted = types
                .iter()
                .map(|t| format!("'{t}'"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("AND cs.imagetyp IN ({quoted})")
        }
        None => String::new(),
    };

    let query = format!(
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
            cs.focallen,
            cs.uuid,
            cs.updated_at,
            cs.superseded_by_set_id
        FROM calibration_set cs
        LEFT JOIN (
            SELECT set_id, MIN(frame_id) AS frame_id
            FROM calibration_set_frames
            GROUP BY set_id
        ) first_member ON first_member.set_id = cs.id
        LEFT JOIN frames f ON f.id = first_member.frame_id
        LEFT JOIN files fi ON fi.id = f.file_id
        WHERE cs.instrume = ?1
        AND cs.is_master_library = {is_master_library}
        {imagetyp_clause}
        ORDER BY {order_by}",
        is_master_library = is_master as i32,
    );

    let mut stmt = conn.prepare(&query)?;

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
                is_master,
                // Extended fields from frame metadata
                naxis1: row.get(15)?,
                naxis2: row.get(16)?,
                bayerpat: row.get(17)?,
                swcreate: row.get(18)?,
                xpixsz: row.get(19)?,
                format: row.get(20)?,
                focallen: row.get(21)?,
                uuid: row.get(22)?,
                updated_at: row.get(23)?,
                superseded_by_set_id: row.get(24)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sets)
}

/// Get calibration sets for a specific camera
pub fn get_camera_dark_library(
    conn: &Connection,
    instrume: &str,
) -> Result<Vec<CalibrationSetDetail>> {
    query_camera_calibration_sets(
        conn,
        instrume,
        false,
        None,
        "cs.imagetyp, cs.exptime, cs.ccd_temp",
    )
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
    query_camera_calibration_sets(
        conn,
        instrume,
        true,
        Some(&["MasterDark", "MasterBias", "MasterDarkFlat"]),
        "cs.imagetyp, cs.exptime, cs.ccd_temp",
    )
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
    query_camera_calibration_sets(
        conn,
        instrume,
        true,
        Some(&["MasterFlat"]),
        "cs.filter, cs.exptime, cs.ccd_temp",
    )
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
    let query = format!(
        "SELECT {select}
         FROM calibration_set_frames csf
         JOIN frames fr ON csf.frame_id = fr.id
         JOIN files f ON fr.file_id = f.id
         WHERE csf.set_id = ?1
         ORDER BY fr.date_obs ASC",
        select = super::operations::FILE_FRAME_SELECT,
    );
    let mut stmt = conn.prepare(&query)?;

    let frames = stmt.query_map(params![set_id], |row| {
        let (file, frame) = super::operations::map_file_frame_row(row)?;
        let frame = super::operations::require_joined_frame(frame)?;
        Ok(FileWithFrame {
            file,
            frame: Some(frame),
        })
    })?;

    frames.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
}

/// DB-hygiene Task 13 (2026-08-03 audit I6) — the calibration-set frame
/// listing maps the same `files.modified_at`/`created_at` columns as
/// `db::operations`, and used the same panicking `.unwrap()`. One malformed
/// string must not take down the whole set view.
#[cfg(test)]
mod stored_timestamp_tests {
    use crate::db::schema::init_db;
    use chrono::{DateTime, Utc};
    use rusqlite::Connection;

    #[test]
    fn malformed_stored_timestamp_does_not_panic_calibration_set_listing() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES ('/t/bias.fits','bias.fits',1,'not-a-timestamp','FITS','also-bad')",
            [],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, instrume) VALUES (?1, 'BIAS', 'CamA')",
            [file_id],
        )
        .unwrap();
        let frame_id: i64 = conn
            .query_row("SELECT id FROM frames", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (1, 'BIAS', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (1, ?1)",
            [frame_id],
        )
        .unwrap();

        // Previously: thread panic on DateTime::parse_from_rfc3339(...).unwrap().
        let rows = super::get_frames_for_calibration_set(&conn, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file.modified_at, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(rows[0].file.created_at, DateTime::<Utc>::UNIX_EPOCH);
    }
}

#[cfg(test)]
mod cfa_projection_tests {
    use crate::db::schema::init_db;
    use rusqlite::Connection;

    /// The calibration-set frame listing shares the 47-column SELECT that
    /// erased the CFA columns — pinned here against the canonical projection.
    #[test]
    fn calibration_set_frames_carry_cfa_columns() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1, '/t/flat.fits', 'flat.fits', 1, '2026-01-01T00:00:00+00:00', 'FITS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, swcreate,
                                 bayerpat, xbayroff, ybayroff, roworder, rotation)
             VALUES (1, 1, 'FLAT', 'CamA', 'CaptureApp', 'RGGB', 1, 0, 'BOTTOM-UP', 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (1, 'FLAT', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (1, 1)",
            [],
        )
        .unwrap();

        let rows = super::get_frames_for_calibration_set(&conn, 1).unwrap();
        assert_eq!(rows.len(), 1);
        let frame = rows[0].frame.as_ref().expect("joined frame");
        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(frame.xbayroff, Some(1));
        assert_eq!(frame.ybayroff, Some(0));
        assert_eq!(frame.roworder.as_deref(), Some("BOTTOM-UP"));
        assert_eq!(frame.swcreate.as_deref(), Some("CaptureApp"));
    }
}

/// DB-hygiene Task 15 (2026-08-03 audit I7) — the three library queries used to
/// select bare, non-aggregated frame columns beside `GROUP BY cs.id`. Per
/// SQLite's documented semantics those values come from an *arbitrary* member
/// row, not the first one the comments claimed. The `MIN(frame_id)` subquery
/// makes "first member" mean something.
#[cfg(test)]
mod first_member_tests {
    use super::get_camera_dark_library;
    use rusqlite::Connection;

    #[test]
    fn library_metadata_comes_from_the_lowest_frame_id() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, instrume, date_start, date_end)
         VALUES ('Dark','2026-01-01','CamX','2026-01-01T00:00:00Z','2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();
        for (name, naxis1) in [("d1", 6000_i64), ("d2", 9999_i64)] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![format!("/t/{name}.fits"), format!("{name}.fits")],
            )
            .unwrap();
            let fid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, naxis1, instrume) VALUES (?1, ?2, 'CamX')",
                rusqlite::params![fid, naxis1],
            )
            .unwrap();
            let frid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frid],
            )
            .unwrap();
        }
        let sets = get_camera_dark_library(&conn, "CamX").unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(
            sets[0].naxis1,
            Some(6000),
            "must be the FIRST member (lowest frame id), not an arbitrary one"
        );
    }
}

/// DB-hygiene Task 16 pin-behavior test — written and run against the
/// pre-refactor three-copies-of-one-query code to pin the WHERE-clause
/// divergences before `query_camera_calibration_sets` consolidates them:
/// `get_camera_dark_library` filters only `is_master_library = 0` (no
/// `imagetyp` filter — it is really "raw calibration library" despite the
/// name); `get_camera_master_dark_library` adds
/// `imagetyp IN ('MasterDark','MasterBias','MasterDarkFlat')`;
/// `get_camera_master_flat_library` adds `imagetyp = 'MasterFlat'`. Each
/// accessor must return exactly its own set and none of the other two.
#[cfg(test)]
mod camera_library_accessor_tests {
    use super::{
        get_camera_dark_library, get_camera_master_dark_library, get_camera_master_flat_library,
    };
    use rusqlite::Connection;

    #[test]
    fn each_accessor_returns_only_its_own_set() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, instrume, is_master_library, date_start, date_end)
             VALUES ('Dark', '2026-01-01', 'CamPin', 0, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
        let raw_dark_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, instrume, is_master_library, date_start, date_end)
             VALUES ('MasterDark', '2026-01-01', 'CamPin', 1, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
        let master_dark_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, instrume, is_master_library, date_start, date_end)
             VALUES ('MasterFlat', '2026-01-01', 'CamPin', 1, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
        let master_flat_id = conn.last_insert_rowid();

        let dark_library = get_camera_dark_library(&conn, "CamPin").unwrap();
        assert_eq!(
            dark_library.iter().filter_map(|s| s.id).collect::<Vec<_>>(),
            vec![raw_dark_id],
            "get_camera_dark_library must return only the raw (is_master_library = 0) set"
        );

        let master_dark_library = get_camera_master_dark_library(&conn, "CamPin").unwrap();
        assert_eq!(
            master_dark_library
                .iter()
                .filter_map(|s| s.id)
                .collect::<Vec<_>>(),
            vec![master_dark_id],
            "get_camera_master_dark_library must return only the MasterDark set"
        );

        let master_flat_library = get_camera_master_flat_library(&conn, "CamPin").unwrap();
        assert_eq!(
            master_flat_library
                .iter()
                .filter_map(|s| s.id)
                .collect::<Vec<_>>(),
            vec![master_flat_id],
            "get_camera_master_flat_library must return only the MasterFlat set"
        );
    }
}
