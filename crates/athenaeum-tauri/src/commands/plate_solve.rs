use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{Emitter, State};

use athenaeum_core::catalog::CatalogEngine;
use athenaeum_core::plate_solve::config::{self, PlateSolveConfig};
use athenaeum_core::plate_solve::dso_lookup::DsoCatalog;
use athenaeum_core::plate_solve::hints::extract_hints;
use athenaeum_core::plate_solve::quad_index::QuadIndex;
use athenaeum_core::plate_solve::service::{self, SolveResult};
use athenaeum_core::plate_solve::{storage, SolveHints};
use athenaeum_core::services::PlateSolveHandle;

use super::AppState;

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

/// Load the quad index lazily from the catalog directory. Returns an error
/// if the index file doesn't exist (user must build it first).
fn require_quad_index(state: &AppState) -> Result<Arc<QuadIndex>, String> {
    // Fast path: already loaded
    {
        let guard = state.ctx.quad_index.read().unwrap();
        if let Some(ref idx) = *guard {
            return Ok(idx.clone());
        }
    }

    // Slow path: try to load from disk
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let db_path = db.path();
    let parent = std::path::Path::new(&db_path)
        .parent()
        .ok_or("Cannot determine app data directory")?;
    let index_path = parent.join("catalogs").join("tycho2").join("quad_index.bin");
    if !index_path.exists() {
        return Err(
            "Quad index not found. Please build it from Settings \u{2192} Plate Solving."
                .to_string(),
        );
    }
    let loaded = QuadIndex::load(&index_path).map_err(|e| format!("Failed to load quad index: {e}"))?;
    let arc = Arc::new(loaded);
    {
        let mut guard = state.ctx.quad_index.write().unwrap();
        *guard = Some(arc.clone());
    }
    Ok(arc)
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
    let catalog = build_catalog_engine(&state)?;
    let index = require_quad_index(&state)?;
    let dso = get_dso_catalog(&state);
    let pool = Some(state.ctx.image_pool.clone());

    let (frame, file_path) = load_frame_with_path(&conn, frame_id)?;

    let result = service::solve_frame(
        &frame, &file_path, &conn, &catalog, &index, &ps_config, pool,
    )
    .map_err(|e| e.to_string())?;

    service::store_result(&conn, frame_id, &result, dso.as_deref(), &ps_config)
        .map_err(|e| e.to_string())?;

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
    let (ps_config, catalog, index, dso, pool, cancel_flag, work_items) = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        let ps_config = Arc::new(config::load_config(&conn));
        let catalog = Arc::new(build_catalog_engine(&state)?);
        let index = require_quad_index(&state)?;
        let dso = get_dso_catalog(&state);
        let pool = state.ctx.image_pool.clone();
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

        (ps_config, catalog, index, dso, pool, cancel_flag, work_items)
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
    let catalog_arc = Arc::clone(&catalog);
    let index_arc = Arc::clone(&index);

    let results: Vec<WorkResult> = tokio::task::spawn_blocking(move || {
        let work = Mutex::new(work_items.into_iter());
        let results: Mutex<Vec<WorkResult>> = Mutex::new(Vec::with_capacity(total));

        std::thread::scope(|s| {
            for _ in 0..concurrency {
                s.spawn(|| loop {
                    if cancel_worker.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(item) = work.lock().unwrap().next() else { break };

                    let outcome = match item {
                        WorkItem::LoadFailed { frame_id, error } => {
                            WorkResult::Failed { frame_id, error }
                        }
                        WorkItem::Ready { frame_id, frame, file_path, hints } => {
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
                                },
                            );

                            let filename = std::path::Path::new(&file_path)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| file_path.clone());
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
                                    catalog_arc.as_ref(),
                                    index_arc.as_ref(),
                                    ps_config_arc.as_ref(),
                                    Some(Arc::clone(&pool)),
                                )
                            }));
                            match solve {
                                Ok(Ok(result)) => WorkResult::Solved { frame_id, result },
                                Ok(Err(e)) => {
                                    eprintln!(
                                        "plate_solve: solve failed for {filename} (frame {frame_id}): {e}"
                                    );
                                    WorkResult::Failed { frame_id, error: e.to_string() }
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
                                        error: format!("solver panicked: {msg}"),
                                    }
                                }
                            }
                        }
                    };

                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    match &outcome {
                        WorkResult::Solved { frame_id, result } => {
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
                                },
                            );
                        }
                        WorkResult::Failed { frame_id, error } => {
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
                WorkResult::Solved { frame_id, result } => {
                    if let Err(e) =
                        service::store_result(&conn, *frame_id, result, dso.as_deref(), ps_config.as_ref())
                    {
                        eprintln!(
                            "plate_solve: failed to store result for frame {frame_id}: {e}"
                        );
                        failed += 1;
                    } else {
                        solved += 1;
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
    Solved { frame_id: i64, result: SolveResult },
    Failed { frame_id: i64, error: String },
}

#[tauri::command]
pub async fn cancel_plate_solve(state: State<'_, AppState>) -> Result<(), String> {
    let solves = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = solves.get(&0) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
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

#[tauri::command]
pub async fn get_catalog_status(
    state: State<'_, AppState>,
) -> Result<Vec<CatalogStatusInfo>, String> {
    let catalog = build_catalog_engine(&state)?;
    let infos = catalog.available_catalogs();
    Ok(infos
        .into_iter()
        .map(|c| CatalogStatusInfo {
            name: c.name,
            installed: true,
            epoch: c.epoch,
            star_count_approx: c.star_count_approx,
            mag_limit: c.mag_limit,
        })
        .collect())
}

#[derive(Clone, Serialize)]
pub struct CatalogStatusInfo {
    pub name: String,
    pub installed: bool,
    pub epoch: f64,
    pub star_count_approx: u64,
    pub mag_limit: f32,
}

// ========== Quad Index ==========

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuadIndexStatus {
    pub built: bool,
    pub path: Option<String>,
    pub quad_count: u64,
    pub size_bytes: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuadIndexProgressEvent {
    phase: String,
    pixel: u64,
    total: u64,
    quads_so_far: u64,
    percent: f64,
}

fn quad_index_path(state: &AppState) -> Result<std::path::PathBuf, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let db_path = db.path().to_path_buf();
    let parent = db_path.parent().ok_or("Cannot determine app data dir")?;
    Ok(parent.join("catalogs").join("tycho2").join("quad_index.bin"))
}

#[tauri::command]
pub async fn get_quad_index_status(
    state: State<'_, AppState>,
) -> Result<QuadIndexStatus, String> {
    let path = quad_index_path(&state)?;
    if !path.exists() {
        return Ok(QuadIndexStatus {
            built: false,
            path: None,
            quad_count: 0,
            size_bytes: 0,
        });
    }

    // Load the index header to report counts
    match athenaeum_core::plate_solve::quad_index::QuadIndex::load(&path) {
        Ok(idx) => {
            let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            Ok(QuadIndexStatus {
                built: true,
                path: Some(path.to_string_lossy().to_string()),
                quad_count: idx.quad_count(),
                size_bytes,
            })
        }
        Err(_) => Ok(QuadIndexStatus {
            built: false,
            path: None,
            quad_count: 0,
            size_bytes: 0,
        }),
    }
}

#[tauri::command]
pub async fn build_quad_index(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<QuadIndexStatus, String> {
    let path = quad_index_path(&state)?;
    let catalog_dir = path
        .parent()
        .ok_or_else(|| format!("Quad index path has no parent directory: {}", path.display()))?
        .to_path_buf();
    if !catalog_dir.exists() {
        return Err(format!(
            "Tycho-2 catalog directory not found at {}. Please download the catalog first.",
            catalog_dir.display()
        ));
    }

    let ps_config = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        config::load_config(&conn)
    };

    let app_clone = app.clone();
    let path_clone = path.clone();
    let catalog_dir_clone = catalog_dir.clone();
    let mag_limit = ps_config.index_mag_limit;
    let hash_tolerance = ps_config.hash_tolerance;

    let result = tokio::task::spawn_blocking(move || {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        athenaeum_core::plate_solve::index_builder::IndexBuilder::build(
            &catalog_dir_clone,
            &path_clone,
            mag_limit,
            hash_tolerance,
            cancel_flag,
            &move |progress| {
                use athenaeum_core::plate_solve::index_builder::IndexBuildProgress;
                let event = match progress {
                    IndexBuildProgress::Reading { pixel, total, quads_so_far } => {
                        QuadIndexProgressEvent {
                            phase: "reading".into(),
                            pixel,
                            total,
                            quads_so_far,
                            percent: pixel as f64 / total as f64 * 100.0,
                        }
                    }
                    IndexBuildProgress::Writing { bytes_written, total_bytes } => {
                        QuadIndexProgressEvent {
                            phase: "writing".into(),
                            pixel: bytes_written,
                            total: total_bytes,
                            quads_so_far: 0,
                            percent: if total_bytes > 0 { bytes_written as f64 / total_bytes as f64 * 100.0 } else { 0.0 },
                        }
                    }
                    IndexBuildProgress::Complete { quad_count, size_bytes: _ } => {
                        QuadIndexProgressEvent {
                            phase: "complete".into(),
                            pixel: 0,
                            total: 0,
                            quads_so_far: quad_count,
                            percent: 100.0,
                        }
                    }
                };
                let _ = app_clone.emit("quad-index-progress", event);
            },
        )
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?
    .map_err(|e| format!("Index build failed: {e}"))?;

    // Refresh the cached index
    {
        let mut guard = state.ctx.quad_index.write().unwrap();
        *guard = None;
    }

    let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(QuadIndexStatus {
        built: true,
        path: Some(path.to_string_lossy().to_string()),
        quad_count: result,
        size_bytes,
    })
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
pub async fn download_tycho2_catalog(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let db_path = db.path().to_path_buf();
    let app_data_dir = db_path.parent()
        .ok_or("Cannot determine app data directory")?
        .to_path_buf();

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let app_clone = app.clone();

    let result = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::tycho2::setup_tycho2_catalog(
            &app_data_dir,
            cancel_flag,
            &|progress| {
                let event = match progress {
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Downloading {
                        file_index,
                        total_files,
                        ..
                    } => CatalogDownloadProgress {
                        phase: "downloading".into(),
                        current: file_index,
                        total: total_files,
                        percent: file_index as f64 / total_files as f64 * 100.0,
                    },
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Converting {
                        stars_processed,
                        total_stars,
                    } => CatalogDownloadProgress {
                        phase: "converting".into(),
                        current: stars_processed,
                        total: total_stars,
                        percent: stars_processed as f64 / total_stars as f64 * 100.0,
                    },
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Complete { total_stars } => {
                        CatalogDownloadProgress {
                            phase: "complete".into(),
                            current: total_stars,
                            total: total_stars,
                            percent: 100.0,
                        }
                    }
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Error(ref _msg) => {
                        CatalogDownloadProgress {
                            phase: "error".into(),
                            current: 0,
                            total: 0,
                            percent: 0.0,
                        }
                    }
                };
                let _ = app_clone.emit("catalog-download-progress", event);
            },
        )
    })
    .await
    .map_err(|e| format!("Download task failed: {e}"))?
    .map_err(|e| format!("Tycho-2 setup failed: {e}"))?;

    Ok(result.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn download_gaia_dr3_catalog(
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
        athenaeum_core::catalog::gaia::setup_gaia_dr3_catalog(
            &app_data_dir,
            cancel_flag,
            &|progress| {
                let event = match progress {
                    athenaeum_core::catalog::gaia::GaiaProgress::Querying {
                        tile,
                        total_tiles,
                        ..
                    } => CatalogDownloadProgress {
                        phase: "downloading".into(),
                        current: tile as usize,
                        total: total_tiles as usize,
                        percent: tile as f64 / total_tiles as f64 * 100.0,
                    },
                    athenaeum_core::catalog::gaia::GaiaProgress::Converting {
                        stars_processed,
                        total_stars,
                    } => CatalogDownloadProgress {
                        phase: "converting".into(),
                        current: stars_processed,
                        total: total_stars,
                        percent: if total_stars > 0 {
                            stars_processed as f64 / total_stars as f64 * 100.0
                        } else {
                            0.0
                        },
                    },
                    athenaeum_core::catalog::gaia::GaiaProgress::Complete { total_stars } => {
                        CatalogDownloadProgress {
                            phase: "complete".into(),
                            current: total_stars,
                            total: total_stars,
                            percent: 100.0,
                        }
                    }
                    athenaeum_core::catalog::gaia::GaiaProgress::Error(ref _msg) => {
                        CatalogDownloadProgress {
                            phase: "error".into(),
                            current: 0,
                            total: 0,
                            percent: 0.0,
                        }
                    }
                };
                let _ = app_clone.emit("catalog-download-progress", event);
            },
        )
    })
    .await
    .map_err(|e| format!("Download task failed: {e}"))?
    .map_err(|e| format!("Gaia DR3 setup failed: {e}"))?;

    Ok(result.to_string_lossy().to_string())
}

// ========== Helpers ==========

fn build_catalog_engine(state: &AppState) -> Result<CatalogEngine, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get the catalog directory from settings, or use app data dir
    let catalog_dir_str: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'catalog.directory'",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(dir) = catalog_dir_str {
        let path = std::path::PathBuf::from(dir);
        if path.exists() {
            return Ok(CatalogEngine::with_catalog_dir(&path));
        }
    }

    // Fall back to app data dir / catalogs
    let db_path = db.path();
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        let catalog_dir = parent.join("catalogs");
        if catalog_dir.exists() {
            return Ok(CatalogEngine::with_catalog_dir(&catalog_dir));
        }
    }

    Ok(CatalogEngine::new())
}

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
