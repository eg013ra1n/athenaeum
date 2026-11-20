//! The plan's data: what the window shows (`McpPlanView` and its parts), what the user answers (`PlanAnswer`), and
//! what the store keeps to re-check an item before it applies (`DiskStamp`, `ItemSnapshot`, `PlanContent`).
//!
//! Every struct is camelCase on the wire, every enum value snake_case (plan Seams); `PlanState` is tagged `kind` and
//! `PlanAnswer` is tagged `action`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::PlanApplier;
use crate::mcp::McpSource;

/// Where a plan is in its life. On the wire `{"kind":"preparing","done":3,"total":9}`, `{"kind":"waiting"}`, ...
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanState {
    /// The planning tool is still working (a byte check, a stamp pass).
    Preparing { done: usize, total: usize },
    /// Published and waiting for the user.
    Waiting,
    /// The user pressed Apply; the items are being re-checked and applied.
    Applying,
    Applied,
    Cancelled,
    /// A newer plan took its place before the user decided.
    Replaced,
    Expired,
    Failed { error: String },
}

impl PlanState {
    /// A plan nobody has decided: a new plan replaces it.
    pub fn is_undecided(&self) -> bool {
        matches!(self, PlanState::Preparing { .. } | PlanState::Waiting)
    }

    /// The plan has reached an end (it is kept for a while so a status question can still read it).
    pub fn is_finished(&self) -> bool {
        !matches!(self, PlanState::Preparing { .. } | PlanState::Waiting | PlanState::Applying)
    }
}

/// What a plan changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    BlackHole,
    Metadata,
    PlateSolve,
}

impl PlanKind {
    /// The wire word, also the `kind` field of the apply's log line.
    pub fn as_str(self) -> &'static str {
        match self {
            PlanKind::BlackHole => "black_hole",
            PlanKind::Metadata => "metadata",
            PlanKind::PlateSolve => "plate_solve",
        }
    }
}

/// One header value column of the plan's table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanColumn {
    pub key: String,
    pub label: String,
}

/// One file (or frame field) a plan may act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    /// `item:<file_id>` (metadata `item:<frame_id>:<field>`).
    pub id: String,
    pub file_name: String,
    pub folder: String,
    /// Column key to the text shown in that column.
    pub values: BTreeMap<String, String>,
    /// What applying does to this item, in words.
    pub action: String,
    /// Why the planner proposes it (a byte check, a rule).
    pub evidence: Option<String>,
    /// Included in the plan's proposal; the user's tick overrides it.
    pub selected: bool,
    /// Why this item cannot be included; a locked item never is.
    pub locked: Option<String>,
    /// May be the group's chosen copy (`chosen` must be one).
    pub candidate: bool,
    pub size_bytes: Option<i64>,
}

/// A duplicate group, a frame, a light group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanGroup {
    /// `group:<n>`.
    pub id: String,
    pub title: String,
    pub note: Option<String>,
    /// Set when the user picks one candidate of the group: the label of that choice ("Copy that stays").
    pub choose_one: Option<String>,
    /// The id of the chosen candidate.
    pub chosen: Option<String>,
    pub items: Vec<PlanItem>,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ReportLine {
    pub item_id: String,
    pub file_name: String,
    pub folder: String,
    pub reason: String,
}

/// What an apply did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanReport {
    pub applied: Vec<ReportLine>,
    pub skipped: Vec<ReportLine>,
    pub failed: Vec<ReportLine>,
    /// The run a plan started (a plate-solve batch).
    pub run_id: Option<String>,
}

/// A plan as the window and the status tool read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct McpPlanView {
    pub plan_id: String,
    pub kind: PlanKind,
    pub title: String,
    pub source: McpSource,
    pub state: PlanState,
    /// Set when `state` is `replaced`: the plan that took its place.
    pub replaced_by: Option<String>,
    /// `YYYY-MM-DD HH:MM:SS` (local time), display only; set once the plan waits.
    pub expires_at: Option<String>,
    pub columns: Vec<PlanColumn>,
    pub groups: Vec<PlanGroup>,
    pub notes: Vec<String>,
    pub report: Option<PlanReport>,
}

/// The user's answer for one group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GroupSelection {
    pub group_id: String,
    pub chosen: Option<String>,
    /// The ids of the items the user included.
    pub included: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanSelection {
    pub groups: Vec<GroupSelection>,
}

/// What the window sends: apply this selection, or cancel. Only the window's command sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PlanAnswer {
    Apply { selection: PlanSelection },
    Cancel,
}

// ---- what the store keeps ----

/// A file's state on disk: path, size and modification time (ms since the epoch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskStamp {
    pub path: PathBuf,
    pub size: u64,
    pub modified_ms: i64,
}

/// Stamps `path`. A zip-resident row stamps its zip: the caller passes [`stamp_target`]'s result. The path is read as
/// it is stored, never `canonicalize`d.
pub fn disk_stamp(path: &Path) -> std::io::Result<DiskStamp> {
    let meta = std::fs::metadata(path)?;
    let modified_ms = match meta.modified()?.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    };
    Ok(DiskStamp { path: path.to_path_buf(), size: meta.len(), modified_ms })
}

