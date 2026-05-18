use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use athenaeum_core::catalog::CatalogEngine;
use athenaeum_core::plate_solve::config::{self, PlateSolveConfig};
use athenaeum_core::plate_solve::dso_lookup::DsoCatalog;
use athenaeum_core::plate_solve::hints::extract_hints;
use athenaeum_core::plate_solve::quad_index::QuadIndex;
use athenaeum_core::plate_solve::service::SolveResult;
use athenaeum_core::plate_solve::{service, storage, SolveHints};
use athenaeum_core::services::PlateSolveHandle;

use crate::events::SseEvent;
use crate::WebAppState;

fn get_dso_catalog(state: &WebAppState) -> Option<Arc<DsoCatalog>> {
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

fn require_quad_index(state: &WebAppState) -> Result<Arc<QuadIndex>, (StatusCode, String)> {
    {
        let guard = state.ctx.quad_index.read().unwrap();
        if let Some(ref idx) = *guard {
            return Ok(idx.clone());
        }
    }
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let db_path = db.path();
    let parent = std::path::Path::new(&db_path).parent().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "Cannot determine app data dir".into(),
    ))?;
    let index_path = parent.join("catalogs").join("tycho2").join("quad_index.bin");
    if !index_path.exists() {
        return Err((StatusCode::PRECONDITION_FAILED, "Quad index not built".into()));
    }
    let loaded = QuadIndex::load(&index_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Load failed: {e}")))?;
    let arc = Arc::new(loaded);
    {
        let mut guard = state.ctx.quad_index.write().unwrap();
        *guard = Some(arc.clone());
    }
    Ok(arc)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameIdArgs {
    pub frame_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchArgs {
    pub frame_ids: Vec<i64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
struct PlateSolveCompleteEvent {
    solved: usize,
    failed: usize,
    total: usize,
    total_time_ms: u64,
}

pub async fn get_plate_solve_config(
    State(state): State<WebAppState>,
) -> Result<Json<PlateSolveConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    Ok(Json(config::load_config(&conn)))
}

pub async fn set_plate_solve_config(
    State(state): State<WebAppState>,
    Json(cfg): Json<PlateSolveConfig>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    config::save_config(&conn, &cfg).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(()))
}

pub async fn reset_plate_solve_config(
    State(state): State<WebAppState>,
) -> Result<Json<PlateSolveConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    let default_config = PlateSolveConfig::default();
    config::save_config(&conn, &default_config).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(default_config))
}

