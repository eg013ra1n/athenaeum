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

/// What a stacking run stacks (wave 6, spec 2026-10-02 §4.3, ruling W8): a
/// catalog frame set, or a collaboration project. One pipeline, two
/// subjects; a run has exactly one. Lives here, in the ungated persistence
/// layer, because `db::stacking` and `services::StackHandle` take it in a
/// headless build (plan 6b-1 B7); re-exported as `stacking::StackSubject`.
///
/// A frame id is always read in the subject's namespace: a catalog
/// `frames.id` for a frame set, a `stack_project_frames.id` handle for a
/// project. The two never meet in one map or one query.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackSubject {
    FrameSet(i64),
    Project(String),
}

impl StackSubject {
    pub fn frame_set_id(&self) -> Option<i64> {
        match self {
            StackSubject::FrameSet(id) => Some(*id),
            StackSubject::Project(_) => None,
        }
    }

    pub fn project_id(&self) -> Option<&str> {
        match self {
            StackSubject::FrameSet(_) => None,
            StackSubject::Project(id) => Some(id),
        }
    }

    /// The `(frame_id, project_frame)` column pair for a frame id of this
    /// subject's namespace.
    fn frame_columns(&self, frame: Option<i64>) -> (Option<i64>, Option<i64>) {
        match self {
            StackSubject::FrameSet(_) => (frame, None),
            StackSubject::Project(_) => (None, frame),
        }
    }

    /// The run-frame column that holds this subject's frame id.
    fn frame_column(&self) -> &'static str {
        match self {
            StackSubject::FrameSet(_) => "frame_id",
            StackSubject::Project(_) => "project_frame",
        }
    }

    fn from_columns(
        frames_set_id: Option<i64>,
        project_id: Option<String>,
    ) -> rusqlite::Result<Self> {
        match (frames_set_id, project_id) {
            (Some(id), None) => Ok(StackSubject::FrameSet(id)),
            (None, Some(id)) => Ok(StackSubject::Project(id)),
            other => Err(rusqlite::Error::InvalidColumnType(
                1,
                format!("stacking subject pair {other:?}"),
                rusqlite::types::Type::Null,
            )),
        }
    }
}

impl std::fmt::Display for StackSubject {
    /// Byte-identical to the pre-6b error texts (`frame set {id}`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackSubject::FrameSet(id) => write!(f, "frame set {id}"),
            StackSubject::Project(id) => write!(f, "project {id}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingRunRow {
    pub id: i64,
    // Exactly one of `frames_set_id` / `project_id` is set (the table's CHECK).
    pub frames_set_id: Option<i64>,
    pub project_id: Option<String>,
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

impl StackingRunRow {
    /// What this run stacks.
    pub fn subject(&self) -> StackSubject {
        match (&self.project_id, self.frames_set_id) {
            (Some(id), _) => StackSubject::Project(id.clone()),
            (None, Some(id)) => StackSubject::FrameSet(id),
            // Unreachable for a row read from the database: the table's
            // CHECK guarantees exactly one of the pair.
            (None, None) => StackSubject::FrameSet(0),
        }
    }
}

fn row_to_run(r: &Row) -> rusqlite::Result<StackingRunRow> {
    Ok(StackingRunRow {
        id: r.get(0)?,
        frames_set_id: r.get(1)?,
        project_id: r.get(2)?,
        status: r.get(3)?,
        started_at: r.get(4)?,
        finished_at: r.get(5)?,
        config_json: r.get(6)?,
        config_hash: r.get(7)?,
        reference_frame_id: r.get(8)?,
        reference_mode: r.get(9)?,
        working_dir: r.get(10)?,
        output_dir: r.get(11)?,
        summary_json: r.get(12)?,
        error: r.get(13)?,
    })
}

const RUN_COLUMNS: &str =
    "id, frames_set_id, project_id, status, started_at, finished_at, config_json, \
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
    /// Owner decision 2026-09-10 (groups are camera-agnostic): the group's
    /// reference-ANCHOR member's own native `NAXIS1`/`NAXIS2`
    /// (`stacking::run::group_anchor_geometry`), recorded once at
    /// `insert_group` time — NOT the run's actual output/reference geometry
    /// (`RunContext::reference_width`/`height`, resolved later, in stage 5
    /// Register). There is no post-Register write-back today (`GroupUpdate`
    /// carries no width/height field), so a group whose reference frame
    /// ends up being a DIFFERENT camera than its anchor member will show
    /// this row's `width`/`height` as that anchor's size, not the actual
    /// master's.
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
    /// Perf tier C Task 1 (ruling C-1/C-1a): the group β Measure resolved
    /// once from its members' own `Auto` picks — `psf_signal::group_beta`,
    /// always one of `psf_signal::AUTO_BETAS`. `None` for a run predating
    /// this task, or a group that measured no frame at all.
    ///
    /// Read by the PLAN GATE since M1 (final fix wave, ruling C-30): the β
    /// is an LN input (stage 6 fits the reference and re-fits every
    /// member's seeds at it), so `stacking::plan::normalization_hash_for`
    /// folds it in, and the gate — which never runs a measurement of its
    /// own — recovers it from this column on the LAST run for the group.
    /// The run itself uses `RunContext::measured_groups`, the in-memory
    /// value stage 3 just resolved; the two must agree, which is what
    /// `local_normalization_sidecars_are_cached_on_the_second_run` pins.
    pub beta: Option<f64>,
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
        beta: r.get(19)?,
    })
}

