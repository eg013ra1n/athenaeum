//! Plate-solve commands both hosts call: the batch solve (`prepare_plate_solve_batch`
//! + `run_plate_solve_batch`), the object autofind (`prepare_autofind_objects` +
//! `run_autofind_objects`), the catalog download, and the trigger the auto-publish
//! worker gets after a batch (collab v3 wave 2, Task 10). The hosts keep what they
//! resolve from their own state (star-catalog layers, index packs, the DSO catalog)
//! and keep their own waiting: the desktop awaits a run, the web starts it and
//! returns.

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
use crate::plate_solve::{describe_solve_failure, SolveHints, StoreOutcome};
use crate::services::PlateSolveHandle;

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
pub async fn download_catalog_layers(
    ctx: &ServiceContext,
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

    result
        .map(|p| p.display().to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))
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
/// cancel handle (key 0) registered. Hand it to [`run_plate_solve_batch`],
/// which always ends the batch.
pub struct PreparedPlateSolveBatch {
    ps_config: Arc<PlateSolveConfig>,
    layer_caches: Vec<Arc<solvemyastro::StarCache>>,
    index_packs: Vec<Arc<solvemyastro::QuadIndex>>,
    dso: Option<Arc<DsoCatalog>>,
    cancel_flag: Arc<AtomicBool>,
    work_items: Vec<WorkItem>,
    concurrency: usize,
    total: usize,
}

/// Phase 1 of a plate-solve batch, synchronous: load the config, register the
/// cancel handle (key 0), pre-load frame metadata and hints so workers never
/// touch the DB, run the input gate, and choose the concurrency. The star
/// catalog layers, index packs and DSO catalog are what the host resolved from
/// its own state. Frames that fail to load or are refused by the gate become
/// per-frame failures in the run, not errors here.
pub fn prepare_plate_solve_batch(
    ctx: &ServiceContext,
    frame_ids: &[i64],
    layer_caches: Vec<Arc<solvemyastro::StarCache>>,
    index_packs: Vec<Arc<solvemyastro::QuadIndex>>,
    dso: Option<Arc<DsoCatalog>>,
) -> Result<PreparedPlateSolveBatch, ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    let ps_config = Arc::new(config::load_config(&conn));
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Register the cancel handle (key 0 = plate-solve batch).
    {
        let mut solves = ctx.active_plate_solves.lock().unwrap();
        solves.insert(0, PlateSolveHandle { cancel_flag: cancel_flag.clone() });
    }

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

    // Choose concurrency: config override, else auto (~1 worker per 3 cores).
    // Lower cap than analysis (8 vs 16) because each solve already uses the
    // shared rayon pool for intra-frame star detection.
    let concurrency = if ps_config.batch_concurrency == 0 {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 3).max(2).min(8)
    } else {
        (ps_config.batch_concurrency as usize).clamp(1, 16)
    };
    tracing::info!(total, workers = concurrency, "plate solve batch starting");

    Ok(PreparedPlateSolveBatch {
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

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
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
    batch: PreparedPlateSolveBatch,
) -> Result<(), ApiError> {
    let total = batch.total;
    let start = std::time::Instant::now();

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        solve_and_persist(&ctx, emitter.as_ref(), batch, start)
    }));
    let end = match outcome {
        Ok(end) => end,
        Err(panic) => {
            // A panic outside a frame (the per-frame ones are caught in the
            // worker): no frame result is trustworthy, report every frame failed.
            let msg = panic_message(panic.as_ref());
            tracing::error!(error = %msg, "plate solve batch worker panicked");
            BatchEnd {
                solved: 0,
                failed: total,
                total_time_ms: 0,
                error: Some(ApiError::Internal(format!("plate solve batch panicked: {msg}"))),
            }
        }
    };

    {
        let mut solves = ctx.active_plate_solves.lock().unwrap();
        solves.remove(&0);
    }
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
    batch: PreparedPlateSolveBatch,
    start: std::time::Instant,
) -> BatchEnd {
    let PreparedPlateSolveBatch {
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
                                    let msg = panic_message(panic.as_ref());
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
                        Ok(StoreOutcome::Persisted) => solved += 1,
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
        handles.insert(1, PlateSolveHandle { cancel_flag: cancel_flag.clone() });
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
            let msg = panic_message(panic.as_ref());
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
            prepare_plate_solve_batch(&ctx, &[9001, 9002, 9003], Vec::new(), Vec::new(), None)
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
        assert!(ctx.active_plate_solves.lock().unwrap().contains_key(&1));

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
}