pub async fn plate_solve_frame(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<storage::PlateSolveRecord>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    let ps_config = config::load_config(&conn);
    let catalog = build_catalog_engine(&state)?;
    let index = require_quad_index(&state)?;
    let pool = Some(state.ctx.image_pool.clone());

    let (frame, file_path) = load_frame_with_path(&conn, args.frame_id)?;

    let dso = get_dso_catalog(&state);
    let result = service::solve_frame(&frame, &file_path, &conn, &catalog, &index, &ps_config, pool)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    service::store_result(&conn, args.frame_id, &result, dso.as_deref(), &ps_config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let record = storage::get_plate_solve(&conn, args.frame_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Failed to read back result".into()))?;

    Ok(Json(record))
}

pub async fn plate_solve_batch(
    State(state): State<WebAppState>,
    Json(args): Json<BatchArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    // ── Phase 1: load everything on the main thread (holds DB lock) ──
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let ps_config = Arc::new(config::load_config(&db.conn()));
    let catalog = Arc::new(build_catalog_engine(&state)?);
    let index = require_quad_index(&state)?;
    let dso = get_dso_catalog(&state);
    let pool = state.ctx.image_pool.clone();
    let event_tx = state.event_tx.clone();

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut solves = state.ctx.active_plate_solves.lock().unwrap();
        solves.insert(0, PlateSolveHandle { cancel_flag: cancel_flag.clone() });
    }

    // Pre-load frames + hints in the main thread so workers never touch the DB.
    let mut work_items: Vec<WorkItem> = Vec::with_capacity(args.frame_ids.len());
    {
        let conn = db.conn();
        for frame_id in &args.frame_ids {
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
                Err((_status, msg)) => {
                    eprintln!("plate_solve: failed to load frame {frame_id}: {msg}");
                    work_items.push(WorkItem::LoadFailed {
                        frame_id: *frame_id,
                        error: msg,
                    });
                }
            }
        }
    }

    let total = work_items.len();

    // Concurrency: config override, else auto (~1 worker per 3 cores, max 8).
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

    let ctx = state.ctx.clone();

    // ── Phase 2: parallel solve on a scoped thread pool inside spawn_blocking ──
    tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        let completed = Arc::new(AtomicUsize::new(0));

        let results: Vec<WorkResult> = {
            let work = Mutex::new(work_items.into_iter());
            let results: Mutex<Vec<WorkResult>> = Mutex::new(Vec::with_capacity(total));

            std::thread::scope(|s| {
                for _ in 0..concurrency {
                    s.spawn(|| loop {
                        if cancel_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some(item) = work.lock().unwrap().next() else { break };

                        let outcome = match item {
                            WorkItem::LoadFailed { frame_id, error } => {
                                WorkResult::Failed { frame_id, error }
                            }
                            WorkItem::Ready { frame_id, frame, file_path, hints } => {
                                let done_so_far = completed.load(Ordering::Relaxed);
                                let _ = event_tx.send(SseEvent {
                                    event_name: "plate-solve-progress".into(),
                                    data: serde_json::to_value(PlateSolveProgressEvent {
                                        frame_id,
                                        current: done_so_far,
                                        total,
                                        status: "solving".into(),
                                        matched_stars: None,
                                        rms_arcsec: None,
                                        error: None,
                                    })
                                    .unwrap_or_default(),
                                });

                                let filename = std::path::Path::new(&file_path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| file_path.clone());
                                match service::solve_frame_with_hints(
                                    &frame,
                                    &file_path,
                                    &hints,
                                    catalog.as_ref(),
                                    index.as_ref(),
                                    ps_config.as_ref(),
                                    Some(Arc::clone(&pool)),
                                ) {
                                    Ok(result) => WorkResult::Solved { frame_id, result },
                                    Err(e) => {
                                        eprintln!(
                                            "plate_solve: solve failed for {filename} (frame {frame_id}): {e}"
                                        );
                                        WorkResult::Failed {
                                            frame_id,
                                            error: e.to_string(),
                                        }
                                    }
                                }
                            }
                        };

                        let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        match &outcome {
                            WorkResult::Solved { frame_id, result } => {
                                let _ = event_tx.send(SseEvent {
                                    event_name: "plate-solve-progress".into(),
                                    data: serde_json::to_value(PlateSolveProgressEvent {
                                        frame_id: *frame_id,
                                        current: done,
                                        total,
                                        status: "solved".into(),
                                        matched_stars: Some(result.matched_stars),
                                        rms_arcsec: Some(result.rms_residual_arcsec),
                                        error: None,
                                    })
                                    .unwrap_or_default(),
                                });
                            }
                            WorkResult::Failed { frame_id, error } => {
                                let _ = event_tx.send(SseEvent {
                                    event_name: "plate-solve-progress".into(),
                                    data: serde_json::to_value(PlateSolveProgressEvent {
                                        frame_id: *frame_id,
                                        current: done,
                                        total,
                                        status: "failed".into(),
                                        matched_stars: None,
                                        rms_arcsec: None,
                                        error: Some(error.clone()),
                                    })
                                    .unwrap_or_default(),
                                });
                            }
                        }

                        results.lock().unwrap().push(outcome);
                    });
                }
            });

            results.into_inner().unwrap()
        };

        // ── Phase 3: persist all solved frames in a single DB transaction ──
        let db = ctx.db.get().expect("DB not initialized");
        let mut solved = 0usize;
        let mut failed = 0usize;
        {
            let conn = db.conn();
            if let Err(e) = conn.execute_batch("BEGIN") {
                eprintln!("plate_solve: BEGIN failed: {e}");
            }
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
            if let Err(e) = conn.execute_batch("COMMIT") {
                eprintln!("plate_solve: COMMIT failed: {e}");
            }
        }

        {
            let mut solves = ctx.active_plate_solves.lock().unwrap();
            solves.remove(&0);
        }

        let _ = event_tx.send(SseEvent {
            event_name: "plate-solve-complete".into(),
            data: serde_json::to_value(PlateSolveCompleteEvent {
                solved,
                failed,
                total,
                total_time_ms: start.elapsed().as_millis() as u64,
            })
            .unwrap_or_default(),
        });
    });

    Ok(Json(()))
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