const GROUP_COLUMNS: &str = "id, run_id, group_key, instrume, color_mode, filter, binning, width, \
    height, exposure, frame_count, included_count, master_path, drizzle_path, rejection_low_path, \
    rejection_high_path, stats_json, status, error, beta";

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingRunFrameRow {
    pub id: i64,
    pub run_id: i64,
    pub group_id: i64,
    // The frame id in the run's namespace (a catalog id or a project handle).
    // Plain comment: a doc comment would leak into the generated TS.
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

const FRAME_COLUMNS: &str =
    "id, run_id, group_id, COALESCE(frame_id, project_frame), included, exclusion_reason, weight, \
    weight_channels_json, metrics_json, reg_status, reg_model, reg_rms_px, reg_inliers, \
    reg_inlier_ratio, reg_flipped, rejected_fraction";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackingArtifactRow {
    pub id: i64,
    pub subject: StackSubject,
    /// The frame id in the subject's namespace (a catalog id or a project
    /// handle); `None` for a group-level artifact.
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
        subject: StackSubject::from_columns(r.get(1)?, r.get(2)?)?,
        frame_id: r.get(3)?,
        group_key: r.get(4)?,
        kind: r.get(5)?,
        path: r.get(6)?,
        config_hash: r.get(7)?,
        size: r.get(8)?,
        modified_at: r.get(9)?,
        payload_json: r.get(10)?,
        created_at: r.get(11)?,
    })
}

const ARTIFACT_COLUMNS: &str = "id, frames_set_id, project_id, COALESCE(frame_id, project_frame), group_key, kind, path, config_hash, \
    size, modified_at, payload_json, created_at";

/// One master light a run WROTE (M4d Task 3, ruling R-M4d-4, spec §9.1):
/// the group's master, its drizzled master, that drizzle's weight map. A
/// master light is cataloged here and nowhere else — it never becomes a
/// `frames` row (the scanner's `CALSTAT` + `ATH_CSRC` skip rule is
/// untouched), so this table is the only record of what a run produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterLight {
    pub id: i64,
    pub subject: StackSubject,
    pub run_id: i64,
    pub group_key: String,
    /// `master | drizzle | weight_map`.
    pub kind: String,
    pub path: String,
    /// `fits | xisf` — the run's own `output.format`.
    pub format: String,
    /// The geometry the writer actually wrote: a drizzled master is
    /// `scale x` the master's, and a weight map matches its drizzle.
    pub width: i64,
    pub height: i64,
    pub channels: i64,
    /// Frames that went INTO this output (the group's included count).
    pub frames: i64,
    /// Sum of the included frames' exposures, or `None` when a member
    /// carries no `EXPTIME` and the total would be a lie.
    pub total_exposure_s: Option<f64>,
    pub created_at: String,
}

fn row_to_master_light(r: &Row) -> rusqlite::Result<MasterLight> {
    Ok(MasterLight {
        id: r.get(0)?,
        subject: StackSubject::from_columns(r.get(1)?, r.get(2)?)?,
        run_id: r.get(3)?,
        group_key: r.get(4)?,
        kind: r.get(5)?,
        path: r.get(6)?,
        format: r.get(7)?,
        width: r.get(8)?,
        height: r.get(9)?,
        channels: r.get(10)?,
        frames: r.get(11)?,
        total_exposure_s: r.get(12)?,
        created_at: r.get(13)?,
    })
}

const MASTER_LIGHT_COLUMNS: &str =
    "id, frames_set_id, project_id, run_id, group_key, kind, path, format, \
    width, height, channels, frames, total_exposure_s, created_at";

pub struct NewArtifact<'a> {
    pub subject: &'a StackSubject,
    pub frame_id: Option<i64>,
    pub group_key: &'a str,
    pub kind: &'a str,
    pub path: Option<&'a str>,
    pub config_hash: &'a str,
    pub size: Option<i64>,
    pub modified_at: Option<&'a str>,
    pub payload_json: Option<&'a str>,
}

pub struct NewMasterLight<'a> {
    pub subject: &'a StackSubject,
    pub run_id: i64,
    pub group_key: &'a str,
    pub kind: &'a str,
    pub path: &'a str,
    pub format: &'a str,
    pub width: i64,
    pub height: i64,
    pub channels: i64,
    pub frames: i64,
    pub total_exposure_s: Option<f64>,
}

pub struct NewRun<'a> {
    pub subject: &'a StackSubject,
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
    /// See [`StackingRunGroupRow::width`]/`height`: the anchor member's
    /// native geometry at plan time, not the run's eventual reference/
    /// output geometry.
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
    /// M3 Task 5: the group's drizzled master, when the run's drizzle stage
    /// wrote one for it — set via its own, later `update_group` call (the
    /// column exists since Plan 5a; drizzle just never had a writer for it
    /// until now). `None` means "leave the column alone" like every other
    /// field here, never "clear it".
    pub drizzle_path: Option<&'a str>,
    pub rejection_low_path: Option<&'a str>,
    pub rejection_high_path: Option<&'a str>,
    pub stats_json: Option<&'a str>,
    pub status: Option<&'a str>,
    pub error: Option<&'a str>,
    /// Perf tier C Task 1: the resolved group β, written once at the end of
    /// stage 3 alongside `included_count` — see [`StackingRunGroupRow::beta`].
    pub beta: Option<f64>,
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
         (frames_set_id, project_id, status, started_at, config_json, config_hash,
          reference_frame_id, reference_mode, working_dir, output_dir)
         VALUES (?1, ?2, 'planning', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            run.subject.frame_set_id(),
            run.subject.project_id(),
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

/// Runs for a subject, newest first.
pub fn list_runs(
    conn: &Connection,
    subject: &StackSubject,
    limit: usize,
) -> Result<Vec<StackingRunRow>> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM stacking_runs \
         WHERE frames_set_id IS ?1 AND project_id IS ?2 \
         ORDER BY started_at DESC, id DESC LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(
            params![subject.frame_set_id(), subject.project_id(), limit as i64],
            row_to_run,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The id of the subject's in-flight run (`status IN ('planning',
/// 'running')`), if any. A subject has at most one active run by
/// construction (the orchestration layer's gate, not enforced here).
pub fn active_run_for_subject(conn: &Connection, subject: &StackSubject) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM stacking_runs
             WHERE frames_set_id IS ?1 AND project_id IS ?2
               AND status IN ('planning', 'running')
             ORDER BY id DESC LIMIT 1",
            params![subject.frame_set_id(), subject.project_id()],
            |r| r.get(0),
        )
        .optional()?)
}

