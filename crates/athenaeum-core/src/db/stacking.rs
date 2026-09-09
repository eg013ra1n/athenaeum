//! Persistence for the in-app stacking pipeline (M1 Plan 5a): one row per
//! stacking run, its per-group results, its per-frame decisions, cross-run
//! cached artifacts (keyed by a config hash so a later run can reuse work),
//! and the per-frame-set persisted configuration (the "Stacking" tab's saved
//! settings + excluded-frame list). Schema: `db::schema::init_db`, spec
//! §9.1 (`docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`).
//!
//! Plain rusqlite, feature-free — every other module in the stacking
//! pipeline (`stacking::*`, `api::stacking`) is gated
//! `all(feature = "render", feature = "solver")`, but this one is not: the
//! tables and this CRUD layer must stay reachable from a headless build.

use anyhow::Result;
use chrono::SecondsFormat;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingRunRow {
    pub id: i64,
    pub frames_set_id: i64,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub config_json: String,
    pub config_hash: String,
    pub reference_frame_id: Option<i64>,
    pub reference_mode: String,
    pub working_dir: String,
    pub output_dir: String,
    pub summary_json: Option<String>,
    pub error: Option<String>,
}

fn row_to_run(r: &Row) -> rusqlite::Result<StackingRunRow> {
    Ok(StackingRunRow {
        id: r.get(0)?,
        frames_set_id: r.get(1)?,
        status: r.get(2)?,
        started_at: r.get(3)?,
        finished_at: r.get(4)?,
        config_json: r.get(5)?,
        config_hash: r.get(6)?,
        reference_frame_id: r.get(7)?,
        reference_mode: r.get(8)?,
        working_dir: r.get(9)?,
        output_dir: r.get(10)?,
        summary_json: r.get(11)?,
        error: r.get(12)?,
    })
}

const RUN_COLUMNS: &str = "id, frames_set_id, status, started_at, finished_at, config_json, \
    config_hash, reference_frame_id, reference_mode, working_dir, output_dir, summary_json, error";

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingRunGroupRow {
    pub id: i64,
    pub run_id: i64,
    pub group_key: String,
    pub instrume: Option<String>,
    pub color_mode: String,
    pub filter: Option<String>,
    pub binning: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub exposure: Option<f64>,
    pub frame_count: i64,
    pub included_count: i64,
    pub master_path: Option<String>,
    pub drizzle_path: Option<String>,
    pub rejection_low_path: Option<String>,
    pub rejection_high_path: Option<String>,
    pub stats_json: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

fn row_to_group(r: &Row) -> rusqlite::Result<StackingRunGroupRow> {
    Ok(StackingRunGroupRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        group_key: r.get(2)?,
        instrume: r.get(3)?,
        color_mode: r.get(4)?,
        filter: r.get(5)?,
        binning: r.get(6)?,
        width: r.get(7)?,
        height: r.get(8)?,
        exposure: r.get(9)?,
        frame_count: r.get(10)?,
        included_count: r.get(11)?,
        master_path: r.get(12)?,
        drizzle_path: r.get(13)?,
        rejection_low_path: r.get(14)?,
        rejection_high_path: r.get(15)?,
        stats_json: r.get(16)?,
        status: r.get(17)?,
        error: r.get(18)?,
    })
}

const GROUP_COLUMNS: &str = "id, run_id, group_key, instrume, color_mode, filter, binning, width, \
    height, exposure, frame_count, included_count, master_path, drizzle_path, rejection_low_path, \
    rejection_high_path, stats_json, status, error";

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingRunFrameRow {
    pub id: i64,
    pub run_id: i64,
    pub group_id: i64,
    pub frame_id: i64,
    pub included: bool,
    pub exclusion_reason: Option<String>,
    pub weight: Option<f64>,
    pub weight_channels_json: Option<String>,
    pub metrics_json: Option<String>,
    pub reg_status: Option<String>,
    pub reg_model: Option<String>,
    pub reg_rms_px: Option<f64>,
    pub reg_inliers: Option<i64>,
    pub reg_inlier_ratio: Option<f64>,
    pub reg_flipped: Option<bool>,
    pub rejected_fraction: Option<f64>,
}

