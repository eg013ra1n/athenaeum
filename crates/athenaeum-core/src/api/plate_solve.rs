//! Plate-solve commands both hosts call: the batch solve (`prepare_plate_solve_batch`
//! + `run_plate_solve_batch`), the object autofind (`prepare_autofind_objects` +
//! `run_autofind_objects`), the catalog and index-pack downloads, and the trigger the auto-publish
//! worker gets after a batch (collab v3 wave 2, Task 10). Core resolves the star-catalog
//! layers, index packs and DSO catalog (`resolve_layer_caches`, `resolve_index_packs`,
//! `dso_catalog`) and holds the one batch slot (`claim_batch_slot`, `cancel_plate_solve`,
//! `plate_solve_activity`); the hosts keep only their own waiting: the desktop awaits a
//! run, the web starts it and returns.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::db::load_frame_with_path;
use crate::models::Frame;
use crate::plate_solve::config::{self, PlateSolveConfig};
use crate::plate_solve::dso_lookup::DsoCatalog;
use crate::plate_solve::hints::extract_hints;
use crate::plate_solve::object_fill::{self, AutofindProgress, AutofindStatus};
use crate::plate_solve::service::{self, SolveResult};
use crate::plate_solve::{describe_solve_failure, CatalogOpenError, SolveHints, StoreOutcome};
use crate::services::{PlateSolveHandle, PlateSolveOrigin, SolveProgress};

use crate::api::{db, ApiError};
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;

/// Called by both hosts right after `plate-solve-complete` fires. A
/// plate-solve batch is a free-form list of frame ids (not scoped to one
/// frame set), so mapping it cheaply to "which linked projects care" isn't
/// worth it here — this dirties every auto-publish-enabled project instead
/// (the worker's non-Manual-mode + linked-set filter still applies, and a
/// run with nothing new to generate is cheap — Task 7).
pub fn on_batch_finished(_ctx: &ServiceContext) {
    crate::api::collab_autopublish::request_auto_publish(None);
}

/// Payload of the `catalog-download-progress` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogDownloadProgress {
    phase: String,
    current: usize,
    total: usize,
    percent: f64,
    tier_density: u32,
    tier_index: usize,
    n_tiers: usize,
}

