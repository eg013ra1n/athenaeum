use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::models::{File, Frame};

/// Outcome of [`rederive_for_frame_set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RederiveSummary {
    pub frames: usize,
    pub nights: usize,
    pub sessions: usize,
}

/// Re-derive a frame set's imaging nights and sessions from the union of its
/// current member frames and `extra_frame_ids` (frames about to join).
///
/// Runs inside the caller's transaction: the set's night rows are deleted
/// (sessions and members cascade), [`detect_sessions`] runs over the whole
/// membership, and the rows are written back. A night is derived data with
/// one definition — the gap rule — so it is recomputed, never stitched from
/// the rows two sets happened to carry: stitching by date + range overlap is
/// what stored one night as two rows after a merge (LDN 1272, 2026-09-05).
/// A member without `DATE-OBS` cannot be placed by the gap rule and is kept
/// on a fallback night, so a recalculation never loses a frame.
pub fn rederive_for_frame_set(
    conn: &Connection,
    frames_set_id: i64,
    extra_frame_ids: &[i64],
    gap_threshold_hours: f64,
) -> Result<RederiveSummary> {
    let mut frame_ids = crate::db::get_frame_ids_for_frame_set(conn, frames_set_id)?;
    frame_ids.extend_from_slice(extra_frame_ids);
    frame_ids.sort_unstable();
    frame_ids.dedup();

    let frames = crate::db::get_frames_with_files_by_ids(conn, &frame_ids)?;
    let known: Vec<i64> = frames.iter().filter_map(|(_, _, f)| f.id).collect();
    let detected = detect_sessions(frames, gap_threshold_hours)?;

    crate::db::delete_imaging_nights_for_frame_set(conn, frames_set_id)?;

    let mut placed: HashSet<i64> = HashSet::new();
    let mut nights = 0usize;
    let mut sessions = 0usize;
    for night in &detected {
        let night_id = crate::db::create_imaging_night(
            conn,
            frames_set_id,
            &night.start_time,
            &night.end_time,
        )?;
        nights += 1;
        for session in &night.sessions {
            let session_id = crate::db::create_session(
                conn,
                night_id,
                &session.instrume,
                session.frame_ids.len() as i32,
                session.total_exp_time,
            )?;
            crate::db::insert_session_members(conn, session_id, &session.frame_ids)?;
            placed.extend(session.frame_ids.iter().copied());
            sessions += 1;
        }
    }

    // Members the gap rule could not place (no DATE-OBS) stay on a fallback
    // night — the same shape a selection with no timestamps gets — rather
    // than silently leaving the set.
    let leftover: Vec<i64> = known.iter().copied().filter(|id| !placed.contains(id)).collect();
    if !leftover.is_empty() {
        tracing::warn!(
            set_id = frames_set_id,
            count = leftover.len(),
            "frames without date_obs kept on a fallback night"
        );
        let now = Utc::now();
        let start = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let end = (now + Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let night_id = crate::db::create_imaging_night(conn, frames_set_id, &start, &end)?;
        let session_id =
            crate::db::create_session(conn, night_id, "Unknown", leftover.len() as i32, None)?;
        crate::db::insert_session_members(conn, session_id, &leftover)?;
        nights += 1;
        sessions += 1;
    }

    tracing::info!(
        set_id = frames_set_id,
        frames = known.len(),
        nights,
        sessions,
        "nights re-derived"
    );
    Ok(RederiveSummary { frames: known.len(), nights, sessions })
}

/// Detected imaging night structure
pub struct DetectedNight {
    pub start_time: String,
    pub end_time: String,
    pub sessions: Vec<DetectedSession>,
}

/// Detected session structure
pub struct DetectedSession {
    pub instrume: String,
    pub frame_ids: Vec<i64>,
    pub total_exp_time: Option<f64>,
}

/// Frame with parsed timestamp for sorting and gap detection
struct FrameWithTime {
    #[allow(dead_code)]
    file_id: i64,
    frame_id: i64,
    frame: Frame,
    timestamp: DateTime<Utc>,
}

/// Detect imaging nights and sessions from frames
///
/// Algorithm:
/// 1. Filter frames that have date_obs
/// 2. Sort all frames by date_obs
/// 3. Detect night boundaries using gap threshold
/// 4. Within each night, group by instrume to create sessions
///
/// # Arguments
/// * `frames` - List of (file_id, file, frame) tuples
/// * `gap_threshold_hours` - Time gap to detect new night
pub fn detect_sessions(
    frames: Vec<(i64, File, Frame)>,
    gap_threshold_hours: f64,
) -> Result<Vec<DetectedNight>> {
    // Filter frames with valid timestamps
    let total_frames = frames.len();
    let mut frames_with_time: Vec<FrameWithTime> = frames
        .into_iter()
        .filter_map(|(file_id, _file, frame)| {
            let frame_id = frame.id?;

            if frame.date_obs.is_none() {
                tracing::debug!(frame_id, "frame has no date_obs, excluded from session detection");
                return None;
            }

            let timestamp = frame.date_obs?;

            Some(FrameWithTime {
                file_id,
                frame_id,
                frame,
                timestamp,
            })
        })
        .collect();

    tracing::info!(
        count = frames_with_time.len(),
        total = total_frames,
        "filtered frames with valid timestamps"
    );

    if frames_with_time.is_empty() {
        tracing::warn!("no frames with valid date_obs found, returning no sessions");
        return Ok(Vec::new());
    }

    // Sort by timestamp
    frames_with_time.sort_by_key(|f| f.timestamp);

    // Detect night boundaries using gap threshold
    let gap_duration = Duration::hours(gap_threshold_hours as i64);
    let mut nights: Vec<Vec<FrameWithTime>> = Vec::new();
    let mut current_night: Vec<FrameWithTime> = Vec::new();

    for (i, frame) in frames_with_time.into_iter().enumerate() {
        if i == 0 {
            current_night.push(frame);
        } else {
            let last_frame = current_night.last().unwrap();
            let time_diff = frame.timestamp - last_frame.timestamp;

            if time_diff > gap_duration {
                // Start new night
                if !current_night.is_empty() {
                    nights.push(current_night);
                }
                current_night = vec![frame];
            } else {
                current_night.push(frame);
            }
        }
    }

    // Don't forget the last night
    if !current_night.is_empty() {
        nights.push(current_night);
    }

    // Process each night
    let mut detected_nights = Vec::new();

    for night_frames in nights {
        if night_frames.is_empty() {
            continue;
        }

        let start_time = night_frames.first().unwrap().timestamp;
        let end_time = night_frames.last().unwrap().timestamp;

        // Group by instrume within this night
        let mut instrume_groups: HashMap<String, Vec<&FrameWithTime>> = HashMap::new();

        for frame in &night_frames {
            let instrume = frame.frame.instrume.clone().unwrap_or_else(|| "Unknown".to_string());
            instrume_groups.entry(instrume).or_default().push(frame);
        }

        // Create sessions
        let mut sessions = Vec::new();

        for (instrume, frames) in instrume_groups {
            let frame_ids: Vec<i64> = frames.iter().map(|f| f.frame_id).collect();

            // Calculate total exposure time
            let total_exp_time: Option<f64> = {
                let exptimes: Vec<f64> = frames
                    .iter()
                    .filter_map(|f| f.frame.exptime)
                    .collect();

                if exptimes.is_empty() {
                    None
                } else {
                    Some(exptimes.iter().sum())
                }
            };

            sessions.push(DetectedSession {
                instrume,
                frame_ids,
                total_exp_time,
            });
        }

        // Sort sessions by instrume name for consistency
        sessions.sort_by(|a, b| a.instrume.cmp(&b.instrume));

        detected_nights.push(DetectedNight {
            start_time: start_time.to_rfc3339(),
            end_time: end_time.to_rfc3339(),
            sessions,
        });
    }

    Ok(detected_nights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{File, Frame, FileFormat};

    fn create_test_frame(
        id: i64,
        date_obs: &str,
        instrume: &str,
        exptime: f64,
    ) -> (i64, File, Frame) {
        let file = File {
            id: Some(id),
            path: format!("/test/{}.fits", id),
            filename: format!("test_{}.fits", id),
            size: 1000,
            modified_at: DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            format: FileFormat::FITS,
            created_at: DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        };

        let frame = Frame {
            id: Some(id),
            file_id: id,
            object: Some("M31".to_string()),
            date_obs: Some(DateTime::parse_from_rfc3339(date_obs)
                .unwrap()
                .with_timezone(&Utc)),
            telescop: None,
            instrume: Some(instrume.to_string()),
            exptime: Some(exptime),
            filter: None,
            imagetyp: None,
            is_master: false,
            gain: None,
            offset: None,
            binning: None,
            xbinning: None,
            ybinning: None,
            ccd_temp: None,
            set_temp: None,
            focallen: None,
            xpixsz: None,
            ypixsz: None,
            naxis1: None,
            naxis2: None,
            ra: None,
            dec: None,
            sitelat: None,
            lat_obs: None,
            sitelong: None,
            long_obs: None,
            objctra: None,
            objctdec: None,
            override_: false,
            swcreate: None,
            bayerpat: None,
            xbayroff: None,
            ybayroff: None,
            roworder: None,
            rotation: None,
            uuid: None,
            updated_at: None,
        };

        (id, file, frame)
    }

    #[test]
    fn test_single_night_single_camera() {
        let frames = vec![
            create_test_frame(1, "2024-01-15T19:30:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(2, "2024-01-15T19:40:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(3, "2024-01-16T02:00:00Z", "ZWO ASI2600MM", 300.0),
        ];

        let nights = detect_sessions(frames, 7.0).unwrap();

        assert_eq!(nights.len(), 1);
        assert_eq!(nights[0].sessions.len(), 1);
        assert_eq!(nights[0].sessions[0].frame_ids.len(), 3);
        assert_eq!(nights[0].sessions[0].instrume, "ZWO ASI2600MM");
        assert_eq!(nights[0].sessions[0].total_exp_time, Some(900.0));
    }

    #[test]
    fn test_single_night_multiple_cameras() {
        let frames = vec![
            create_test_frame(1, "2024-01-15T19:30:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(2, "2024-01-15T19:35:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(3, "2024-01-15T19:40:00Z", "Canon EOS Ra", 180.0),
            create_test_frame(4, "2024-01-16T02:00:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(5, "2024-01-16T02:05:00Z", "Canon EOS Ra", 180.0),
        ];

        let nights = detect_sessions(frames, 7.0).unwrap();

        assert_eq!(nights.len(), 1);
        assert_eq!(nights[0].sessions.len(), 2);

        // Sessions are sorted by instrume name
        let canon_session = nights[0].sessions.iter().find(|s| s.instrume == "Canon EOS Ra").unwrap();
        let zwo_session = nights[0].sessions.iter().find(|s| s.instrume == "ZWO ASI2600MM").unwrap();

        assert_eq!(canon_session.frame_ids.len(), 2);
        assert_eq!(canon_session.total_exp_time, Some(360.0));

        assert_eq!(zwo_session.frame_ids.len(), 3);
        assert_eq!(zwo_session.total_exp_time, Some(900.0));
    }

    #[test]
    fn test_multiple_nights() {
        let frames = vec![
            // Night 1
            create_test_frame(1, "2024-01-15T19:30:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(2, "2024-01-16T02:00:00Z", "ZWO ASI2600MM", 300.0),
            // 8 hour gap
            // Night 2
            create_test_frame(3, "2024-01-16T18:00:00Z", "ZWO ASI2600MM", 300.0),
            create_test_frame(4, "2024-01-16T22:00:00Z", "ZWO ASI2600MM", 300.0),
        ];

        let nights = detect_sessions(frames, 7.0).unwrap();

        assert_eq!(nights.len(), 2);
        assert_eq!(nights[0].sessions[0].frame_ids.len(), 2);
        assert_eq!(nights[1].sessions[0].frame_ids.len(), 2);
    }
}

#[cfg(test)]
mod rederive_tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::{params, Connection};

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_db(&c).unwrap();
        c
    }

    fn light(conn: &Connection, id: i64, date_obs: Option<&str>, instrume: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-01-01T00:00:00Z', 'FITS')",
            params![id, format!("/t/{id}.fits"), format!("{id}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, date_obs)
             VALUES (?1, ?1, 'Light', ?2, ?3)",
            params![id, instrume, date_obs],
        )
        .unwrap();
    }

    /// A night row with one session holding `frame_ids` — the rows as a
    /// merge used to stitch them.
    fn night(conn: &Connection, set_id: i64, start: &str, end: &str, instrume: &str, ids: &[i64]) {
        let night_id = crate::db::create_imaging_night(conn, set_id, start, end).unwrap();
        let session_id =
            crate::db::create_session(conn, night_id, instrume, ids.len() as i32, None).unwrap();
        crate::db::insert_session_members(conn, session_id, ids).unwrap();
    }

    /// `(start, end, member count)` per night row, by start.
    fn night_rows(conn: &Connection, set_id: i64) -> Vec<(String, String, i64)> {
        let mut st = conn
            .prepare(
                "SELECT n.start_time, n.end_time,
                        (SELECT COUNT(*) FROM sessions s JOIN session_members m ON m.session_id = s.id
                          WHERE s.imaging_night_id = n.id)
                 FROM imaging_nights n WHERE n.frames_set_id = ?1 ORDER BY n.start_time",
            )
            .unwrap();
        st.query_map([set_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// Two rows for one continuous night — the shape a merge used to leave
    /// behind — come back as one night holding every frame.
    #[test]
    fn rederive_folds_stitched_rows_into_one_night() {
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'LDN 1272')", []).unwrap();
        light(&conn, 10, Some("2025-09-13T21:55:00Z"), "CamA");
        light(&conn, 11, Some("2025-09-13T23:30:00Z"), "CamA");
        light(&conn, 12, Some("2025-09-13T22:36:00Z"), "CamA");
        light(&conn, 13, Some("2025-09-14T01:59:00Z"), "CamA");
        night(&conn, 1, "2025-09-13T21:55:00Z", "2025-09-13T23:30:00Z", "CamA", &[10, 11]);
        night(&conn, 1, "2025-09-13T22:36:00Z", "2025-09-14T01:59:00Z", "CamA", &[12, 13]);
        assert_eq!(night_rows(&conn, 1).len(), 2);

        let summary = rederive_for_frame_set(&conn, 1, &[], 6.0).unwrap();
        assert_eq!((summary.frames, summary.nights, summary.sessions), (4, 1, 1));
        let rows = night_rows(&conn, 1);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].2, 4);
        assert!(rows[0].0.starts_with("2025-09-13T21:55"), "{rows:?}");
        assert!(rows[0].1.starts_with("2025-09-14T01:59"), "{rows:?}");
    }

    /// A real gap still splits, and frames about to join are counted in.
    #[test]
    fn rederive_keeps_real_gaps_and_takes_extra_frames() {
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'X')", []).unwrap();
        light(&conn, 10, Some("2025-10-17T22:04:00Z"), "CamA");
        light(&conn, 11, Some("2025-10-18T02:25:00Z"), "CamA");
        light(&conn, 12, Some("2025-10-18T17:56:00Z"), "CamA"); // 15.5 h later
        night(&conn, 1, "2025-10-17T22:04:00Z", "2025-10-18T02:25:00Z", "CamA", &[10, 11]);

        let summary = rederive_for_frame_set(&conn, 1, &[12], 6.0).unwrap();
        assert_eq!((summary.frames, summary.nights), (3, 2));
        assert_eq!(night_rows(&conn, 1).iter().map(|r| r.2).sum::<i64>(), 3);
    }

    /// A member without DATE-OBS is never dropped by a recalculation.
    #[test]
    fn rederive_never_loses_a_frame_without_date_obs() {
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'X')", []).unwrap();
        light(&conn, 10, Some("2025-10-17T22:04:00Z"), "CamA");
        light(&conn, 11, None, "CamA");
        night(&conn, 1, "2025-10-17T22:04:00Z", "2025-10-17T23:00:00Z", "CamA", &[10, 11]);

        let summary = rederive_for_frame_set(&conn, 1, &[], 6.0).unwrap();
        assert_eq!(summary.frames, 2);
        assert_eq!(night_rows(&conn, 1).iter().map(|r| r.2).sum::<i64>(), 2);
    }
}