fn row_to_frame(r: &Row) -> rusqlite::Result<StackingRunFrameRow> {
    Ok(StackingRunFrameRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        group_id: r.get(2)?,
        frame_id: r.get(3)?,
        included: r.get(4)?,
        exclusion_reason: r.get(5)?,
        weight: r.get(6)?,
        weight_channels_json: r.get(7)?,
        metrics_json: r.get(8)?,
        reg_status: r.get(9)?,
        reg_model: r.get(10)?,
        reg_rms_px: r.get(11)?,
        reg_inliers: r.get(12)?,
        reg_inlier_ratio: r.get(13)?,
        reg_flipped: r.get(14)?,
        rejected_fraction: r.get(15)?,
    })
}

const FRAME_COLUMNS: &str = "id, run_id, group_id, frame_id, included, exclusion_reason, weight, \
    weight_channels_json, metrics_json, reg_status, reg_model, reg_rms_px, reg_inliers, \
    reg_inlier_ratio, reg_flipped, rejected_fraction";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackingArtifactRow {
    pub id: i64,
    pub frames_set_id: i64,
    pub frame_id: Option<i64>,
    pub group_key: String,
    pub kind: String,
    pub path: Option<String>,
    pub config_hash: String,
    pub size: Option<i64>,
    pub modified_at: Option<String>,
    pub payload_json: Option<String>,
    pub created_at: String,
}

fn row_to_artifact(r: &Row) -> rusqlite::Result<StackingArtifactRow> {
    Ok(StackingArtifactRow {
        id: r.get(0)?,
        frames_set_id: r.get(1)?,
        frame_id: r.get(2)?,
        group_key: r.get(3)?,
        kind: r.get(4)?,
        path: r.get(5)?,
        config_hash: r.get(6)?,
        size: r.get(7)?,
        modified_at: r.get(8)?,
        payload_json: r.get(9)?,
        created_at: r.get(10)?,
    })
}

const ARTIFACT_COLUMNS: &str = "id, frames_set_id, frame_id, group_key, kind, path, config_hash, \
    size, modified_at, payload_json, created_at";

pub struct NewArtifact<'a> {
    pub frames_set_id: i64,
    pub frame_id: Option<i64>,
    pub group_key: &'a str,
    pub kind: &'a str,
    pub path: Option<&'a str>,
    pub config_hash: &'a str,
    pub size: Option<i64>,
    pub modified_at: Option<&'a str>,
    pub payload_json: Option<&'a str>,
}

pub struct NewRun<'a> {
    pub frames_set_id: i64,
    pub config_json: &'a str,
    pub config_hash: &'a str,
    pub reference_frame_id: Option<i64>,
    pub reference_mode: &'a str,
    pub working_dir: &'a str,
    pub output_dir: &'a str,
}

pub struct NewGroup<'a> {
    pub run_id: i64,
    pub group_key: &'a str,
    pub instrume: Option<&'a str>,
    pub color_mode: &'a str,
    pub filter: Option<&'a str>,
    pub binning: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub exposure: Option<f64>,
    pub frame_count: i64,
    pub included_count: i64,
}

#[derive(Default)]
pub struct GroupUpdate<'a> {
    pub included_count: Option<i64>,
    pub master_path: Option<&'a str>,
    pub rejection_low_path: Option<&'a str>,
    pub rejection_high_path: Option<&'a str>,
    pub stats_json: Option<&'a str>,
    pub status: Option<&'a str>,
    pub error: Option<&'a str>,
}