/// Download the prebuilt star-catalog layers up to `target_density` into the
/// app-data directory (the catalog database's parent) and return the catalog
/// root path. Runs the download on a blocking thread and emits
/// `catalog-download-progress` through `emitter`. The cancel flag is local to
/// the call and registered nowhere, so the download cannot be cancelled.
///
/// A finished install is a new catalogue identity, so every stored zero point reads as outdated: the Analysis tab shows
/// "N to update" and the next Analyze measures those frames (nothing is read at install time).
pub async fn download_catalog_layers(
    ctx: &Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    target_density: u32,
) -> Result<String, ApiError> {
    let app_data_dir = db(ctx)?
        .path()
        .to_path_buf()
        .parent()
        .ok_or_else(|| ApiError::Internal("Cannot determine app data directory".into()))?
        .to_path_buf();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let result = tokio::task::spawn_blocking(move || {
        use crate::catalog::gaia_prebuilt::GaiaPrebuiltProgress as P;
        let cur = std::sync::Mutex::new((0u32, 0usize, 0usize)); // (density, index, n_tiers)
        crate::catalog::gaia_prebuilt::download_catalog_layers(
            &app_data_dir,
            target_density,
            cancel_flag,
            &|progress| {
                let (td, ti, nt) = *cur.lock().unwrap();
                let event = match progress {
                    P::Tier { density, index, n_tiers } => {
                        *cur.lock().unwrap() = (density, index, n_tiers);
                        CatalogDownloadProgress {
                            phase: "tier".into(), current: index, total: n_tiers,
                            percent: 0.0, tier_density: density, tier_index: index, n_tiers,
                        }
                    }
                    P::Downloading { received, total } => CatalogDownloadProgress {
                        phase: "downloading".into(), current: received as usize, total: total as usize,
                        percent: if total > 0 { received as f64 / total as f64 * 100.0 } else { 0.0 },
                        tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Verifying => CatalogDownloadProgress {
                        phase: "verifying".into(), current: 0, total: 0,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Extracting { done, total } => CatalogDownloadProgress {
                        phase: "extracting".into(), current: done, total,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Complete { files } => CatalogDownloadProgress {
                        phase: "complete".into(), current: files, total: files,
                        percent: 100.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Error(_) => CatalogDownloadProgress {
                        phase: "error".into(), current: 0, total: 0,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                };
                emit_event(emitter.as_ref(), "catalog-download-progress", &event);
            },
        )
    })
    .await
    .map_err(|e| ApiError::Internal(format!("download task panicked: {e}")))?;

    let root = result
        .map(|p| p.display().to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(root)
}

/// Download and install the named quad-index packs into the app-data directory
/// (the catalog database's parent) and return the index root path. Runs the
/// download on a blocking thread; progress rides the `index-download-progress`
/// event through `emitter`, and a failure is also emitted as an `error`
/// progress event before it is returned. Already-installed packs are skipped.
/// The cancel flag is local to the call and registered nowhere.
pub async fn download_index_packs(
    ctx: &ServiceContext,
    emitter: Arc<dyn ProgressEmitter>,
    names: Vec<String>,
) -> Result<String, ApiError> {
    use crate::catalog::index_prebuilt::{self, IndexProgressMapper};
    let app_data_dir = db(ctx)?
        .path()
        .to_path_buf()
        .parent()
        .ok_or_else(|| ApiError::Internal("Cannot determine app data directory".into()))?
        .to_path_buf();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let mapper = Arc::new(IndexProgressMapper::new());
    let (emitter_progress, mapper_progress) = (emitter.clone(), mapper.clone());

    let result = tokio::task::spawn_blocking(move || {
        index_prebuilt::download_index_packs(&app_data_dir, &names, cancel_flag, &|progress| {
            emit_event(
                emitter_progress.as_ref(),
                "index-download-progress",
                &mapper_progress.map(progress),
            );
        })
    })
    .await
    .map_err(|e| ApiError::Internal(format!("download task panicked: {e}")))?;

    match result {
        Ok(p) => Ok(p.display().to_string()),
        Err(e) => {
            let msg = format!("{e:#}");
            emit_event(
                emitter.as_ref(),
                "index-download-progress",
                &mapper.error(msg.clone()),
            );
            Err(ApiError::Internal(msg))
        }
    }
}

// ========== Resolvers, cancel, the one slot ==========

/// Resolve and open the additive density-tier catalog stack under
/// `<app-data>/catalogs/smac_gaia` (base to deepest), or a legacy single
/// `stars.smac` there as a 1-layer fallback. Opens fresh each call (mmap is
/// cheap); the batch path opens once and shares across workers. Errors when no
/// catalog is installed, in the words both hosts show.
pub fn resolve_layer_caches(
    ctx: &ServiceContext,
) -> Result<Vec<Arc<solvemyastro::StarCache>>, ApiError> {
    let parent = resolve_app_data(ctx)?;
    crate::plate_solve::open_catalog_layers(&parent).map_err(|e| match e {
        CatalogOpenError::NotInstalled { root } => ApiError::Invalid(format!(
            "solvemyastro star catalog not found under {}. \
             Please download the Gaia DR3 prebuilt catalog from Settings \u{2192} Plate Solving.",
            root.display()
        )),
        CatalogOpenError::Open { dir, error } => ApiError::Internal(format!(
            "Failed to open star cache at {}: {error}",
            dir.display()
        )),
    })
}

fn resolve_app_data(ctx: &ServiceContext) -> Result<std::path::PathBuf, ApiError> {
    let database = db(ctx)?;
    database
        .path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| ApiError::Internal("Cannot determine app data directory".into()))
}

/// Open the installed quad-index packs under `<app-data>/catalogs/smac_index`
/// (empty when none are installed or the database is not open; a pack that
/// does not open is skipped with a warning). Opened once per batch, like the
/// catalog layers.
pub fn resolve_index_packs(ctx: &ServiceContext) -> Vec<Arc<solvemyastro::QuadIndex>> {
    match ctx.app_data_dir() {
        Some(parent) => crate::plate_solve::open_index_packs(&parent),
        None => Vec::new(),
    }
}

/// The DSO catalog, loaded once and cached in the `ServiceContext`. `None`
/// (logged) when the bundled catalog cannot be loaded.
pub fn dso_catalog(ctx: &ServiceContext) -> Option<Arc<DsoCatalog>> {
    {
        let guard = ctx.dso_catalog.read().unwrap();
        if let Some(ref cat) = *guard {
            return Some(cat.clone());
        }
    }
    match DsoCatalog::load() {
        Ok(cat) => {
            let arc = Arc::new(cat);
            let mut guard = ctx.dso_catalog.write().unwrap();
            *guard = Some(arc.clone());
            Some(arc)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load DSO catalog");
            None
        }
    }
}

/// Refusal while an assistant run holds the batch slot; `{done}` and `{total}` are filled in.
pub const BUSY_ASSISTANT: &str = "A plate solve started from the assistant is running ({done} of {total} frames). Wait for it or cancel it.";
/// Refusal for the assistant while a run started in Athenaeum holds the slot.
pub const BUSY_UI: &str = "A plate solve batch started in Athenaeum is running ({done} of {total} frames); the assistant's run can start when it ends.";
/// Refusal for a second run started in Athenaeum while one started there runs.
pub const BUSY_UI_SELF: &str = "A plate solve batch is already running ({done} of {total} frames). Wait for it or cancel it.";

fn busy_text(holder: PlateSolveOrigin, requester: PlateSolveOrigin, p: &SolveProgress) -> String {
    let template = match (holder, requester) {
        (PlateSolveOrigin::Assistant, _) => BUSY_ASSISTANT,
        (PlateSolveOrigin::Ui, PlateSolveOrigin::Assistant) => BUSY_UI,
        (PlateSolveOrigin::Ui, PlateSolveOrigin::Ui) => BUSY_UI_SELF,
    };
    template.replace("{done}", &p.done.to_string()).replace("{total}", &p.total.to_string())
}

/// The id of a plate-solve batch run: `solve-<uuid>`.
fn new_solve_run_id() -> String {
    format!("solve-{}", uuid::Uuid::new_v4())
}

/// Check and register under one lock (key 0 = plate-solve batch): a held slot
/// refuses, it is never replaced. The handle starts with `total` set to the
/// batch's size, so a refusal while the batch is still preparing names it right.
fn claim_batch_slot(
    ctx: &ServiceContext,
    origin: PlateSolveOrigin,
    total: usize,
) -> Result<(Arc<AtomicBool>, String, Arc<Mutex<SolveProgress>>), ApiError> {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let run_id = new_solve_run_id();
    let progress = Arc::new(Mutex::new(SolveProgress { total, ..Default::default() }));
    let mut solves = ctx.active_plate_solves.lock().unwrap();
    if let Some(held) = solves.get(&0) {
        let text = busy_text(
            held.origin,
            origin,
            &held.progress.lock().unwrap_or_else(|e| e.into_inner()),
        );
        tracing::info!(origin = ?origin, run_id = %held.run_id, "plate solve refused, a batch is running");
        return Err(ApiError::Conflict(text));
    }
    solves.insert(
        0,
        PlateSolveHandle {
            cancel_flag: cancel_flag.clone(),
            origin,
            run_id: run_id.clone(),
            progress: progress.clone(),
        },
    );
    Ok((cancel_flag, run_id, progress))
}

/// Frees the batch slot (key 0) when dropped, if it still holds this run.
/// A [`PreparedPlateSolveBatch`] owns one and [`run_plate_solve_batch`] takes
/// it over, so a batch dropped before `run` never blocks a later solve.
struct SlotGuard {
    slots: Arc<Mutex<std::collections::HashMap<i64, PlateSolveHandle>>>,
    run_id: String,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut solves = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if solves.get(&0).is_some_and(|h| h.run_id == self.run_id) {
            solves.remove(&0);
        }
    }
}

/// What a plate solve is doing right now, for the app and the assistant.
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlateSolveActivity {
    pub running: bool,
    pub origin: Option<PlateSolveOrigin>,
    pub run_id: Option<String>,
    pub done: usize,
    pub total: usize,
    pub current_file: Option<String>,
}

/// The batch slot's state: the run holding key 0 and its progress, or idle.
pub fn plate_solve_activity(ctx: &ServiceContext) -> PlateSolveActivity {
    let solves = ctx.active_plate_solves.lock().unwrap();
    match solves.get(&0) {
        Some(h) => {
            let p = h.progress.lock().unwrap_or_else(|e| e.into_inner());
            PlateSolveActivity {
                running: true,
                origin: Some(h.origin),
                run_id: Some(h.run_id.clone()),
                done: p.done,
                total: p.total,
                current_file: p.current_file.clone(),
            }
        }
        None => PlateSolveActivity {
            running: false,
            origin: None,
            run_id: None,
            done: 0,
            total: 0,
            current_file: None,
        },
    }
}

/// Set the cancel flag of the running batch (slot 0). Returns whether there
/// was one; a cancel with nothing running is logged and has no effect.
pub fn cancel_plate_solve(ctx: &ServiceContext) -> bool {
    let solves = ctx.active_plate_solves.lock().unwrap();
    tracing::debug!(active_keys = ?solves.keys().collect::<Vec<_>>(), "cancel_plate_solve invoked");
    match solves.get(&0) {
        Some(handle) => {
            handle.cancel_flag.store(true, Ordering::Relaxed);
            tracing::info!(run_id = %handle.run_id, "plate solve cancel flag set");
            true
        }
        None => {
            tracing::warn!("no active plate solve handle, cancel had no effect");
            false
        }
    }
}

/// Set the cancel flag of the running batch only when slot 0 holds `run_id`, checked and set under the slot's one
/// lock (the assistant cancels its own run and nothing else). Returns whether it did.
pub fn cancel_plate_solve_run(ctx: &ServiceContext, run_id: &str) -> bool {
    let solves = ctx.active_plate_solves.lock().unwrap();
    match solves.get(&0) {
        Some(handle) if handle.run_id == run_id => {
            handle.cancel_flag.store(true, Ordering::Relaxed);
            tracing::info!(run_id = %handle.run_id, "plate solve cancel flag set");
            true
        }
        _ => {
            tracing::info!(run_id, "plate solve cancel left alone, the slot does not hold that run");
            false
        }
    }
}

/// Wraps the run's emitter: copies each `plate-solve-progress` event into the
/// handle's [`SolveProgress`], then forwards every event unchanged.
struct RecordingEmitter {
    inner: Arc<dyn ProgressEmitter>,
    progress: Arc<Mutex<SolveProgress>>,
}

impl ProgressEmitter for RecordingEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if event_name == "plate-solve-progress" {
            let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            let current = payload["current"].as_u64().unwrap_or(0) as usize;
            let file = payload["filename"].as_str().map(str::to_string);
            // The correction for a low-confidence rejection reports `current == total`
            // for a frame already counted as solved; `done` only moves forward.
            p.done = p.done.max(current);
            if let Some(total) = payload["total"].as_u64() {
                p.total = total as usize;
            }
            match payload["status"].as_str() {
                Some("solving") => p.current_file = file,
                Some("solved") => p.solved += 1,
                Some("failed") => {
                    if payload["failure_code"].as_str() == Some("REJECTED_LOW_CONFIDENCE") {
                        p.solved = p.solved.saturating_sub(1);
                    }
                    let name = file.unwrap_or_else(|| match payload["frame_id"].as_i64() {
                        Some(id) => format!("frame {id}"),
                        None => "frame".to_string(),
                    });
                    let reason = payload["error"].as_str().unwrap_or("failed").to_string();
                    p.failures.push((name, reason));
                }
                _ => {}
            }
        }
        self.inner.emit_json(event_name, payload);
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlateSolveStartedEvent {
    origin: PlateSolveOrigin,
    run_id: String,
    total: usize,
}

// ========== Batch solve ==========

// snake_case on the wire (no `rename_all`): the UI reads `frame_id`,
// `matched_stars`, `total_time_ms`, ...
#[derive(Clone, Serialize)]
struct PlateSolveProgressEvent {
    frame_id: i64,
    current: usize,
    total: usize,
    status: String,
    matched_stars: Option<usize>,
    rms_arcsec: Option<f64>,
    error: Option<String>,
    /// Machine code for a failure (solvemyastro `FailureClass` or
    /// `REJECTED_LOW_CONFIDENCE` / `PANIC`); lets the UI group/style reasons.
    failure_code: Option<String>,
    /// Frame filename, so the UI can label per-frame rows without a lookup.
    filename: Option<String>,
}

#[derive(Clone, Serialize)]
struct PlateSolveCompleteEvent {
    solved: usize,
    failed: usize,
    total: usize,
    total_time_ms: u64,
}

/// The frame's file name for UI labels (the full path when it has none).
fn file_name_of(file_path: &str) -> String {
    std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string())
}

/// Work item passed from the preparing (DB) thread to parallel workers.
enum WorkItem {
    Ready {
        frame_id: i64,
        frame: Frame,
        file_path: String,
        hints: SolveHints,
    },
    LoadFailed {
        frame_id: i64,
        error: String,
        /// Known when the frame loaded but was refused before solving.
        filename: Option<String>,
    },
}

/// Result of a single worker attempting to solve a frame.
enum WorkResult {
    Solved {
        frame_id: i64,
        result: SolveResult,
        filename: String,
    },
    Failed {
        frame_id: i64,
        error: String,
        code: Option<String>,
        filename: Option<String>,
    },
}

/// A plate-solve batch after phase 1: config, frames and hints loaded, the
/// batch slot (key 0) held. Hand it to [`run_plate_solve_batch`], which always
/// ends the batch; dropping it unrun frees the slot.
pub struct PreparedPlateSolveBatch {
    slot: Option<SlotGuard>,
    origin: PlateSolveOrigin,
    run_id: String,
    progress: Arc<Mutex<SolveProgress>>,
    ps_config: Arc<PlateSolveConfig>,
    layer_caches: Vec<Arc<solvemyastro::StarCache>>,
    index_packs: Vec<Arc<solvemyastro::QuadIndex>>,
    dso: Option<Arc<DsoCatalog>>,
    cancel_flag: Arc<AtomicBool>,
    work_items: Vec<WorkItem>,
    concurrency: usize,
    total: usize,
}

impl PreparedPlateSolveBatch {
    /// The id this run's handle carries (also in `plate-solve-started` and the activity).
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

/// Phase 1 of a plate-solve batch, synchronous: claim the batch slot (key 0;
/// `Conflict` in words when it is held), load the config, pre-load frame metadata and hints so workers never
/// touch the DB, run the input gate, and choose the concurrency. The star
/// catalog layers, index packs and DSO catalog are what the caller resolved with
/// the core resolvers. Frames that fail to load or are refused by the gate become
/// per-frame failures in the run, not errors here.
pub fn prepare_plate_solve_batch(
    ctx: &ServiceContext,
    frame_ids: &[i64],
    layer_caches: Vec<Arc<solvemyastro::StarCache>>,
    index_packs: Vec<Arc<solvemyastro::QuadIndex>>,
    dso: Option<Arc<DsoCatalog>>,
    origin: PlateSolveOrigin,
) -> Result<PreparedPlateSolveBatch, ApiError> {
    let database = db(ctx)?;
    let (cancel_flag, run_id, progress) = claim_batch_slot(ctx, origin, frame_ids.len())?;
    // From here the guard frees the slot on any early return.
    let slot = SlotGuard { slots: ctx.active_plate_solves.clone(), run_id: run_id.clone() };

    let conn = database.conn();
    let ps_config = Arc::new(config::load_config(&conn));

    // Frames that fail to load become `LoadFailed` placeholders so their
    // status is reported alongside successful/failed solves.
    let mut work_items: Vec<WorkItem> = Vec::with_capacity(frame_ids.len());
    for frame_id in frame_ids {
        match load_frame_with_path(&conn, *frame_id) {
            Ok(Some((frame, file_path))) => {
                // A frame whose own analysis says its stars are streaks has
                // nothing for the solver to match, and letting it through
                // costs minutes while the autoFOV ladder is walked to its
                // end. Refused here, with the reason the UI shows.
                if let Some(reason) = service::input_gate_reason(&conn, *frame_id, &ps_config) {
                    tracing::info!(
                        frame_id = *frame_id,
                        stage = "gate",
                        outcome = "refused",
                        reason = %reason,
                        "frame refused before solving"
                    );
                    work_items.push(WorkItem::LoadFailed {
                        frame_id: *frame_id,
                        error: reason,
                        filename: Some(file_name_of(&file_path)),
                    });
                    continue;
                }
                let mut hints = extract_hints(&frame, Some(&conn));
                // Last source of a position before a blind search: what the
                // frame says it was pointed at. Only fires when the header
                // carried no usable RA/Dec at all.
                if let Some(cat) = dso.as_deref() {
                    if let Some(name) =
                        crate::plate_solve::hints::apply_object_name_fallback(&mut hints, &frame, cat)
                    {
                        tracing::debug!(
                            frame_id = *frame_id,
                            object = %name,
                            "position hint taken from the frame's object name"
                        );
                    }
                }
                work_items.push(WorkItem::Ready {
                    frame_id: *frame_id,
                    frame,
                    file_path,
                    hints,
                });
            }
            Ok(None) => {
                let e = format!("Frame {frame_id} not found");
                tracing::warn!(frame_id = *frame_id, error = %e, "failed to load frame for plate solve");
                work_items.push(WorkItem::LoadFailed {
                    frame_id: *frame_id,
                    error: e,
                    filename: None,
                });
            }
            Err(e) => {
                let e = e.to_string();
                tracing::warn!(frame_id = *frame_id, error = %e, "failed to load frame for plate solve");
                work_items.push(WorkItem::LoadFailed {
                    frame_id: *frame_id,
                    error: e,
                    filename: None,
                });
            }
        }
    }

    let total = work_items.len();
    progress.lock().unwrap_or_else(|e| e.into_inner()).total = total;

    // Choose concurrency: config override, else auto (~1 worker per 3 cores).
    // Lower cap than analysis (8 vs 16) because each solve already uses the
    // shared rayon pool for intra-frame star detection.
    let concurrency = if ps_config.batch_concurrency == 0 {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 3).max(2).min(8)
    } else {
        (ps_config.batch_concurrency as usize).clamp(1, 16)
    };
    tracing::info!(total, workers = concurrency, origin = ?origin, run_id = %run_id, "plate solve batch starting");

    Ok(PreparedPlateSolveBatch {
        slot: Some(slot),
        origin,
        run_id,
        progress,
        ps_config,
        layer_caches,
        index_packs,
        dso,
        cancel_flag,
        work_items,
        concurrency,
        total,
    })
}

/// How phases 2 and 3 ended, for the common ending of a run.
struct BatchEnd {
    solved: usize,
    failed: usize,
    total_time_ms: u64,
    error: Option<ApiError>,
}

/// Phases 2 and 3 of a plate-solve batch, blocking: solve the frames on a
/// scoped worker pool, persist the solved ones in one transaction, then end the
/// batch. The batch is ended on every path (success, cancel, failed commit,
/// missing DB, a panic outside a frame): cancel handle 0 is removed and
/// `plate-solve-complete` is emitted. A failed commit or missing DB is still
/// returned as the error, after the batch is ended; a panic inside one frame
/// only fails that frame.
pub fn run_plate_solve_batch(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    mut batch: PreparedPlateSolveBatch,
) -> Result<(), ApiError> {
    let total = batch.total;
    let start = std::time::Instant::now();
    // The run owns the slot from here; it is freed on every path, before the complete event.
    let slot = batch.slot.take();
    // Every progress event is copied into the handle's progress on its way out.
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(RecordingEmitter { inner: emitter, progress: batch.progress.clone() });
    emit_event(
        emitter.as_ref(),
        "plate-solve-started",
        &PlateSolveStartedEvent { origin: batch.origin, run_id: batch.run_id.clone(), total },
    );

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        solve_and_persist(&ctx, emitter.as_ref(), &emitter, batch, start)
    }));
    let end = match outcome {
        Ok(end) => end,
        Err(panic) => {
            // A panic outside a frame (the per-frame ones are caught in the
            // worker): no frame result is trustworthy, report every frame failed.
            let msg = crate::api::analysis::panic_message(panic.as_ref());
            tracing::error!(error = %msg, "plate solve batch worker panicked");
            BatchEnd {
                solved: 0,
                failed: total,
                total_time_ms: 0,
                error: Some(ApiError::Internal(format!("plate solve batch panicked: {msg}"))),
            }
        }
    };

    drop(slot);
    emit_event(
        emitter.as_ref(),
        "plate-solve-complete",
        &PlateSolveCompleteEvent {
            solved: end.solved,
            failed: end.failed,
            total,
            total_time_ms: end.total_time_ms,
        },
    );
    on_batch_finished(&ctx);

    match end.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn solve_and_persist(
    ctx: &Arc<ServiceContext>,
    emitter: &dyn ProgressEmitter,
    // The owned handle the pass after the batch keeps to announce its end (`zero-point-pass-complete`).
    #[cfg_attr(not(feature = "solver"), allow(unused_variables))] pass_emitter: &Arc<dyn ProgressEmitter>,
    batch: PreparedPlateSolveBatch,
    start: std::time::Instant,
) -> BatchEnd {
    let PreparedPlateSolveBatch {
        slot: _,
        origin: _,
        run_id: _,
        progress: _,
        ps_config,
        layer_caches,
        index_packs,
        dso,
        cancel_flag,
        work_items,
        concurrency,
        total,
    } = batch;

    // ── Phase 2: parallel solve on a scoped thread pool ──
    let completed = Arc::new(AtomicUsize::new(0));
    let results: Vec<WorkResult> = {
        let work = Mutex::new(work_items.into_iter());
        let results: Mutex<Vec<WorkResult>> = Mutex::new(Vec::with_capacity(total));

        std::thread::scope(|s| {
            for _ in 0..concurrency {
                s.spawn(|| loop {
                    if cancel_flag.load(Ordering::Relaxed) {
                        tracing::debug!("plate solve worker saw cancel flag, breaking");
                        break;
                    }
                    let Some(item) = work.lock().unwrap().next() else { break };
                    tracing::trace!(
                        cancelled = cancel_flag.load(Ordering::Relaxed),
                        "plate solve worker picked a frame"
                    );

                    let outcome = match item {
                        WorkItem::LoadFailed { frame_id, error, filename } => {
                            WorkResult::Failed { frame_id, error, code: None, filename }
                        }
                        WorkItem::Ready { frame_id, frame, file_path, hints } => {
                            let filename = file_name_of(&file_path);

                            // Emit "solving" as the worker picks up the frame
                            // so the UI shows forward progress even on slow solves.
                            let done_so_far = completed.load(Ordering::Relaxed);
                            emit_event(
                                emitter,
                                "plate-solve-progress",
                                &PlateSolveProgressEvent {
                                    frame_id,
                                    current: done_so_far,
                                    total,
                                    status: "solving".into(),
                                    matched_stars: None,
                                    rms_arcsec: None,
                                    error: None,
                                    failure_code: None,
                                    filename: Some(filename.clone()),
                                },
                            );

                            // Isolate per-frame panics: a single degenerate
                            // frame (e.g. a solver assertion deep in the
                            // candidate verifier) must not propagate out of
                            // the scoped thread and abort the whole batch,
                            // discarding every other frame's result. Convert
                            // a panic into a normal per-frame failure — same
                            // pattern as operation_queue / scanner. The
                            // global panic hook still logs the full backtrace.
                            let layer_refs: Vec<&solvemyastro::StarCache> =
                                layer_caches.iter().map(|a| a.as_ref()).collect();
                            let caches = solvemyastro::Caches::layered(&layer_refs);
                            let index_refs: Vec<&solvemyastro::QuadIndex> =
                                index_packs.iter().map(|a| a.as_ref()).collect();
                            let solve = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                service::solve_catalog_frame_with_hints(
                                    ctx,
                                    &frame,
                                    &file_path,
                                    &hints,
                                    &caches,
                                    &index_refs,
                                    ps_config.as_ref(),
                                    Some(cancel_flag.as_ref()),
                                )
                            }));
                            match solve {
                                Ok(Ok(result)) => WorkResult::Solved { frame_id, result, filename },
                                Ok(Err(e)) => {
                                    // Translate the structured solvemyastro failure
                                    // into a user-facing reason + code for the UI.
                                    let info = describe_solve_failure(&e);
                                    tracing::warn!(
                                        frame_id,
                                        filename = %filename,
                                        stage = "solve",
                                        outcome = "failed",
                                        code = info.code.as_deref().unwrap_or("?"),
                                        error = %info.message,
                                        "plate solve failed"
                                    );
                                    WorkResult::Failed {
                                        frame_id,
                                        error: info.message,
                                        code: info.code,
                                        filename: Some(filename),
                                    }
                                }
                                Err(panic) => {
                                    let msg = crate::api::analysis::panic_message(panic.as_ref());
                                    tracing::error!(frame_id, filename = %filename, error = %msg, "plate solve panicked");
                                    WorkResult::Failed {
                                        frame_id,
                                        error: format!("Solver crashed: {msg}"),
                                        code: Some("PANIC".to_string()),
                                        filename: Some(filename),
                                    }
                                }
                            }
                        }
                    };

                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    match &outcome {
                        WorkResult::Solved { frame_id, result, filename } => {
                            emit_event(
                                emitter,
                                "plate-solve-progress",
                                &PlateSolveProgressEvent {
                                    frame_id: *frame_id,
                                    current: done,
                                    total,
                                    status: "solved".into(),
                                    matched_stars: Some(result.matched_stars),
                                    rms_arcsec: Some(result.rms_residual_arcsec),
                                    error: None,
                                    failure_code: None,
                                    filename: Some(filename.clone()),
                                },
                            );
                        }
                        WorkResult::Failed { frame_id, error, code, filename } => {
                            emit_event(
                                emitter,
                                "plate-solve-progress",
                                &PlateSolveProgressEvent {
                                    frame_id: *frame_id,
                                    current: done,
                                    total,
                                    status: "failed".into(),
                                    matched_stars: None,
                                    rms_arcsec: None,
                                    error: Some(error.clone()),
                                    failure_code: code.clone(),
                                    filename: filename.clone(),
                                },
                            );
                        }
                    }

                    results.lock().unwrap().push(outcome);
                });
            }
        });

        results.into_inner().unwrap()
    };

    // ── Phase 3: persist all solved frames in a single DB transaction ──
    let Some(database) = ctx.db.get() else {
        tracing::error!("plate solve persist: database not initialized, batch results lost");
        return BatchEnd {
            solved: 0,
            failed: total,
            total_time_ms: 0,
            error: Some(ApiError::Internal("Database not initialized".into())),
        };
    };
    let mut solved = 0usize;
    let mut failed = 0usize;
    let mut error = None;
    // Frames whose solve was written: the pass after a solve batch measures their zero points once the transaction is
    // committed. (A new solve has already cleared each one's `zp_*`: `insert_plate_solve`.)
    let mut persisted: Vec<i64> = Vec::new();
    {
        let conn = database.conn();
        // Drop-safe: an early return or panic rolls back instead of leaking
        // an open transaction onto the pooled connection. A failed BEGIN
        // falls back to per-row autocommit — losing atomicity, never the
        // batch's computed results (audit I12: the old `?` discarded every
        // solve result on a BEGIN failure).
        let tx = match conn.unchecked_transaction() {
            Ok(tx) => Some(tx),
            Err(e) => {
                tracing::warn!(error = %e, "plate solve persist: BEGIN failed; persisting per-row");
                None
            }
        };
        for r in &results {
            match r {
                WorkResult::Solved { frame_id, result, filename } => {
                    match service::store_result(
                        &conn,
                        *frame_id,
                        result,
                        dso.as_deref(),
                        ps_config.as_ref(),
                    ) {
                        Ok(StoreOutcome::Persisted) => {
                            solved += 1;
                            persisted.push(*frame_id);
                        }
                        Ok(StoreOutcome::RejectedLowConfidence { reason }) => {
                            // The Phase-2 "solved" event already reached the UI;
                            // emit a correction so the frame flips to failed with
                            // the rejection reason (totals count it as failed too).
                            failed += 1;
                            emit_event(
                                emitter,
                                "plate-solve-progress",
                                &PlateSolveProgressEvent {
                                    frame_id: *frame_id,
                                    current: total,
                                    total,
                                    status: "failed".into(),
                                    matched_stars: None,
                                    rms_arcsec: None,
                                    error: Some(reason),
                                    failure_code: Some("REJECTED_LOW_CONFIDENCE".to_string()),
                                    filename: Some(filename.clone()),
                                },
                            );
                        }
                        Err(e) => {
                            tracing::error!(frame_id = *frame_id, error = %e, "failed to store plate solve result, solve outcome lost");
                            failed += 1;
                        }
                    }
                }
                WorkResult::Failed { .. } => {
                    failed += 1;
                }
            }
        }
        if let Some(tx) = tx {
            if let Err(e) = tx.commit() {
                tracing::error!(error = %e, "plate solve persist: COMMIT failed; batch results rolled back");
                error = Some(ApiError::Internal(e.to_string()));
            }
        }
    }

    // The pass after a solve batch: Analyze's own worker over the newly solved frames, on its own thread. Not after a
    // failed commit (the solves were rolled back).
    #[cfg(feature = "solver")]
    if error.is_none() && !persisted.is_empty() {
        crate::api::analysis::spawn_solve_zero_point_pass(Arc::clone(ctx), Arc::clone(pass_emitter), persisted);
    }

    BatchEnd {
        solved,
        failed,
        total_time_ms: start.elapsed().as_millis() as u64,
        error,
    }
}

