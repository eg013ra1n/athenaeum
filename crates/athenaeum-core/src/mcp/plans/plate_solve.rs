//! The plate-solve plan (MCP plan Task 18, §6.3 and §6.5): an assistant asks Athenaeum to plate-solve frames, for example
//! the lights that miss coordinates. Athenaeum prepares a plan; only the user applies it, in Athenaeum's window, and
//! applying STARTS one solve run (`api::plate_solve`, origin `assistant`). The assistant then follows the run with
//! `solve_status` and may cancel only its own run with `cancel_solve`.
//!
//! The items are the lights of the scope (those that miss coordinates when `onlyUnsolved`, the default), one group per
//! folder. An item the solver must not be handed is locked with the reason: the star catalog is not installed, the
//! input gate refuses it (`service::input_gate_reason`) or its file cannot be reached. Everything the planner needs is
//! read before the plan begins, so a request that cannot be planned (a scope that names nothing, no light to solve, too
//! many items, a batch already running) is refused in words and never replaces the plan the user is looking at.
//!
//! The run is core's (`run_plate_solve_batch`, the one slot, the handle's progress); this module only starts it on a
//! thread of its own (`mcp-plate-solve`, never the store's apply thread), records what the run said about each failed
//! frame and keeps the final snapshot in the host ([`SolveRuns`]) for `solve_status` after the handle is gone. The
//! snapshot counts from the run's own result, not from the progress events: a failed store or COMMIT sends no event.
//! No reader guard and no pooled connection is held across a call that takes the service context.

use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rmcp::schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::black_hole::{guard, PlanDescription};
use super::{
    catalog_values, plan_opened_text, stamp_for_plan, stamp_target, ItemSnapshot, McpPlanView, PlanApplier, PlanColumn,
    PlanContent, PlanGroup, PlanItem, PlanKind, PlanReport, PlanSelection, PlanState, ReportLine, PLAN_ITEM_CAP,
};
use crate::api::plate_solve::{
    cancel_plate_solve_run, dso_catalog, plate_solve_activity, prepare_plate_solve_batch, resolve_index_packs,
    resolve_layer_caches, run_plate_solve_batch, BUSY_ASSISTANT, BUSY_UI,
};
use crate::api::ApiError;
use crate::catalog_query::{fetch_scoped_frames, missing_flags, GROUPS_SHOWN};
use crate::events::ProgressEmitter;
use crate::mcp::answer::count_of;
use crate::mcp::tools::{prepare as prepare_scope, to_json, Prep};
use crate::mcp::{Answer, McpHost};
use crate::models::ImageType;
use crate::plate_solve::config::{self, PlateSolveConfig};
use crate::plate_solve::dso_lookup::DsoCatalog;
use crate::plate_solve::service::input_gate_reason;
use crate::scope::Scope;
use crate::services::{PlateSolveOrigin, SolveProgress};

const TITLE: &str = "Plate solve";
/// The plan's note on what a solve writes besides the coordinates (the app's own behaviour, `store_result`).
pub const NAMES_THE_OBJECT: &str = "A solved frame gets its coordinates; a frame with no object name also gets the name of the catalogued object the solve finds in its field, as a solve started in Athenaeum does.";
/// A plan answer lists at most this many groups with their items (the rest is summed) ...
const GROUPS_LISTED: usize = GROUPS_SHOWN;
/// ... and at most this many items of a group.
const ITEMS_LISTED: usize = 20;
/// Failed files the text half of a status names (the structured half names them all).
const FAILURES_IN_TEXT: usize = 20;
/// Runs the host remembers for `solve_status` (the oldest finished one goes first).
const RUNS_KEPT: usize = 20;
/// The lock reason when the star catalog is not installed.
pub const NO_CATALOG: &str = "the star catalog is not installed (Settings \u{2192} Plate Solving)";

/// Arguments of `plan_plate_solve`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct PlanPlateSolveArgs {
    /// Which frames. Only the light frames of the scope are planned. Empty: the whole catalog.
    #[serde(default)]
    pub scope: Scope,
    /// Plan only the lights that miss coordinates (the default). False plans every light of the scope, solved or not.
    #[serde(default = "yes")]
    pub only_unsolved: bool,
}

fn yes() -> bool {
    true
}

impl Default for PlanPlateSolveArgs {
    fn default() -> Self {
        Self { scope: Scope::default(), only_unsolved: true }
    }
}

/// Arguments of `solve_status` and `cancel_solve`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct SolveRunArgs {
    /// The run's handle (`solve-…`), as the plan's report gave it.
    pub run_id: String,
}

/// How the planner and the apply reach the star catalog, the quad index packs and the DSO catalog: core's resolvers,
/// and a seam for the tests (which have no catalog installed).
#[derive(Clone, Copy)]
pub struct Resolvers {
    pub layers: fn(&crate::services::ServiceContext) -> Result<Vec<Arc<solvemyastro::StarCache>>, ApiError>,
    pub packs: fn(&crate::services::ServiceContext) -> Vec<Arc<solvemyastro::QuadIndex>>,
    pub dso: fn(&crate::services::ServiceContext) -> Option<Arc<DsoCatalog>>,
}

impl Resolvers {
    pub fn real() -> Self {
        Self { layers: resolve_layer_caches, packs: resolve_index_packs, dso: dso_catalog }
    }
}

fn read_failed(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "plan could not read the catalog");
    ApiError::Internal(format!("could not read {what}: {e}"))
}

// ---- the runs the host remembers ----

/// A frame that failed in a run: where it is and what the run said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedFile {
    pub file_name: String,
    pub folder: String,
    pub error: String,
}

/// What a run ended with, counted from the run's own result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveOutcome {
    pub solved: usize,
    pub failed: usize,
    /// Frames a cancel left unattempted.
    pub not_attempted: usize,
    pub failures: Vec<FailedFile>,
    /// Failed frames no progress event named (their result could not be saved).
    pub unlisted: usize,
    /// Set when the run itself failed (its results were not kept).
    pub error: Option<String>,
    /// Frames of the run that had no object name before and have one after; `None` when the catalog could not tell.
    pub named: Option<usize>,
    pub total_time_ms: u64,
}

