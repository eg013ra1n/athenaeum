use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::models::{File, Frame};

/// Outcome of [`rederive_for_frame_set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RederiveSummary {
    pub frames: usize,
    pub nights: usize,
    pub sessions: usize,
}

/// Stable span for the fallback night that holds members with no
/// `DATE-OBS` (the gap rule can't place them). Anchored on the set's own
/// dated members — the min/max `date_obs` across *every* dated member of
/// the set, not just the leftover ones — so the span only moves when the
/// set's actual observed time range moves, which is already a real change
/// to the real nights. Previously this was stamped `Utc::now()`, which
/// moved on every single run and made the fallback night look like drift
/// to any comparison (see [`reconcile_for_frame_set`]).
///
/// `frames_set` carries no `created_at` column to fall back on, so a set
/// with *no* dated member at all (nothing to anchor on) collapses onto a
/// fixed epoch sentinel instead — harmless, since such a set never had
/// real activity to anchor on either.
///
/// Truncated to whole seconds before it is returned: the DB round trip
/// stores this night at `SecondsFormat::Secs` precision (unlike a real
/// night's start/end, which keep `DATE-OBS`'s own precision verbatim), so a
/// `DATE-OBS` with a fractional-second component would otherwise look like
/// drift the instant it round-tripped through the database.
fn fallback_night_span(frames: &[(i64, File, Frame)]) -> (DateTime<Utc>, DateTime<Utc>) {
    let mut range: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    for (_, _, frame) in frames {
        if let Some(d) = frame.date_obs {
            range = Some(match range {
                Some((min, max)) => (min.min(d), max.max(d)),
                None => (d, d),
            });
        }
    }
    let (start, end) = range.unwrap_or_else(|| {
        let epoch = DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is representable");
        (epoch, epoch + Duration::hours(1))
    });
    (truncate_to_secs(start), truncate_to_secs(end))
}

fn truncate_to_secs(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(dt.timestamp(), 0).unwrap_or(dt)
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
    let fallback_span = fallback_night_span(&frames);
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
        let (fallback_start, fallback_end) = fallback_span;
        let start = fallback_start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let end = fallback_end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
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

/// Outcome of [`reconcile_for_frame_set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The stored nights/sessions already match what a re-derivation would
    /// produce right now. Nothing was written — every `sessions.uuid` and
    /// `created_at` is untouched.
    Unchanged,
    /// Something differed, so [`rederive_for_frame_set`] ran for real.
    Rewritten(RederiveSummary),
}

/// One night's frame membership, comparable independent of session order
/// or instant formatting (a session's `frame_ids` is a set, and start/end
/// are parsed instants — never the raw RFC3339 text — so a stored
/// `"…:00Z"` and a recomputed `"…:00.000Z"` for the same instant compare
/// equal). `Ord` is derived so a list of nights can be sorted into a
/// canonical order by full content, not just by `start`: the fallback
/// night's span is anchored on *every* dated member of the set, so on a
/// set with exactly one real night it frequently lands on the exact same
/// instant as that night's own start/end — sorting by `start` alone would
/// then leave the tie-break between the two nights up to whatever
/// arbitrary order SQLite happened to return them in, which the freshly
/// recomputed side has no way to match.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ComparableSession {
    instrume: String,
    frame_ids: BTreeSet<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ComparableNight {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    /// Sorted by `instrume` — order-independent, matching how
    /// `rederive_for_frame_set` groups by instrument within a night.
    sessions: Vec<ComparableSession>,
}

fn comparable_sessions(mut sessions: Vec<ComparableSession>) -> Vec<ComparableSession> {
    sessions.sort_by(|a, b| a.instrume.cmp(&b.instrume));
    sessions
}

/// What `rederive_for_frame_set` would write right now, in the same
/// comparable shape as [`stored_nights`] — the detected nights plus the
/// same stable fallback night for undated members, sorted by start
/// instant (not insertion order: the fallback night's own start can sort
/// anywhere once it is anchored on the set's dated range instead of
/// `Utc::now()`).
fn expected_nights(
    frames: Vec<(i64, File, Frame)>,
    gap_threshold_hours: f64,
    fallback_span: (DateTime<Utc>, DateTime<Utc>),
) -> Result<Vec<ComparableNight>> {
    let known: Vec<i64> = frames.iter().filter_map(|(_, _, f)| f.id).collect();
    let detected = detect_sessions(frames, gap_threshold_hours)?;

    let mut placed: HashSet<i64> = HashSet::new();
    let mut nights: Vec<ComparableNight> = Vec::new();
    for night in &detected {
        let start = DateTime::parse_from_rfc3339(&night.start_time)
            .map_err(|e| anyhow!("bad detected start_time {:?}: {e}", night.start_time))?
            .with_timezone(&Utc);
        let end = DateTime::parse_from_rfc3339(&night.end_time)
            .map_err(|e| anyhow!("bad detected end_time {:?}: {e}", night.end_time))?
            .with_timezone(&Utc);
        let sessions = night
            .sessions
            .iter()
            .map(|s| {
                placed.extend(s.frame_ids.iter().copied());
                ComparableSession {
                    instrume: s.instrume.clone(),
                    frame_ids: s.frame_ids.iter().copied().collect(),
                }
            })
            .collect();
        nights.push(ComparableNight { start, end, sessions: comparable_sessions(sessions) });
    }

    let leftover: BTreeSet<i64> = known.into_iter().filter(|id| !placed.contains(id)).collect();
    if !leftover.is_empty() {
        let (start, end) = fallback_span;
        nights.push(ComparableNight {
            start,
            end,
            sessions: vec![ComparableSession { instrume: "Unknown".to_string(), frame_ids: leftover }],
        });
    }

    nights.sort();
    Ok(nights)
}

