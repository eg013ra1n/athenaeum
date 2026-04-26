//! Persistence for the per-frame-set auto-merge audit log.
//!
//! Each row captures one merge operation (button or monitor-triggered):
//! who, when, threshold in effect at the time, and the per-frame breakdown
//! (stored as JSON for cheap schema evolution).

use crate::models::{MergeLogEntry, MergeReport};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// Insert one audit row using the caller's own connection (non-transactional
/// use). Prefer `insert_log_entry_tx` when the caller already holds a tx.
pub fn insert_log_entry(conn: &Connection, report: &MergeReport) -> Result<i64> {
    let occurred_at = Utc::now().to_rfc3339();
    let added_json = serde_json::to_string(&report.frames_added)?;
    let skipped_json = serde_json::to_string(&report.frames_skipped)?;

    conn.execute(
        "INSERT INTO frame_set_merge_log
         (frames_set_id, occurred_at, source, threshold_arcmin,
          frames_added_json, frames_skipped_json, added_count, skipped_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            report.frames_set_id,
            occurred_at,
            report.source,
            report.threshold_arcmin,
            added_json,
            skipped_json,
            report.added_count,
            report.skipped_count,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert using an existing transaction so the audit row is committed atomically
/// with the session-membership changes it describes.
pub fn insert_log_entry_tx(tx: &Transaction, report: &MergeReport) -> Result<i64> {
    let occurred_at = Utc::now().to_rfc3339();
    let added_json = serde_json::to_string(&report.frames_added)?;
    let skipped_json = serde_json::to_string(&report.frames_skipped)?;

    tx.execute(
        "INSERT INTO frame_set_merge_log
         (frames_set_id, occurred_at, source, threshold_arcmin,
          frames_added_json, frames_skipped_json, added_count, skipped_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            report.frames_set_id,
            occurred_at,
            report.source,
            report.threshold_arcmin,
            added_json,
            skipped_json,
            report.added_count,
            report.skipped_count,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Fetch audit log entries for a frame set, newest first. `limit` caps the
/// result; pass `None` to fetch all rows.
pub fn get_log_entries(
    conn: &Connection,
    frames_set_id: i64,
    limit: Option<i64>,
) -> Result<Vec<MergeLogEntry>> {
    let sql = format!(
        "SELECT id, frames_set_id, occurred_at, source, threshold_arcmin,
                frames_added_json, frames_skipped_json, added_count, skipped_count
         FROM frame_set_merge_log
         WHERE frames_set_id = ?1
         ORDER BY occurred_at DESC, id DESC
         {}",
        match limit {
            Some(n) => format!("LIMIT {}", n),
            None => String::new(),
        }
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![frames_set_id], |row| {
        let id: i64 = row.get(0)?;
        let frames_set_id: i64 = row.get(1)?;
        let occurred_at_str: String = row.get(2)?;
        let source: String = row.get(3)?;
        let threshold_arcmin: f64 = row.get(4)?;
        let frames_added_json: String = row.get(5)?;
        let frames_skipped_json: String = row.get(6)?;
        let added_count: i64 = row.get(7)?;
        let skipped_count: i64 = row.get(8)?;
        Ok((
            id,
            frames_set_id,
            occurred_at_str,
            source,
            threshold_arcmin,
            frames_added_json,
            frames_skipped_json,
            added_count,
            skipped_count,
        ))
    })?;

    let mut entries = Vec::new();
    for row in rows {
        let (
            id,
            frames_set_id,
            occurred_at_str,
            source,
            threshold_arcmin,
            added_json,
            skipped_json,
            added_count,
            skipped_count,
        ) = row?;
        let occurred_at: DateTime<Utc> = DateTime::parse_from_rfc3339(&occurred_at_str)
            .map_err(|e| anyhow!("bad occurred_at '{}': {}", occurred_at_str, e))?
            .with_timezone(&Utc);

        entries.push(MergeLogEntry {
            id,
            frames_set_id,
            occurred_at,
            source,
            threshold_arcmin,
            frames_added: serde_json::from_str(&added_json)?,
            frames_skipped: serde_json::from_str(&skipped_json)?,
            added_count,
            skipped_count,
        });
    }
    Ok(entries)
}

/// Count log entries for a frame set. Cheap — used by the UI to decide
/// whether to show the History tab badge.
#[allow(dead_code)]
pub fn count_log_entries(conn: &Connection, frames_set_id: i64) -> Result<i64> {
    let count: Option<i64> = conn
        .query_row(
            "SELECT COUNT(*) FROM frame_set_merge_log WHERE frames_set_id = ?1",
            params![frames_set_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(count.unwrap_or(0))
}