/// A run as `solve_status` reads it.
#[derive(Debug, Clone)]
pub struct RunView {
    pub run_id: String,
    pub plan_id: String,
    pub total: usize,
    pub cancel_requested: bool,
    /// The run's own cancel flag was set and the assistant never asked for it: the user cancelled it in Athenaeum's
    /// window.
    pub cancelled_in_app: bool,
    /// The frames that failed so far (while it runs) with file name and folder.
    pub failures: Vec<FailedFile>,
    pub outcome: Option<SolveOutcome>,
}

struct RunRecord {
    view: RunView,
    /// Frame id to file name and folder, from the plan.
    names: HashMap<i64, (String, String)>,
    failed_frames: Vec<i64>,
    complete: Option<(usize, usize, u64)>,
    named: Option<usize>,
}

/// The assistant's plate-solve runs of this host, newest last.
#[derive(Clone, Default)]
pub struct SolveRuns {
    inner: Arc<Mutex<Vec<RunRecord>>>,
}

impl SolveRuns {
    fn lock(&self) -> MutexGuard<'_, Vec<RunRecord>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A run began: `names` is each planned frame's file name and folder.
    pub fn start(&self, run_id: &str, plan_id: &str, total: usize, names: HashMap<i64, (String, String)>) {
        let mut runs = self.lock();
        runs.push(RunRecord {
            view: RunView {
                run_id: run_id.to_string(),
                plan_id: plan_id.to_string(),
                total,
                cancel_requested: false,
                cancelled_in_app: false,
                failures: Vec::new(),
                outcome: None,
            },
            names,
            failed_frames: Vec::new(),
            complete: None,
            named: None,
        });
        while runs.len() > RUNS_KEPT {
            match runs.iter().position(|r| r.view.outcome.is_some()) {
                Some(i) => {
                    runs.remove(i);
                }
                None => break,
            }
        }
    }

    pub fn get(&self, run_id: &str) -> Option<RunView> {
        self.lock().iter().find(|r| r.view.run_id == run_id).map(|r| r.view.clone())
    }

    /// A frame failed (a later event for the same frame replaces the earlier one).
    pub fn note_failed(&self, run_id: &str, frame_id: i64, file_name: &str, error: &str) {
        let mut runs = self.lock();
        let Some(r) = runs.iter_mut().find(|r| r.view.run_id == run_id) else { return };
        let (name, folder) = r.names.get(&frame_id).cloned().unwrap_or_else(|| (file_name.to_string(), String::new()));
        let failed = FailedFile { file_name: name, folder, error: error.to_string() };
        match r.failed_frames.iter().position(|f| *f == frame_id) {
            Some(i) => r.view.failures[i] = failed,
            None => {
                r.failed_frames.push(frame_id);
                r.view.failures.push(failed);
            }
        }
    }

    /// The run's own `plate-solve-complete` counts.
    pub fn note_complete(&self, run_id: &str, solved: usize, failed: usize, total_time_ms: u64) {
        if let Some(r) = self.lock().iter_mut().find(|r| r.view.run_id == run_id) {
            r.complete = Some((solved, failed, total_time_ms));
        }
    }

    /// How many frames of the run got an object name from their solve.
    pub fn note_named(&self, run_id: &str, named: usize) {
        if let Some(r) = self.lock().iter_mut().find(|r| r.view.run_id == run_id) {
            r.named = Some(named);
        }
    }

    /// The cancel was set; the run is still winding down.
    fn note_cancel(&self, run_id: &str) {
        if let Some(r) = self.lock().iter_mut().find(|r| r.view.run_id == run_id) {
            r.view.cancel_requested = true;
        }
    }

    /// The run ended with its cancel flag set. When `cancel_solve` did not set it, someone cancelled it in
    /// Athenaeum's window, and the snapshot says so.
    fn note_cancel_seen(&self, run_id: &str) {
        if let Some(r) = self.lock().iter_mut().find(|r| r.view.run_id == run_id) {
            if !r.view.cancel_requested {
                r.view.cancelled_in_app = true;
            }
        }
    }

    /// The run ended: keep its snapshot. The counts are the complete event's; a run that returned an error kept no
    /// result (a failed COMMIT rolled the solves back), so nothing was solved whatever the events said.
    pub fn finish(&self, run_id: &str, result: Result<(), ApiError>) {
        let mut runs = self.lock();
        let Some(r) = runs.iter_mut().find(|r| r.view.run_id == run_id) else { return };
        let total = r.view.total;
        let failures = r.view.failures.clone();
        let outcome = match result {
            Err(e) => SolveOutcome {
                solved: 0,
                failed: total,
                not_attempted: 0,
                unlisted: total.saturating_sub(failures.len()),
                failures,
                named: Some(0),
                error: Some(e.to_string()),
                total_time_ms: r.complete.map_or(0, |c| c.2),
            },
            Ok(()) => {
                let (solved, failed, ms) = r.complete.unwrap_or((0, failures.len(), 0));
                SolveOutcome {
                    solved,
                    failed,
                    not_attempted: total.saturating_sub(solved + failed),
                    unlisted: failed.saturating_sub(failures.len()),
                    failures,
                    named: r.named,
                    error: None,
                    total_time_ms: ms,
                }
            }
        };
        tracing::info!(
            via = "mcp",
            run_id,
            solved = outcome.solved,
            failed = outcome.failed,
            "plate solve run from the assistant ended"
        );
        r.view.outcome = Some(outcome);
    }
}

/// A run's end, on the run's own thread: a cancel the window made (the flag is set, `cancel_solve` did not set it)
/// is noted, then the snapshot is kept.
fn end_run(runs: &SolveRuns, run_id: &str, cancel_flag: &AtomicBool, result: Result<(), ApiError>) {
    if cancel_flag.load(Ordering::Relaxed) {
        runs.note_cancel_seen(run_id);
    }
    runs.finish(run_id, result);
}

/// Frame ids per query: well under SQLite's variable limit.
const IDS_PER_QUERY: usize = 500;

