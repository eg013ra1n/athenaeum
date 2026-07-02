//! DB helpers for the `registration_results` table.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// A single row from `registration_results`, returned to callers.
///
/// All WCS and affine fields are `Option<f64>` because the reference row
/// stores identity/solved values while failed rows may have `NULL` affine
/// columns.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationRecord {
    pub id: Option<i64>,
    pub frames_set_id: i64,
    pub frame_id: i64,
    pub reference_frame_id: i64,
    pub is_reference: bool,

    // Refined WCS (sub-frame or reference). Null on failed rows.
    pub crpix1: Option<f64>,
    pub crpix2: Option<f64>,
    pub crval1: Option<f64>,
    pub crval2: Option<f64>,
    pub cd1_1: Option<f64>,
    pub cd1_2: Option<f64>,
    pub cd2_1: Option<f64>,
    pub cd2_2: Option<f64>,

    // Affine transform sub_pixel → reference_pixel. Identity for the reference
    // row; null on failed rows.
    pub affine_a1: Option<f64>,
    pub affine_b1: Option<f64>,
    pub affine_c1: Option<f64>,
    pub affine_a2: Option<f64>,
    pub affine_b2: Option<f64>,
    pub affine_c2: Option<f64>,

    pub matched_stars: i64,
    pub rms_residual_px: f64,
    /// RMS in arcsec: `rms_residual_px * pixel_scale_arcsec`. `None` when the
    /// reference pixel scale is unavailable (rare).
    pub rms_residual_arcsec: Option<f64>,

    /// `"aligned"` | `"aligned_flipped"` | `"reference"` | `"failed"`.
    /// `"aligned_flipped"` is an aligned variant (not a distinct outcome): the
    /// fitted transform includes a reflection (meridian-flipped sub).
    pub status: String,
    /// Error message for failed rows.
    pub error: Option<String>,

    pub compute_time_ms: i64,
    pub registered_at: String,
}

// ── write helpers ─────────────────────────────────────────────────────────────

/// Insert or replace a single registration row (upsert by frames_set_id +
/// frame_id per the UNIQUE constraint).
pub fn upsert_registration(conn: &Connection, rec: &RegistrationRecord) -> Result<i64> {
    conn.execute(
        "INSERT OR REPLACE INTO registration_results (
            frames_set_id, frame_id, reference_frame_id, is_reference,
            crpix1, crpix2, crval1, crval2,
            cd1_1, cd1_2, cd2_1, cd2_2,
            affine_a1, affine_b1, affine_c1,
            affine_a2, affine_b2, affine_c2,
            matched_stars, rms_residual_px, rms_residual_arcsec,
            status, error, compute_time_ms, registered_at
        ) VALUES (
            ?1, ?2, ?3, ?4,
            ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12,
            ?13, ?14, ?15,
            ?16, ?17, ?18,
            ?19, ?20, ?21,
            ?22, ?23, ?24, ?25
        )",
        rusqlite::params![
            rec.frames_set_id,
            rec.frame_id,
            rec.reference_frame_id,
            rec.is_reference as i64,
            rec.crpix1,
            rec.crpix2,
            rec.crval1,
            rec.crval2,
            rec.cd1_1,
            rec.cd1_2,
            rec.cd2_1,
            rec.cd2_2,
            rec.affine_a1,
            rec.affine_b1,
            rec.affine_c1,
            rec.affine_a2,
            rec.affine_b2,
            rec.affine_c2,
            rec.matched_stars,
            rec.rms_residual_px,
            rec.rms_residual_arcsec,
            rec.status,
            rec.error,
            rec.compute_time_ms,
            rec.registered_at,
        ],
    )
    .context("Failed to upsert registration_results row")?;
    Ok(conn.last_insert_rowid())
}

// ── read helpers ──────────────────────────────────────────────────────────────