/// The frame set's currently stored nights/sessions, in the same
/// comparable shape as [`expected_nights`].
fn stored_nights(conn: &Connection, frames_set_id: i64) -> Result<Vec<ComparableNight>> {
    let mut nights = Vec::new();
    for night in crate::db::get_imaging_nights_for_set(conn, frames_set_id)? {
        let night_id = night.id.ok_or_else(|| anyhow!("stored night has no id"))?;
        let start = DateTime::parse_from_rfc3339(&night.start_time)
            .map_err(|e| anyhow!("bad stored start_time on night {night_id}: {e}"))?
            .with_timezone(&Utc);
        let end = DateTime::parse_from_rfc3339(&night.end_time)
            .map_err(|e| anyhow!("bad stored end_time on night {night_id}: {e}"))?
            .with_timezone(&Utc);

        let mut sessions = Vec::new();
        for session in crate::db::get_sessions_for_night(conn, night_id)? {
            let session_id = session.id.ok_or_else(|| anyhow!("stored session has no id"))?;
            let frame_ids = crate::db::get_frame_ids_for_session(conn, session_id)?;
            sessions.push(ComparableSession {
                instrume: session.instrume,
                frame_ids: frame_ids.into_iter().collect(),
            });
        }
        nights.push(ComparableNight { start, end, sessions: comparable_sessions(sessions) });
    }
    nights.sort();
    Ok(nights)
}