/// Which of `ids` have no object name now, in the order given.
fn unnamed_among(conn: &rusqlite::Connection, ids: &[i64]) -> rusqlite::Result<Vec<i64>> {
    let mut found: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for chunk in ids.chunks(IDS_PER_QUERY) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut stmt =
            conn.prepare(&format!("SELECT id FROM frames WHERE id IN ({marks}) AND (object IS NULL OR object = '')"))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| r.get::<_, i64>(0))?;
        for row in rows {
            found.insert(row?);
        }
    }
    Ok(ids.iter().copied().filter(|id| found.contains(id)).collect())
}

/// How many of `ids` have an object name now.
fn named_among(conn: &rusqlite::Connection, ids: &[i64]) -> rusqlite::Result<usize> {
    let mut named = 0usize;
    for chunk in ids.chunks(IDS_PER_QUERY) {
        let marks = vec!["?"; chunk.len()].join(",");
        let n: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM frames WHERE id IN ({marks}) AND object IS NOT NULL AND object <> ''"),
            rusqlite::params_from_iter(chunk.iter()),
            |r| r.get(0),
        )?;
        named += usize::try_from(n).unwrap_or(0);
    }
    Ok(named)
}

/// The frames of the run that have no object name yet; `None` (logged) when the catalog cannot say.
fn frames_without_object(ctx: &crate::services::ServiceContext, frame_ids: &[i64]) -> Option<Vec<i64>> {
    let database = crate::api::db(ctx).ok()?;
    let conn = database.conn();
    match unnamed_among(&conn, frame_ids) {
        Ok(ids) => Some(ids),
        Err(e) => {
            tracing::warn!(error = %e, "plate solve run could not read which frames have no object name");
            None
        }
    }
}

/// After a run: how many of the frames that had no object name have one, kept for `solve_status`.
fn note_named_frames(runs: &SolveRuns, ctx: &crate::services::ServiceContext, run_id: &str, unnamed_before: &[i64]) {
    let Ok(database) = crate::api::db(ctx) else { return };
    let conn = database.conn();
    match named_among(&conn, unnamed_before) {
        Ok(n) => runs.note_named(run_id, n),
        Err(e) => tracing::warn!(error = %e, run_id, "plate solve run could not count the frames it gave an object name"),
    }
}

/// The run's emitter: tells the host what each frame did, then forwards every event unchanged.
struct RunEmitter {
    inner: Arc<dyn ProgressEmitter>,
    runs: SolveRuns,
    run_id: String,
}

impl ProgressEmitter for RunEmitter {
    fn emit_json(&self, event_name: &str, payload: Value) {
        match event_name {
            "plate-solve-progress" if payload["status"].as_str() == Some("failed") => {
                let name = payload["filename"].as_str().unwrap_or("a frame");
                let error = payload["error"].as_str().unwrap_or("failed");
                if let Some(frame_id) = payload["frame_id"].as_i64() {
                    self.runs.note_failed(&self.run_id, frame_id, name, error);
                }
            }
            "plate-solve-complete" => {
                let n = |k: &str| usize::try_from(payload[k].as_u64().unwrap_or(0)).unwrap_or(0);
                self.runs.note_complete(&self.run_id, n("solved"), n("failed"), payload["total_time_ms"].as_u64().unwrap_or(0));
            }
            _ => {}
        }
        self.inner.emit_json(event_name, payload);
    }
}

// ---- planning ----

/// The refusal while a batch holds the slot, in the running run's own words.
fn busy_refusal(host: &McpHost) -> Option<ApiError> {
    let a = plate_solve_activity(host.ctx());
    if !a.running {
        return None;
    }
    let template = if a.origin == Some(PlateSolveOrigin::Assistant) { BUSY_ASSISTANT } else { BUSY_UI };
    Some(ApiError::Conflict(template.replace("{done}", &a.done.to_string()).replace("{total}", &a.total.to_string())))
}

/// What the plan knows about one light.
struct Candidate {
    frame_id: i64,
    file_id: i64,
    file_name: String,
    folder: String,
    path: String,
    zip: Option<String>,
    object: Option<String>,
    filter: Option<String>,
    exposure: Option<f64>,
    coordinates: Option<(f64, f64)>,
    /// Why the input gate refuses it.
    gate: Option<String>,
    catalog: BTreeMap<String, Option<String>>,
}

struct Read {
    description: String,
    candidates: Vec<Candidate>,
    /// Frames of the scope that are not raw lights.
    not_lights: usize,
    /// Lights that already have coordinates and are left out (`onlyUnsolved`).
    located: usize,
    concurrency: usize,
}

fn keys_of(only_unsolved: bool) -> &'static [&'static str] {
    if only_unsolved { &["IMAGETYP", "RA", "DEC"] } else { &["IMAGETYP"] }
}

/// The frames the solver is handed at a time, as `prepare_plate_solve_batch` chooses.
fn workers_for(cfg: &PlateSolveConfig) -> usize {
    if cfg.batch_concurrency == 0 {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 3).clamp(2, 8)
    } else {
        (cfg.batch_concurrency as usize).clamp(1, 16)
    }
}