/// Return all registration rows for a frame set, ordered by frame_id.
pub fn get_registration_for_frame_set(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<RegistrationRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, frames_set_id, frame_id, reference_frame_id, is_reference,
                    crpix1, crpix2, crval1, crval2,
                    cd1_1, cd1_2, cd2_1, cd2_2,
                    affine_a1, affine_b1, affine_c1,
                    affine_a2, affine_b2, affine_c2,
                    matched_stars, rms_residual_px, rms_residual_arcsec,
                    status, error, compute_time_ms, registered_at
             FROM registration_results
             WHERE frames_set_id = ?1
             ORDER BY frame_id",
        )
        .context("Failed to prepare registration_results SELECT")?;

    let rows = stmt
        .query_map([frames_set_id], |row| {
            Ok(RegistrationRecord {
                id: row.get(0)?,
                frames_set_id: row.get(1)?,
                frame_id: row.get(2)?,
                reference_frame_id: row.get(3)?,
                is_reference: row.get::<_, i64>(4)? != 0,
                crpix1: row.get(5)?,
                crpix2: row.get(6)?,
                crval1: row.get(7)?,
                crval2: row.get(8)?,
                cd1_1: row.get(9)?,
                cd1_2: row.get(10)?,
                cd2_1: row.get(11)?,
                cd2_2: row.get(12)?,
                affine_a1: row.get(13)?,
                affine_b1: row.get(14)?,
                affine_c1: row.get(15)?,
                affine_a2: row.get(16)?,
                affine_b2: row.get(17)?,
                affine_c2: row.get(18)?,
                matched_stars: row.get(19)?,
                rms_residual_px: row.get(20)?,
                rms_residual_arcsec: row.get(21)?,
                status: row.get(22)?,
                error: row.get(23)?,
                compute_time_ms: row.get(24)?,
                registered_at: row.get(25)?,
            })
        })
        .context("Failed to query registration_results")?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect registration_results rows")
}

/// Delete all registration rows for a frame set. Called before re-running
/// registration so stale rows from a previous run are removed atomically.
pub fn clear_registration_for_frame_set(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<usize> {
    let n = conn
        .execute(
            "DELETE FROM registration_results WHERE frames_set_id = ?1",
            [frames_set_id],
        )
        .context("Failed to clear registration_results")?;
    Ok(n)
}

// ── user-chosen reference frame store ────────────────────────────────────────

/// A row from `frame_set_reference`, returned to callers and serialised across
/// the IPC/HTTP boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetReference {
    pub frames_set_id: i64,
    pub reference_frame_id: i64,
    /// ISO-8601 timestamp stored as TEXT in SQLite.
    pub set_at: String,
}

/// Persist (or overwrite) the user-chosen reference frame for a frame set.
///
/// Validates that `frame_id` is a LIGHT member of `frames_set_id` before
/// writing; returns an error if the frame is not a member.
pub fn set_frame_set_reference(
    conn: &Connection,
    frames_set_id: i64,
    frame_id: i64,
) -> Result<()> {
    let members = get_light_frame_ids_for_frame_set(conn, frames_set_id)?;
    if !members.contains(&frame_id) {
        anyhow::bail!(
            "Frame {frame_id} is not a LIGHT member of frame set {frames_set_id}"
        );
    }
    conn.execute(
        "INSERT OR REPLACE INTO frame_set_reference
             (frames_set_id, reference_frame_id, set_at)
         VALUES (?1, ?2, strftime('%Y-%m-%d %H:%M:%S', 'now'))",
        rusqlite::params![frames_set_id, frame_id],
    )
    .context("Failed to upsert frame_set_reference")?;
    Ok(())
}