// ========== Autofind objects ==========

// No `rename_all = "camelCase"`: the wire is snake_case, matching the TS type
// in `src/types/plate-solve.ts`.
#[derive(Clone, Serialize)]
struct AutofindProgressEvent {
    frame_id: i64,
    current: usize,
    total: usize,
    status: String,
    designation: Option<String>,
    distance_deg: Option<f64>,
    reason: Option<String>,
    frame_ra: Option<f64>,
    frame_dec: Option<f64>,
    closest_designation: Option<String>,
    closest_distance_deg: Option<f64>,
}

#[derive(Clone, Serialize)]
struct AutofindCompleteEvent {
    total: usize,
    labeled: usize,
    no_match: usize,
    already_labeled: usize,
    missing_coords: usize,
    errors: usize,
    cancelled: bool,
    total_time_ms: u64,
}

/// An object autofind after its checks: the DSO catalog is in hand, the
/// tolerance is read, the cancel handle (key 1) is registered. Hand it to
/// [`run_autofind_objects`], which always removes the handle.
pub struct PreparedAutofind {
    dso: Arc<DsoCatalog>,
    tolerance_deg: f64,
    cancel_flag: Arc<AtomicBool>,
}

/// Synchronous start of an object autofind: check the catalog database and the
/// DSO catalog the host resolved, read the tolerance from the plate-solve
/// config, and register the cancel handle (key 1; key 0 is the plate-solve batch).
pub fn prepare_autofind_objects(
    ctx: &ServiceContext,
    dso: Option<Arc<DsoCatalog>>,
) -> Result<PreparedAutofind, ApiError> {
    let database = db(ctx)?;
    let dso = dso.ok_or_else(|| ApiError::Internal("DSO catalog unavailable".into()))?;
    let tolerance_deg = config::load_config(&database.conn()).autofind_tolerance_deg;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut handles = ctx.active_plate_solves.lock().unwrap();
        handles.insert(
            1,
            PlateSolveHandle {
                cancel_flag: cancel_flag.clone(),
                origin: PlateSolveOrigin::Ui,
                run_id: new_solve_run_id(),
                progress: Arc::new(Mutex::new(SolveProgress::default())),
            },
        );
    }
    Ok(PreparedAutofind { dso, tolerance_deg, cancel_flag })
}

