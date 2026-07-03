// Analysis route handlers — mirrors athenaeum-tauri/src/commands/analysis.rs

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use athenaeum_core::analysis::analyzer;
use athenaeum_core::analysis::config::{self, AnalysisConfig};
use athenaeum_core::db::analysis as db_analysis;
use athenaeum_core::flat_analysis::{self, FlatContourOpts};
use athenaeum_core::models::{FrameAnalysis, StarMetric, StarMetricsResponse};

use crate::events::SseEvent;
use crate::WebAppState;

// ── Request / Response structs ───────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeFrameSetArgs {
    pub frame_set_id: i64,
    pub force: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frame_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameIdArgs {
    pub frame_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAnalysisArgs {
    pub frame_set_id: i64,
}

#[derive(Serialize)]
pub struct AnalyzeFrameSetResult {
    pub analyzed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

#[derive(Clone, Serialize)]
struct AnalysisProgressEvent {
    frame_set_id: i64,
    current: usize,
    total: usize,
    current_file: String,
    percent: f64,
}

#[derive(Clone, Serialize)]
struct AnalysisCompleteEvent {
    frame_set_id: i64,
    analyzed: usize,
    skipped: usize,
    failed: usize,
    errors: Vec<String>,
    cancelled: bool,
}

// ── Error helper ─────────────────────────────────────────────────────────────

// The raw stderr print formerly here duplicated the `#[tracing::instrument(err(Debug))]`
// attribute on every caller below, which already logs each returned Err at
// the command boundary — see the T7 sweep report.
fn err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

/// Request body for `set_analysis_config`. The frontend calls
/// `api.invoke('set_analysis_config', { config })` per the Tauri named-arg
/// convention used by every `api.invoke` call, so the HTTP body is
/// `{ "config": { ... } }`, not a bare `AnalysisConfig`. See
/// `.superpowers/sdd/task-10-report.md` (Web wrapper rider).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAnalysisConfigArgs {
    pub config: AnalysisConfig,
}

// ── Config routes ────────────────────────────────────────────────────────────

/// POST /api/get_analysis_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_analysis_config(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<AnalysisConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    Ok(Json(config::load_config(&conn)))
}

/// POST /api/set_analysis_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_analysis_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetAnalysisConfigArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    config::save_config(&conn, &args.config).map_err(err)?;
    Ok(Json(()))
}

/// POST /api/reset_analysis_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn reset_analysis_config(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<AnalysisConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    let default_config = AnalysisConfig::default();
    config::save_config(&conn, &default_config).map_err(err)?;
    Ok(Json(default_config))
}

// ── Query / Delete routes ────────────────────────────────────────────────────

/// POST /api/get_analysis_for_frame_set
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_analysis_for_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<Vec<FrameAnalysis>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    let results = db_analysis::get_frame_analyses_for_frame_set(&conn, args.frame_set_id)
        .map_err(err)?;
    Ok(Json(results))
}

// ── Frame set analysis (with SSE progress) ───────────────────────────────────