fn read_plan(host: &McpHost, args: &PlanPlateSolveArgs) -> Result<Read, ApiError> {
    let conn = host.reader()?;
    let prepared = match prepare_scope(&conn, &args.scope)? {
        // A scope that names nothing the catalog holds: the sentence says what it does hold, and no plan begins.
        Prep::Answered(answer) => {
            return Err(ApiError::Invalid(format!("{}; nothing was planned", answer.text.trim_end_matches('.'))));
        }
        Prep::Ready(p) => p,
    };
    let frames = fetch_scoped_frames(&conn, &prepared.filter).map_err(|e| read_failed("the frames in scope", format!("{e:#}")))?;
    let cfg = config::load_config(&conn);
    // The lights the plan would hold are counted first, from the rows already read: a request over the cap is
    // refused before any per-frame read (the input gate, the catalog values).
    let (mut not_lights, mut located) = (0usize, 0usize);
    let mut eligible = Vec::new();
    for sf in &frames {
        let Some(frame) = sf.file.frame.as_ref() else {
            not_lights += 1;
            continue;
        };
        let (Some(frame_id), Some(file_id)) = (frame.id, sf.file.file.id) else {
            not_lights += 1;
            continue;
        };
        if frame.imagetyp != Some(ImageType::Light) || frame.is_master {
            not_lights += 1;
            continue;
        }
        let missing = missing_flags(frame).coordinates;
        if args.only_unsolved && !missing {
            located += 1;
            continue;
        }
        eligible.push((sf, frame, frame_id, file_id, missing));
    }
    if eligible.len() > PLAN_ITEM_CAP {
        return Err(ApiError::Invalid(format!(
            "this plan would hold {} items, and a plan holds at most 2 000; narrow the scope (a folder, a night) and ask again",
            eligible.len()
        )));
    }
    let mut candidates: Vec<Candidate> = Vec::with_capacity(eligible.len());
    for (sf, frame, frame_id, file_id, missing) in eligible {
        let coordinates = match (frame.ra, frame.dec) {
            (Some(ra), Some(dec)) if !missing => Some((ra, dec)),
            _ => None,
        };
        candidates.push(Candidate {
            frame_id,
            file_id,
            file_name: sf.file.file.filename.clone(),
            folder: sf.folder.clone(),
            path: sf.file.file.path.clone(),
            zip: sf.file.file.archive_zip_path.clone(),
            object: frame.object.clone().filter(|o| !o.is_empty()),
            filter: frame.filter.clone().filter(|f| !f.is_empty()),
            exposure: frame.exptime,
            coordinates,
            gate: input_gate_reason(&conn, frame_id, &cfg),
            catalog: catalog_values(&conn, frame_id, keys_of(args.only_unsolved)).map_err(|e| read_failed("a frame", e))?,
        });
    }
    Ok(Read { description: prepared.resolved.description.clone(), candidates, not_lights, located, concurrency: workers_for(&cfg) })
}

/// Why there is nothing to plan, so the assistant knows what to tell the user.
fn nothing_to_solve(read: &Read, only_unsolved: bool) -> String {
    let d = &read.description;
    if read.located > 0 && only_unsolved {
        return format!(
            "every light frame in the scope ({d}) already has coordinates ({}); pass onlyUnsolved false to solve them again; nothing was planned",
            read.located
        );
    }
    if read.not_lights > 0 {
        return format!(
            "no light frame in the scope ({d}); its {} not light frames, which are never solved; nothing was planned",
            read.not_lights
        );
    }
    format!("no light frame in the scope ({d}); nothing was planned")
}

fn plan_columns() -> Vec<PlanColumn> {
    let col = |key: &str, label: &str| PlanColumn { key: key.to_string(), label: label.to_string() };
    vec![col("object", "Object"), col("filter", "Filter"), col("exposure", "Exposure"), col("coordinates", "Coordinates")]
}

fn exposure_text(s: Option<f64>) -> String {
    s.map_or_else(|| "(unknown)".to_string(), |s| format!("{s} s"))
}

fn coordinates_text(c: Option<(f64, f64)>) -> String {
    c.map_or_else(|| "none".to_string(), |(ra, dec)| format!("RA {ra:.4}, Dec {dec:.4}"))
}

/// What the plan remembers to apply.
struct ItemInfo {
    frame_id: i64,
    file_name: String,
    folder: String,
}

/// Stamps the files, builds the groups and publishes the plan.
fn build_and_publish(
    host: &McpHost,
    id: &str,
    read: Read,
    catalog: &CatalogState,
    only_unsolved: bool,
    resolve: Resolvers,
) -> Result<Result<(), Stopped>, ApiError> {
    let Read { candidates, not_lights, located, concurrency, .. } = read;
    let mut by_folder: BTreeMap<String, Vec<&Candidate>> = BTreeMap::new();
    for c in &candidates {
        by_folder.entry(c.folder.clone()).or_default().push(c);
    }
    let mut groups: Vec<PlanGroup> = Vec::new();
    let mut snapshots: HashMap<String, ItemSnapshot> = HashMap::new();
    let mut infos: HashMap<String, ItemInfo> = HashMap::new();
    // Locked items by reason, for the notes.
    let mut refused: BTreeMap<String, usize> = BTreeMap::new();
    let mut solvable = 0usize;
    for (folder, members) in by_folder {
        let mut items = Vec::with_capacity(members.len());
        for c in members {
            let item_id = format!("item:{}", c.file_id);
            let mut lock = catalog.lock_reason();
            if lock.is_none() {
                lock = c.gate.clone();
            }
            let stamp = if lock.is_none() { Some(stamp_for_plan(&stamp_target(&c.path, c.zip.as_deref()))) } else { None };
            if let Some(Err(reason)) = &stamp {
                lock = Some(reason.clone());
            }
            if let Some(reason) = &lock {
                if catalog.is_installed() {
                    *refused.entry(reason.clone()).or_default() += 1;
                }
            } else if let Some(Ok(stamp)) = stamp {
                solvable += 1;
                snapshots.insert(
                    item_id.clone(),
                    ItemSnapshot { file_id: c.file_id, frame_id: Some(c.frame_id), stamp, catalog: c.catalog.clone() },
                );
                infos.insert(item_id.clone(), ItemInfo { frame_id: c.frame_id, file_name: c.file_name.clone(), folder: c.folder.clone() });
            }
            items.push(PlanItem {
                id: item_id,
                file_name: c.file_name.clone(),
                folder: c.folder.clone(),
                values: BTreeMap::from([
                    ("object".to_string(), c.object.clone().unwrap_or_else(|| "(none)".to_string())),
                    ("filter".to_string(), c.filter.clone().unwrap_or_else(|| "(none)".to_string())),
                    ("exposure".to_string(), exposure_text(c.exposure)),
                    ("coordinates".to_string(), coordinates_text(c.coordinates)),
                ]),
                action: "Solve for its coordinates".to_string(),
                evidence: only_unsolved.then(|| "the catalog holds no coordinates for this frame".to_string()),
                selected: lock.is_none(),
                locked: lock,
                candidate: false,
                size_bytes: None,
            });
        }
        groups.push(PlanGroup {
            id: format!("group:{}", groups.len() + 1),
            title: folder,
            note: None,
            choose_one: None,
            chosen: None,
            items,
        });
    }

    let mut notes: Vec<String> = vec![format!("{solvable} of {} frames can be solved", candidates.len())];
    let mut reasons: Vec<(String, usize)> = refused.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (reason, n) in reasons {
        notes.push(format!("{} refused: {reason}", count_of(n, "frame", "frames")));
    }
    notes.push(if catalog.is_installed() { "star catalog: installed".to_string() } else { "star catalog: not installed".to_string() });
    notes.push(format!("quad index packs: {} installed", catalog.packs));
    notes.push(format!("runs {concurrency} frames at a time"));
    notes.push(NAMES_THE_OBJECT.to_string());
    if not_lights > 0 {
        notes.push(format!(
            "{} {} not a light frame and is left out",
            not_lights,
            if not_lights == 1 { "frame is" } else { "frames are" }
        ));
    }
    if located > 0 {
        notes.push(format!(
            "{} already {} coordinates and {} left out (onlyUnsolved)",
            count_of(located, "light frame", "light frames"),
            if located == 1 { "has" } else { "have" },
            if located == 1 { "is" } else { "are" }
        ));
    }
    let content = PlanContent {
        title: TITLE.to_string(),
        columns: plan_columns(),
        groups,
        notes,
        snapshots,
        applier: Box::new(PlateSolveApplier { items: infos, resolve }),
    };
    match host.plans.publish(id, content) {
        Ok(()) => Ok(Ok(())),
        Err(message) => match host.plans.view(Some(id)).map(|v| v.state) {
            Some(PlanState::Cancelled | PlanState::Replaced) => Ok(Err(Stopped::Ended)),
            _ => Err(ApiError::Invalid(message)),
        },
    }
}