pub struct NewFrameRow<'a> {
    pub run_id: i64,
    pub group_id: i64,
    pub frame_id: i64,
    pub included: bool,
    pub exclusion_reason: Option<&'a str>,
    pub weight: Option<f64>,
    pub weight_channels_json: Option<&'a str>,
    pub metrics_json: Option<&'a str>,
    pub reg_status: Option<&'a str>,
    pub reg_model: Option<&'a str>,
    pub reg_rms_px: Option<f64>,
    pub reg_inliers: Option<i64>,
    pub reg_inlier_ratio: Option<f64>,
    pub reg_flipped: Option<bool>,
    pub rejected_fraction: Option<f64>,
}

pub struct SetConfigRow {
    pub config_json: String,
    pub excluded_frame_ids: Vec<i64>,
    pub updated_at: String,
}

// ---------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------

/// Insert a new run. Status starts `'planning'`, `started_at` is now (RFC
/// 3339, UTC, millisecond precision).
pub fn insert_run(conn: &Connection, run: &NewRun<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO stacking_runs
         (frames_set_id, status, started_at, config_json, config_hash, reference_frame_id,
          reference_mode, working_dir, output_dir)
         VALUES (?1, 'planning', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            run.frames_set_id,
            now(),
            run.config_json,
            run.config_hash,
            run.reference_frame_id,
            run.reference_mode,
            run.working_dir,
            run.output_dir,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn set_run_status(conn: &Connection, run_id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE stacking_runs SET status = ?1 WHERE id = ?2",
        params![status, run_id],
    )?;
    Ok(())
}

pub fn set_run_reference(
    conn: &Connection,
    run_id: i64,
    reference_frame_id: i64,
    reference_mode: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE stacking_runs SET reference_frame_id = ?1, reference_mode = ?2 WHERE id = ?3",
        params![reference_frame_id, reference_mode, run_id],
    )?;
    Ok(())
}

/// Mark a run finished: sets `status`, `finished_at` (now), and optionally
/// `summary_json`/`error`.
pub fn finish_run(
    conn: &Connection,
    run_id: i64,
    status: &str,
    summary_json: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE stacking_runs SET status = ?1, finished_at = ?2, summary_json = ?3, error = ?4
         WHERE id = ?5",
        params![status, now(), summary_json, error, run_id],
    )?;
    Ok(())
}

pub fn get_run(conn: &Connection, run_id: i64) -> Result<Option<StackingRunRow>> {
    let sql = format!("SELECT {RUN_COLUMNS} FROM stacking_runs WHERE id = ?1");
    Ok(conn
        .query_row(&sql, params![run_id], row_to_run)
        .optional()?)
}

/// Runs for a frame set, newest first.
pub fn list_runs(
    conn: &Connection,
    frames_set_id: i64,
    limit: usize,
) -> Result<Vec<StackingRunRow>> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM stacking_runs WHERE frames_set_id = ?1 \
         ORDER BY started_at DESC, id DESC LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![frames_set_id, limit as i64], row_to_run)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The id of the frame set's in-flight run (`status IN ('planning',
/// 'running')`), if any. A frame set has at most one active run by
/// construction (the orchestration layer's gate, not enforced here).
pub fn active_run_for_set(conn: &Connection, frames_set_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM stacking_runs
             WHERE frames_set_id = ?1 AND status IN ('planning', 'running')
             ORDER BY id DESC LIMIT 1",
            params![frames_set_id],
            |r| r.get(0),
        )
        .optional()?)
}

// ---------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------

