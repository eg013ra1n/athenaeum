use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use athenaeum_core::plate_solve::config::{self, PlateSolveConfig};
use athenaeum_core::plate_solve::dso_lookup::DsoCatalog;
use athenaeum_core::plate_solve::hints::extract_hints;
use athenaeum_core::plate_solve::service::SolveResult;
use athenaeum_core::plate_solve::{describe_solve_failure, service, storage, SolveHints, StoreOutcome};
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

/// Resolve and open the additive density-tier catalog stack under
/// `<app-data>/catalogs/smac_gaia` (base → deepest), or a legacy single
/// `stars.smac` there as a 1-layer fallback. Opens fresh each call (mmap is
/// cheap); the batch path opens once and shares across workers.
fn resolve_layer_caches(
    state: &WebAppState,
) -> Result<Vec<Arc<solvemyastro::StarCache>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".into(),
    ))?;
    let db_path = db.path();
    let parent = std::path::Path::new(&db_path).parent().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "Cannot determine app data dir".into(),
    ))?;
    let catalog_root = parent.join("catalogs").join("smac_gaia");
    let layer_dirs = athenaeum_core::plate_solve::discover_layers(&catalog_root);
    if layer_dirs.is_empty() {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            format!(
                "solvemyastro star catalog not found under {}. \
                 Please download the Gaia DR3 prebuilt catalog.",
                catalog_root.display()
            ),
        ));
    }
    let mut layers = Vec::with_capacity(layer_dirs.len());
    for dir in &layer_dirs {
        let sc = solvemyastro::StarCache::open(dir).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open star cache at {}: {e}", dir.display()),
            )
        })?;
        layers.push(Arc::new(sc));
    }
    eprintln!(
        "plate_solve: opened {} catalog layer(s) under {}",
        layers.len(),
        catalog_root.display()
    );
    Ok(layers)
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
    /// Machine code for a failure (solvemyastro `FailureClass` or
    /// `REJECTED_LOW_CONFIDENCE`); lets the UI group/style reasons.
    failure_code: Option<String>,
    /// Frame filename, so the UI can label per-frame rows without a lookup.
    filename: Option<String>,
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
    let layer_caches = resolve_layer_caches(&state)?;
    let dso = get_dso_catalog(&state);
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
                                WorkResult::Failed { frame_id, error, code: None, filename: None }
                            }
                            WorkItem::Ready { frame_id, frame, file_path, hints } => {
                                let filename = std::path::Path::new(&file_path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| file_path.clone());

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
                                        failure_code: None,
                                        filename: Some(filename.clone()),
                                    })
                                    .unwrap_or_default(),
                                });

                                let layer_refs: Vec<&solvemyastro::StarCache> =
                                    layer_caches.iter().map(|a| a.as_ref()).collect();
                                let caches = solvemyastro::Caches::layered(&layer_refs);
                                match service::solve_frame_with_hints(
                                    &frame,
                                    &file_path,
                                    &hints,
                                    &caches,
                                    ps_config.as_ref(),
                                    Some(cancel_flag.as_ref()),
                                ) {
                                    Ok(result) => WorkResult::Solved { frame_id, result, filename },
                                    Err(e) => {
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
                                }
                            }
                        };

                        let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        match &outcome {
                            WorkResult::Solved { frame_id, result, filename } => {
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
                                        failure_code: None,
                                        filename: Some(filename.clone()),
                                    })
                                    .unwrap_or_default(),
                                });
                            }
                            WorkResult::Failed { frame_id, error, code, filename } => {
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
                                        failure_code: code.clone(),
                                        filename: filename.clone(),
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
                                // The Phase-2 "solved" event already reached the
                                // UI; emit a correction so the frame flips to
                                // failed with the rejection reason.
                                failed += 1;
                                let _ = event_tx.send(SseEvent {
                                    event_name: "plate-solve-progress".into(),
                                    data: serde_json::to_value(PlateSolveProgressEvent {
                                        frame_id: *frame_id,
                                        current: total,
                                        total,
                                        status: "failed".into(),
                                        matched_stars: None,
                                        rms_arcsec: None,
                                        error: Some(reason),
                                        failure_code: Some("REJECTED_LOW_CONFIDENCE".to_string()),
                                        filename: Some(filename.clone()),
                                    })
                                    .unwrap_or_default(),
                                });
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

/// Delete the `plate_solves` row for a frame. Mirror of the Tauri command;
/// used by the metadata pane's "Revert WCS to FITS header" flow so the
/// stored WCS doesn't outlive the cleared ra/dec/rotation/objctra/objctdec
/// columns.
pub async fn delete_plate_solve_for_frame(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    storage::delete_plate_solve(&conn, args.frame_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// Note: no `rename_all` — matches the Tauri side and the snake_case
// `CatalogStatusInfo` TS interface (`star_count_approx`, `min_fov_deg`, …).
#[derive(Clone, Serialize)]
pub struct CatalogStatusInfo {
    pub name: String,
    pub density: u32,
    pub installed: bool,
    pub epoch: f64,
    pub star_count_approx: u64,
    pub size_bytes: u64,
    pub min_fov_deg: f64,
    pub mag_limit: f32,
}

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

/// Return one `CatalogStatusInfo` per declared density tier, merged with
/// on-disk installed state. Returns an empty Vec when no manifest is available.
pub async fn get_catalog_status(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<CatalogStatusInfo>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".to_string(),
    ))?;
    let app_data = db.path().to_path_buf().parent()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".to_string()))?
        .to_path_buf();
    let rows = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::gaia_prebuilt::tier_status(&app_data)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("catalog status task failed: {e}")))?;
    Ok(Json(rows.into_iter().map(|t| CatalogStatusInfo {
        name: format!("Gaia tier {} (≤{:.2}° FOV)", t.density, t.min_fov_deg),
        density: t.density,
        installed: t.installed,
        epoch: t.epoch,
        star_count_approx: t.star_count,
        size_bytes: t.size_bytes,
        min_fov_deg: t.min_fov_deg,
        mag_limit: 19.0,
    }).collect()))
}

pub async fn get_frame_fov_summary(
    State(state): State<WebAppState>,
) -> Result<Json<athenaeum_core::plate_solve::FovSummary>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".to_string()))?;
    let conn = db.conn();
    athenaeum_core::plate_solve::frame_fov_summary(&conn)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDensityArgs {
    target_density: u32,
}

pub async fn download_catalog_layers(
    State(state): State<WebAppState>,
    Json(args): Json<TargetDensityArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let app_data_dir = db.path().to_path_buf().parent()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".into()))?
        .to_path_buf();

    let event_tx = state.event_tx.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let result = tokio::task::spawn_blocking(move || {
        use athenaeum_core::catalog::gaia_prebuilt::GaiaPrebuiltProgress as P;
        let cur = std::sync::Mutex::new((0u32, 0usize, 0usize)); // (density, index, n_tiers)
        athenaeum_core::catalog::gaia_prebuilt::download_catalog_layers(
            &app_data_dir,
            args.target_density,
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
                let _ = event_tx.send(SseEvent {
                    event_name: "catalog-download-progress".into(),
                    data: serde_json::to_value(event).unwrap_or_default(),
                });
            },
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("download task panicked: {e}")))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(result.display().to_string()))
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