enum Stopped {
    /// The user cancelled it, or a newer plan replaced it, while it was being prepared.
    Ended,
}

/// What the star catalog and the index packs are, found before the plan begins.
struct CatalogState {
    /// `None` when installed; else the lock reason.
    missing: Option<String>,
    packs: usize,
}

impl CatalogState {
    fn is_installed(&self) -> bool {
        self.missing.is_none()
    }

    fn lock_reason(&self) -> Option<String> {
        self.missing.clone()
    }
}

fn catalog_state(host: &McpHost, resolve: Resolvers) -> CatalogState {
    let missing = match (resolve.layers)(host.ctx()) {
        Ok(_) => None,
        Err(ApiError::Invalid(_)) => Some(NO_CATALOG.to_string()),
        Err(e) => {
            tracing::warn!(error = %e, "plate-solve plan could not open the star catalog");
            Some(format!("the star catalog could not be opened ({e})"))
        }
    };
    CatalogState { missing, packs: (resolve.packs)(host.ctx()).len() }
}

/// `plan_plate_solve`: reads, begins the plan, stamps the files and publishes it before it answers. Runs on the tool's
/// blocking thread.
pub fn prepare(host: &Arc<McpHost>, args: PlanPlateSolveArgs, resolve: Resolvers) -> Result<Answer, ApiError> {
    if let Some(refusal) = busy_refusal(host) {
        return Err(refusal);
    }
    // The resolvers take the context: no reader guard is held while they run.
    let catalog = catalog_state(host, resolve);
    let read = read_plan(host, &args)?;
    if read.candidates.is_empty() {
        return Err(ApiError::Invalid(nothing_to_solve(&read, args.only_unsolved)));
    }
    // From here the plan exists: a failure ends it `failed` (a preparing plan is never swept).
    let (id, _cancel) = host.plans.begin(host, PlanKind::PlateSolve, TITLE);
    let published = guard(host, &id, || build_and_publish(host, &id, read, &catalog, args.only_unsolved, resolve))?;
    match published {
        Ok(()) => {
            let Some(view) = host.plans.view(Some(&id)) else {
                return Err(ApiError::Internal("the plan is gone".to_string()));
            };
            Ok(answer_of(host, &view))
        }
        Err(Stopped::Ended) => Ok(ended_answer(host, &id)),
    }
}

fn ended_answer(host: &McpHost, id: &str) -> Answer {
    let state = host.plans.view(Some(id)).map(|v| v.state);
    let words = match &state {
        Some(PlanState::Replaced) => "replaced by a newer plan",
        _ => "cancelled in Athenaeum's window",
    };
    Answer::new(
        format!("The plan was {words} while it was being prepared; nothing was started. Ask again if it is still wanted."),
        json!({ "planId": id, "state": state.as_ref().map_or(Value::Null, |s| to_json(s).unwrap_or(Value::Null)) }),
    )
}

// ---- what the answer says about the plan ----

fn item_json(item: &PlanItem) -> Value {
    json!({
        "fileName": item.file_name,
        "folder": item.folder,
        "values": item.values,
        "selected": item.selected,
        "locked": item.locked,
        "evidence": item.evidence,
    })
}

fn fate_of(item: &PlanItem) -> String {
    match &item.locked {
        Some(reason) => format!("locked: {reason}"),
        None if item.selected => "will be solved".to_string(),
        None => "not selected".to_string(),
    }
}