pub async fn cancel_plate_solve(
    State(state): State<WebAppState>,
) -> Json<()> {
    let solves = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = solves.get(&0) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
    }
    Json(())
}

// ========== Autofind Object Routes ==========

// Note: no `rename_all = "camelCase"` — matches the Tauri side and the
// existing TS type in `src/types/plate-solve.ts`. See the adjacent
// PlateSolveProgressEvent above for the opposite convention: that one
// was written camelCase but has a pre-existing frontend mismatch.

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

/// Fill `frame.object` from stored RA/Dec for frames in the batch that have
/// coordinates but no object name. Tight 0.2° proximity tolerance; labels
/// are written as `"Autofind: <designation>"`.
///
/// Returns immediately; progress is streamed via SSE on event names
/// `autofind-objects-progress` and `autofind-objects-complete`.
pub async fn autofind_objects_from_coordinates(
    State(state): State<WebAppState>,
    Json(args): Json<BatchArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    use athenaeum_core::plate_solve::object_fill::{
        self, AutofindProgress, AutofindStatus,
    };

    let db_check = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let dso = get_dso_catalog(&state)
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DSO catalog unavailable".into()))?;
    let tolerance_deg = config::load_config(&db_check.conn()).autofind_tolerance_deg;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut handles = state.ctx.active_plate_solves.lock().unwrap();
        handles.insert(1, PlateSolveHandle { cancel_flag: cancel_flag.clone() });
    }

    let event_tx = state.event_tx.clone();
    let ctx = state.ctx.clone();
    let frame_ids = args.frame_ids;

    tokio::task::spawn_blocking(move || {
        let db = ctx.db.get().expect("DB not initialized");
        let start = std::time::Instant::now();

        let progress = {
            let event_tx = event_tx.clone();
            move |p: AutofindProgress| {
                let status = match p.status {
                    AutofindStatus::Processing => "processing",
                    AutofindStatus::Labeled => "labeled",
                    AutofindStatus::NoMatch => "no_match",
                    AutofindStatus::AlreadyLabeled => "already_labeled",
                    AutofindStatus::MissingCoords => "missing_coords",
                    AutofindStatus::Error => "error",
                };
                let _ = event_tx.send(SseEvent {
                    event_name: "autofind-objects-progress".into(),
                    data: serde_json::to_value(AutofindProgressEvent {
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
                    }).unwrap_or_default(),
                });
            }
        };

        let summary = {
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
            let mut handles = ctx.active_plate_solves.lock().unwrap();
            handles.remove(&1);
        }

        match summary {
            Ok(s) => {
                let _ = event_tx.send(SseEvent {
                    event_name: "autofind-objects-complete".into(),
                    data: serde_json::to_value(AutofindCompleteEvent {
                        total: s.total,
                        labeled: s.labeled,
                        no_match: s.no_match,
                        already_labeled: s.already_labeled,
                        missing_coords: s.missing_coords,
                        errors: s.errors,
                        cancelled: s.cancelled,
                        total_time_ms: start.elapsed().as_millis() as u64,
                    }).unwrap_or_default(),
                });
            }
            Err(e) => {
                eprintln!("autofind: {e}");
            }
        }
    });

    Ok(Json(()))
}