/// Retrieve the persisted user-chosen reference for a frame set, if any.
pub fn get_frame_set_reference(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Option<FrameSetReference>> {
    let mut stmt = conn
        .prepare(
            "SELECT frames_set_id, reference_frame_id, set_at
             FROM frame_set_reference
             WHERE frames_set_id = ?1",
        )
        .context("Failed to prepare frame_set_reference SELECT")?;

    let result = stmt.query_row([frames_set_id], |row| {
        Ok(FrameSetReference {
            frames_set_id: row.get(0)?,
            reference_frame_id: row.get(1)?,
            set_at: row.get(2)?,
        })
    });

    match result {
        Ok(r) => Ok(Some(r)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(anyhow::anyhow!(e).context("Failed to query frame_set_reference")),
    }
}

/// Remove the persisted user-chosen reference for a frame set (if any).
pub fn clear_frame_set_reference(conn: &Connection, frames_set_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM frame_set_reference WHERE frames_set_id = ?1",
        [frames_set_id],
    )
    .context("Failed to delete frame_set_reference")?;
    Ok(())
}

// ── frame-set LIGHT member helper ─────────────────────────────────────────────

/// Return the `frames.id` values of every LIGHT member of a frame set.
///
/// Joins through `frames_set → imaging_nights → sessions → session_members →
/// frames`, filtering on `UPPER(imagetyp) = 'LIGHT'`. Frame sets with no LIGHT
/// members return an empty `Vec`.
pub fn get_light_frame_ids_for_frame_set(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<i64>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT sm.frame_id
             FROM session_members sm
             JOIN sessions s       ON s.id  = sm.session_id
             JOIN imaging_nights n ON n.id  = s.imaging_night_id
             JOIN frames fr        ON fr.id = sm.frame_id
             WHERE n.frames_set_id = ?1
               AND UPPER(COALESCE(fr.imagetyp, '')) = 'LIGHT'
             ORDER BY sm.frame_id",
        )
        .context("Failed to prepare LIGHT frame query for frame set")?;

    let rows = stmt
        .query_map([frames_set_id], |row| row.get::<_, i64>(0))
        .context("Failed to query LIGHT frames")?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect LIGHT frame IDs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    /// Seed a minimal in-memory DB with one file + frame so FK constraints pass.
    fn seed_frame(conn: &Connection, file_id: i64, frame_id: i64, imagetyp: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, created_at)
             VALUES (?1, ?2, ?3, 0, '2025-01-01', 'FITS', '2025-01-01')",
            rusqlite::params![
                file_id,
                format!("/x/frame{file_id}.fits"),
                format!("frame{file_id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, ?3)",
            rusqlite::params![frame_id, file_id, imagetyp],
        )
        .unwrap();
    }

    fn seed_frames_set(conn: &Connection, fs_id: i64) {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, 'TestSet')",
            [fs_id],
        )
        .unwrap();
    }

    fn seed_session_member(conn: &Connection, fs_id: i64, frame_id: i64) {
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2025-01-01', '2025-01-01')",
            [fs_id],
        )
        .unwrap();
        let night_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'Cam')",
            [night_id],
        )
        .unwrap();
        let session_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, frame_id],
        )
        .unwrap();
    }

    fn make_record(fs_id: i64, frame_id: i64, ref_id: i64, status: &str) -> RegistrationRecord {
        RegistrationRecord {
            id: None,
            frames_set_id: fs_id,
            frame_id,
            reference_frame_id: ref_id,
            is_reference: frame_id == ref_id,
            crpix1: Some(1000.0),
            crpix2: Some(800.0),
            crval1: Some(83.82),
            crval2: Some(-5.39),
            cd1_1: Some(-2.8e-4),
            cd1_2: Some(0.0),
            cd2_1: Some(0.0),
            cd2_2: Some(2.8e-4),
            affine_a1: Some(1.0),
            affine_b1: Some(0.0),
            affine_c1: Some(0.0),
            affine_a2: Some(0.0),
            affine_b2: Some(1.0),
            affine_c2: Some(0.0),
            matched_stars: 42,
            rms_residual_px: 0.35,
            rms_residual_arcsec: Some(0.35 * 1.0),
            status: status.to_string(),
            error: None,
            compute_time_ms: 120,
            registered_at: "2026-01-01 00:00:00".to_string(),
        }
    }

    #[test]
    fn upsert_and_get_round_trip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frame(&conn, 2, 2, "LIGHT");
        seed_frames_set(&conn, 10);

        let rec1 = make_record(10, 1, 1, "reference");
        let rec2 = make_record(10, 2, 1, "aligned");
        upsert_registration(&conn, &rec1).unwrap();
        upsert_registration(&conn, &rec2).unwrap();

        let rows = get_registration_for_frame_set(&conn, 10).unwrap();
        assert_eq!(rows.len(), 2);
        let ref_row = rows.iter().find(|r| r.is_reference).unwrap();
        assert_eq!(ref_row.frame_id, 1);
        assert_eq!(ref_row.status, "reference");

        let aligned = rows.iter().find(|r| !r.is_reference).unwrap();
        assert_eq!(aligned.frame_id, 2);
        assert!((aligned.rms_residual_px - 0.35).abs() < 1e-9);
        assert_eq!(aligned.matched_stars, 42);
    }

    #[test]
    fn upsert_overwrites_existing_row() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frames_set(&conn, 10);

        let mut rec = make_record(10, 1, 1, "reference");
        upsert_registration(&conn, &rec).unwrap();
        rec.rms_residual_px = 0.99;
        rec.status = "aligned".to_string();
        upsert_registration(&conn, &rec).unwrap();

        let rows = get_registration_for_frame_set(&conn, 10).unwrap();
        assert_eq!(rows.len(), 1, "UNIQUE must collapse duplicate rows");
        assert!((rows[0].rms_residual_px - 0.99).abs() < 1e-9);
    }

    #[test]
    fn clear_removes_all_rows_for_set() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frame(&conn, 2, 2, "LIGHT");
        seed_frame(&conn, 3, 3, "LIGHT");
        seed_frames_set(&conn, 10);

        for fid in [1_i64, 2, 3] {
            upsert_registration(&conn, &make_record(10, fid, 1, "aligned")).unwrap();
        }
        assert_eq!(get_registration_for_frame_set(&conn, 10).unwrap().len(), 3);
        let n = clear_registration_for_frame_set(&conn, 10).unwrap();
        assert_eq!(n, 3);
        assert_eq!(get_registration_for_frame_set(&conn, 10).unwrap().len(), 0);
    }

    #[test]
    fn get_light_frame_ids_excludes_darks() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frame(&conn, 2, 2, "Dark");
        seed_frame(&conn, 3, 3, "LIGHT");
        seed_frames_set(&conn, 10);
        // Insert all three into the same session
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (10, '2025-01-01', '2025-01-01')",
            [],
        )
        .unwrap();
        let night_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'C')",
            [night_id],
        )
        .unwrap();
        let session_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        for fid in [1_i64, 2, 3] {
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![session_id, fid],
            )
            .unwrap();
        }

        let ids = get_light_frame_ids_for_frame_set(&conn, 10).unwrap();
        assert_eq!(ids, vec![1_i64, 3], "only LIGHT frames returned");
    }

    #[test]
    fn seed_session_member_test() {
        // Regression: seed_session_member creates a separate imaging_night each
        // call, so two calls create two nights in the same frame set — both must
        // resolve correctly.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frame(&conn, 2, 2, "LIGHT");
        seed_frames_set(&conn, 10);
        seed_session_member(&conn, 10, 1);

        // Night already inserted by seed_session_member above; add frame 2 into
        // the existing session via a direct insert.
        let session_id: i64 = conn
            .query_row("SELECT id FROM sessions LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, 2_i64],
        )
        .unwrap();

        let ids = get_light_frame_ids_for_frame_set(&conn, 10).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    // ── frame_set_reference store tests ──────────────────────────────────────

    /// Set / get / clear round-trip.
    #[test]
    fn reference_store_round_trip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frame(&conn, 2, 2, "LIGHT");
        seed_frames_set(&conn, 10);
        seed_session_member(&conn, 10, 1);
        // Add frame 2 to the same session.
        let session_id: i64 = conn
            .query_row("SELECT id FROM sessions LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, 2_i64],
        )
        .unwrap();

        // No reference stored yet.
        assert!(get_frame_set_reference(&conn, 10).unwrap().is_none());

        // Set frame 1 as reference.
        set_frame_set_reference(&conn, 10, 1).unwrap();
        let stored = get_frame_set_reference(&conn, 10).unwrap().unwrap();
        assert_eq!(stored.frames_set_id, 10);
        assert_eq!(stored.reference_frame_id, 1);
        assert!(!stored.set_at.is_empty());

        // Overwrite with frame 2.
        set_frame_set_reference(&conn, 10, 2).unwrap();
        let stored2 = get_frame_set_reference(&conn, 10).unwrap().unwrap();
        assert_eq!(stored2.reference_frame_id, 2);

        // Clear.
        clear_frame_set_reference(&conn, 10).unwrap();
        assert!(get_frame_set_reference(&conn, 10).unwrap().is_none());
    }

    /// Membership validation: setting a non-LIGHT member (or a completely
    /// unrelated frame) as the reference must return an error.
    #[test]
    fn reference_store_rejects_non_member() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Frame 1 is LIGHT and a member; frame 99 is not a member at all.
        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frame(&conn, 99, 99, "LIGHT");
        seed_frames_set(&conn, 10);
        seed_session_member(&conn, 10, 1);

        let err = set_frame_set_reference(&conn, 10, 99);
        assert!(err.is_err(), "expected error for non-member frame");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("not a LIGHT member"),
            "error should mention membership: {msg}"
        );
    }

    /// A DARK frame in the session must not be accepted as a reference.
    #[test]
    fn reference_store_rejects_dark_member() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        seed_frame(&conn, 1, 1, "LIGHT");
        seed_frame(&conn, 2, 2, "Dark");
        seed_frames_set(&conn, 10);
        // Both frames in the same session.
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (10, '2025-01-01', '2025-01-01')",
            [],
        )
        .unwrap();
        let night_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'Cam')",
            [night_id],
        )
        .unwrap();
        let session_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();
        for fid in [1_i64, 2] {
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![session_id, fid],
            )
            .unwrap();
        }

        // Frame 1 (LIGHT) should work; frame 2 (Dark) must be rejected.
        set_frame_set_reference(&conn, 10, 1).unwrap();
        let err = set_frame_set_reference(&conn, 10, 2);
        assert!(err.is_err(), "expected error for Dark frame as reference");
    }
}