/// A plan that waits for the user, in words and as data: one function for the planning answer and for `plan_status`.
pub fn describe_plan(view: &McpPlanView) -> PlanDescription {
    let all = || view.groups.iter().flat_map(|g| g.items.iter());
    let in_scope = all().count();
    let solvable = all().filter(|i| i.locked.is_none()).count();
    let locked = in_scope - solvable;
    let summary = format!("{solvable} of {in_scope} frames can be solved");
    let mut lines: Vec<String> = Vec::new();
    if locked > 0 {
        lines.push(format!("{} locked and will not be solved (the reason is on each).", count_of(locked, "frame is", "frames are")));
    }
    let mut groups: Vec<Value> = Vec::new();
    for g in view.groups.iter().take(GROUPS_LISTED) {
        let locked_here = g.items.iter().filter(|i| i.locked.is_some()).count();
        lines.push(format!("- {}: {}", g.title, count_of(g.items.len(), "frame", "frames")));
        for i in g.items.iter().take(ITEMS_LISTED) {
            lines.push(format!("    {} in {}: {}", i.file_name, i.folder, fate_of(i)));
        }
        let hidden: Vec<&PlanItem> = g.items.iter().skip(ITEMS_LISTED).collect();
        if !hidden.is_empty() {
            lines.push(format!(
                "    and {} more ({} to solve, {} locked)",
                count_of(hidden.len(), "frame", "frames"),
                hidden.iter().filter(|i| i.locked.is_none()).count(),
                hidden.iter().filter(|i| i.locked.is_some()).count()
            ));
        }
        groups.push(json!({
            "title": g.title,
            "frames": g.items.len(),
            "locked": locked_here,
            "items": g.items.iter().take(ITEMS_LISTED).map(item_json).collect::<Vec<_>>(),
            "itemsNotShown": (!hidden.is_empty()).then(|| json!({
                "frames": hidden.len(),
                "solvable": hidden.iter().filter(|i| i.locked.is_none()).count(),
                "locked": hidden.iter().filter(|i| i.locked.is_some()).count(),
            })),
        }));
    }
    let beyond: Vec<&PlanItem> = view.groups.iter().skip(GROUPS_LISTED).flat_map(|g| g.items.iter()).collect();
    let other_groups = view.groups.len().saturating_sub(GROUPS_LISTED);
    if other_groups > 0 {
        lines.push(format!(
            "- and {} more ({}, {} to solve, {} locked)",
            count_of(other_groups, "group", "groups"),
            count_of(beyond.len(), "frame", "frames"),
            beyond.iter().filter(|i| i.locked.is_none()).count(),
            beyond.iter().filter(|i| i.locked.is_some()).count()
        ));
    }
    // The first note is the summary itself.
    for n in view.notes.iter().skip(1) {
        lines.push(n.clone());
    }
    let mut data = Map::new();
    data.insert("solvable".to_string(), json!(solvable));
    data.insert("inScope".to_string(), json!(in_scope));
    data.insert("lockedItems".to_string(), json!(locked));
    data.insert("groups".to_string(), Value::Array(groups));
    data.insert(
        "otherGroups".to_string(),
        if other_groups > 0 {
            json!({
                "groups": other_groups,
                "frames": beyond.len(),
                "solvable": beyond.iter().filter(|i| i.locked.is_none()).count(),
                "locked": beyond.iter().filter(|i| i.locked.is_some()).count(),
            })
        } else {
            Value::Null
        },
    );
    data.insert("notes".to_string(), json!(view.notes));
    PlanDescription { summary, lines, data }
}

fn answer_of(host: &McpHost, view: &McpPlanView) -> Answer {
    let described = describe_plan(view);
    let mut lines = vec![format!("Plan {} is ready: {}.", view.plan_id, described.summary)];
    lines.extend(described.lines);
    lines.push(format!("{}.", plan_opened_text(host)));
    match &view.expires_at {
        Some(t) => lines.push(format!(
            "It waits for the user until {t}. Nothing starts until they press Apply there; applying starts one plate-solve run."
        )),
        None => lines.push("Nothing starts until the user presses Apply there.".to_string()),
    }
    lines.push("Ask plan_status for the plan; once it is applied, solve_status for the run.".to_string());
    let mut data = Map::new();
    data.insert("planId".to_string(), json!(view.plan_id));
    data.insert("kind".to_string(), json!(view.kind.as_str()));
    data.insert("state".to_string(), to_json(&view.state).unwrap_or(Value::Null));
    data.insert("expiresAt".to_string(), json!(view.expires_at));
    data.extend(described.data);
    Answer::new(lines.join("\n"), Value::Object(data))
}

// ---- what a plan that was applied says: it STARTED a run, it did not solve anything ----

/// The state's words for a plate-solve plan that ended `applied`: the user pressed Apply and the run started (or did
/// not). Never says the frames were solved.
pub fn started_words(v: &McpPlanView) -> String {
    let report = v.report.clone().unwrap_or_default();
    match &report.run_id {
        Some(run_id) => format!(
            "started a plate solve of {} (run {run_id}); ask solve_status for its progress",
            count_of(report.applied.len(), "frame", "frames")
        ),
        None => {
            let why = report
                .failed
                .first()
                .or(report.skipped.first())
                .map_or_else(|| "nothing was left to solve".to_string(), |l| l.reason.clone());
            format!("did not start a plate solve: {}", why.trim_end_matches('.'))
        }
    }
}

/// Why a plate-solve plan started nothing: no run, and at least one failed line (the first one's reason).
pub fn nothing_started_reason(report: &PlanReport) -> Option<String> {
    if report.run_id.is_some() {
        return None;
    }
    report.failed.first().map(|l| l.reason.clone())
}

/// The report of a plate-solve plan: frames started, skipped by the re-check, failed to start.
pub fn report_text(r: &PlanReport) -> String {
    let mut out = format!(
        "\nReport: {} started, {} skipped, {} could not start.",
        r.applied.len(),
        r.skipped.len(),
        r.failed.len()
    );
    for (heading, lines) in [("Skipped", &r.skipped), ("Could not start", &r.failed)] {
        if lines.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{heading}:"));
        for l in lines.iter().take(20) {
            out.push_str(&format!("\n- {} in {}: {}", l.file_name, l.folder, l.reason));
        }
        if lines.len() > 20 {
            out.push_str(&format!("\n- and {} more (the structured half lists them all)", lines.len() - 20));
        }
    }
    if let Some(run_id) = &r.run_id {
        out.push_str(&format!("\nThe run is {run_id}."));
    }
    out
}

// ---- the applier ----

/// What the plan remembers to apply: each item's frame, and how to reach the catalogs.
pub struct PlateSolveApplier {
    items: HashMap<String, ItemInfo>,
    resolve: Resolvers,
}

fn report_line(items: &HashMap<String, ItemInfo>, item_id: &str, reason: impl Into<String>) -> ReportLine {
    let (file_name, folder) = items.get(item_id).map(|i| (i.file_name.clone(), i.folder.clone())).unwrap_or_default();
    ReportLine { item_id: item_id.to_string(), file_name, folder, reason: reason.into() }
}