pub async fn cancel_autofind_objects(
    State(state): State<WebAppState>,
) -> Json<()> {
    let handles = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = handles.get(&1) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
    }
    Json(())
}

pub async fn get_plate_solve_result(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<Option<storage::PlateSolveRecord>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    storage::get_plate_solve(&conn, args.frame_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogStatusInfo {
    pub name: String,
    pub installed: bool,
    pub epoch: f64,
    pub star_count_approx: u64,
    pub mag_limit: f32,
}

pub async fn get_catalog_status(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<CatalogStatusInfo>>, (StatusCode, String)> {
    let catalog = build_catalog_engine(&state)?;
    let infos: Vec<CatalogStatusInfo> = catalog.available_catalogs().into_iter()
        .map(|c| CatalogStatusInfo {
            name: c.name, installed: true, epoch: c.epoch,
            star_count_approx: c.star_count_approx, mag_limit: c.mag_limit,
        }).collect();
    Ok(Json(infos))
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuadIndexStatus {
    pub built: bool,
    pub path: Option<String>,
    pub quad_count: u64,
    pub size_bytes: u64,
}

fn quad_index_path(state: &WebAppState) -> Result<std::path::PathBuf, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let db_path = db.path().to_path_buf();
    let parent = db_path.parent().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".into()))?.to_path_buf();
    Ok(parent.join("catalogs").join("tycho2").join("quad_index.bin"))
}

pub async fn get_quad_index_status(
    State(state): State<WebAppState>,
) -> Result<Json<QuadIndexStatus>, (StatusCode, String)> {
    let path = quad_index_path(&state)?;
    if !path.exists() {
        return Ok(Json(QuadIndexStatus { built: false, path: None, quad_count: 0, size_bytes: 0 }));
    }
    match QuadIndex::load(&path) {
        Ok(idx) => {
            let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            Ok(Json(QuadIndexStatus {
                built: true,
                path: Some(path.to_string_lossy().to_string()),
                quad_count: idx.quad_count(),
                size_bytes,
            }))
        }
        Err(_) => Ok(Json(QuadIndexStatus { built: false, path: None, quad_count: 0, size_bytes: 0 })),
    }
}

pub async fn build_quad_index(
    State(state): State<WebAppState>,
) -> Result<Json<QuadIndexStatus>, (StatusCode, String)> {
    let path = quad_index_path(&state)?;
    let catalog_dir = path.parent().unwrap().to_path_buf();
    if !catalog_dir.exists() {
        return Err((StatusCode::PRECONDITION_FAILED, "Tycho-2 catalog not found".into()));
    }
    let ps_config = {
        let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
        let conn = db.conn();
        config::load_config(&conn)
    };
    let event_tx = state.event_tx.clone();
    let path_clone = path.clone();
    let catalog_dir_clone = catalog_dir.clone();

    let result = tokio::task::spawn_blocking(move || {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        athenaeum_core::plate_solve::index_builder::IndexBuilder::build(
            &catalog_dir_clone,
            &path_clone,
            ps_config.index_mag_limit,
            ps_config.hash_tolerance,
            cancel_flag,
            &move |progress| {
                use athenaeum_core::plate_solve::index_builder::IndexBuildProgress;
                let payload = match progress {
                    IndexBuildProgress::Reading { pixel, total, quads_so_far } => serde_json::json!({
                        "phase": "reading", "pixel": pixel, "total": total,
                        "quads_so_far": quads_so_far,
                        "percent": pixel as f64 / total as f64 * 100.0,
                    }),
                    IndexBuildProgress::Writing { bytes_written, total_bytes } => serde_json::json!({
                        "phase": "writing", "bytes_written": bytes_written, "total_bytes": total_bytes,
                        "percent": if total_bytes > 0 { bytes_written as f64 / total_bytes as f64 * 100.0 } else { 0.0 },
                    }),
                    IndexBuildProgress::Complete { quad_count, size_bytes } => serde_json::json!({
                        "phase": "complete", "quad_count": quad_count, "size_bytes": size_bytes,
                        "percent": 100.0,
                    }),
                };
                let _ = event_tx.send(SseEvent {
                    event_name: "quad-index-progress".into(),
                    data: payload,
                });
            },
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {e}")))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Build failed: {e}")))?;

    {
        let mut guard = state.ctx.quad_index.write().unwrap();
        *guard = None;
    }
    let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(Json(QuadIndexStatus {
        built: true,
        path: Some(path.to_string_lossy().to_string()),
        quad_count: result,
        size_bytes,
    }))
}

pub async fn download_tycho2_catalog(
    State(state): State<WebAppState>,
) -> Result<Json<String>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let db_path = db.path().to_path_buf();
    let app_data_dir = db_path.parent()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".into()))?
        .to_path_buf();

    let event_tx = state.event_tx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let result = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::tycho2::setup_tycho2_catalog(
            &app_data_dir,
            cancel_flag,
            &|progress| {
                let (phase, current, total) = match progress {
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Downloading { file_index, total_files, .. } =>
                        ("downloading", file_index, total_files),
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Converting { stars_processed, total_stars } =>
                        ("converting", stars_processed, total_stars),
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Complete { total_stars } =>
                        ("complete", total_stars, total_stars),
                    athenaeum_core::catalog::tycho2::Tycho2Progress::Error(_) =>
                        ("error", 0, 0),
                };
                let _ = event_tx.send(SseEvent {
                    event_name: "catalog-download-progress".into(),
                    data: serde_json::json!({ "phase": phase, "current": current, "total": total,
                        "percent": if total > 0 { current as f64 / total as f64 * 100.0 } else { 0.0 } }),
                });
            },
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {e}")))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Setup failed: {e}")))?;

    Ok(Json(result.to_string_lossy().to_string()))
}