/// Every run row still `planning`/`running` for ANY subject, oldest first
/// — the healing sweep's own input (final fix wave item 1): a row left in
/// one of these two statuses by a killed/crashed process, since neither is
/// ever reached again except by the run thread itself. `(id, subject)`
/// only — the caller needs nothing else to decide whether a live
/// `active_stacks` handle still covers it.
pub fn list_unfinished_runs(conn: &Connection) -> Result<Vec<(i64, StackSubject)>> {
    let mut stmt = conn.prepare(
        "SELECT id, frames_set_id, project_id FROM stacking_runs
         WHERE status IN ('planning', 'running')
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, StackSubject::from_columns(r.get(1)?, r.get(2)?)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
    set_field!("drizzle_path", u.drizzle_path);
    set_field!("rejection_low_path", u.rejection_low_path);
    set_field!("rejection_high_path", u.rejection_high_path);
    set_field!("stats_json", u.stats_json);
    set_field!("status", u.status);
    set_field!("error", u.error);
    set_field!("beta", u.beta);

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

/// Insert or replace the per-frame decision for `(run_id, frame)` —
/// registration/weighting is re-run per stage, and each stage's result
/// simply overwrites the previous one for that frame within the run. The
/// frame id lands in the column of the subject's namespace.
pub fn upsert_frame_row(
    conn: &Connection,
    subject: &StackSubject,
    f: &NewFrameRow<'_>,
) -> Result<i64> {
    let col = subject.frame_column();
    let sql = format!(
        "INSERT INTO stacking_run_frames
         (run_id, group_id, {col}, included, exclusion_reason, weight, weight_channels_json,
          metrics_json, reg_status, reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio,
          reg_flipped, rejected_fraction)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(run_id, {col}) DO UPDATE SET
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
         RETURNING id"
    );
    conn.query_row(
        &sql,
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

/// Set just `rejected_fraction` on an existing `(run_id, frame)` row —
/// the Output stage (Plan 5a Task 8) calls this once per frame
/// `integrate_group` actually combined, after the row already exists from
/// [`upsert_frame_row`] (stage 5), without re-deriving every other column a
/// full re-upsert would otherwise require repeating. A no-op (no error) when
/// the row does not exist — the caller (a frame `integrate_group` combined)
/// always already has one, but this is deliberately not an assumption this
/// function enforces.
pub fn set_frame_rejected_fraction(
    conn: &Connection,
    subject: &StackSubject,
    run_id: i64,
    frame_id: i64,
    fraction: f64,
) -> Result<()> {
    let col = subject.frame_column();
    let sql = format!(
        "UPDATE stacking_run_frames SET rejected_fraction = ?1 WHERE run_id = ?2 AND {col} = ?3"
    );
    conn.execute(&sql, params![fraction, run_id, frame_id])?;
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

/// Insert or refresh a cached artifact, keyed on the six expressions of
/// `stacking_artifacts_subject_key` — `(COALESCE(frames_set_id, 0),
/// COALESCE(project_id, ''), group_key, kind, COALESCE(frame_id, 0),
/// COALESCE(project_frame, 0))` — so a NULL frame collapses onto the same row
/// (a set-level artifact) rather than creating a new one on every rebuild.
pub fn upsert_artifact(conn: &Connection, a: &NewArtifact<'_>) -> Result<i64> {
    let created_at = now();
    let (fid, pf) = a.subject.frame_columns(a.frame_id);
    conn.query_row(
        "INSERT INTO stacking_artifacts
         (frames_set_id, project_id, frame_id, project_frame, group_key, kind, path,
          config_hash, size, modified_at, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(COALESCE(frames_set_id, 0), COALESCE(project_id, ''), group_key, kind,
                     COALESCE(frame_id, 0), COALESCE(project_frame, 0)) DO UPDATE SET
           path = excluded.path,
           config_hash = excluded.config_hash,
           size = excluded.size,
           modified_at = excluded.modified_at,
           payload_json = excluded.payload_json,
           created_at = excluded.created_at
         RETURNING id",
        params![
            a.subject.frame_set_id(),
            a.subject.project_id(),
            fid,
            pf,
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

/// The SQL of [`find_artifact`], written in the unique key's own six
/// expressions so it stays a covering point lookup on
/// `stacking_artifacts_subject_key` (an `IS`-based WHERE would fall back to a
/// scan of every artifact of the subject). `NULL ≡ 0` / `NULL ≡ ''` is the
/// key's own equivalence ([`upsert_artifact`] relies on it); no frame id 0,
/// set id 0 or project id `''` exists.
fn find_artifact_sql() -> String {
    format!(
        "SELECT {ARTIFACT_COLUMNS} FROM stacking_artifacts
         WHERE COALESCE(frames_set_id, 0) = ?1 AND COALESCE(project_id, '') = ?2
           AND group_key = ?3 AND kind = ?4
           AND COALESCE(frame_id, 0) = ?5 AND COALESCE(project_frame, 0) = ?6"
    )
}

pub fn find_artifact(
    conn: &Connection,
    subject: &StackSubject,
    group_key: &str,
    kind: &str,
    frame_id: Option<i64>,
) -> Result<Option<StackingArtifactRow>> {
    let (fid, pf) = subject.frame_columns(frame_id);
    let sql = find_artifact_sql();
    Ok(conn
        .query_row(
            &sql,
            params![
                subject.frame_set_id().unwrap_or(0),
                subject.project_id().unwrap_or(""),
                group_key,
                kind,
                fid.unwrap_or(0),
                pf.unwrap_or(0),
            ],
            row_to_artifact,
        )
        .optional()?)
}

pub fn list_artifacts(
    conn: &Connection,
    subject: &StackSubject,
    kind: Option<&str>,
) -> Result<Vec<StackingArtifactRow>> {
    match kind {
        Some(k) => {
            let sql = format!(
                "SELECT {ARTIFACT_COLUMNS} FROM stacking_artifacts \
                 WHERE frames_set_id IS ?1 AND project_id IS ?2 AND kind = ?3 ORDER BY id"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    params![subject.frame_set_id(), subject.project_id(), k],
                    row_to_artifact,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
        None => {
            let sql = format!(
                "SELECT {ARTIFACT_COLUMNS} FROM stacking_artifacts \
                 WHERE frames_set_id IS ?1 AND project_id IS ?2 ORDER BY id"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    params![subject.frame_set_id(), subject.project_id()],
                    row_to_artifact,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
    }
}

/// The SQL of [`delete_artifacts`] for `kind_count` kinds, on the subject-key
/// prefix (the `IS` form plans on `idx_stacking_artifacts_project` and visits
/// every frame-set artifact of the catalog).
fn delete_artifacts_sql(kind_count: usize) -> String {
    let placeholders: Vec<String> = (0..kind_count).map(|i| format!("?{}", i + 3)).collect();
    format!(
        "DELETE FROM stacking_artifacts
         WHERE COALESCE(frames_set_id, 0) = ?1 AND COALESCE(project_id, '') = ?2
           AND kind IN ({})",
        placeholders.join(", ")
    )
}

/// The SQL of [`delete_all_artifacts`], on the same subject-key prefix.
const DELETE_ALL_ARTIFACTS_SQL: &str = "DELETE FROM stacking_artifacts
     WHERE COALESCE(frames_set_id, 0) = ?1 AND COALESCE(project_id, '') = ?2";

/// Delete every artifact of the given `kinds` for a subject (e.g. when a
/// config change invalidates cached masters). Returns the number of rows
/// removed.
pub fn delete_artifacts(
    conn: &Connection,
    subject: &StackSubject,
    kinds: &[&str],
) -> Result<usize> {
    if kinds.is_empty() {
        return Ok(0);
    }
    let sql = delete_artifacts_sql(kinds.len());
    let set_id = subject.frame_set_id().unwrap_or(0);
    let project_id = subject.project_id().unwrap_or("");
    let mut vals: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(kinds.len() + 2);
    vals.push(&set_id);
    vals.push(&project_id);
    for k in kinds {
        vals.push(k);
    }
    let n = conn.execute(&sql, vals.as_slice())?;
    Ok(n)
}

/// Delete every artifact row for a subject, regardless of `kind` — unlike
/// [`delete_artifacts`], which only removes the kinds it is given.
/// `stacking_artifacts.kind` is free-form TEXT, not an enum the schema can
/// enumerate, so a caller clearing "everything" (`CleanupWhat::All`) must not
/// name a fixed kind list: a future stage adding a new kind this crate
/// doesn't know about yet would otherwise survive `All` and keep pointing at
/// files that cleanup just deleted. Returns the number of rows removed.
pub fn delete_all_artifacts(conn: &Connection, subject: &StackSubject) -> Result<usize> {
    let n = conn.execute(
        DELETE_ALL_ARTIFACTS_SQL,
        params![
            subject.frame_set_id().unwrap_or(0),
            subject.project_id().unwrap_or("")
        ],
    )?;
    Ok(n)
}

// ---------------------------------------------------------------------
// Master lights
// ---------------------------------------------------------------------

/// Record a master light a run wrote, keyed on `UNIQUE(run_id, group_key,
/// kind)` so a stage that writes the same output twice (a re-entered stage
/// 9, a group re-processed after a partial failure) REFRESHES the row
/// instead of growing a second one. Returns the row id — the existing one
/// when the key conflicted.
pub fn insert_master_light(conn: &Connection, m: &NewMasterLight<'_>) -> Result<i64> {
    let created_at = now();
    conn.query_row(
        "INSERT INTO master_lights
         (frames_set_id, project_id, run_id, group_key, kind, path, format, width, height,
          channels, frames, total_exposure_s, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(run_id, group_key, kind) DO UPDATE SET
           frames_set_id = excluded.frames_set_id,
           project_id = excluded.project_id,
           path = excluded.path,
           format = excluded.format,
           width = excluded.width,
           height = excluded.height,
           channels = excluded.channels,
           frames = excluded.frames,
           total_exposure_s = excluded.total_exposure_s,
           created_at = excluded.created_at
         RETURNING id",
        params![
            m.subject.frame_set_id(),
            m.subject.project_id(),
            m.run_id,
            m.group_key,
            m.kind,
            m.path,
            m.format,
            m.width,
            m.height,
            m.channels,
            m.frames,
            m.total_exposure_s,
            created_at,
        ],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Every master light a run wrote, oldest row first.
pub fn list_master_lights(conn: &Connection, run_id: i64) -> Result<Vec<MasterLight>> {
    let sql =
        format!("SELECT {MASTER_LIGHT_COLUMNS} FROM master_lights WHERE run_id = ?1 ORDER BY id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![run_id], row_to_master_light)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The point lookup the preview command resolves a file path with.
pub fn find_master_light(
    conn: &Connection,
    run_id: i64,
    group_key: &str,
    kind: &str,
) -> Result<Option<MasterLight>> {
    let sql = format!(
        "SELECT {MASTER_LIGHT_COLUMNS} FROM master_lights \
         WHERE run_id = ?1 AND group_key = ?2 AND kind = ?3"
    );
    Ok(conn
        .query_row(&sql, params![run_id, group_key, kind], row_to_master_light)
        .optional()?)
}

// ---------------------------------------------------------------------
// Project-frame handles (wave 6b, spec 2026-10-02 §4.1)
// ---------------------------------------------------------------------

/// The stable `i64` handle of each `(project_id, frame_uuid)`, minted on
/// first use and never re-used for another frame: inside the stacking
/// pipeline a project frame is this id, exactly as a catalog frame is its
/// `frames.id`. A new content version keeps the uuid and therefore the
/// handle (its artifacts go stale through the content hash). Handles live
/// as long as their project (`collab_projects` cascade) and are never
/// deleted otherwise.
///
/// One IMMEDIATE transaction (read-then-write, CLAUDE.md): never call this
/// with a transaction already open on `conn`.
pub fn project_frame_handles(
    conn: &Connection,
    project_id: &str,
    frame_uuids: &[&str],
) -> Result<std::collections::HashMap<String, i64>> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut out = std::collections::HashMap::with_capacity(frame_uuids.len());
    {
        let mut insert = tx.prepare_cached(
            "INSERT OR IGNORE INTO stack_project_frames (project_id, frame_uuid) VALUES (?1, ?2)",
        )?;
        let mut select = tx.prepare_cached(
            "SELECT id FROM stack_project_frames WHERE project_id = ?1 AND frame_uuid = ?2",
        )?;
        for uuid in frame_uuids {
            insert.execute(params![project_id, uuid])?;
            let id: i64 = select.query_row(params![project_id, uuid], |r| r.get(0))?;
            out.insert((*uuid).to_string(), id);
        }
    }
    tx.commit()?;
    Ok(out)
}

/// Test helper: a minimal `collab_projects` row (slug and title are the id).
/// Writes every NOT NULL column; the rest take schema defaults.
#[cfg(test)]
pub(crate) fn insert_test_project(conn: &Connection, project_id: &str) {
    conn.execute(
        "INSERT INTO collab_projects (project_id, slug, title, data_role, target_name,
           target_ra_deg, target_dec_deg, target_radius_deg, membership_version,
           snapshot_payload_b64, snapshot_signature_b64, members_json)
         VALUES (?1, ?1, ?1, 'processor', 'M42', 83.8, -5.4, 1.0, 1, '', '', '[]')",
        [project_id],
    )
    .unwrap();
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
                subject: &StackSubject::FrameSet(set),
                config_json: "{}",
                config_hash: "abc",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();
        assert_eq!(
            active_run_for_subject(&c, &StackSubject::FrameSet(set)).unwrap(),
            Some(run)
        );
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
            &StackSubject::FrameSet(set),
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
            &StackSubject::FrameSet(set),
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
        assert_eq!(
            active_run_for_subject(&c, &StackSubject::FrameSet(set)).unwrap(),
            None
        );
        assert_eq!(
            list_runs(&c, &StackSubject::FrameSet(set), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn list_unfinished_runs_reports_only_planning_and_running() {
        let c = conn();
        let set = seed_set(&c);

        let new_run = |status: &str| {
            let run = insert_run(
                &c,
                &NewRun {
                    subject: &StackSubject::FrameSet(set),
                    config_json: "{}",
                    config_hash: "h",
                    reference_frame_id: None,
                    reference_mode: "auto",
                    working_dir: "/w",
                    output_dir: "/o",
                },
            )
            .unwrap();
            if status != "planning" {
                set_run_status(&c, run, status).unwrap();
            }
            run
        };

        let planning_run = new_run("planning");
        let running_run = new_run("running");
        let done_run = new_run("done");
        let failed_run = new_run("failed");

        let unfinished: Vec<i64> = list_unfinished_runs(&c)
            .unwrap()
            .into_iter()
            .map(|(id, subject)| {
                assert_eq!(subject, StackSubject::FrameSet(set));
                id
            })
            .collect();
        assert_eq!(unfinished, vec![planning_run, running_run]);
        assert!(!unfinished.contains(&done_run));
        assert!(!unfinished.contains(&failed_run));
    }

    #[test]
    fn artifact_key_treats_null_frame_as_one_row() {
        let c = conn();
        let set = seed_set(&c);
        seed_frame(&c, 5);
        let a = NewArtifact {
            subject: &StackSubject::FrameSet(set),
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
            find_artifact(&c, &StackSubject::FrameSet(set), "g", "ln_reference", None)
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
        assert_eq!(
            list_artifacts(&c, &StackSubject::FrameSet(set), None)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            delete_artifacts(&c, &StackSubject::FrameSet(set), &["calibrated"]).unwrap(),
            1
        );
        assert_eq!(
            list_artifacts(&c, &StackSubject::FrameSet(set), None)
                .unwrap()
                .len(),
            1,
            "only the calibrated row is gone"
        );
        assert_eq!(
            delete_all_artifacts(&c, &StackSubject::FrameSet(set)).unwrap(),
            1,
            "the remaining ln_reference row goes too, by no kind name at all"
        );
        assert!(list_artifacts(&c, &StackSubject::FrameSet(set), None)
            .unwrap()
            .is_empty());
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

    /// M4d Task 3, brief Step 1: `insert_master_light` then
    /// `list_master_lights(run_id)` round-trips EVERY column, the
    /// `UNIQUE(run_id, group_key, kind)` key upserts rather than
    /// duplicating, and a run delete cascades the rows away.
    #[test]
    fn master_light_round_trip_and_cascade() {
        let c = conn();
        let set = seed_set(&c);
        let run = insert_run(
            &c,
            &NewRun {
                subject: &StackSubject::FrameSet(set),
                config_json: "{}",
                config_hash: "abc",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();

        let id = insert_master_light(
            &c,
            &NewMasterLight {
                subject: &StackSubject::FrameSet(set),
                run_id: run,
                group_key: "mono__NoFilter__bin1__180s",
                kind: "master",
                path: "/o/LDN_1272_mono_180s_12x.fits",
                format: "fits",
                width: 6224,
                height: 4168,
                channels: 1,
                frames: 12,
                total_exposure_s: Some(2160.0),
            },
        )
        .unwrap();
        assert!(id > 0);

        let rows = list_master_lights(&c, run).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.id, id);
        assert_eq!(r.subject, StackSubject::FrameSet(set));
        assert_eq!(r.run_id, run);
        assert_eq!(r.group_key, "mono__NoFilter__bin1__180s");
        assert_eq!(r.kind, "master");
        assert_eq!(r.path, "/o/LDN_1272_mono_180s_12x.fits");
        assert_eq!(r.format, "fits");
        assert_eq!(r.width, 6224);
        assert_eq!(r.height, 4168);
        assert_eq!(r.channels, 1);
        assert_eq!(r.frames, 12);
        assert_eq!(r.total_exposure_s, Some(2160.0));
        assert!(!r.created_at.is_empty());

        // A second kind for the same group is its own row; the same
        // (run, group, kind) is an UPSERT, never a duplicate.
        insert_master_light(
            &c,
            &NewMasterLight {
                kind: "drizzle",
                path: "/o/LDN_1272_mono_180s_12x_drizzle2x.fits",
                width: 12448,
                height: 8336,
                ..NewMasterLight {
                    subject: &StackSubject::FrameSet(set),
                    run_id: run,
                    group_key: "mono__NoFilter__bin1__180s",
                    kind: "master",
                    path: "",
                    format: "fits",
                    width: 0,
                    height: 0,
                    channels: 1,
                    frames: 12,
                    total_exposure_s: Some(2160.0),
                }
            },
        )
        .unwrap();
        let re_id = insert_master_light(
            &c,
            &NewMasterLight {
                subject: &StackSubject::FrameSet(set),
                run_id: run,
                group_key: "mono__NoFilter__bin1__180s",
                kind: "master",
                path: "/o/LDN_1272_mono_180s_12x_2.fits",
                format: "xisf",
                width: 6224,
                height: 4168,
                channels: 1,
                frames: 11,
                total_exposure_s: None,
            },
        )
        .unwrap();
        assert_eq!(re_id, id, "the unique key upserts the same row");
        let rows = list_master_lights(&c, run).unwrap();
        assert_eq!(rows.len(), 2, "master (upserted) + drizzle");
        let master = rows.iter().find(|r| r.kind == "master").unwrap();
        assert_eq!(master.path, "/o/LDN_1272_mono_180s_12x_2.fits");
        assert_eq!(master.format, "xisf");
        assert_eq!(master.frames, 11);
        assert_eq!(master.total_exposure_s, None);

        // The point-lookup the preview command resolves its path with.
        assert_eq!(
            find_master_light(&c, run, "mono__NoFilter__bin1__180s", "drizzle")
                .unwrap()
                .unwrap()
                .width,
            12448
        );
        assert!(find_master_light(&c, run, "nope", "master")
            .unwrap()
            .is_none());

        // Cascade: the rows die with their run.
        c.execute("DELETE FROM stacking_runs WHERE id = ?1", params![run])
            .unwrap();
        assert!(list_master_lights(&c, run).unwrap().is_empty());
    }

    #[test]
    fn project_frame_handles_are_stable_and_never_reused() {
        let conn = conn();
        insert_test_project(&conn, "p1");
        insert_test_project(&conn, "p2");
        let a = project_frame_handles(&conn, "p1", &["u-a", "u-b"]).unwrap();
        let b = project_frame_handles(&conn, "p1", &["u-b", "u-c"]).unwrap();
        assert_eq!(a["u-b"], b["u-b"], "a uuid keeps its handle");
        assert_ne!(b["u-c"], a["u-a"]);
        assert_ne!(b["u-c"], a["u-b"]);
        let other = project_frame_handles(&conn, "p2", &["u-a"]).unwrap();
        assert_ne!(other["u-a"], a["u-a"], "handles are per project");
        conn.execute("DELETE FROM collab_projects WHERE project_id = 'p1'", [])
            .unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stack_project_frames WHERE project_id = 'p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "handles live as long as their project");
        let c = project_frame_handles(&conn, "p2", &[]).unwrap();
        assert!(c.is_empty());
        // AUTOINCREMENT: a handle is never re-used, even the highest one.
        let minted = project_frame_handles(&conn, "p2", &["u-z"]).unwrap()["u-z"];
        conn.execute("DELETE FROM stack_project_frames WHERE id = ?1", [minted])
            .unwrap();
        let next = project_frame_handles(&conn, "p2", &["u-y"]).unwrap()["u-y"];
        assert!(
            next > minted,
            "{next} must be greater than the deleted {minted}"
        );
    }

    fn new_run<'a>(subject: &'a StackSubject) -> NewRun<'a> {
        NewRun {
            subject,
            config_json: "{}",
            config_hash: "h",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: "/w",
            output_dir: "/o",
        }
    }

    fn new_group(run_id: i64) -> NewGroup<'static> {
        NewGroup {
            run_id,
            group_key: "g",
            instrume: None,
            color_mode: "mono",
            filter: None,
            binning: Some(1),
            width: Some(10),
            height: Some(10),
            exposure: None,
            frame_count: 1,
            included_count: 1,
        }
    }

    fn new_frame_row(
        run_id: i64,
        group_id: i64,
        frame_id: i64,
        included: bool,
    ) -> NewFrameRow<'static> {
        NewFrameRow {
            run_id,
            group_id,
            frame_id,
            included,
            exclusion_reason: None,
            weight: None,
            weight_channels_json: None,
            metrics_json: None,
            reg_status: None,
            reg_model: None,
            reg_rms_px: None,
            reg_inliers: None,
            reg_inlier_ratio: None,
            reg_flipped: None,
            rejected_fraction: None,
        }
    }

    #[test]
    fn subject_display_and_columns() {
        let fs = StackSubject::FrameSet(7);
        let pr = StackSubject::Project("p1".into());
        assert_eq!(fs.to_string(), "frame set 7");
        assert_eq!(pr.to_string(), "project p1");
        assert_eq!((fs.frame_set_id(), fs.project_id()), (Some(7), None));
        assert_eq!((pr.frame_set_id(), pr.project_id()), (None, Some("p1")));
    }

    #[test]
    fn frame_set_and_project_artifacts_never_collide() {
        let c = conn();
        let set = seed_set(&c);
        let frame = 501;
        seed_frame(&c, frame);
        insert_test_project(&c, "p1");
        let handle = project_frame_handles(&c, "p1", &["u1"]).unwrap()["u1"];
        let fs = StackSubject::FrameSet(set);
        let pr = StackSubject::Project("p1".into());
        for (subject, f, path) in [(&fs, frame, "/a"), (&pr, handle, "/b")] {
            for _ in 0..2 {
                upsert_artifact(
                    &c,
                    &NewArtifact {
                        subject,
                        frame_id: Some(f),
                        group_key: "g",
                        kind: "calibrated",
                        path: Some(path),
                        config_hash: "h",
                        size: None,
                        modified_at: None,
                        payload_json: None,
                    },
                )
                .unwrap();
            }
        }
        assert_eq!(
            list_artifacts(&c, &fs, None).unwrap().len(),
            1,
            "an upsert refreshes in place"
        );
        assert_eq!(list_artifacts(&c, &pr, None).unwrap().len(), 1);
        let a = find_artifact(&c, &pr, "g", "calibrated", Some(handle))
            .unwrap()
            .unwrap();
        assert_eq!(
            (a.subject.clone(), a.frame_id, a.path.as_deref()),
            (pr.clone(), Some(handle), Some("/b"))
        );
        let b = find_artifact(&c, &fs, "g", "calibrated", Some(frame))
            .unwrap()
            .unwrap();
        assert_eq!(
            (b.subject.clone(), b.frame_id, b.path.as_deref()),
            (fs.clone(), Some(frame), Some("/a"))
        );
        // The first minted handle is 1, the catalog frame is 501: a frame id of
        // one namespace never finds the other subject's row.
        assert_ne!(handle, frame);
        assert!(find_artifact(&c, &fs, "g", "calibrated", Some(handle))
            .unwrap()
            .is_none());
        assert!(find_artifact(&c, &pr, "g", "calibrated", Some(frame))
            .unwrap()
            .is_none());
        assert_eq!(delete_all_artifacts(&c, &pr).unwrap(), 1);
        assert_eq!(
            list_artifacts(&c, &fs, None).unwrap().len(),
            1,
            "the frame set's row survives"
        );
    }

    #[test]
    fn run_frame_rows_of_both_subjects_upsert_in_place() {
        let c = conn();
        let set = seed_set(&c);
        seed_frame(&c, 501);
        insert_test_project(&c, "p1");
        let handle = project_frame_handles(&c, "p1", &["u1"]).unwrap()["u1"];
        let fs = StackSubject::FrameSet(set);
        let pr = StackSubject::Project("p1".into());
        let fs_run = insert_run(&c, &new_run(&fs)).unwrap();
        let pr_run = insert_run(&c, &new_run(&pr)).unwrap();
        let fs_group = insert_group(&c, &new_group(fs_run)).unwrap();
        let pr_group = insert_group(&c, &new_group(pr_run)).unwrap();
        for _ in 0..2 {
            upsert_frame_row(&c, &fs, &new_frame_row(fs_run, fs_group, 501, true)).unwrap();
            upsert_frame_row(&c, &pr, &new_frame_row(pr_run, pr_group, handle, true)).unwrap();
        }
        let fs_rows = list_frame_rows(&c, fs_run).unwrap();
        let pr_rows = list_frame_rows(&c, pr_run).unwrap();
        assert_eq!(fs_rows.len(), 1, "one row per run");
        assert_eq!(pr_rows.len(), 1);
        assert_eq!(fs_rows[0].frame_id, 501);
        assert_eq!(pr_rows[0].frame_id, handle);
        set_frame_rejected_fraction(&c, &pr, pr_run, handle, 0.25).unwrap();
        assert_eq!(
            list_frame_rows(&c, pr_run).unwrap()[0].rejected_fraction,
            Some(0.25)
        );
        assert_eq!(
            list_frame_rows(&c, fs_run).unwrap()[0].rejected_fraction,
            None
        );
    }

    #[test]
    fn a_project_run_row_leaves_the_frame_set_queries_alone() {
        let c = conn();
        let set = seed_set(&c);
        insert_test_project(&c, "p1");
        let fs = StackSubject::FrameSet(set);
        let pr = StackSubject::Project("p1".into());
        let fs_run = insert_run(&c, &new_run(&fs)).unwrap();
        let pr_run = insert_run(&c, &new_run(&pr)).unwrap();
        set_run_status(&c, fs_run, "running").unwrap();
        set_run_status(&c, pr_run, "running").unwrap();
        assert_eq!(
            list_unfinished_runs(&c).unwrap(),
            vec![(fs_run, fs.clone()), (pr_run, pr.clone())]
        );
        let fs_runs = list_runs(&c, &fs, 10).unwrap();
        assert_eq!(
            fs_runs.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![fs_run]
        );
        let pr_runs = list_runs(&c, &pr, 10).unwrap();
        assert_eq!(
            pr_runs.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![pr_run]
        );
        assert_eq!(active_run_for_subject(&c, &fs).unwrap(), Some(fs_run));
        assert_eq!(active_run_for_subject(&c, &pr).unwrap(), Some(pr_run));
        let row = get_run(&c, pr_run).unwrap().unwrap();
        assert_eq!(row.subject(), pr);
        assert_eq!(
            row.frames_set_id, None,
            "a project run has no set id on the wire"
        );
        assert_eq!(row.project_id.as_deref(), Some("p1"));
        assert_eq!(get_run(&c, fs_run).unwrap().unwrap().subject(), fs);
    }

    #[test]
    fn master_lights_carry_their_subject() {
        let c = conn();
        insert_test_project(&c, "p1");
        let pr = StackSubject::Project("p1".into());
        let run = insert_run(&c, &new_run(&pr)).unwrap();
        insert_master_light(
            &c,
            &NewMasterLight {
                subject: &pr,
                run_id: run,
                group_key: "g",
                kind: "master",
                path: "/o/m.fits",
                format: "fits",
                width: 10,
                height: 10,
                channels: 1,
                frames: 3,
                total_exposure_s: None,
            },
        )
        .unwrap();
        assert_eq!(list_master_lights(&c, run).unwrap()[0].subject, pr);
    }

    #[test]
    fn an_artifact_written_before_the_rebuild_is_found_after_it() {
        let c = conn();
        let set = seed_set(&c);
        seed_frame(&c, 101);
        crate::db::schema::downgrade_stacking_tables_to_legacy(&c);
        c.execute(
            "INSERT INTO stacking_artifacts (frames_set_id, frame_id, group_key, kind,
               config_hash, created_at)
             VALUES (?1, 101, 'g', 'calibrated', 'c1', 't')",
            [set],
        )
        .unwrap();
        let legacy_id = c.last_insert_rowid();
        init_db(&c).unwrap();
        let row = find_artifact(
            &c,
            &StackSubject::FrameSet(set),
            "g",
            "calibrated",
            Some(101),
        )
        .unwrap()
        .expect("the legacy row is found through the new key");
        assert_eq!((row.id, row.config_hash.as_str()), (legacy_id, "c1"));
    }

    #[test]
    fn find_artifact_is_a_point_lookup_on_the_artifacts_key() {
        let c = conn();
        let sql = find_artifact_sql();
        let plan: Vec<String> = c
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(params![7_i64, "", "g", "calibrated", 5_i64, 0_i64], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|d| d.starts_with("SEARCH") && d.contains("stacking_artifacts_subject_key")),
            "{plan:?}"
        );
    }

    fn plan_details(c: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Vec<String> {
        c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(params, |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn delete_artifacts_is_a_search_on_the_subject_key_prefix() {
        let c = conn();
        let sql = delete_artifacts_sql(2);
        let plan = plan_details(&c, &sql, &[&7_i64, &"", &"calibrated", &"metrics"]);
        assert!(
            plan.iter()
                .any(|d| d.starts_with("SEARCH") && d.contains("stacking_artifacts_subject_key")),
            "{plan:?}"
        );
    }

    #[test]
    fn delete_all_artifacts_is_a_search_on_the_subject_key_prefix() {
        let c = conn();
        let plan = plan_details(&c, DELETE_ALL_ARTIFACTS_SQL, &[&7_i64, &""]);
        assert!(
            plan.iter()
                .any(|d| d.starts_with("SEARCH") && d.contains("stacking_artifacts_subject_key")),
            "{plan:?}"
        );
    }
}