impl PlateSolveApplier {
    /// Nothing started: every item is a failure with the reason.
    fn none_started(&self, mut report: PlanReport, ids: &[&str], reason: &str) -> PlanReport {
        for id in ids {
            report.failed.push(report_line(&self.items, id, reason));
        }
        report
    }
}

impl PlanApplier for PlateSolveApplier {
    /// Every item is an independent include/exclude: the generic check is all a selection needs.
    fn validate(&self, _view: &McpPlanView, _sel: &PlanSelection) -> Result<(), String> {
        Ok(())
    }

    fn start_failure(&self, report: &PlanReport) -> Option<String> {
        nothing_started_reason(report)
    }

    /// A batch that holds the slot refuses the Apply in its own words, and the plan keeps waiting: the user presses
    /// Apply again once the batch ends.
    fn ready(&self, host: &McpHost) -> Result<(), String> {
        match busy_refusal(host) {
            Some(refusal) => Err(refusal.to_string()),
            None => Ok(()),
        }
    }

    fn apply(&self, host: &McpHost, plan_id: &str, sel: &PlanSelection, fresh: &[String]) -> PlanReport {
        let report = PlanReport::default();
        let fresh: std::collections::HashSet<&str> = fresh.iter().map(String::as_str).collect();
        let included: Vec<&str> =
            sel.groups.iter().flat_map(|g| g.included.iter().map(String::as_str)).filter(|id| fresh.contains(id)).collect();
        let frame_ids: Vec<i64> = included.iter().filter_map(|id| self.items.get(*id).map(|i| i.frame_id)).collect();
        let ctx = host.ctx();

        // The catalogs, resolved now (an install or a removal since the plan counts), with no guard held.
        let layers = match (self.resolve.layers)(ctx) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, plan_id, "plate-solve plan could not open the star catalog");
                let reason = match e {
                    ApiError::Invalid(_) => NO_CATALOG.to_string(),
                    other => format!("the star catalog could not be opened ({other})"),
                };
                return self.none_started(report, &included, &reason);
            }
        };
        let packs = (self.resolve.packs)(ctx);
        let dso = (self.resolve.dso)(ctx);
        // A batch started in Athenaeum meanwhile refuses this one, in its own words; the slot is never replaced.
        let prepared = match prepare_plate_solve_batch(ctx, &frame_ids, layers, packs, dso, PlateSolveOrigin::Assistant) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, plan_id, "plate-solve plan could not start its run");
                return self.none_started(report, &included, &e.to_string());
            }
        };
        let run_id = prepared.run_id().to_string();
        let cancel_flag = prepared.cancel_flag();
        let names: HashMap<i64, (String, String)> =
            self.items.values().map(|i| (i.frame_id, (i.file_name.clone(), i.folder.clone()))).collect();
        host.solves.start(&run_id, plan_id, frame_ids.len(), names);

        let runs = host.solves.clone();
        let emitter: Arc<dyn ProgressEmitter> =
            Arc::new(RunEmitter { inner: Arc::clone(host.emitter()), runs: runs.clone(), run_id: run_id.clone() });
        let ctx = Arc::clone(host.ctx());
        let unnamed_before = frames_without_object(&ctx, &frame_ids);
        let counting_ctx = Arc::clone(&ctx);
        // A tracing dispatcher is per thread: carry the caller's so the run's lines land with it.
        let dispatch = tracing::dispatcher::get_default(tracing::Dispatch::clone);
        let thread_run = run_id.clone();
        let spawned = std::thread::Builder::new().name("mcp-plate-solve".to_string()).spawn(move || {
            tracing::dispatcher::with_default(&dispatch, || {
                // The run owns the context and the emitter and lets them go when it ends, before the snapshot is kept.
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| run_plate_solve_batch(ctx, emitter, prepared)))
                    .unwrap_or_else(|_| {
                        tracing::error!(run_id = %thread_run, "plate solve run panicked");
                        Err(ApiError::Internal("the plate solve run stopped unexpectedly".to_string()))
                    });
                if let Err(e) = &result {
                    tracing::error!(error = %e, run_id = %thread_run, "plate solve run failed");
                }
                if let (Ok(()), Some(unnamed)) = (&result, &unnamed_before) {
                    note_named_frames(&runs, &counting_ctx, &thread_run, unnamed);
                }
                drop(counting_ctx);
                end_run(&runs, &thread_run, &cancel_flag, result);
            });
        });
        if let Err(e) = spawned {
            tracing::error!(error = %e, plan_id, "plate solve thread could not start");
            // The closure went with the error: the prepared batch is dropped, which frees the slot.
            host.solves.finish(&run_id, Err(ApiError::Internal(format!("the run could not start: {e}"))));
            return self.none_started(PlanReport::default(), &included, &format!("the run could not start: {e}"));
        }
        tracing::info!(via = "mcp", plan_id, run_id = %run_id, count = frame_ids.len(), "plate solve run started from the assistant");
        let mut report = PlanReport { run_id: Some(run_id.clone()), ..PlanReport::default() };
        for id in &included {
            report.applied.push(report_line(&self.items, id, format!("started in run {run_id}")));
        }
        report
    }
}

// ---- solve_status and cancel_solve ----

fn failure_json(f: &FailedFile) -> Value {
    json!({ "fileName": f.file_name, "folder": f.folder, "error": f.error })
}

fn failures_text(out: &mut Vec<String>, failures: &[FailedFile]) {
    for f in failures.iter().take(FAILURES_IN_TEXT) {
        out.push(format!("- {} in {}: {}", f.file_name, f.folder, f.error));
    }
    if failures.len() > FAILURES_IN_TEXT {
        out.push(format!("- and {} more (the structured half lists them all)", failures.len() - FAILURES_IN_TEXT));
    }
}

fn progress_of(host: &McpHost, run_id: &str) -> Option<(PlateSolveOrigin, SolveProgress)> {
    let solves = host.ctx().active_plate_solves.lock().unwrap_or_else(PoisonError::into_inner);
    let h = solves.get(&0).filter(|h| h.run_id == run_id)?;
    let p = h.progress.lock().unwrap_or_else(PoisonError::into_inner).clone();
    Some((h.origin, p))
}

