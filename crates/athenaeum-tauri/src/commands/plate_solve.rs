use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{Emitter, State};

use athenaeum_core::plate_solve::config::{self, PlateSolveConfig};
use athenaeum_core::plate_solve::dso_lookup::DsoCatalog;
use athenaeum_core::plate_solve::hints::extract_hints;
use athenaeum_core::plate_solve::service::{self, SolveResult};
use athenaeum_core::plate_solve::{describe_solve_failure, storage, SolveHints, StoreOutcome};
use athenaeum_core::services::PlateSolveHandle;

use super::AppState;

// ── solvemyastro star-cache helpers ─────────────────────────────────────────

/// Resolve and open the optional bright sub-catalog. Honours an explicit
/// `PlateSolveConfig::bright_cache_path` if set; otherwise tries the
/// convention path `<app-data>/catalogs/smac_gaia_bright/`. Returns
/// `None` (with a log line) if neither exists — never fatal: the
/// solver falls back to the deep cache.
pub(super) fn require_bright_cache(state: &AppState) -> Option<Arc<solvemyastro::StarCache>> {
    // Fast path: already open.
    {
        let guard = state.ctx.bright_cache.read().unwrap();
        if let Some(ref bc) = *guard {
            return Some(bc.clone());
        }
    }

    let db = state.ctx.db.get()?;
    let ps_config = config::load_config(&db.conn());

    // Resolve candidate path: explicit config override > convention.
    let cache_dir: std::path::PathBuf = match &ps_config.bright_cache_path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let db_path = db.path().to_path_buf();
            let parent = db_path.parent()?;
            parent.join("catalogs").join("smac_gaia_bright")
        }
    };

    if !cache_dir.exists() {
        eprintln!(
            "plate_solve: no bright sub-catalog at {} — using deep cache only",
            cache_dir.display()
        );
        return None;
    }
    match solvemyastro::StarCache::open(&cache_dir) {
        Ok(bc) => {
            let arc = Arc::new(bc);
            eprintln!(
                "plate_solve: opened bright sub-catalog at {} ({} stars)",
                cache_dir.display(),
                arc.star_count()
            );
            let mut guard = state.ctx.bright_cache.write().unwrap();
            *guard = Some(arc.clone());
            Some(arc)
        }
        Err(e) => {
            eprintln!(
                "plate_solve: failed to open bright cache at {}: {e} — using deep only",
                cache_dir.display()
            );
            None
        }
    }
}

/// Resolve the smac_gaia star-cache directory from the app-data catalogs dir,
/// open `StarCache`, cache it in state, and return an `Arc` to it.
///
/// The path mirrors Phase 1's build step: `<app-data>/catalogs/smac_gaia/`.
pub(super) fn require_star_cache(state: &AppState) -> Result<Arc<solvemyastro::StarCache>, String> {
    // Fast path: already open.
    {
        let guard = state.ctx.star_cache.read().unwrap();
        if let Some(ref sc) = *guard {
            return Ok(sc.clone());
        }
    }

    // Slow path: locate the stars.smac file and open the cache.
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let db_path = db.path().to_path_buf();
    let parent = db_path
        .parent()
        .ok_or("Cannot determine app data directory")?;
    let cache_dir = parent.join("catalogs").join("smac_gaia");
    if !cache_dir.exists() {
        return Err(format!(
            "solvemyastro star cache not found at {}. \
             Please download the Gaia DR3 prebuilt catalog from Settings → Plate Solving.",
            cache_dir.display()
        ));
    }
    let sc = solvemyastro::StarCache::open(&cache_dir)
        .map_err(|e| format!("Failed to open star cache at {}: {e}", cache_dir.display()))?;
    let arc = Arc::new(sc);
    {
        let mut guard = state.ctx.star_cache.write().unwrap();
        *guard = Some(arc.clone());
    }
    Ok(arc)
}

/// Lazy-load the DSO catalog once and cache it in the ServiceContext.
fn get_dso_catalog(state: &AppState) -> Option<Arc<DsoCatalog>> {
    {
        let guard = state.ctx.dso_catalog.read().unwrap();
        if let Some(ref cat) = *guard {
            return Some(cat.clone());
        }
    }
    match DsoCatalog::load() {
        Ok(cat) => {
            let arc = Arc::new(cat);
            let mut guard = state.ctx.dso_catalog.write().unwrap();
            *guard = Some(arc.clone());
            Some(arc)
        }
        Err(e) => {
            eprintln!("plate_solve: failed to load DSO catalog: {e}");
            None
        }
    }
}

// ========== Config Commands ==========

