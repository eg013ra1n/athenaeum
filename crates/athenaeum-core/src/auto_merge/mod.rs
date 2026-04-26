//! Auto-merge engine: find and optionally attach unclustered LIGHT frames
//! that match an existing frame set's coordinate centroid.
//!
//! Design: a candidate is any LIGHT frame that (a) is not currently a member
//! of any session (i.e., not part of any frame set), and (b) lies within
//! `threshold_deg` of the target frame set's centroid.
//!
//! Merging delegates to `sessions::detect_sessions` and
//! `frames_set_merge::find_matching_night` to stitch new frames into the
//! target set's existing nights/sessions structure, reusing well-tested
//! logic from the manual "merge frame sets" path.

pub mod log_ops;

use crate::coordinates::{angular_distance, parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::models::{
    CandidateFrame, ImagingNight, MergeReport, SkipReason, SkippedFrame,
};
use crate::sessions::detect_sessions;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};

/// Find LIGHT frames that are currently unclustered and fall within
/// `threshold_deg` of the frame set's coordinate centroid.
///
/// Returns an empty vector if the frame set has no centroid (e.g. a custom
/// set created without coordinates).
pub fn find_candidates_for_set(
    conn: &Connection,
    frames_set_id: i64,
    threshold_deg: f64,
) -> Result<Vec<CandidateFrame>> {
    // 1. Load the set's centroid (stored as HMS/DMS strings).
    let (objctra, objctdec): (Option<String>, Option<String>) = conn.query_row(
        "SELECT objctra, objctdec FROM frames_set WHERE id = ?1",
        params![frames_set_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let Some(ra_str) = objctra else {
        return Ok(Vec::new());
    };
    let Some(dec_str) = objctdec else {
        return Ok(Vec::new());
    };

    let centroid_ra = parse_ra_sexagesimal(&ra_str)
        .map_err(|e| anyhow!("frame set has unparseable objctra '{}': {}", ra_str, e))?;
    let centroid_dec = parse_dec_sexagesimal(&dec_str)
        .map_err(|e| anyhow!("frame set has unparseable objctdec '{}': {}", dec_str, e))?;

    // 2. Select all LIGHT frames that are NOT already a session member.
    //    Filter by coordinate validity here to minimize work in Rust.
    let mut stmt = conn.prepare(
        "SELECT fr.id, fr.file_id, f.filename, fr.ra, fr.dec, fr.date_obs,
                fr.filter, fr.instrume
         FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE fr.imagetyp = 'Light'
           AND fr.ra IS NOT NULL
           AND fr.dec IS NOT NULL
           AND fr.id NOT IN (SELECT frame_id FROM session_members)",
    )?;

    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let file_id: i64 = row.get(1)?;
        let filename: String = row.get(2)?;
        let ra: Option<f64> = row.get(3)?;
        let dec: Option<f64> = row.get(4)?;
        let date_obs_str: Option<String> = row.get(5)?;
        let filter: Option<String> = row.get(6)?;
        let instrume: Option<String> = row.get(7)?;
        Ok((id, file_id, filename, ra, dec, date_obs_str, filter, instrume))
    })?;

    let mut candidates = Vec::new();
    for row in rows {
        let (id, file_id, filename, ra, dec, date_obs_str, filter, instrume) = row?;
        let (Some(ra), Some(dec)) = (ra, dec) else { continue };

        let distance_deg = angular_distance(centroid_ra, centroid_dec, ra, dec);
        if distance_deg > threshold_deg {
            continue;
        }

        let date_obs = date_obs_str
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        candidates.push(CandidateFrame {
            frame_id: id,
            file_id,
            filename,
            ra: Some(ra),
            dec: Some(dec),
            date_obs,
            filter,
            instrume,
        });
    }

    Ok(candidates)
}