/// `solve_status`: a running run's progress, a finished run's snapshot, or words for a run nobody knows.
pub fn solve_status(host: &McpHost, run_id: &str) -> Result<Answer, ApiError> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err(ApiError::Invalid("solve_status needs the run's handle (runId), as the plan's report gave it".to_string()));
    }
    // The catalog must be open before any answer (line 1 names it).
    host.source()?;
    let record = host.solves.get(run_id);
    if let Some((origin, p)) = progress_of(host, run_id) {
        let mut failures = record.as_ref().map(|r| r.failures.clone()).unwrap_or_default();
        if failures.is_empty() {
            failures = p.failures.iter().map(|(n, e)| FailedFile { file_name: n.clone(), folder: String::new(), error: e.clone() }).collect();
        }
        let mut lines = vec![format!(
            "Run {run_id} is running: {} of {} frames done, {} failed so far.",
            p.done,
            p.total,
            failures.len()
        )];
        if origin == PlateSolveOrigin::Ui {
            lines.push("It was started in Athenaeum's window, not by the assistant.".to_string());
        }
        if let Some(file) = &p.current_file {
            lines.push(format!("Solving now: {file}."));
        }
        if record.as_ref().is_some_and(|r| r.cancel_requested) {
            lines.push("A cancel was requested; the frames being solved finish first.".to_string());
        }
        if !failures.is_empty() {
            lines.push("Failed so far:".to_string());
            failures_text(&mut lines, &failures);
        }
        let data = json!({
            "runId": run_id,
            "state": "running",
            "origin": origin,
            "done": p.done,
            "total": p.total,
            "failed": failures.len(),
            "currentFile": p.current_file,
            "cancelRequested": record.as_ref().is_some_and(|r| r.cancel_requested),
            "failures": failures.iter().map(failure_json).collect::<Vec<_>>(),
        });
        return Ok(Answer::new(lines.join("\n"), data));
    }
    match record {
        Some(RunView { outcome: Some(o), plan_id, total, cancel_requested, cancelled_in_app, .. }) => {
            let mut lines = Vec::new();
            match &o.error {
                Some(e) => lines.push(format!(
                    "Run {run_id} did not finish: {}. No frame was updated ({} planned).",
                    e.trim_end_matches('.'),
                    count_of(total, "frame", "frames")
                )),
                None => lines.push(format!(
                    "Run {run_id} ended: {} solved, {} failed of {}.",
                    o.solved,
                    o.failed,
                    count_of(total, "frame", "frames")
                )),
            }
            if let Some(n) = o.named.filter(|n| *n > 0 && o.error.is_none()) {
                lines.push(format!(
                    "{} also got an object name from the catalogued object in their field.",
                    count_of(n, "frame", "frames")
                ));
            }
            if cancelled_in_app {
                lines.push(format!(
                    "The run was cancelled in Athenaeum; {} not attempted.",
                    if o.not_attempted == 1 { "1 frame was".to_string() } else { format!("{} frames were", o.not_attempted) }
                ));
            } else if o.not_attempted > 0 {
                lines.push(format!(
                    "{} {} not attempted{}.",
                    o.not_attempted,
                    if o.not_attempted == 1 { "frame was" } else { "frames were" },
                    if cancel_requested { " (the run was cancelled)" } else { "" }
                ));
            }
            if !o.failures.is_empty() {
                lines.push("Failed:".to_string());
                failures_text(&mut lines, &o.failures);
            }
            if o.unlisted > 0 && o.error.is_none() {
                lines.push(format!("{} more failed when their results were saved.", count_of(o.unlisted, "frame", "frames")));
            }
            let data = json!({
                "runId": run_id,
                "planId": plan_id,
                "state": "finished",
                "total": total,
                "solved": o.solved,
                "failed": o.failed,
                "notAttempted": o.not_attempted,
                "failedWithoutFile": o.unlisted,
                "objectNamed": o.named,
                "cancelRequested": cancel_requested,
                "cancelledInApp": cancelled_in_app,
                "error": o.error,
                "totalTimeMs": o.total_time_ms,
                "failures": o.failures.iter().map(failure_json).collect::<Vec<_>>(),
            });
            Ok(Answer::new(lines.join("\n"), data))
        }
        Some(r) => Ok(Answer::new(
            format!("Run {run_id} has stopped solving and is saving its results; ask again in a moment."),
            json!({ "runId": run_id, "planId": r.plan_id, "state": "finishing", "total": r.total }),
        )),
        None => Ok(Answer::new(
            format!(
                "No run {run_id} is known: Athenaeum remembers the runs the assistant started since it opened (the last {RUNS_KEPT}), and none is running under that handle."
            ),
            json!({ "runId": run_id, "state": "unknown" }),
        )),
    }
}

/// `cancel_solve`: sets the cancel flag of the assistant's own run, and only while slot 0 holds that exact run. Any
/// other answer is in words and leaves every run alone.
pub fn cancel_solve(host: &McpHost, run_id: &str) -> Result<Answer, ApiError> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err(ApiError::Invalid("cancel_solve needs the run's handle (runId), as the plan's report gave it".to_string()));
    }
    host.source()?;
    let said = |cancelled: bool, text: String| Answer::new(text, json!({ "runId": run_id, "cancelled": cancelled }));
    let Some(record) = host.solves.get(run_id) else {
        return Ok(said(
            false,
            format!("No run {run_id} of the assistant is known, so nothing was cancelled. A plate solve started in Athenaeum's window is not the assistant's to cancel."),
        ));
    };
    if record.outcome.is_some() {
        return Ok(said(false, format!("Run {run_id} has already ended; nothing was cancelled. Ask solve_status for its result.")));
    }
    if cancel_plate_solve_run(host.ctx(), run_id) {
        host.solves.note_cancel(run_id);
        tracing::info!(via = "mcp", run_id, "plate solve run cancel requested from the assistant");
        return Ok(said(
            true,
            format!("Cancel requested for run {run_id}. The frames being solved now finish first; ask solve_status for the result."),
        ));
    }
    Ok(said(false, format!("Run {run_id} is no longer running; nothing was cancelled. Ask solve_status for its result.")))
}

#[cfg(test)]
#[path = "plate_solve_tests.rs"]
pub(crate) mod tests;
