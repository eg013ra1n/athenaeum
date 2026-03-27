// Analysis route handlers — mirrors athenaeum-tauri/src/commands/analysis.rs

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use athenaeum_core::analysis::analyzer;
use athenaeum_core::analysis::config::{self, AnalysisConfig};
use athenaeum_core::db::analysis as db_analysis;
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

#[derive(Serialize)]
pub struct AnalyzeFrameSetResult {
    pub analyzed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Serialize)]
struct AnalysisProgressEvent {
    current: usize,
    total: usize,
    current_file: String,
    percent: f64,
}

// ── Error helper ─────────────────────────────────────────────────────────────

fn err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    eprintln!("analysis error: {}", s);
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

// ── Config routes ────────────────────────────────────────────────────────────

/// POST /api/get_analysis_config
pub async fn get_analysis_config(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<AnalysisConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    Ok(Json(config::load_config(&conn)))
}

/// POST /api/set_analysis_config
pub async fn set_analysis_config(
    State(state): State<WebAppState>,
    Json(config): Json<AnalysisConfig>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    config::save_config(&conn, &config).map_err(err)?;
    Ok(Json(()))
}

/// POST /api/reset_analysis_config
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

/// POST /api/delete_analysis_for_frame_set
pub async fn delete_analysis_for_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    let deleted = db_analysis::delete_analyses_for_frame_set(&conn, args.frame_set_id)
        .map_err(err)?;
    Ok(Json(deleted))
}

// ── Single frame analysis ────────────────────────────────────────────────────

/// POST /api/analyze_single_frame
pub async fn analyze_single_frame(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<FrameAnalysis>, (StatusCode, String)> {
    let frame_id = args.frame_id;

    let (analysis_config, file_id, path) = {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);

        let (file_id, path): (i64, String) = conn
            .query_row(
                "SELECT fi.id, fi.path FROM frames f
                 INNER JOIN files fi ON fi.id = f.file_id
                 WHERE f.id = ?1",
                rusqlite::params![frame_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| err(format!("Frame not found: {}", e)))?;

        (analysis_config, file_id, path)
    };

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = analysis_config.config_hash();
    let path_owned = path.clone();

    let (mut analysis, stars) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| err(format!("Analysis panicked: {}", e)))?
        .map_err(|e| err(format!("Analysis failed: {}", e)))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0); // Single frame gets perfect score

    // Persist analysis + stars
    {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();
        let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis).map_err(err)?;
        db_analysis::upsert_star_metrics(&conn, analysis_id, &stars).map_err(err)?;
    }

    Ok(Json(analysis))
}

// ── Frame set analysis (with SSE progress) ───────────────────────────────────

/// POST /api/analyze_frame_set
///
/// Analyzes all LIGHT frames in a frame set.
/// Uses rayon par_iter inside pool.install() for natural work-stealing across frames.
/// Emits `analysis-progress` SSE events during processing.
pub async fn analyze_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<AnalyzeFrameSetArgs>,
) -> Result<Json<AnalyzeFrameSetResult>, (StatusCode, String)> {
    let frame_set_id = args.frame_set_id;
    let force = args.force.unwrap_or(false);

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
    let concurrency = (analysis_config.batch_concurrency.max(1).min(8)) as usize;

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
                        let item = work.lock().unwrap().next();
                        let Some((frame_id, file_id, path)) = item else { break };

                        let result = match analyzer::analyze_frame(&path, &img_analyzer, &config_hash) {
                            Ok((mut analysis, stars)) => {
                                analysis.frame_id = frame_id;
                                analysis.file_id = file_id;
                                Ok((frame_id, analysis, stars))
                            }
                            Err(e) => {
                                let msg = format!("{}: {}", path, e);
                                eprintln!("Analysis failed for {}", msg);
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

    // Separate stars from analyses for quality scoring
    let mut stars_by_frame: std::collections::HashMap<i64, Vec<StarMetric>> = std::collections::HashMap::new();
    let mut analyses: Vec<FrameAnalysis> = Vec::new();
    for (frame_id, analysis, stars) in all_analyses {
        stars_by_frame.insert(frame_id, stars);
        analyses.push(analysis);
    }
    analyzer::compute_quality_scores(&mut analyses, &analysis_config);

    // Persist all results in a single transaction
    {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();

        if !analyses.is_empty() && !force {
            let existing = db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)
                .map_err(err)?;

            let mut combined: Vec<FrameAnalysis> = Vec::new();
            let new_frame_ids: HashSet<i64> = analyses.iter().map(|a| a.frame_id).collect();

            for existing_a in existing {
                if !new_frame_ids.contains(&existing_a.frame_id) {
                    combined.push(existing_a);
                }
            }
            combined.append(&mut analyses);
            analyzer::compute_quality_scores(&mut combined, &analysis_config);

            conn.execute_batch("BEGIN").map_err(err)?;
            for a in &combined {
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
        } else if !analyses.is_empty() {
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

    Ok(Json(AnalyzeFrameSetResult {
        analyzed,
        skipped,
        failed,
        errors,
    }))
}

// ── Star metrics ─────────────────────────────────────────────────────────────

/// POST /api/get_frame_star_metrics
///
/// Get star metrics for a frame. Returns from DB if fresh, otherwise analyzes on-demand.
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
                        }));
                    }
                }
            }
        }

        // Stale or missing — need to analyze on-demand
        let (file_id, path): (i64, String) = conn
            .query_row(
                "SELECT fi.id, fi.path FROM frames f
                 INNER JOIN files fi ON fi.id = f.file_id
                 WHERE f.id = ?1",
                rusqlite::params![frame_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| err(format!("Frame not found: {}", e)))?;

        (analysis_config, current_hash, file_id, path)
    };

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = current_hash.clone();
    let path_owned = path.clone();

    let (mut analysis, mut stars) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| err(format!("Analysis panicked: {}", e)))?
        .map_err(|e| err(format!("Analysis failed: {}", e)))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0);

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
    }))
}
