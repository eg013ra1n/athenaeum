use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use serde::Serialize;
use tauri::{Emitter, State};
use athenaeum_core::analysis::config::{self, AnalysisConfig};
use athenaeum_core::analysis::analyzer;
use athenaeum_core::db::analysis as db_analysis;
use athenaeum_core::models::{FrameAnalysis, StarMetric, StarMetricsResponse};

use super::AppState;

// ========== Config Commands ==========

#[tauri::command]
pub async fn get_analysis_config(
    state: State<'_, AppState>,
) -> Result<AnalysisConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    Ok(config::load_config(&conn))
}

#[tauri::command]
pub async fn set_analysis_config(
    state: State<'_, AppState>,
    config: AnalysisConfig,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    config::save_config(&conn, &config)
}

#[tauri::command]
pub async fn reset_analysis_config(
    state: State<'_, AppState>,
) -> Result<AnalysisConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let default_config = AnalysisConfig::default();
    config::save_config(&conn, &default_config)?;
    Ok(default_config)
}

// ========== Analysis Commands ==========

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

#[derive(Clone, Serialize)]
pub struct AnalyzeFrameSetResult {
    pub analyzed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

/// Analyze all LIGHT frames in a frame set.
/// Uses rayon par_iter inside pool.install() for natural work-stealing across frames.
/// Emits "analysis-progress" events during processing.
#[tauri::command]
pub async fn analyze_frame_set(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    frame_set_id: i64,
    force: Option<bool>,
) -> Result<AnalyzeFrameSetResult, String> {
    let force = force.unwrap_or(false);

    // Guard against concurrent analysis of same frame set
    {
        let analyses = state.ctx.active_analyses.lock().unwrap();
        if analyses.contains_key(&frame_set_id) {
            return Err("Analysis already in progress for this frame set".into());
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
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);
        let config_hash = analysis_config.config_hash();

        let mut stmt = conn.prepare(
            "SELECT f.id as frame_id, fi.id as file_id, fi.path
             FROM frames f
             INNER JOIN files fi ON fi.id = f.file_id
             INNER JOIN session_members sm ON sm.frame_id = f.id
             INNER JOIN sessions s ON s.id = sm.session_id
             INNER JOIN imaging_nights n ON n.id = s.imaging_night_id
             WHERE n.frames_set_id = ?1
               AND f.imagetyp = 'Light'"
        ).map_err(|e| e.to_string())?;

        let frame_rows: Vec<(i64, i64, String)> = stmt.query_map(
            rusqlite::params![frame_set_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        // Filter out already-analyzed frames (unless force)
        let frames_to_analyze: Vec<(i64, i64, String)> = if force {
            frame_rows
        } else {
            frame_rows.into_iter().filter(|(frame_id, _, _)| {
                match db_analysis::get_frame_analysis(&conn, *frame_id) {
                    Ok(Some(existing)) => {
                        existing.config_hash.as_deref() != Some(&config_hash)
                    }
                    _ => true,
                }
            }).collect()
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
    let app_handle_complete = app_handle.clone();
    let cancel_flag_worker = Arc::clone(&cancel_flag);
    let concurrency = if analysis_config.batch_concurrency == 0 {
        // Auto: ~1 worker per 3 pool threads
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
                                eprintln!("Analysis failed for {}", msg);
                                Err(msg)
                            }
                        };

                        let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        let filename = std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.clone());
                        let _ = app_handle.emit(
                            "analysis-progress",
                            AnalysisProgressEvent {
                                frame_set_id,
                                current: done,
                                total,
                                current_file: filename,
                                percent: if total > 0 { (done as f64 / total as f64) * 100.0 } else { 100.0 },
                            },
                        );

                        results.lock().unwrap().push(result);
                    }
                });
            }
        });

        results.into_inner().unwrap()
    }).await.map_err(|e| format!("Analysis task panicked: {}", e))?;

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
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        // If incremental (not force), combine with existing analyses for scoring
        if !analyses.is_empty() && !force {
            let existing = db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)
                .map_err(|e| e.to_string())?;

            let mut combined: Vec<FrameAnalysis> = Vec::new();
            let new_frame_ids: std::collections::HashSet<i64> = analyses.iter().map(|a| a.frame_id).collect();

            for existing_a in existing {
                if !new_frame_ids.contains(&existing_a.frame_id) {
                    combined.push(existing_a);
                }
            }
            combined.append(&mut analyses);
            analyzer::compute_quality_scores(&mut combined, &analysis_config);

            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            for a in &combined {
                let analysis_id = db_analysis::upsert_frame_analysis(&conn, a).map_err(|e| {
                    let _ = conn.execute_batch("ROLLBACK");
                    e.to_string()
                })?;
                if let Some(stars) = stars_by_frame.get(&a.frame_id) {
                    db_analysis::upsert_star_metrics(&conn, analysis_id, stars).map_err(|e| {
                        let _ = conn.execute_batch("ROLLBACK");
                        e.to_string()
                    })?;
                }
            }
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        } else if !analyses.is_empty() {
            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            for a in &analyses {
                let analysis_id = db_analysis::upsert_frame_analysis(&conn, a).map_err(|e| {
                    let _ = conn.execute_batch("ROLLBACK");
                    e.to_string()
                })?;
                if let Some(stars) = stars_by_frame.get(&a.frame_id) {
                    db_analysis::upsert_star_metrics(&conn, analysis_id, stars).map_err(|e| {
                        let _ = conn.execute_batch("ROLLBACK");
                        e.to_string()
                    })?;
                }
            }
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        }
    }

    let skipped = total.saturating_sub(analyzed + failed);

    let _ = app_handle_complete.emit("analysis-complete", AnalysisCompleteEvent {
        frame_set_id,
        analyzed,
        skipped,
        failed,
        errors: errors.clone(),
        cancelled: was_cancelled,
    });

    Ok(AnalyzeFrameSetResult {
        analyzed,
        skipped,
        failed,
        errors,
        cancelled: was_cancelled,
    })
}