fn autofind_progress_status(status: &AutofindStatus) -> &'static str {
    match status {
        AutofindStatus::Processing => "processing",
        AutofindStatus::Labeled => "labeled",
        AutofindStatus::NoMatch => "no_match",
        AutofindStatus::AlreadyLabeled => "already_labeled",
        AutofindStatus::MissingCoords => "missing_coords",
        AutofindStatus::Error => "error",
    }
}

/// Fill `frame.object` from stored RA/Dec for the frames that have coordinates
/// but no object name, blocking. Uses the bundled DSO catalog with the
/// configured proximity tolerance; labels are prefixed `"Autofind: "` so the
/// origin is visible to the user.
///
/// Emits `autofind-objects-progress` per frame and `autofind-objects-complete`
/// once, on every path: the summary on success, `errors: 1` when the run failed
/// or panicked. The cancel handle (key 1) is removed on every path. A failed
/// run is still returned as the error, after the event.
pub fn run_autofind_objects(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    prepared: PreparedAutofind,
    frame_ids: Vec<i64>,
) -> Result<(), ApiError> {
    let start = std::time::Instant::now();
    let PreparedAutofind { dso, tolerance_deg, cancel_flag } = prepared;

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let database = db(&ctx)?;
        let progress = |p: AutofindProgress| {
            let status = autofind_progress_status(&p.status);
            emit_event(
                emitter.as_ref(),
                "autofind-objects-progress",
                &AutofindProgressEvent {
                    frame_id: p.frame_id,
                    current: p.current,
                    total: p.total,
                    status: status.into(),
                    designation: p.designation,
                    distance_deg: p.distance_deg,
                    reason: p.reason,
                    frame_ra: p.frame_ra,
                    frame_dec: p.frame_dec,
                    closest_designation: p.closest_designation,
                    closest_distance_deg: p.closest_distance_deg,
                },
            );
        };
        let conn = database.conn();
        object_fill::autofind_objects_from_coordinates(
            &conn,
            &dso,
            &frame_ids,
            tolerance_deg,
            cancel_flag,
            &progress,
        )
        .map_err(|e| ApiError::Internal(e.to_string()))
    }));

    {
        let mut handles = ctx.active_plate_solves.lock().unwrap();
        handles.remove(&1);
    }

    let failure_event = |total_time_ms: u64| AutofindCompleteEvent {
        total: 0,
        labeled: 0,
        no_match: 0,
        already_labeled: 0,
        missing_coords: 0,
        errors: 1,
        cancelled: false,
        total_time_ms,
    };

    match outcome {
        Ok(Ok(s)) => {
            emit_event(
                emitter.as_ref(),
                "autofind-objects-complete",
                &AutofindCompleteEvent {
                    total: s.total,
                    labeled: s.labeled,
                    no_match: s.no_match,
                    already_labeled: s.already_labeled,
                    missing_coords: s.missing_coords,
                    errors: s.errors,
                    cancelled: s.cancelled,
                    total_time_ms: start.elapsed().as_millis() as u64,
                },
            );
            Ok(())
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "autofind batch failed");
            // Without the event the progress UI never resolves.
            emit_event(
                emitter.as_ref(),
                "autofind-objects-complete",
                &failure_event(start.elapsed().as_millis() as u64),
            );
            Err(e)
        }
        Err(panic) => {
            let msg = crate::api::analysis::panic_message(panic.as_ref());
            tracing::error!(error = %msg, "autofind batch worker panicked");
            emit_event(emitter.as_ref(), "autofind-objects-complete", &failure_event(0));
            Err(ApiError::Internal(format!("autofind batch panicked: {msg}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every emitted `(event_name, payload)` pair.
    #[derive(Default)]
    struct CapturingEmitter(Mutex<Vec<(String, serde_json::Value)>>);

    impl ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    impl CapturingEmitter {
        fn named(&self, name: &str) -> Vec<serde_json::Value> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .collect()
        }
    }

    fn test_ctx() -> (tempfile::TempDir, Arc<ServiceContext>) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, Arc::new(ctx))
    }

    /// A batch over frame ids that are not in the catalog fails every frame, and
    /// still ends: the complete event is emitted and cancel handle 0 is removed.
    #[test]
    fn plate_solve_batch_over_missing_frames_ends_with_every_frame_failed() {
        // The run ends with `on_batch_finished`, which sets the process-global
        // auto-publish dirty flag: hold the lock its own tests hold, and leave
        // the flag as it was found.
        let _guard = crate::api::collab_autopublish::test_lock();
        let (_tmp, ctx) = test_ctx();
        let emitter = Arc::new(CapturingEmitter::default());

        let prepared =
            prepare_plate_solve_batch(
                &ctx,
                &[9001, 9002, 9003],
                Vec::new(),
                Vec::new(),
                None,
                PlateSolveOrigin::Ui,
            )
            .expect("prepare");
        assert!(ctx.active_plate_solves.lock().unwrap().contains_key(&0));

        run_plate_solve_batch(ctx.clone(), emitter.clone(), prepared).expect("run");
        crate::api::collab_autopublish::reset_dirty_state_for_test();

        let complete = emitter.named("plate-solve-complete");
        assert_eq!(complete.len(), 1);
        assert_eq!(complete[0]["total"], 3);
        assert_eq!(complete[0]["failed"], 3);
        assert_eq!(complete[0]["solved"], 0);
        assert!(!ctx.active_plate_solves.lock().unwrap().contains_key(&0));
        let failed_rows = emitter
            .named("plate-solve-progress")
            .iter()
            .filter(|p| p["status"] == "failed")
            .count();
        assert_eq!(failed_rows, 3);
    }

    /// An autofind over frame ids that are not in the catalog counts them as
    /// per-frame errors, emits the complete event once and removes handle 1.
    #[test]
    fn autofind_over_missing_frames_emits_complete_and_removes_the_handle() {
        let (_tmp, ctx) = test_ctx();
        let emitter = Arc::new(CapturingEmitter::default());
        let dso = Arc::new(DsoCatalog::load().expect("bundled DSO catalog"));

        let prepared = prepare_autofind_objects(&ctx, Some(dso)).expect("prepare");
        {
            let solves = ctx.active_plate_solves.lock().unwrap();
            let handle = solves.get(&1).expect("autofind holds key 1");
            assert_eq!(handle.origin, PlateSolveOrigin::Ui);
            assert!(!handle.run_id.is_empty());
        }

        run_autofind_objects(ctx.clone(), emitter.clone(), prepared, vec![9001, 9002])
            .expect("run");

        let complete = emitter.named("autofind-objects-complete");
        assert_eq!(complete.len(), 1);
        assert_eq!(complete[0]["total"], 2);
        // A frame id with no row is a per-frame lookup error, counted and skipped.
        assert_eq!(complete[0]["errors"], 2);
        assert_eq!(complete[0]["labeled"], 0);
        assert!(!ctx.active_plate_solves.lock().unwrap().contains_key(&1));
    }

    fn prepare_for(
        ctx: &ServiceContext,
        frame_ids: &[i64],
        origin: PlateSolveOrigin,
    ) -> Result<PreparedPlateSolveBatch, ApiError> {
        prepare_plate_solve_batch(ctx, frame_ids, Vec::new(), Vec::new(), None, origin)
    }

    fn slot_run_id(ctx: &ServiceContext) -> Option<String> {
        ctx.active_plate_solves.lock().unwrap().get(&0).map(|h| h.run_id.clone())
    }

    /// A second batch is refused in words while slot 0 is held; the first run
    /// keeps its handle (same run id) and its text names who holds the slot.
    #[test]
    fn second_batch_is_refused_not_replaced() {
        let (_tmp, ctx) = test_ctx();
        let first = prepare_for(&ctx, &[9001, 9002], PlateSolveOrigin::Assistant).expect("first");
        let held = slot_run_id(&ctx).expect("slot held");
        assert_eq!(held, first.run_id());

        let err = prepare_for(&ctx, &[9003], PlateSolveOrigin::Ui).err().expect("refused");
        match err {
            ApiError::Conflict(m) => {
                assert!(
                    m.starts_with("A plate solve started from the assistant is running"),
                    "refusal text: {m}"
                );
                assert!(m.contains("(0 of 2 frames)"), "refusal text: {m}");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        assert_eq!(slot_run_id(&ctx).as_deref(), Some(held.as_str()));
    }

    #[test]
    fn assistant_refused_while_ui_runs() {
        let (_tmp, ctx) = test_ctx();
        let _ui = prepare_for(&ctx, &[9001], PlateSolveOrigin::Ui).expect("ui");
        let err = prepare_for(&ctx, &[9002], PlateSolveOrigin::Assistant).err().expect("refused");
        match err {
            ApiError::Conflict(m) => {
                assert!(
                    m.starts_with("A plate solve batch started in Athenaeum is running"),
                    "refusal text: {m}"
                );
                assert!(m.contains("(0 of 1 frames)"), "refusal text: {m}");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn idle_activity() {
        let (_tmp, ctx) = test_ctx();
        let a = plate_solve_activity(&ctx);
        assert!(!a.running);
        assert!(a.origin.is_none() && a.run_id.is_none() && a.current_file.is_none());
        assert_eq!((a.done, a.total), (0, 0));
    }

    #[test]
    fn activity_reports_the_running_run() {
        let (_tmp, ctx) = test_ctx();
        let batch = prepare_for(&ctx, &[9001, 9002, 9003], PlateSolveOrigin::Assistant).expect("prepare");
        {
            let solves = ctx.active_plate_solves.lock().unwrap();
            let mut p = solves.get(&0).unwrap().progress.lock().unwrap();
            p.done = 1;
            p.current_file = Some("light_0002.fits".into());
        }
        let a = plate_solve_activity(&ctx);
        assert!(a.running);
        assert_eq!(a.origin, Some(PlateSolveOrigin::Assistant));
        assert_eq!(a.run_id.as_deref(), Some(batch.run_id()));
        assert_eq!((a.done, a.total), (1, 3));
        assert_eq!(a.current_file.as_deref(), Some("light_0002.fits"));
        let wire = serde_json::to_value(&a).unwrap();
        assert_eq!(wire["origin"], "assistant");
        assert_eq!(wire["runId"], batch.run_id());
        assert_eq!(wire["currentFile"], "light_0002.fits");
    }

    #[test]
    fn cancel_sets_the_flag_of_slot_zero() {
        let (_tmp, ctx) = test_ctx();
        assert!(!cancel_plate_solve(&ctx), "nothing to cancel");
        let _batch = prepare_for(&ctx, &[9001], PlateSolveOrigin::Ui).expect("prepare");
        let flag = ctx.active_plate_solves.lock().unwrap().get(&0).unwrap().cancel_flag.clone();
        assert!(!flag.load(Ordering::Relaxed));
        assert!(cancel_plate_solve(&ctx));
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn resolvers_report_a_missing_catalog_in_the_same_words() {
        let (tmp, ctx) = test_ctx();
        let err = resolve_layer_caches(&ctx).err().expect("no catalog installed");
        let root = tmp.path().join("catalogs").join("smac_gaia");
        let expected = format!(
            "solvemyastro star catalog not found under {}. \
             Please download the Gaia DR3 prebuilt catalog from Settings \u{2192} Plate Solving.",
            root.display()
        );
        assert_eq!(err.to_string(), expected);
        assert!(matches!(err, ApiError::Invalid(_)));
        assert!(resolve_index_packs(&ctx).is_empty());
    }

    #[test]
    fn started_event_names_its_origin() {
        let _guard = crate::api::collab_autopublish::test_lock();
        let (_tmp, ctx) = test_ctx();
        let emitter = Arc::new(CapturingEmitter::default());
        let prepared = prepare_for(&ctx, &[9001, 9002], PlateSolveOrigin::Assistant).expect("prepare");
        let run_id = prepared.run_id().to_string();
        run_plate_solve_batch(ctx.clone(), emitter.clone(), prepared).expect("run");
        crate::api::collab_autopublish::reset_dirty_state_for_test();

        let started = emitter.named("plate-solve-started");
        assert_eq!(started.len(), 1);
        assert_eq!(started[0]["origin"], "assistant");
        assert_eq!(started[0]["runId"], run_id.as_str());
        assert!(run_id.starts_with("solve-"), "run id: {run_id}");
        assert_eq!(started[0]["total"], 2);
    }

    /// The slot's handle knows the batch's size from the moment it is claimed,
    /// before any frame is loaded, so a refusal then is not "(0 of 0 frames)".
    #[test]
    fn a_refusal_while_preparing_names_the_batch_size() {
        let (_tmp, ctx) = test_ctx();
        let (_flag, run_id, _progress) =
            claim_batch_slot(&ctx, PlateSolveOrigin::Assistant, 5).expect("claim");
        assert!(run_id.starts_with("solve-"), "run id: {run_id}");
        let err = claim_batch_slot(&ctx, PlateSolveOrigin::Ui, 1).err().expect("refused");
        match err {
            ApiError::Conflict(m) => assert!(m.contains("(0 of 5 frames)"), "refusal text: {m}"),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    /// A second Athenaeum run is refused with its own words (the Ui + Ui branch).
    #[test]
    fn ui_refused_while_ui_runs() {
        let (_tmp, ctx) = test_ctx();
        let _first = prepare_for(&ctx, &[9001, 9002], PlateSolveOrigin::Ui).expect("first");
        let err = prepare_for(&ctx, &[9003], PlateSolveOrigin::Ui).err().expect("refused");
        match err {
            ApiError::Conflict(m) => assert_eq!(
                m,
                "A plate solve batch is already running (0 of 2 frames). Wait for it or cancel it."
            ),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    /// A batch dropped before `run` (an error between the two, a cancelled
    /// plan) frees the slot: the next prepare is `Ok`.
    #[test]
    fn dropped_batch_frees_the_slot() {
        let (_tmp, ctx) = test_ctx();
        let first = prepare_for(&ctx, &[9001], PlateSolveOrigin::Assistant).expect("first");
        assert!(slot_run_id(&ctx).is_some());
        drop(first);
        assert!(slot_run_id(&ctx).is_none());
        let second = prepare_for(&ctx, &[9002], PlateSolveOrigin::Ui).expect("slot is free again");
        assert_eq!(slot_run_id(&ctx).as_deref(), Some(second.run_id()));
    }

    /// Two prepares at once: exactly one gets the slot.
    #[test]
    fn check_and_insert_are_atomic() {
        let (_tmp, ctx) = test_ctx();
        let barrier = std::sync::Barrier::new(2);
        let results: Vec<Result<PreparedPlateSolveBatch, ApiError>> = std::thread::scope(|s| {
            let handles: Vec<_> = [PlateSolveOrigin::Ui, PlateSolveOrigin::Assistant]
                .into_iter()
                .map(|origin| {
                    let (ctx, barrier) = (&ctx, &barrier);
                    s.spawn(move || {
                        barrier.wait();
                        prepare_for(ctx, &[], origin)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread")).collect()
        });
        let ok = results.iter().filter(|r| r.is_ok()).count();
        let conflicts =
            results.iter().filter(|r| matches!(r, Err(ApiError::Conflict(_)))).count();
        assert_eq!((ok, conflicts), (1, 1));
    }

    /// The recorder copies each `plate-solve-progress` event into the handle's
    /// progress and forwards every event unchanged.
    #[test]
    fn recorder_copies_progress_events() {
        let progress = Arc::new(Mutex::new(SolveProgress { total: 3, ..Default::default() }));
        let inner = Arc::new(CapturingEmitter::default());
        let rec = RecordingEmitter { inner: inner.clone(), progress: progress.clone() };
        let ev = |frame_id: i64, current: usize, status: &str, file: &str, error: Option<&str>, code: Option<&str>| {
            serde_json::json!({
                "frame_id": frame_id, "current": current, "total": 3, "status": status,
                "filename": file, "error": error, "failure_code": code,
            })
        };
        rec.emit_json("plate-solve-progress", ev(1, 0, "solving", "a.fits", None, None));
        assert_eq!(progress.lock().unwrap().current_file.as_deref(), Some("a.fits"));
        rec.emit_json("plate-solve-progress", ev(1, 1, "solved", "a.fits", None, None));
        rec.emit_json("plate-solve-progress", ev(2, 2, "failed", "b.fits", Some("no stars"), Some("X")));
        rec.emit_json("other-event", serde_json::json!({}));
        {
            let p = progress.lock().unwrap();
            assert_eq!((p.done, p.total, p.solved), (2, 3, 1));
            assert_eq!(p.failures, vec![("b.fits".to_string(), "no stars".to_string())]);
        }
        // The low-confidence correction flips a counted solve to a failure.
        rec.emit_json(
            "plate-solve-progress",
            ev(1, 3, "failed", "a.fits", Some("too few"), Some("REJECTED_LOW_CONFIDENCE")),
        );
        let p = progress.lock().unwrap();
        assert_eq!((p.done, p.solved, p.failures.len()), (3, 0, 2));
        assert_eq!(inner.named("plate-solve-progress").len(), 4);
        assert_eq!(inner.named("other-event").len(), 1);
    }
}