/// Cheap, idempotent check that only rewrites a frame set's nights and
/// sessions when they actually differ from what a fresh derivation would
/// produce.
///
/// `rederive_for_frame_set` (the force path behind the toolbar's
/// "Recalculate nights" button) always deletes and re-inserts every night
/// row, which mints a fresh `sessions.uuid` and `created_at` on every
/// session because `sessions` is a UUID table
/// ([`crate::db::schema::UUID_TABLES`]) — correct for an explicit user
/// action, but ruinous if run on every frame-set page open. This computes
/// the same detection the force path would and compares it against the
/// stored rows; only on an actual mismatch does it fall through to
/// [`rederive_for_frame_set`] to write.
pub fn reconcile_for_frame_set(
    conn: &Connection,
    frames_set_id: i64,
    gap_threshold_hours: f64,
) -> Result<ReconcileOutcome> {
    let frame_ids = crate::db::get_frame_ids_for_frame_set(conn, frames_set_id)?;
    let frames = crate::db::get_frames_with_files_by_ids(conn, &frame_ids)?;
    let fallback_span = fallback_night_span(&frames);
    let expected = expected_nights(frames, gap_threshold_hours, fallback_span)?;
    let actual = stored_nights(conn, frames_set_id)?;

    if expected == actual {
        return Ok(ReconcileOutcome::Unchanged);
    }

    let summary = rederive_for_frame_set(conn, frames_set_id, &[], gap_threshold_hours)?;
    Ok(ReconcileOutcome::Rewritten(summary))
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

#[cfg(test)]
mod reconcile_tests {
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

    /// A night row with one session holding `frame_ids`, inserted directly
    /// (not through `rederive_for_frame_set`) — used to plant a stored
    /// state a reconcile should compare against.
    fn night(conn: &Connection, set_id: i64, start: &str, end: &str, instrume: &str, ids: &[i64]) {
        let night_id = crate::db::create_imaging_night(conn, set_id, start, end).unwrap();
        let session_id =
            crate::db::create_session(conn, night_id, instrume, ids.len() as i32, None).unwrap();
        crate::db::insert_session_members(conn, session_id, ids).unwrap();
    }

    /// Every `sessions.uuid` currently stored for a frame set, in a stable
    /// order (`sessions` is a UUID table — see `db::schema::UUID_TABLES` —
    /// so a spurious rewrite would mint fresh ones here).
    fn session_uuids(conn: &Connection, set_id: i64) -> Vec<Option<String>> {
        let mut st = conn
            .prepare(
                "SELECT s.uuid FROM sessions s
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1
                 ORDER BY s.id",
            )
            .unwrap();
        st.query_map([set_id], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    fn night_count(conn: &Connection, set_id: i64) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = ?1",
            [set_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// (a) Reconciling right after a fresh derivation finds nothing to do:
    /// `Unchanged`, and every session's uuid — which a real rewrite would
    /// mint fresh, `sessions` being a UUID table — is untouched.
    #[test]
    fn reconcile_on_a_fresh_derivation_is_unchanged_and_keeps_uuids() {
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'X')", []).unwrap();
        light(&conn, 10, Some("2025-10-17T22:04:00Z"), "CamA");
        light(&conn, 11, Some("2025-10-17T22:14:00Z"), "CamA");
        // `rederive_for_frame_set`'s `extra_frame_ids` is how a frame
        // actually becomes a member — membership lives entirely in
        // `session_members`, so a set with no night rows yet has no known
        // members at all until something puts them there.
        rederive_for_frame_set(&conn, 1, &[10, 11], 6.0).unwrap();

        let before = session_uuids(&conn, 1);
        assert!(!before.is_empty());

        let outcome = reconcile_for_frame_set(&conn, 1, 6.0).unwrap();
        assert_eq!(outcome, ReconcileOutcome::Unchanged);
        assert_eq!(session_uuids(&conn, 1), before);
    }

    /// (b) A frame lands as a member of the set's one existing session
    /// (the kind of membership drift `reconcile_for_frame_set` exists to
    /// catch — e.g. two sets stitched by a merge) with a `DATE-OBS` a real
    /// gap away from the rest. Reconcile must notice and rewrite, landing
    /// on exactly what `detect_sessions` would produce from scratch.
    #[test]
    fn reconcile_rewrites_when_a_member_drifts_into_a_real_gap() {
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'X')", []).unwrap();
        light(&conn, 10, Some("2025-10-17T22:04:00Z"), "CamA");
        light(&conn, 11, Some("2025-10-17T22:14:00Z"), "CamA");
        rederive_for_frame_set(&conn, 1, &[10, 11], 6.0).unwrap();
        assert_eq!(reconcile_for_frame_set(&conn, 1, 6.0).unwrap(), ReconcileOutcome::Unchanged);

        light(&conn, 12, Some("2025-10-18T17:56:00Z"), "CamA"); // 19.7h later
        let session_id: i64 = conn
            .query_row(
                "SELECT s.id FROM sessions s JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, 12)",
            params![session_id],
        )
        .unwrap();

        let outcome = reconcile_for_frame_set(&conn, 1, 6.0).unwrap();
        let summary = match outcome {
            ReconcileOutcome::Rewritten(summary) => summary,
            ReconcileOutcome::Unchanged => panic!("expected the drift to be detected"),
        };
        assert_eq!((summary.frames, summary.nights), (3, 2));
        assert_eq!(night_count(&conn, 1), 2);

        // Idempotent from here: the just-written state matches a fresh
        // derivation exactly.
        assert_eq!(reconcile_for_frame_set(&conn, 1, 6.0).unwrap(), ReconcileOutcome::Unchanged);
    }

    /// (c) A member with no `DATE-OBS` lands on the fallback night. The
    /// stored fallback night here is deliberately stale — the kind of span
    /// the OLD `Utc::now()`-stamped code would have left behind on a
    /// previous run. The first reconcile must notice and rewrite it onto
    /// the stable, dated-member-anchored span; the second reconcile must
    /// then be `Unchanged` — proof the new span does not itself drift
    /// between runs the way `Utc::now()` always did.
    #[test]
    fn reconcile_on_a_dateless_member_is_stable_across_runs() {
        let conn = db();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'X')", []).unwrap();
        light(&conn, 10, Some("2025-10-17T22:04:00Z"), "CamA");
        light(&conn, 11, None, "CamA");
        // A real night for frame 10, matching what `detect_sessions` would
        // produce, plus a fallback night for frame 11 stamped with a stale
        // span unrelated to the set's own dated range.
        night(&conn, 1, "2025-10-17T22:04:00Z", "2025-10-17T22:04:00Z", "CamA", &[10]);
        night(&conn, 1, "1999-01-01T00:00:00Z", "1999-01-01T01:00:00Z", "Unknown", &[11]);

        let first = reconcile_for_frame_set(&conn, 1, 6.0).unwrap();
        assert!(matches!(first, ReconcileOutcome::Rewritten(_)), "{first:?}");

        let second = reconcile_for_frame_set(&conn, 1, 6.0).unwrap();
        assert_eq!(second, ReconcileOutcome::Unchanged);
    }
}