/// What a plan stamps for a catalog row: the zip for a zip-resident row (`files.archive_zip_path`), else the file.
pub fn stamp_target(path: &str, archive_zip_path: Option<&str>) -> PathBuf {
    PathBuf::from(archive_zip_path.filter(|z| !z.is_empty()).unwrap_or(path))
}

/// The lock reason of an item whose file cannot be stamped at planning.
pub const NOT_REACHABLE: &str = "not reachable (its volume may be unmounted)";

/// [`disk_stamp`] for a planning tool: `Err` is the lock reason for the item.
pub fn stamp_for_plan(path: &Path) -> Result<DiskStamp, String> {
    disk_stamp(path).map_err(|e| {
        // One file in a plan of up to 2 000: the plan's publish logs the count once, at warn.
        tracing::debug!(error = %e, path = %path.display(), "plan item could not be stamped");
        NOT_REACHABLE.to_string()
    })
}

/// Why `now` is not the state `planned` recorded; `None` when it is the same file.
pub(super) fn stamp_difference(planned: &DiskStamp, now: &std::io::Result<DiskStamp>) -> Option<String> {
    match now {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some("missing".to_string()),
        Err(_) => Some(NOT_REACHABLE.to_string()),
        Ok(s) if s.size != planned.size => {
            Some(format!("changed on disk since the plan (size {} → {})", planned.size, s.size))
        }
        Ok(s) if s.modified_ms != planned.modified_ms => {
            Some("changed on disk since the plan (modified time)".to_string())
        }
        Ok(_) => None,
    }
}

/// What the planner read for one item: its stamp, its catalog row and the catalog values the plan depends on.
#[derive(Debug, Clone)]
pub struct ItemSnapshot {
    pub file_id: i64,
    pub frame_id: Option<i64>,
    pub stamp: DiskStamp,
    /// [`catalog_values`] of the keys the plan reads; re-read at apply.
    pub catalog: BTreeMap<String, Option<String>>,
}

/// The frame column behind a header key a plan may depend on.
fn frame_column(key: &str) -> Option<&'static str> {
    Some(match key.to_ascii_uppercase().as_str() {
        "OBJECT" => "object",
        "FILTER" => "filter",
        "INSTRUME" => "instrume",
        "TELESCOP" => "telescop",
        "IMAGETYP" => "imagetyp",
        "EXPTIME" => "exptime",
        "DATE-OBS" => "date_obs",
        "GAIN" => "gain",
        "OFFSET" => "offset",
        "BINNING" => "binning",
        "XPIXSZ" => "xpixsz",
        "YPIXSZ" => "ypixsz",
        "FOCALLEN" => "focallen",
        "RA" => "ra",
        "DEC" => "dec",
        _ => return None,
    })
}

/// The stored text of the given header keys for one frame, `None` for an empty value. The planner records these in
/// [`ItemSnapshot::catalog`] and the apply re-reads them with the same function, so the two always compare like for
/// like. `Err` names a key this build does not track or a frame that is gone.
pub fn catalog_values(
    conn: &Connection,
    frame_id: i64,
    keys: &[&str],
) -> Result<BTreeMap<String, Option<String>>, String> {
    let mut out = BTreeMap::new();
    for key in keys {
        let column = frame_column(key).ok_or_else(|| format!("the catalog value {key} is not tracked by a plan"))?;
        let sql = format!("SELECT CAST({column} AS TEXT) FROM frames WHERE id = ?1");
        let found: Option<Option<String>> = conn
            .query_row(&sql, [frame_id], |r| r.get(0))
            .optional()
            .map_err(|e| {
                tracing::error!(error = %e, frame_id, "plan could not read a catalog value");
                format!("the catalog could not be read: {e}")
            })?;
        let value = found.ok_or_else(|| "its catalog entry is gone".to_string())?;
        out.insert((*key).to_string(), value.filter(|v| !v.is_empty()));
    }
    Ok(out)
}

/// The path and zip of a file row, `None` when the row is gone.
pub(super) fn file_location(conn: &Connection, file_id: i64) -> Result<Option<(String, Option<String>)>, String> {
    conn.query_row("SELECT path, archive_zip_path FROM files WHERE id = ?1", [file_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })
    .optional()
    .map_err(|e| {
        tracing::error!(error = %e, file_id, "plan could not read a file row");
        format!("the catalog could not be read: {e}")
    })
}

/// Everything a planning tool hands the store when its plan is ready.
pub struct PlanContent {
    pub title: String,
    pub columns: Vec<PlanColumn>,
    pub groups: Vec<PlanGroup>,
    pub notes: Vec<String>,
    /// By item id: every item that can be included, and every candidate (the copy that stays included).
    pub snapshots: HashMap<String, ItemSnapshot>,
    pub applier: Box<dyn PlanApplier>,
}