/// Cancel an active analysis.
#[tauri::command]
pub async fn cancel_analysis(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let analyses = state.ctx.active_analyses.lock().unwrap();
    if let Some(handle) = analyses.get(&frame_set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err("No active analysis for this frame set".into())
    }
}

/// Analyze a single frame.
#[tauri::command]
pub async fn analyze_single_frame(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<FrameAnalysis, String> {
    let (analysis_config, file_id, path) = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);

        let (file_id, path): (i64, String) = conn.query_row(
            "SELECT fi.id, fi.path FROM frames f
             INNER JOIN files fi ON fi.id = f.file_id
             WHERE f.id = ?1",
            rusqlite::params![frame_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|e| format!("Frame not found: {}", e))?;

        (analysis_config, file_id, path)
    };

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = analysis_config.config_hash();
    let path_owned = path.clone();

    let (mut analysis, stars, _) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| format!("Analysis panicked: {}", e))?
        .map_err(|e| format!("Analysis failed: {}", e))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0); // Single frame gets perfect score

    // Persist analysis + stars
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis)
            .map_err(|e| e.to_string())?;
        db_analysis::upsert_star_metrics(&conn, analysis_id, &stars)
            .map_err(|e| e.to_string())?;
    }

    Ok(analysis)
}

/// Get all stored analysis results for a frame set.
#[tauri::command]
pub async fn get_analysis_for_frame_set(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<Vec<FrameAnalysis>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)
        .map_err(|e| e.to_string())
}

/// Delete all analysis results for a frame set.
#[tauri::command]
pub async fn delete_analysis_for_frame_set(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<usize, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    db_analysis::delete_analyses_for_frame_set(&conn, frame_set_id)
        .map_err(|e| e.to_string())
}

fn detect_flip_vertical(path: &str) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; 2880 * 5];
    let n = file.read(&mut buf).unwrap_or(0);
    let header = String::from_utf8_lossy(&buf[..n]);
    for i in (0..header.len()).step_by(80) {
        let end = (i + 80).min(header.len());
        let card = &header[i..end];
        if card.starts_with("ROWORDER") { return card.contains("TOP-DOWN"); }
        if card.starts_with("END") && card[3..].trim().is_empty() { break; }
    }
    false
}

/// Get star metrics for a frame. Returns from DB if fresh, otherwise analyzes on-demand.
#[tauri::command]
pub async fn get_frame_star_metrics(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<StarMetricsResponse, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let analysis_config = config::load_config(&conn);
    let current_hash = analysis_config.config_hash();

    // Always need the file path for flip_vertical detection and possible on-demand analysis
    let (file_id, path): (i64, String) = conn.query_row(
        "SELECT fi.id, fi.path FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         WHERE f.id = ?1",
        rusqlite::params![frame_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(|e| format!("Frame not found: {}", e))?;

    let flip_vertical = detect_flip_vertical(&path);

    // Check if we have fresh analysis data with stars
    if let Ok(Some(existing)) = db_analysis::get_frame_analysis(&conn, frame_id) {
        if existing.config_hash.as_deref() == Some(&current_hash) {
            if let Ok(stars) = db_analysis::get_star_metrics_by_frame_id(&conn, frame_id) {
                if !stars.is_empty() {
                    return Ok(StarMetricsResponse {
                        image_width: existing.width,
                        image_height: existing.height,
                        metrics: existing,
                        stars,
                        flip_vertical,
                    });
                }
            }
        }
    }

    // Stale or missing — analyze on-demand
    drop(conn);
    let _ = db;

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = current_hash.clone();
    let path_owned = path.clone();

    let (mut analysis, mut stars, flip_vertical) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| format!("Analysis panicked: {}", e))?
        .map_err(|e| format!("Analysis failed: {}", e))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0);

    // Persist
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis)
        .map_err(|e| e.to_string())?;

    for s in &mut stars {
        s.frame_analysis_id = analysis_id;
    }
    db_analysis::upsert_star_metrics(&conn, analysis_id, &stars)
        .map_err(|e| e.to_string())?;

    Ok(StarMetricsResponse {
        image_width: analysis.width,
        image_height: analysis.height,
        metrics: analysis,
        stars,
        flip_vertical,
    })
}