/// Attach `candidates` to an existing frame set's night/session structure,
/// write a `frame_set_merge_log` audit row, and recalculate the set's
/// aggregate metadata — all inside a single SQL transaction.
///
/// Candidates without a `date_obs` are skipped (reason: NoCoords — they
/// lack the observation time needed to place them in a session); the skip
/// reason is recorded in the audit log but never aborts the batch.
pub fn merge_candidates(
    conn: &mut Connection,
    frames_set_id: i64,
    candidates: Vec<CandidateFrame>,
    source: &str,
    threshold_arcmin: f64,
    session_gap_hours: f64,
) -> Result<MergeReport> {
    if source != "button" && source != "monitor" {
        return Err(anyhow!("invalid merge source '{}'", source));
    }

    // Partition: we can only place frames with a valid date_obs. Frames
    // without are logged as skipped (NoCoords reason covers "no usable
    // temporal/spatial data").
    let (mergeable, skipped_no_date): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|c| c.date_obs.is_some());

    let mut frames_skipped: Vec<SkippedFrame> = skipped_no_date
        .into_iter()
        .map(|c| SkippedFrame {
            frame_id: c.frame_id,
            filename: c.filename,
            reason: SkipReason::NoCoords,
        })
        .collect();

    if mergeable.is_empty() {
        // Still record an audit entry — a run that skipped everything is
        // informative.
        let report = MergeReport {
            frames_set_id,
            added_count: 0,
            skipped_count: frames_skipped.len() as i64,
            frames_added: Vec::new(),
            frames_skipped: frames_skipped.clone(),
            threshold_arcmin,
            source: source.to_string(),
        };
        log_ops::insert_log_entry(conn, &report)?;
        return Ok(report);
    }

    // Load File + Frame tuples for the mergeable candidates so we can run
    // the same session-detection pipeline used when creating a new set.
    let frame_ids: Vec<i64> = mergeable.iter().map(|c| c.frame_id).collect();
    let frames_with_files = crate::db::get_frames_with_files_by_ids(conn, &frame_ids)?;

    // Detect nights/sessions for the new frames.
    let detected_nights = detect_sessions(frames_with_files, session_gap_hours)?;

    // Load existing nights on the target so we can match and append.
    let target_nights: Vec<ImagingNight> =
        crate::db::get_imaging_nights_for_set(conn, frames_set_id)?;

    let tx = conn.transaction()?;

    for detected in detected_nights {
        // Build an ImagingNight view of the detected night for the matcher.
        let detected_view = ImagingNight {
            id: None,
            frames_set_id,
            start_time: detected.start_time.clone(),
            end_time: detected.end_time.clone(),
            created_at: None,
        };

        let matching_target =
            crate::frames_set_merge::find_matching_night(&detected_view, &target_nights)?;

        let night_id = match matching_target {
            Some(existing_night_id) => {
                // Extend the existing night's time range to cover the new frames.
                let existing = target_nights
                    .iter()
                    .find(|n| n.id == Some(existing_night_id))
                    .ok_or_else(|| anyhow!("matching night {} vanished", existing_night_id))?;
                let (new_start, new_end) = crate::frames_set_merge::calculate_time_range_union(
                    &detected.start_time,
                    &detected.end_time,
                    &existing.start_time,
                    &existing.end_time,
                )?;
                crate::db::update_imaging_night_time_range(
                    &tx,
                    existing_night_id,
                    &new_start,
                    &new_end,
                )?;
                existing_night_id
            }
            None => crate::db::create_imaging_night(
                &tx,
                frames_set_id,
                &detected.start_time,
                &detected.end_time,
            )?,
        };

        // For each detected session (by instrume), either append to an
        // existing session on this night or create a new one.
        let existing_sessions = crate::db::get_sessions_for_night(&tx, night_id)?;

        for session in detected.sessions {
            let existing_session_id = existing_sessions
                .iter()
                .find(|s| s.instrume == session.instrume)
                .and_then(|s| s.id);

            let session_id = match existing_session_id {
                Some(id) => id,
                None => crate::db::create_session(
                    &tx,
                    night_id,
                    &session.instrume,
                    0, // updated by recalculate_frame_set_metadata later
                    None,
                )?,
            };

            crate::db::insert_session_members(&tx, session_id, &session.frame_ids)?;
        }
    }

    // Recalculate aggregate metadata (totals, coordinate centroid drift, rotation bounds).
    let metadata =
        crate::frames_set_metadata::calculate_metadata_for_frame_set(frames_set_id, &tx)?;
    crate::db::update_frames_set_metadata(
        &tx,
        frames_set_id,
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        true, // merging makes the set "custom" — don't auto-regenerate it
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    )?;

    // Build the report and write the audit log row inside the same tx.
    let report = MergeReport {
        frames_set_id,
        added_count: mergeable.len() as i64,
        skipped_count: frames_skipped.len() as i64,
        frames_added: mergeable,
        frames_skipped: {
            frames_skipped.sort_by_key(|s| s.frame_id);
            frames_skipped
        },
        threshold_arcmin,
        source: source.to_string(),
    };
    log_ops::insert_log_entry_tx(&tx, &report)?;

    tx.commit()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Build an in-memory DB with just the schema rows we need for the
    /// candidate search — full schema init would pull in too many tables
    /// for a focused unit test. We deliberately mirror only the columns
    /// used by `find_candidates_for_set`.
    fn seed_minimal(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                filename TEXT NOT NULL,
                size INTEGER NOT NULL DEFAULT 0,
                modified_at TEXT NOT NULL DEFAULT '',
                format TEXT NOT NULL DEFAULT 'FITS',
                created_at TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE frames (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL,
                imagetyp TEXT,
                ra REAL,
                dec REAL,
                date_obs TEXT,
                filter TEXT,
                instrume TEXT
            );
            CREATE TABLE frames_set (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT,
                is_custom INTEGER NOT NULL DEFAULT 0,
                objctra TEXT,
                objctdec TEXT,
                total_exp_time REAL
            );
            CREATE TABLE sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                imaging_night_id INTEGER NOT NULL,
                instrume TEXT NOT NULL
            );
            CREATE TABLE session_members (
                session_id INTEGER NOT NULL,
                frame_id INTEGER NOT NULL,
                PRIMARY KEY (session_id, frame_id)
            );",
        )
        .unwrap();
    }

    fn add_file(conn: &Connection, path: &str, filename: &str) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename) VALUES (?1, ?2)",
            rusqlite::params![path, filename],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn add_light_frame(
        conn: &Connection,
        file_id: i64,
        ra: f64,
        dec: f64,
        in_session_id: Option<i64>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, ra, dec, date_obs, filter, instrume)
             VALUES (?1, 'Light', ?2, ?3, '2026-01-01T22:00:00Z', 'Lum', 'ASI2600')",
            rusqlite::params![file_id, ra, dec],
        )
        .unwrap();
        let frame_id = conn.last_insert_rowid();
        if let Some(session_id) = in_session_id {
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![session_id, frame_id],
            )
            .unwrap();
        }
        frame_id
    }

    #[test]
    fn finds_nearby_unclustered_lights_only() {
        let conn = Connection::open_in_memory().unwrap();
        seed_minimal(&conn);

        // A frame set centred near M31 (RA 00h42m44s, Dec +41°16'9").
        conn.execute(
            "INSERT INTO frames_set (name, is_custom, objctra, objctdec)
             VALUES ('M31', 1, '00 42 44.3', '+41 16 09.0')",
            [],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();

        // A pre-existing session (any session_id) to simulate already-clustered frames.
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (1, 'ASI2600')",
            [],
        )
        .unwrap();
        let session_id = conn.last_insert_rowid();

        // ra_m31 ≈ 10.685° dec_m31 ≈ 41.269°
        // 1. A light frame near M31, not in any session → should be a candidate
        let file_a = add_file(&conn, "/data/a.fits", "a.fits");
        let frame_a = add_light_frame(&conn, file_a, 10.70, 41.27, None);

        // 2. A light frame near M31 but already clustered → should be excluded
        let file_b = add_file(&conn, "/data/b.fits", "b.fits");
        add_light_frame(&conn, file_b, 10.71, 41.28, Some(session_id));

        // 3. A light frame far from M31 (near Orion, RA ~83°) → outside threshold
        let file_c = add_file(&conn, "/data/c.fits", "c.fits");
        add_light_frame(&conn, file_c, 83.82, -5.39, None);

        let threshold_deg = 1.0; // generous
        let candidates = find_candidates_for_set(&conn, set_id, threshold_deg).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].frame_id, frame_a);
    }

    #[test]
    fn frame_set_with_no_centroid_yields_no_candidates() {
        let conn = Connection::open_in_memory().unwrap();
        seed_minimal(&conn);
        conn.execute(
            "INSERT INTO frames_set (name, is_custom, objctra, objctdec) VALUES ('Custom', 1, NULL, NULL)",
            [],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();

        let file_a = add_file(&conn, "/x/a.fits", "a.fits");
        add_light_frame(&conn, file_a, 10.0, 40.0, None);

        let candidates = find_candidates_for_set(&conn, set_id, 1.0).unwrap();
        assert!(candidates.is_empty());
    }
}