/// POST /api/analyze_frame_set
///
/// Analyzes all LIGHT frames in a frame set.
/// Uses rayon par_iter inside pool.install() for natural work-stealing across frames.
/// Emits `analysis-progress` SSE events during processing.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn analyze_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<AnalyzeFrameSetArgs>,
) -> Result<Json<AnalyzeFrameSetResult>, (StatusCode, String)> {
    let frame_set_id = args.frame_set_id;
    let force = args.force.unwrap_or(false);

    // Guard against concurrent analysis of same frame set
    {
        let analyses = state.ctx.active_analyses.lock().unwrap();
        if analyses.contains_key(&frame_set_id) {
            return Err(err("Analysis already in progress for this frame set"));
        }
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut analyses = state.ctx.active_analyses.lock().unwrap();
        analyses.insert(frame_set_id, athenaeum_core::services::AnalysisHandle {
            cancel_flag: cancel_flag.clone(),
        });
    }

    // Load config and frame list under DB lock, then release
    let (analysis_config, frames_to_analyze) = {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);
        let config_hash = analysis_config.config_hash();

        let mut stmt = conn
            .prepare(
                "SELECT f.id as frame_id, fi.id as file_id, fi.path
                 FROM frames f
                 INNER JOIN files fi ON fi.id = f.file_id
                 INNER JOIN session_members sm ON sm.frame_id = f.id
                 INNER JOIN sessions s ON s.id = sm.session_id
                 INNER JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1
                   AND f.imagetyp = 'Light'",
            )
            .map_err(err)?;

        let frame_rows: Vec<(i64, i64, String)> = stmt
            .query_map(rusqlite::params![frame_set_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(err)?
            .filter_map(|r| r.ok())
            .collect();

        let frames_to_analyze: Vec<(i64, i64, String)> = if force {
            frame_rows
        } else {
            frame_rows
                .into_iter()
                .filter(|(frame_id, _, _)| {
                    match db_analysis::get_frame_analysis(&conn, *frame_id) {
                        Ok(Some(existing)) => {
                            existing.config_hash.as_deref() != Some(&config_hash)
                        }
                        _ => true,
                    }
                })
                .collect()
        };

        (analysis_config, frames_to_analyze)
    };

    let total = frames_to_analyze.len();

    // Build analyzer ONCE with shared rayon pool — reused across all frames.
    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = Arc::new(analyzer::build_analyzer(
        &analysis_config,
        Some(Arc::clone(&pool)),
    ));
    let config_hash = Arc::new(analysis_config.config_hash());
    let completed = Arc::new(AtomicUsize::new(0));
    let event_tx = state.event_tx.clone();
    let event_tx_complete = event_tx.clone();
    let cancel_flag_worker = Arc::clone(&cancel_flag);
    let concurrency = if analysis_config.batch_concurrency == 0 {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 3).max(2).min(16)
    } else {
        (analysis_config.batch_concurrency as usize).clamp(1, 16)
    };

    // Use N worker threads pulling from a shared queue instead of par_iter.
    // With par_iter, all frames compete for the pool — each gets ~1 thread and runs
    // single-threaded, causing batch-of-N behavior. With limited workers, each frame
    // gets more pool threads for internal parallelism (PSF fitting, background estimation),
    // and work-stealing fills serial gaps between pipeline stages.
    let results: Vec<Result<(i64, FrameAnalysis, Vec<StarMetric>), String>> = tokio::task::spawn_blocking(move || {
        let work = std::sync::Mutex::new(frames_to_analyze.into_iter());
        let results: std::sync::Mutex<Vec<Result<(i64, FrameAnalysis, Vec<StarMetric>), String>>> =
            std::sync::Mutex::new(Vec::with_capacity(total));

        std::thread::scope(|s| {
            for _ in 0..concurrency {
                s.spawn(|| {
                    loop {
                        if cancel_flag_worker.load(Ordering::Relaxed) { break; }
                        let item = work.lock().unwrap().next();
                        let Some((frame_id, file_id, path)) = item else { break };

                        let result = match analyzer::analyze_frame(&path, &img_analyzer, &config_hash) {
                            Ok((mut analysis, stars, _flip)) => {
                                analysis.frame_id = frame_id;
                                analysis.file_id = file_id;
                                Ok((frame_id, analysis, stars))
                            }
                            Err(e) => {
                                let msg = format!("{}: {}", path, e);
                                tracing::warn!(frame_id, path = %path, error = %e, "frame analysis failed");
                                Err(msg)
                            }
                        };

                        let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        let filename = std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.clone());
                        let _ = event_tx.send(SseEvent {
                            event_name: "analysis-progress".to_string(),
                            data: serde_json::to_value(AnalysisProgressEvent {
                                frame_set_id,
                                current: done,
                                total,
                                current_file: filename,
                                percent: if total > 0 { (done as f64 / total as f64) * 100.0 } else { 100.0 },
                            })
                            .unwrap_or_default(),
                        });

                        results.lock().unwrap().push(result);
                    }
                });
            }
        });

        results.into_inner().unwrap()
    }).await.map_err(|e| err(format!("Analysis task panicked: {}", e)))?;

    let was_cancelled = cancel_flag.load(Ordering::Relaxed);

    // Clean up active_analyses entry (always, even on early return)
    {
        let mut analyses = state.ctx.active_analyses.lock().unwrap();
        analyses.remove(&frame_set_id);
    }

    // Partition results
    let mut all_analyses: Vec<(i64, FrameAnalysis, Vec<StarMetric>)> = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(triple) => all_analyses.push(triple),
            Err(msg) => errors.push(msg),
        }
    }
    let analyzed = all_analyses.len();
    let failed = errors.len();

    let mut stars_by_frame: std::collections::HashMap<i64, Vec<StarMetric>> = std::collections::HashMap::new();
    let mut analyses: Vec<FrameAnalysis> = Vec::new();
    for (frame_id, analysis, stars) in all_analyses {
        stars_by_frame.insert(frame_id, stars);
        analyses.push(analysis);
    }

    // Persist all results in a single transaction
    {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();

        if !analyses.is_empty() {
            conn.execute_batch("BEGIN").map_err(err)?;
            for a in &analyses {
                let analysis_id = db_analysis::upsert_frame_analysis(&conn, a).map_err(|e| {
                    let _ = conn.execute_batch("ROLLBACK");
                    err(e)
                })?;
                if let Some(stars) = stars_by_frame.get(&a.frame_id) {
                    db_analysis::upsert_star_metrics(&conn, analysis_id, stars).map_err(|e| {
                        let _ = conn.execute_batch("ROLLBACK");
                        err(e)
                    })?;
                }
            }
            conn.execute_batch("COMMIT").map_err(err)?;
        }
    }

    let skipped = total.saturating_sub(analyzed + failed);

    let _ = event_tx_complete.send(SseEvent {
        event_name: "analysis-complete".to_string(),
        data: serde_json::to_value(AnalysisCompleteEvent {
            frame_set_id,
            analyzed,
            skipped,
            failed,
            errors: errors.clone(),
            cancelled: was_cancelled,
        })
        .unwrap_or_default(),
    });

    Ok(Json(AnalyzeFrameSetResult {
        analyzed,
        skipped,
        failed,
        errors,
        cancelled: was_cancelled,
    }))
}