pub async fn download_gaia_dr3_catalog(
    State(state): State<WebAppState>,
) -> Result<Json<String>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let db_path = db.path().to_path_buf();
    let app_data_dir = db_path.parent()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".into()))?
        .to_path_buf();

    let event_tx = state.event_tx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let result = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::gaia::setup_gaia_dr3_catalog(
            &app_data_dir,
            cancel_flag,
            &|progress| {
                let (phase, current, total) = match progress {
                    athenaeum_core::catalog::gaia::GaiaProgress::Querying { tile, total_tiles, .. } =>
                        ("downloading", tile as usize, total_tiles as usize),
                    athenaeum_core::catalog::gaia::GaiaProgress::Converting { stars_processed, total_stars } =>
                        ("converting", stars_processed, total_stars),
                    athenaeum_core::catalog::gaia::GaiaProgress::Complete { total_stars } =>
                        ("complete", total_stars, total_stars),
                    athenaeum_core::catalog::gaia::GaiaProgress::Error(_) =>
                        ("error", 0, 0),
                };
                let _ = event_tx.send(SseEvent {
                    event_name: "catalog-download-progress".into(),
                    data: serde_json::json!({ "phase": phase, "current": current, "total": total,
                        "percent": if total > 0 { current as f64 / total as f64 * 100.0 } else { 0.0 } }),
                });
            },
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {e}")))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Setup failed: {e}")))?;

    Ok(Json(result.to_string_lossy().to_string()))
}

fn build_catalog_engine(state: &WebAppState) -> Result<CatalogEngine, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();

    let catalog_dir_str: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = 'catalog.directory'",
        [], |row| row.get(0),
    ).ok();

    if let Some(dir) = catalog_dir_str {
        let path = std::path::PathBuf::from(dir);
        if path.exists() {
            return Ok(CatalogEngine::with_catalog_dir(&path));
        }
    }

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
) -> Result<(athenaeum_core::models::Frame, String), (StatusCode, String)> {
    let mut stmt = conn.prepare(
        "SELECT f.*, fl.path FROM frames f JOIN files fl ON fl.id = f.file_id WHERE f.id = ?1",
    ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
            imagetyp: None,
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
    }).map_err(|e| (StatusCode::NOT_FOUND, format!("Frame {frame_id} not found: {e}")))
}