#[tauri::command]
pub async fn get_plate_solve_config(
    state: State<'_, AppState>,
) -> Result<PlateSolveConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    Ok(config::load_config(&conn))
}

#[tauri::command]
pub async fn set_plate_solve_config(
    state: State<'_, AppState>,
    config: PlateSolveConfig,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    config::save_config(&conn, &config).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reset_plate_solve_config(
    state: State<'_, AppState>,
) -> Result<PlateSolveConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let default_config = PlateSolveConfig::default();
    config::save_config(&conn, &default_config).map_err(|e| e.to_string())?;
    Ok(default_config)
}

// ========== Solve Commands ==========

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

#[tauri::command]
pub async fn plate_solve_frame(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<storage::PlateSolveRecord, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let ps_config = config::load_config(&conn);
    let star_cache = require_star_cache(&state)?;
    let bright_cache = require_bright_cache(&state);
    let dso = get_dso_catalog(&state);

    let (frame, file_path) = load_frame_with_path(&conn, frame_id)?;

    let result = service::solve_frame_tiered(
        &frame, &file_path, &conn, &star_cache,
        bright_cache.as_deref(),
        &ps_config,
    )
    .map_err(|e| describe_solve_failure(&e).message)?;

    match service::store_result(&conn, frame_id, &result, dso.as_deref(), &ps_config)
        .map_err(|e| e.to_string())?
    {
        StoreOutcome::Persisted => {}
        StoreOutcome::RejectedLowConfidence { reason } => return Err(reason),
    }

    storage::get_plate_solve(&conn, frame_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Failed to read back plate solve result".to_string())
}

#[tauri::command]
pub async fn plate_solve_batch(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    frame_ids: Vec<i64>,
) -> Result<(), String> {
    // ── Phase 1: load everything on the main thread (holds DB lock) ──
    let (ps_config, star_cache, bright_cache, dso, cancel_flag, work_items) = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        let ps_config = Arc::new(config::load_config(&conn));
        let star_cache = require_star_cache(&state)?;
        let bright_cache = require_bright_cache(&state);
        let dso = get_dso_catalog(&state);
        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Register the cancel handle (key 0 = plate-solve batch).
        {
            let mut solves = state.ctx.active_plate_solves.lock().unwrap();
            solves.insert(0, PlateSolveHandle { cancel_flag: cancel_flag.clone() });
        }

        // Pre-load frame metadata + hints so workers never touch the DB.
        // Frames that fail to load become (frame_id, Err) placeholders so
        // their status is reported alongside successful/failed solves.
        let mut work_items: Vec<WorkItem> = Vec::with_capacity(frame_ids.len());
        for frame_id in &frame_ids {
            match load_frame_with_path(&conn, *frame_id) {
                Ok((frame, file_path)) => {
                    let hints = extract_hints(&frame, Some(&conn));
                    work_items.push(WorkItem::Ready {
                        frame_id: *frame_id,
                        frame,
                        file_path,
                        hints,
                    });
                }
                Err(e) => {
                    eprintln!("plate_solve: failed to load frame {frame_id}: {e}");
                    work_items.push(WorkItem::LoadFailed {
                        frame_id: *frame_id,
                        error: e,
                    });
                }
            }
        }

        (ps_config, star_cache, bright_cache, dso, cancel_flag, work_items)
    };

    let total = work_items.len();
    let start = std::time::Instant::now();

    // Choose concurrency: config override, else auto (~1 worker per 3 cores).
    // Lower cap than analysis (8 vs 16) because each solve already uses the
    // shared rayon pool for intra-frame star detection.
    let concurrency = if ps_config.batch_concurrency == 0 {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 3).max(2).min(8)
    } else {
        (ps_config.batch_concurrency as usize).clamp(1, 16)
    };
    eprintln!(
        "plate_solve: batch starting ({} frames, {} workers)",
        total, concurrency
    );

    // ── Phase 2: parallel solve on a scoped thread pool ──
    let completed = Arc::new(AtomicUsize::new(0));
    let app_workers = app.clone();
    let cancel_worker = Arc::clone(&cancel_flag);
    let ps_config_arc = Arc::clone(&ps_config);
    let star_cache_arc = Arc::clone(&star_cache);
    let bright_cache_arc = bright_cache.clone();

    let results: Vec<WorkResult> = tokio::task::spawn_blocking(move || {
        let work = Mutex::new(work_items.into_iter());
        let results: Mutex<Vec<WorkResult>> = Mutex::new(Vec::with_capacity(total));

        std::thread::scope(|s| {
            for _ in 0..concurrency {
                s.spawn(|| loop {
                    if cancel_worker.load(Ordering::Relaxed) {
                        eprintln!("[cancel-diag] worker saw cancel flag -> breaking");
                        break;
                    }
                    let Some(item) = work.lock().unwrap().next() else { break };
                    eprintln!(
                        "[cancel-diag] worker picked a frame (cancel flag = {})",
                        cancel_worker.load(Ordering::Relaxed)
                    );

                    let outcome = match item {
                        WorkItem::LoadFailed { frame_id, error } => {
                            WorkResult::Failed { frame_id, error, code: None, filename: None }
                        }
                        WorkItem::Ready { frame_id, frame, file_path, hints } => {
                            let filename = std::path::Path::new(&file_path)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| file_path.clone());

                            // Emit "solving" as the worker picks up the frame
                            // so the UI shows forward progress even on slow solves.
                            let done_so_far = completed.load(Ordering::Relaxed);
                            let _ = app_workers.emit(
                                "plate-solve-progress",
                                PlateSolveProgressEvent {
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
                            let solve = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                service::solve_frame_with_hints(
                                    &frame,
                                    &file_path,
                                    &hints,
                                    star_cache_arc.as_ref(),
                                    bright_cache_arc.as_deref(),
                                    ps_config_arc.as_ref(),
                                    Some(cancel_worker.as_ref()),
                                )
                            }));
                            match solve {
                                Ok(Ok(result)) => WorkResult::Solved { frame_id, result, filename },
                                Ok(Err(e)) => {
                                    // Translate the structured solvemyastro failure
                                    // into a user-facing reason + code for the UI.
                                    let info = describe_solve_failure(&e);
                                    eprintln!(
                                        "plate_solve: solve failed for {filename} (frame {frame_id}): {} [{}]",
                                        info.message,
                                        info.code.as_deref().unwrap_or("?")
                                    );
                                    WorkResult::Failed {
                                        frame_id,
                                        error: info.message,
                                        code: info.code,
                                        filename: Some(filename),
                                    }
                                }
                                Err(panic) => {
                                    let msg = panic
                                        .downcast_ref::<&str>()
                                        .map(|s| s.to_string())
                                        .or_else(|| panic.downcast_ref::<String>().cloned())
                                        .unwrap_or_else(|| "unknown panic".to_string());
                                    eprintln!(
                                        "plate_solve: solve PANICKED for {filename} (frame {frame_id}): {msg}"
                                    );
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
                            let _ = app_workers.emit(
                                "plate-solve-progress",
                                PlateSolveProgressEvent {
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
                            let _ = app_workers.emit(
                                "plate-solve-progress",
                                PlateSolveProgressEvent {
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
    })
    .await
    .map_err(|e| format!("plate solve batch panicked: {e}"))?;

    // ── Phase 3: persist all solved frames in a single DB transaction ──
    let mut solved = 0usize;
    let mut failed = 0usize;
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
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
                            let _ = app.emit(
                                "plate-solve-progress",
                                PlateSolveProgressEvent {
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
                            eprintln!(
                                "plate_solve: failed to store result for frame {frame_id}: {e}"
                            );
                            failed += 1;
                        }
                    }
                }
                WorkResult::Failed { .. } => {
                    failed += 1;
                }
            }
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    }

    // Clean up the cancel handle.
    {
        let mut solves = state.ctx.active_plate_solves.lock().unwrap();
        solves.remove(&0);
    }

    let _ = app.emit(
        "plate-solve-complete",
        PlateSolveCompleteEvent {
            solved,
            failed,
            total,
            total_time_ms: start.elapsed().as_millis() as u64,
        },
    );

    Ok(())
}

/// Work item passed from the main (DB) thread to parallel workers.
enum WorkItem {
    Ready {
        frame_id: i64,
        frame: athenaeum_core::models::Frame,
        file_path: String,
        hints: SolveHints,
    },
    LoadFailed {
        frame_id: i64,
        error: String,
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

#[tauri::command]
pub async fn cancel_plate_solve(state: State<'_, AppState>) -> Result<(), String> {
    let solves = state.ctx.active_plate_solves.lock().unwrap();
    eprintln!(
        "[cancel-diag] cancel_plate_solve invoked; active handle keys = {:?}",
        solves.keys().collect::<Vec<_>>()
    );
    if let Some(handle) = solves.get(&0) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
        eprintln!("[cancel-diag] key 0 cancel flag set TRUE");
    } else {
        eprintln!("[cancel-diag] NO handle at key 0 — backend not told to stop");
    }
    Ok(())
}

// ========== Autofind Object Commands ==========

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

/// Batch: fill `frame.object` from stored RA/Dec for frames that have
/// coordinates but no object name. Uses the bundled DSO catalog with a
/// tight 0.2° proximity tolerance. Labels are prefixed with "Autofind: "
/// so the origin is visible to the user.
///
/// Emits `autofind-objects-progress` for each frame and
/// `autofind-objects-complete` once finished. Does NOT return a value —
/// callers should listen for the complete event for the summary.
#[tauri::command]
pub async fn autofind_objects_from_coordinates(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    frame_ids: Vec<i64>,
) -> Result<(), String> {
    use athenaeum_core::plate_solve::object_fill::{
        self, AutofindProgress, AutofindStatus,
    };

    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let dso = get_dso_catalog(&state).ok_or("DSO catalog unavailable")?;
    let tolerance_deg = config::load_config(&db.conn()).autofind_tolerance_deg;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    // Key 1 = autofind batch (key 0 is plate_solve_batch).
    {
        let mut handles = state.ctx.active_plate_solves.lock().unwrap();
        handles.insert(1, PlateSolveHandle { cancel_flag: cancel_flag.clone() });
    }

    let app_clone = app.clone();
    let progress = move |p: AutofindProgress| {
        let status = match p.status {
            AutofindStatus::Processing => "processing",
            AutofindStatus::Labeled => "labeled",
            AutofindStatus::NoMatch => "no_match",
            AutofindStatus::AlreadyLabeled => "already_labeled",
            AutofindStatus::MissingCoords => "missing_coords",
            AutofindStatus::Error => "error",
        };
        let _ = app_clone.emit(
            "autofind-objects-progress",
            AutofindProgressEvent {
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

    let start = std::time::Instant::now();
    let summary_result = {
        let conn = db.conn();
        object_fill::autofind_objects_from_coordinates(
            &conn,
            &dso,
            &frame_ids,
            tolerance_deg,
            cancel_flag,
            &progress,
        )
    };

    {
        let mut handles = state.ctx.active_plate_solves.lock().unwrap();
        handles.remove(&1);
    }

    let summary = summary_result.map_err(|e| {
        eprintln!("autofind: {e}");
        e.to_string()
    })?;

    let _ = app.emit(
        "autofind-objects-complete",
        AutofindCompleteEvent {
            total: summary.total,
            labeled: summary.labeled,
            no_match: summary.no_match,
            already_labeled: summary.already_labeled,
            missing_coords: summary.missing_coords,
            errors: summary.errors,
            cancelled: summary.cancelled,
            total_time_ms: start.elapsed().as_millis() as u64,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn cancel_autofind_objects(state: State<'_, AppState>) -> Result<(), String> {
    let handles = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = handles.get(&1) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub async fn get_plate_solve_result(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<Option<storage::PlateSolveRecord>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    storage::get_plate_solve(&conn, frame_id).map_err(|e| e.to_string())
}

/// Delete the `plate_solves` row for a frame. Called by the metadata pane's
/// "Revert WCS to FITS header" action so the stored WCS doesn't outlive the
/// `frames.ra/dec/rotation/objctra/objctdec` columns that were just cleared
/// — otherwise the pane's Plate-solve result card would render stale data.
#[tauri::command]
pub async fn delete_plate_solve_for_frame(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    storage::delete_plate_solve(&conn, frame_id).map_err(|e| e.to_string())
}

/// Report the installed solver star catalog (the solvemyastro `stars.smac`
/// deep cache). The shape is unchanged so the frontend keeps working; only the
/// data source moved from the legacy `CatalogEngine` to the smac cache.
#[tauri::command]
pub async fn get_catalog_status(
    state: State<'_, AppState>,
) -> Result<Vec<CatalogStatusInfo>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let db_path = db.path().to_path_buf();
    let parent = db_path
        .parent()
        .ok_or("Cannot determine app data directory")?;
    let smac_dir = parent.join("catalogs").join("smac_gaia");

    // Opening the cache memory-maps the header; if `stars.smac` is absent the
    // catalog is simply not installed yet (the download command provides it).
    let (installed, star_count_approx, epoch) = match solvemyastro::StarCache::open(&smac_dir) {
        Ok(cache) => (true, cache.star_count(), cache.catalog_epoch()),
        Err(_) => (false, 0, 2016.0),
    };

    Ok(vec![CatalogStatusInfo {
        name: "Gaia DR3 (stars.smac)".to_string(),
        installed,
        epoch,
        star_count_approx,
        // Nominal solver depth (SolveConfig::catalog_mag_limit); the on-disk
        // cache carries no explicit limit in its header.
        mag_limit: 19.0,
    }])
}

#[derive(Clone, Serialize)]
pub struct CatalogStatusInfo {
    pub name: String,
    pub installed: bool,
    pub epoch: f64,
    pub star_count_approx: u64,
    pub mag_limit: f32,
}

// ========== Catalog Download ==========

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogDownloadProgress {
    phase: String,
    current: usize,
    total: usize,
    percent: f64,
}

#[tauri::command]
pub async fn download_gaia_dr3_prebuilt_catalog(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let db_path = db.path().to_path_buf();
    let app_data_dir = db_path
        .parent()
        .ok_or("Cannot determine app data directory")?
        .to_path_buf();

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let app_clone = app.clone();

    let result = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::gaia_prebuilt::download_gaia_dr3_prebuilt(
            &app_data_dir,
            cancel_flag,
            &|progress| {
                use athenaeum_core::catalog::gaia_prebuilt::GaiaPrebuiltProgress as P;
                let event = match progress {
                    P::Downloading { received, total } => CatalogDownloadProgress {
                        phase: "downloading".into(),
                        current: received as usize,
                        total: total as usize,
                        percent: if total > 0 {
                            received as f64 / total as f64 * 100.0
                        } else {
                            0.0
                        },
                    },
                    P::Verifying => CatalogDownloadProgress {
                        phase: "verifying".into(),
                        current: 0,
                        total: 0,
                        percent: 0.0,
                    },
                    P::Extracting { done, total } => CatalogDownloadProgress {
                        phase: "extracting".into(),
                        current: done,
                        total,
                        percent: if total > 0 {
                            done as f64 / total as f64 * 100.0
                        } else {
                            0.0
                        },
                    },
                    P::Complete { files } => CatalogDownloadProgress {
                        phase: "complete".into(),
                        current: files,
                        total: files,
                        percent: 100.0,
                    },
                    P::Error(ref _msg) => CatalogDownloadProgress {
                        phase: "error".into(),
                        current: 0,
                        total: 0,
                        percent: 0.0,
                    },
                };
                let _ = app_clone.emit("catalog-download-progress", event);
            },
        )
    })
    .await
    .map_err(|e| format!("Download task failed: {e}"))?
    .map_err(|e| format!("Gaia DR3 prebuilt download failed: {e:#}"))?;

    Ok(result.to_string_lossy().to_string())
}

// ========== Helpers ==========

fn load_frame_with_path(
    conn: &rusqlite::Connection,
    frame_id: i64,
) -> Result<(athenaeum_core::models::Frame, String), String> {
    let mut stmt = conn
        .prepare(
            "SELECT f.*, fl.path FROM frames f
             JOIN files fl ON fl.id = f.file_id
             WHERE f.id = ?1",
        )
        .map_err(|e| e.to_string())?;

    stmt.query_row([frame_id], |row| {
        let frame = athenaeum_core::models::Frame {
            id: row.get("id")?,
            file_id: row.get("file_id")?,
            object: row.get("object")?,
            date_obs: row.get::<_, Option<String>>("date_obs").ok().flatten()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok()
                    .or_else(|| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()
                        .map(|ndt| ndt.and_utc().fixed_offset()))
                    .map(|dt| dt.with_timezone(&chrono::Utc))),
            telescop: row.get("telescop")?,
            instrume: row.get("instrume")?,
            exptime: row.get("exptime")?,
            filter: row.get("filter")?,
            imagetyp: None, // not needed for plate solving
            is_master: row.get::<_, i32>("is_master").unwrap_or(0) != 0,
            gain: row.get("gain")?,
            offset: row.get("offset")?,
            binning: row.get("binning")?,
            xbinning: row.get("xbinning")?,
            ybinning: row.get("ybinning")?,
            ccd_temp: row.get("ccd_temp")?,
            set_temp: row.get("set_temp")?,
            focallen: row.get("focallen")?,
            xpixsz: row.get("xpixsz")?,
            ypixsz: row.get("ypixsz")?,
            naxis1: row.get("naxis1")?,
            naxis2: row.get("naxis2")?,
            ra: row.get("ra")?,
            dec: row.get("dec")?,
            sitelat: row.get("sitelat")?,
            lat_obs: row.get("lat_obs")?,
            sitelong: row.get("sitelong")?,
            long_obs: row.get("long_obs")?,
            objctra: row.get("objctra")?,
            objctdec: row.get("objctdec")?,
            override_: row.get::<_, i32>("override").unwrap_or(0) != 0,
            swcreate: row.get("swcreate")?,
            bayerpat: row.get("bayerpat")?,
            rotation: row.get("rotation")?,
        };
        let path: String = row.get("path")?;
        Ok((frame, path))
    })
    .map_err(|e| format!("Frame {frame_id} not found: {e}"))
}