// ── Cancel analysis ──────────────────────────────────────────────────────────

/// POST /api/cancel_analysis
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_analysis(
    State(state): State<WebAppState>,
    Json(args): Json<CancelAnalysisArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let analyses = state.ctx.active_analyses.lock().unwrap();
    if let Some(handle) = analyses.get(&args.frame_set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(Json(()))
    } else {
        Err(err("No active analysis for this frame set"))
    }
}

// ── Star metrics ─────────────────────────────────────────────────────────────

/// Web mirror of the Tauri-side helper — delegates to the central
/// orientation rule in `athenaeum_core::orientation`. See the comment
/// over the Tauri version for why this matters.
fn detect_flip_vertical(path: &str) -> bool {
    athenaeum_core::orientation::flip_vertical_for_path(std::path::Path::new(path))
}

/// POST /api/get_frame_star_metrics
///
/// Get star metrics for a frame. Returns from DB if fresh, otherwise analyzes on-demand.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn get_frame_star_metrics(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<StarMetricsResponse>, (StatusCode, String)> {
    let frame_id = args.frame_id;

    let (analysis_config, current_hash, file_id, path) = {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);
        let current_hash = analysis_config.config_hash();

        // Always need file path for flip_vertical detection
        let (file_id, path): (i64, String) = conn
            .query_row(
                "SELECT fi.id, fi.path FROM frames f
                 INNER JOIN files fi ON fi.id = f.file_id
                 WHERE f.id = ?1",
                rusqlite::params![frame_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| err(format!("Frame not found: {}", e)))?;

        let flip_vertical = detect_flip_vertical(&path);

        // Check if we have fresh analysis data
        if let Ok(Some(existing)) = db_analysis::get_frame_analysis(&conn, frame_id) {
            if existing.config_hash.as_deref() == Some(&current_hash) {
                if let Ok(stars) = db_analysis::get_star_metrics_by_frame_id(&conn, frame_id) {
                    if !stars.is_empty() {
                        return Ok(Json(StarMetricsResponse {
                            image_width: existing.width,
                            image_height: existing.height,
                            metrics: existing,
                            stars,
                            flip_vertical,
                        }));
                    }
                }
            }
        }

        (analysis_config, current_hash, file_id, path)
    };

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = current_hash.clone();
    let path_owned = path.clone();

    let (mut analysis, mut stars, flip_vertical) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| err(format!("Analysis panicked: {}", e)))?
        .map_err(|e| err(format!("Analysis failed: {}", e)))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;

    // Persist
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis).map_err(err)?;

    for s in &mut stars {
        s.frame_analysis_id = analysis_id;
    }
    db_analysis::upsert_star_metrics(&conn, analysis_id, &stars).map_err(err)?;

    Ok(Json(StarMetricsResponse {
        image_width: analysis.width,
        image_height: analysis.height,
        metrics: analysis,
        stars,
        flip_vertical,
    }))
}

