//! Receive sessions (spec 2026-09-29 §7.1): consecutive landings of one
//! project, a gap over five minutes starts a new one. Written inside the
//! landing transaction; the caller owns the transaction.
//!
//! Today every landed collab frame writes one `sync_history` row with
//! `peer_device = "swarm"`, and at scale those rows crowd personal transfers
//! out of the Transfers history. This module groups them instead: one
//! `collab_receive_sessions` row per burst of landings, holding the frame
//! count, total bytes, failure count and the per-device byte totals that
//! delivered them (the frame-level history row still exists and now names
//! its real top source — see `api::collab_live::landing::record_landing`).
//!
//! Spec deviation (ledgered by the controller): the spec's `retried` column
//! is `failed` here — a provider switch mid-fetch is not observable per
//! frame, but a failed landing is (review focus 3).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// A gap of more than five minutes between landings opens a new session.
pub const SESSION_GAP_SECS: i64 = 300;

/// Only the newest sessions are kept per project — older rows are pruned on
/// every new session so the table can't grow without bound.
pub const SESSIONS_KEPT: i64 = 500;

/// One receive session: a burst of landings (or landing failures) for one
/// project, close enough together in time to be shown as one row.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: i64,
    pub project_id: String,
    pub started_at: String,
    pub finished_at: String,
    pub frames: i64,
    pub bytes: i64,
    pub failed: i64,
    pub sources: BTreeMap<String, i64>,
}

/// Does a landing at `now` continue the session last active at
/// `last_finished` (gap `<= SESSION_GAP_SECS`)? An unparsable timestamp never
/// continues — it starts a fresh session rather than guessing.
pub(crate) fn continues(last_finished: &str, now: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(last_finished),
        chrono::DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(a), Ok(b)) => (b - a).num_seconds() <= SESSION_GAP_SECS,
        _ => {
            tracing::warn!(
                last_finished,
                now,
                "receive session time unparsable; starting a new session"
            );
            false
        }
    }
}

/// Extend the project's current session (when `now` continues it) or open a
/// new one — the shared body of [`record_landing`] and [`record_failure`].
fn bump(
    conn: &Connection,
    project_id: &str,
    now: &str,
    frames: i64,
    bytes: i64,
    failed: i64,
    sources: &[(String, u64)],
) -> Result<()> {
    let last: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT id, finished_at, sources_json FROM collab_receive_sessions \
             WHERE project_id = ?1 ORDER BY id DESC LIMIT 1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .context("read last receive session")?;
    match last.filter(|(_, fin, _)| continues(fin, now)) {
        Some((id, _, json)) => {
            let mut map: BTreeMap<String, i64> = serde_json::from_str(&json).unwrap_or_default();
            for (d, b) in sources {
                *map.entry(d.clone()).or_default() += *b as i64;
            }
            conn.execute(
                "UPDATE collab_receive_sessions SET finished_at = ?2, frames = frames + ?3, \
                 bytes = bytes + ?4, failed = failed + ?5, sources_json = ?6 WHERE id = ?1",
                rusqlite::params![id, now, frames, bytes, failed, serde_json::to_string(&map)?],
            )
            .context("extend receive session")?;
        }
        None => {
            let map: BTreeMap<String, i64> = sources
                .iter()
                .map(|(d, b)| (d.clone(), *b as i64))
                .collect();
            conn.execute(
                "INSERT INTO collab_receive_sessions \
                 (project_id, started_at, finished_at, frames, bytes, failed, sources_json) \
                 VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    project_id,
                    now,
                    frames,
                    bytes,
                    failed,
                    serde_json::to_string(&map)?
                ],
            )
            .context("open receive session")?;
            conn.execute(
                "DELETE FROM collab_receive_sessions WHERE project_id = ?1 AND id NOT IN \
                 (SELECT id FROM collab_receive_sessions WHERE project_id = ?1 ORDER BY id DESC LIMIT ?2)",
                rusqlite::params![project_id, SESSIONS_KEPT],
            )
            .context("prune receive sessions")?;
        }
    }
    Ok(())
}