/// Insert a new group row. Status starts `'pending'`.
pub fn insert_group(conn: &Connection, g: &NewGroup<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO stacking_run_groups
         (run_id, group_key, instrume, color_mode, filter, binning, width, height, exposure,
          frame_count, included_count, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending')",
        params![
            g.run_id,
            g.group_key,
            g.instrume,
            g.color_mode,
            g.filter,
            g.binning,
            g.width,
            g.height,
            g.exposure,
            g.frame_count,
            g.included_count,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Apply only the `Some` fields of `u` to the group row.
pub fn update_group(conn: &Connection, group_id: i64, u: &GroupUpdate<'_>) -> Result<()> {
    let mut sets: Vec<String> = Vec::new();
    let mut vals: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    macro_rules! set_field {
        ($col:literal, $val:expr) => {
            if let Some(v) = $val {
                sets.push(format!("{} = ?{}", $col, vals.len() + 1));
                vals.push(Box::new(v));
            }
        };
    }
    set_field!("included_count", u.included_count);
    set_field!("master_path", u.master_path);
    set_field!("rejection_low_path", u.rejection_low_path);
    set_field!("rejection_high_path", u.rejection_high_path);
    set_field!("stats_json", u.stats_json);
    set_field!("status", u.status);
    set_field!("error", u.error);

    if sets.is_empty() {
        return Ok(());
    }

    let sql = format!(
        "UPDATE stacking_run_groups SET {} WHERE id = ?{}",
        sets.join(", "),
        vals.len() + 1
    );
    vals.push(Box::new(group_id));
    let params: Vec<&dyn rusqlite::ToSql> = vals.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, params.as_slice())?;
    Ok(())
}

pub fn list_groups(conn: &Connection, run_id: i64) -> Result<Vec<StackingRunGroupRow>> {
    let sql =
        format!("SELECT {GROUP_COLUMNS} FROM stacking_run_groups WHERE run_id = ?1 ORDER BY id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![run_id], row_to_group)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------

/// Insert or replace the per-frame decision for `(run_id, frame_id)` —
/// registration/weighting is re-run per stage, and each stage's result
/// simply overwrites the previous one for that frame within the run.
pub fn upsert_frame_row(conn: &Connection, f: &NewFrameRow<'_>) -> Result<i64> {
    conn.query_row(
        "INSERT INTO stacking_run_frames
         (run_id, group_id, frame_id, included, exclusion_reason, weight, weight_channels_json,
          metrics_json, reg_status, reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio,
          reg_flipped, rejected_fraction)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(run_id, frame_id) DO UPDATE SET
           group_id = excluded.group_id,
           included = excluded.included,
           exclusion_reason = excluded.exclusion_reason,
           weight = excluded.weight,
           weight_channels_json = excluded.weight_channels_json,
           metrics_json = excluded.metrics_json,
           reg_status = excluded.reg_status,
           reg_model = excluded.reg_model,
           reg_rms_px = excluded.reg_rms_px,
           reg_inliers = excluded.reg_inliers,
           reg_inlier_ratio = excluded.reg_inlier_ratio,
           reg_flipped = excluded.reg_flipped,
           rejected_fraction = excluded.rejected_fraction
         RETURNING id",
        params![
            f.run_id,
            f.group_id,
            f.frame_id,
            f.included,
            f.exclusion_reason,
            f.weight,
            f.weight_channels_json,
            f.metrics_json,
            f.reg_status,
            f.reg_model,
            f.reg_rms_px,
            f.reg_inliers,
            f.reg_inlier_ratio,
            f.reg_flipped,
            f.rejected_fraction,
        ],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Set just `rejected_fraction` on an existing `(run_id, frame_id)` row —
/// the Output stage (Plan 5a Task 8) calls this once per frame
/// `integrate_group` actually combined, after the row already exists from
/// [`upsert_frame_row`] (stage 5), without re-deriving every other column a
/// full re-upsert would otherwise require repeating. A no-op (no error) when
/// the row does not exist — the caller (a frame `integrate_group` combined)
/// always already has one, but this is deliberately not an assumption this
/// function enforces.
pub fn set_frame_rejected_fraction(
    conn: &Connection,
    run_id: i64,
    frame_id: i64,
    fraction: f64,
) -> Result<()> {
    conn.execute(
        "UPDATE stacking_run_frames SET rejected_fraction = ?1 WHERE run_id = ?2 AND frame_id = ?3",
        params![fraction, run_id, frame_id],
    )?;
    Ok(())
}

pub fn list_frame_rows(conn: &Connection, run_id: i64) -> Result<Vec<StackingRunFrameRow>> {
    let sql =
        format!("SELECT {FRAME_COLUMNS} FROM stacking_run_frames WHERE run_id = ?1 ORDER BY id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![run_id], row_to_frame)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------

/// Insert or refresh a cached artifact, keyed on `(frames_set_id, group_key,
/// kind, COALESCE(frame_id, 0))` — the expression unique index means a NULL
/// `frame_id` collapses onto the same row (a set-level artifact) rather than
/// creating a new one on every rebuild.
pub fn upsert_artifact(conn: &Connection, a: &NewArtifact<'_>) -> Result<i64> {
    let created_at = now();
    conn.query_row(
        "INSERT INTO stacking_artifacts
         (frames_set_id, frame_id, group_key, kind, path, config_hash, size, modified_at,
          payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(frames_set_id, group_key, kind, COALESCE(frame_id, 0)) DO UPDATE SET
           path = excluded.path,
           config_hash = excluded.config_hash,
           size = excluded.size,
           modified_at = excluded.modified_at,
           payload_json = excluded.payload_json,
           created_at = excluded.created_at
         RETURNING id",
        params![
            a.frames_set_id,
            a.frame_id,
            a.group_key,
            a.kind,
            a.path,
            a.config_hash,
            a.size,
            a.modified_at,
            a.payload_json,
            created_at,
        ],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

pub fn find_artifact(
    conn: &Connection,
    frames_set_id: i64,
    group_key: &str,
    kind: &str,
    frame_id: Option<i64>,
) -> Result<Option<StackingArtifactRow>> {
    let sql = format!(
        "SELECT {ARTIFACT_COLUMNS} FROM stacking_artifacts
         WHERE frames_set_id = ?1 AND group_key = ?2 AND kind = ?3 AND frame_id IS ?4"
    );
    Ok(conn
        .query_row(
            &sql,
            params![frames_set_id, group_key, kind, frame_id],
            row_to_artifact,
        )
        .optional()?)
}

pub fn list_artifacts(
    conn: &Connection,
    frames_set_id: i64,
    kind: Option<&str>,
) -> Result<Vec<StackingArtifactRow>> {
    match kind {
        Some(k) => {
            let sql = format!(
                "SELECT {ARTIFACT_COLUMNS} FROM stacking_artifacts \
                 WHERE frames_set_id = ?1 AND kind = ?2 ORDER BY id"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![frames_set_id, k], row_to_artifact)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
        None => {
            let sql = format!(
                "SELECT {ARTIFACT_COLUMNS} FROM stacking_artifacts \
                 WHERE frames_set_id = ?1 ORDER BY id"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![frames_set_id], row_to_artifact)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
    }
}

/// Delete every artifact of the given `kinds` for a frame set (e.g. when a
/// config change invalidates cached masters). Returns the number of rows
/// removed.
pub fn delete_artifacts(conn: &Connection, frames_set_id: i64, kinds: &[&str]) -> Result<usize> {
    if kinds.is_empty() {
        return Ok(0);
    }
    let placeholders: Vec<String> = (0..kinds.len()).map(|i| format!("?{}", i + 2)).collect();
    let sql = format!(
        "DELETE FROM stacking_artifacts WHERE frames_set_id = ?1 AND kind IN ({})",
        placeholders.join(", ")
    );
    let mut vals: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(kinds.len() + 1);
    vals.push(&frames_set_id);
    for k in kinds {
        vals.push(k);
    }
    let n = conn.execute(&sql, vals.as_slice())?;
    Ok(n)
}

/// Delete every artifact row for a frame set, regardless of `kind` — unlike
/// [`delete_artifacts`], which only removes the kinds it is given.
/// `stacking_artifacts.kind` is free-form TEXT, not an enum the schema can
/// enumerate, so a caller clearing "everything" (`CleanupWhat::All`) must not
/// name a fixed kind list: a future stage adding a new kind this crate
/// doesn't know about yet would otherwise survive `All` and keep pointing at
/// files that cleanup just deleted. Returns the number of rows removed.
pub fn delete_all_artifacts(conn: &Connection, frames_set_id: i64) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM stacking_artifacts WHERE frames_set_id = ?1",
        params![frames_set_id],
    )?;
    Ok(n)
}

// ---------------------------------------------------------------------
// Per-frame-set persisted configuration
// ---------------------------------------------------------------------

pub fn get_set_config(conn: &Connection, frames_set_id: i64) -> Result<Option<SetConfigRow>> {
    let row = conn
        .query_row(
            "SELECT config_json, excluded_frame_ids_json, updated_at FROM stacking_set_config
             WHERE frames_set_id = ?1",
            params![frames_set_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((config_json, excluded_json, updated_at)) => {
            let excluded_frame_ids: Vec<i64> = serde_json::from_str(&excluded_json)?;
            Ok(Some(SetConfigRow {
                config_json,
                excluded_frame_ids,
                updated_at,
            }))
        }
    }
}

pub fn set_set_config(
    conn: &Connection,
    frames_set_id: i64,
    config_json: &str,
    excluded_frame_ids: &[i64],
) -> Result<()> {
    let excluded_json = serde_json::to_string(excluded_frame_ids)?;
    conn.execute(
        "INSERT INTO stacking_set_config (frames_set_id, config_json, excluded_frame_ids_json, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(frames_set_id) DO UPDATE SET
           config_json = excluded.config_json,
           excluded_frame_ids_json = excluded.excluded_frame_ids_json,
           updated_at = excluded.updated_at",
        params![frames_set_id, config_json, excluded_json, now()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_db(&c).unwrap();
        c
    }

    /// `frames_set` has no NOT NULL column besides the `id` primary key
    /// (`is_custom` is `NOT NULL DEFAULT 0`, so it needs no explicit value)
    /// and, notably, no `created_at` column at all — the brief's seed SQL
    /// named one that doesn't exist in this schema.
    fn seed_set(c: &Connection) -> i64 {
        c.execute(
            "INSERT INTO frames_set (id, name) VALUES (7, 'LDN 1272')",
            [],
        )
        .unwrap();
        7
    }

    /// `stacking_run_frames.frame_id` and `stacking_artifacts.frame_id` are
    /// real FKs to `frames(id)`, itself a real FK to `files(id)` — and this
    /// codebase's bundled SQLite enforces foreign keys by default even on a
    /// bare `Connection::open_in_memory()` (no explicit PRAGMA needed). The
    /// brief's tests reference frame ids without seeding them; do that here.
    fn seed_frame(c: &Connection, frame_id: i64) {
        c.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, 'f.fits', 100, '2026-09-09T00:00:00Z', 'FITS')",
            params![frame_id, format!("/f/{frame_id}.fits")],
        )
        .unwrap();
        c.execute(
            "INSERT INTO frames (id, file_id) VALUES (?1, ?1)",
            params![frame_id],
        )
        .unwrap();
    }

    #[test]
    fn init_db_twice_keeps_the_stacking_tables() {
        let c = conn();
        init_db(&c).unwrap();
        for t in [
            "stacking_runs",
            "stacking_run_groups",
            "stacking_run_frames",
            "stacking_artifacts",
            "stacking_set_config",
        ] {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{t}");
        }
    }

    #[test]
    fn run_group_frame_round_trip() {
        let c = conn();
        let set = seed_set(&c);
        seed_frame(&c, 101);
        let run = insert_run(
            &c,
            &NewRun {
                frames_set_id: set,
                config_json: "{}",
                config_hash: "abc",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();
        assert_eq!(active_run_for_set(&c, set).unwrap(), Some(run));
        let g = insert_group(
            &c,
            &NewGroup {
                run_id: run,
                group_key: "cam__mono__NoFilter__bin1__10x10",
                instrume: Some("cam"),
                color_mode: "mono",
                filter: None,
                binning: Some(1),
                width: Some(10),
                height: Some(10),
                exposure: None,
                frame_count: 3,
                included_count: 3,
            },
        )
        .unwrap();
        upsert_frame_row(
            &c,
            &NewFrameRow {
                run_id: run,
                group_id: g,
                frame_id: 101,
                included: true,
                exclusion_reason: None,
                weight: Some(0.5),
                weight_channels_json: Some("[0.5]"),
                metrics_json: None,
                reg_status: None,
                reg_model: None,
                reg_rms_px: None,
                reg_inliers: None,
                reg_inlier_ratio: None,
                reg_flipped: None,
                rejected_fraction: None,
            },
        )
        .unwrap();
        upsert_frame_row(
            &c,
            &NewFrameRow {
                run_id: run,
                group_id: g,
                frame_id: 101,
                included: false,
                exclusion_reason: Some("registration failed: x"),
                weight: Some(0.5),
                weight_channels_json: None,
                metrics_json: None,
                reg_status: Some("failed"),
                reg_model: None,
                reg_rms_px: None,
                reg_inliers: None,
                reg_inlier_ratio: None,
                reg_flipped: None,
                rejected_fraction: None,
            },
        )
        .unwrap();
        let rows = list_frame_rows(&c, run).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].included);
        assert_eq!(rows[0].reg_status.as_deref(), Some("failed"));
        assert!(
            rows[0].weight_channels_json.is_none(),
            "DO UPDATE must clear a column that became NULL"
        );
        update_group(
            &c,
            g,
            &GroupUpdate {
                master_path: Some("/o/m.fits"),
                status: Some("done"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            list_groups(&c, run).unwrap()[0].master_path.as_deref(),
            Some("/o/m.fits")
        );
        finish_run(&c, run, "done", Some("{}"), None).unwrap();
        let r = get_run(&c, run).unwrap().unwrap();
        assert_eq!(r.status, "done");
        assert!(r.finished_at.is_some());
        assert_eq!(active_run_for_set(&c, set).unwrap(), None);
        assert_eq!(list_runs(&c, set, 10).unwrap().len(), 1);
    }

    #[test]
    fn artifact_key_treats_null_frame_as_one_row() {
        let c = conn();
        let set = seed_set(&c);
        seed_frame(&c, 5);
        let a = NewArtifact {
            frames_set_id: set,
            frame_id: None,
            group_key: "g",
            kind: "ln_reference",
            path: Some("/w/a"),
            config_hash: "h1",
            size: Some(1),
            modified_at: None,
            payload_json: None,
        };
        let id1 = upsert_artifact(&c, &a).unwrap();
        let id2 = upsert_artifact(
            &c,
            &NewArtifact {
                config_hash: "h2",
                ..a
            },
        )
        .unwrap();
        assert_eq!(id1, id2, "NULL frame_id must not create a second row");
        assert_eq!(
            find_artifact(&c, set, "g", "ln_reference", None)
                .unwrap()
                .unwrap()
                .config_hash,
            "h2"
        );
        upsert_artifact(
            &c,
            &NewArtifact {
                frame_id: Some(5),
                kind: "calibrated",
                ..a
            },
        )
        .unwrap();
        assert_eq!(list_artifacts(&c, set, None).unwrap().len(), 2);
        assert_eq!(delete_artifacts(&c, set, &["calibrated"]).unwrap(), 1);
        assert_eq!(
            list_artifacts(&c, set, None).unwrap().len(),
            1,
            "only the calibrated row is gone"
        );
        assert_eq!(
            delete_all_artifacts(&c, set).unwrap(),
            1,
            "the remaining ln_reference row goes too, by no kind name at all"
        );
        assert!(list_artifacts(&c, set, None).unwrap().is_empty());
    }

    #[test]
    fn set_config_upserts() {
        let c = conn();
        let set = seed_set(&c);
        assert!(get_set_config(&c, set).unwrap().is_none());
        set_set_config(&c, set, "{\"version\":1}", &[3, 4]).unwrap();
        set_set_config(&c, set, "{\"version\":1}", &[4]).unwrap();
        let row = get_set_config(&c, set).unwrap().unwrap();
        assert_eq!(row.excluded_frame_ids, vec![4]);
    }
}