// ── Flat contour plot (PixInsight FlatContourPlot port) ──────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatContourPlotArgs {
    pub file_id: i64,
    pub opts: FlatContourOpts,
}

/// Wire response for `compute_flat_contour_plot`. Mirrors the Tauri command's
/// `FlatContourPlotResponse` exactly.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatContourPlotResponse {
    pub width: u32,
    pub height: u32,
    pub peak: f32,
    pub mean: f32,
    pub min: f32,
    pub min_quantile: f32,
    pub max_quantile: f32,
    pub contours: u32,
    pub pixels_b64: String,
}

/// POST /api/compute_flat_contour_plot
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn compute_flat_contour_plot(
    State(state): State<WebAppState>,
    Json(args): Json<FlatContourPlotArgs>,
) -> Result<Json<FlatContourPlotResponse>, (StatusCode, String)> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let path = {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();
        conn.query_row(
            "SELECT path FROM files WHERE id = ?1",
            rusqlite::params![args.file_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| err(format!("File not found (id={}): {}", args.file_id, e)))?
    };

    let opts = args.opts;
    let result = tokio::task::spawn_blocking(move || {
        flat_analysis::compute_flat_contour_plot(&path, opts)
    })
    .await
    .map_err(|e| err(format!("Flat contour analysis panicked: {}", e)))?
    .map_err(|e| err(format!("Flat contour analysis failed: {}", e)))?;

    let pixels_b64 = STANDARD.encode(&result.pixels);

    Ok(Json(FlatContourPlotResponse {
        width: result.width,
        height: result.height,
        peak: result.peak,
        mean: result.mean,
        min: result.min,
        min_quantile: result.min_quantile,
        max_quantile: result.max_quantile,
        contours: result.contours,
        pixels_b64,
    }))
}

#[cfg(test)]
mod analysis_config_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// these tests exercise actual `settings` table reads/writes for the
    /// analysis config. Mirrors `settings::logging_config_tests::test_state`.
    fn test_state(db: Database) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths: Vec::new(),
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
        }
    }

    /// Regression guard for the real frontend payload: `api.invoke` sends
    /// `{ "config": { ... } }`, per the Tauri named-arg convention — not a
    /// bare `AnalysisConfig`. Exercises the handler via the wrapped
    /// `SetAnalysisConfigArgs` struct directly.
    #[tokio::test]
    async fn set_analysis_config_then_get_reflects_change() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let mut cfg = AnalysisConfig::default();
        cfg.max_stars = 750;

        let _ = set_analysis_config(State(state.clone()), Json(SetAnalysisConfigArgs { config: cfg }))
            .await
            .expect("valid config must be accepted");

        let resp = get_analysis_config(State(state), Json(serde_json::json!({})))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.max_stars, 750);
    }

    /// Pins the fix: the handler now requires the `{ "config": ... }`
    /// wrapper (matching `SetAnalysisConfigArgs`). Deserializing a bare
    /// `AnalysisConfig` JSON body must fail hard (serde error), not
    /// silently succeed with the wrong shape.
    #[test]
    fn bare_analysis_config_body_fails_to_deserialize_into_wrapped_args() {
        let bare = serde_json::to_value(AnalysisConfig::default()).unwrap();

        let result: Result<SetAnalysisConfigArgs, _> = serde_json::from_value(bare);
        assert!(
            result.is_err(),
            "bare AnalysisConfig body must NOT deserialize into SetAnalysisConfigArgs — \
             this is what closes the silent-mismatch hole (axum returns 422/400 for this shape)"
        );
    }
}