/// Record one landed frame: `bytes` and `sources` join the project's current
/// session (or open a new one). Call inside the landing transaction.
pub fn record_landing(
    conn: &Connection,
    project_id: &str,
    now: &str,
    bytes: i64,
    sources: &[(String, u64)],
) -> Result<()> {
    bump(conn, project_id, now, 1, bytes, 0, sources)
}

/// Record one failed landing: no frame, no bytes, no sources — the review
/// focus 3 rule (a failed landing writes no frames) applies to this table
/// too, only the failure count moves.
pub fn record_failure(conn: &Connection, project_id: &str, now: &str) -> Result<()> {
    bump(conn, project_id, now, 0, 0, 1, &[])
}

/// The newest sessions, optionally restricted to one project.
pub fn list(conn: &Connection, project_id: Option<&str>, limit: i64) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, started_at, finished_at, frames, bytes, failed, sources_json \
         FROM collab_receive_sessions WHERE (?1 IS NULL OR project_id = ?1) \
         ORDER BY finished_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id, limit], |r| {
        let json: String = r.get(7)?;
        Ok(SessionRow {
            id: r.get(0)?,
            project_id: r.get(1)?,
            started_at: r.get(2)?,
            finished_at: r.get(3)?,
            frames: r.get(4)?,
            bytes: r.get(5)?,
            failed: r.get(6)?,
            sources: serde_json::from_str(&json).unwrap_or_default(),
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("list receive sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&c).unwrap();
        c.execute("INSERT INTO collab_projects (project_id, slug, title, data_role, is_coordinator, require_approval, pending_frames, project_status, target_name, target_ra_deg, target_dec_deg, target_radius_deg, membership_version, snapshot_payload_b64, snapshot_signature_b64, members_json, gov_caps_json) VALUES ('p','p','P','send',0,0,0,'active','T',0,0,1,1,'','','[]','[]')", []).unwrap();
        c
    }

    #[test]
    fn landings_within_five_minutes_share_a_session_and_sum_sources() {
        let c = conn();
        record_landing(
            &c,
            "p",
            "2026-09-29T10:00:00.000Z",
            100,
            &[("A=".into(), 70), ("B=".into(), 30)],
        )
        .unwrap();
        record_landing(
            &c,
            "p",
            "2026-09-29T10:04:59.000Z",
            50,
            &[("A=".into(), 50)],
        )
        .unwrap();
        record_landing(
            &c,
            "p",
            "2026-09-29T10:10:00.000Z",
            10,
            &[("B=".into(), 10)],
        )
        .unwrap();
        let s = list(&c, Some("p"), 10).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            (
                s[1].frames,
                s[1].bytes,
                s[1].started_at.as_str(),
                s[1].finished_at.as_str()
            ),
            (
                2,
                150,
                "2026-09-29T10:00:00.000Z",
                "2026-09-29T10:04:59.000Z"
            )
        );
        assert_eq!(
            s[1].sources,
            [("A=".to_string(), 120), ("B=".to_string(), 30)]
                .into_iter()
                .collect()
        );
        assert_eq!((s[0].frames, s[0].bytes), (1, 10));
    }

    #[test]
    fn a_failed_landing_counts_failed_and_writes_no_frames() {
        let c = conn();
        record_failure(&c, "p", "2026-09-29T10:00:00.000Z").unwrap();
        let s = list(&c, Some("p"), 10).unwrap();
        assert_eq!((s[0].frames, s[0].bytes, s[0].failed), (0, 0, 1));
    }

    #[test]
    fn only_the_newest_sessions_are_kept() {
        let c = conn();
        for i in 0..(SESSIONS_KEPT + 3) {
            let t = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap()
                + chrono::Duration::minutes(10 * i);
            record_landing(&c, "p", &t.to_rfc3339(), 1, &[]).unwrap();
        }
        assert_eq!(
            list(&c, Some("p"), 10_000).unwrap().len() as i64,
            SESSIONS_KEPT
        );
    }

    #[test]
    fn an_unparsable_time_starts_a_new_session() {
        assert!(!continues("garbage", "2026-09-29T10:00:00Z"));
        assert!(continues("2026-09-29T10:00:00Z", "2026-09-29T10:05:00Z"));
        assert!(!continues("2026-09-29T10:00:00Z", "2026-09-29T10:05:01Z"));
    }
}
