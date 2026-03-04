use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use serde::Serialize;
use tauri::{Emitter, State};
use tokio::time::timeout;
use futures::stream::{self, StreamExt};

use athenaeum_core::analysis::config::{self, AnalysisConfig};
use athenaeum_core::analysis::analyzer;
use athenaeum_core::db::analysis as db_analysis;
use athenaeum_core::models::FrameAnalysis;
use athenaeum_core::settings;

use super::AppState;

/// Per-frame analysis timeout (seconds).
const FRAME_ANALYSIS_TIMEOUT_SECS: u64 = 120;

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
    current: usize,
    total: usize,
    current_file: String,
    percent: f64,
}

#[derive(Clone, Serialize)]
pub struct AnalyzeFrameSetResult {
    pub analyzed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// Analyze all LIGHT frames in a frame set.
/// Emits "analysis-progress" events during processing.
#[tauri::command]
pub async fn analyze_frame_set(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    frame_set_id: i64,
    force: Option<bool>,
) -> Result<AnalyzeFrameSetResult, String> {
    let force = force.unwrap_or(false);

    // Load config, concurrency setting, and frame list under lock, then release
    let (analysis_config, frames_to_analyze, concurrency) = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);
        let config_hash = analysis_config.config_hash();

        let concurrency: usize = state.ctx.settings
            .get_with_precedence(&conn, settings::keys::BLINK_THREADS, settings::defaults::BLINK_THREADS)
            .unwrap_or_else(|_| settings::defaults::BLINK_THREADS.to_string())
            .parse()
            .unwrap_or(4)
            .max(1);

        // Get all LIGHT frame file paths for this frame set
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
                        // Skip if config hash matches
                        existing.config_hash.as_deref() != Some(&config_hash)
                    }
                    _ => true, // No existing analysis, needs analysis
                }
            }).collect()
        };

        (analysis_config, frames_to_analyze, concurrency)
    };

    let total = frames_to_analyze.len();
    let thread_pool = Some(Arc::clone(&state.ctx.image_pool));
    let completed = Arc::new(AtomicUsize::new(0));

    // Process frames concurrently using buffer_unordered
    let results: Vec<Result<(i64, FrameAnalysis), String>> = stream::iter(
        frames_to_analyze.into_iter().map(|(frame_id, file_id, path)| {
            let cfg = analysis_config.clone();
            let pool = thread_pool.clone();
            let app = app_handle.clone();
            let completed = Arc::clone(&completed);

            async move {
                let path_owned = path.clone();
                let analysis_future = tokio::task::spawn_blocking(move || {
                    analyzer::analyze_frame(&path_owned, &cfg, pool)
                });

                let result = match timeout(Duration::from_secs(FRAME_ANALYSIS_TIMEOUT_SECS), analysis_future).await {
                    Ok(Ok(Ok(mut analysis))) => {
                        analysis.frame_id = frame_id;
                        analysis.file_id = file_id;
                        Ok((frame_id, analysis))
                    }
                    Ok(Ok(Err(e))) => {
                        let msg = format!("{}: {}", path, e);
                        eprintln!("Analysis failed for {}", msg);
                        Err(msg)
                    }
                    Ok(Err(e)) => {
                        let msg = format!("{}: task panicked: {}", path, e);
                        eprintln!("Analysis panicked for {}", msg);
                        Err(msg)
                    }
                    Err(_) => {
                        let msg = format!("{}: timed out after {}s", path, FRAME_ANALYSIS_TIMEOUT_SECS);
                        eprintln!("Analysis timed out for {}", msg);
                        Err(msg)
                    }
                };

                // Emit progress after each frame completes
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                let filename = std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone());
                let _ = app.emit(
                    "analysis-progress",
                    AnalysisProgressEvent {
                        current: done,
                        total,
                        current_file: filename,
                        percent: if total > 0 { (done as f64 / total as f64) * 100.0 } else { 100.0 },
                    },
                );

                result
            }
        })
    ).buffer_unordered(concurrency).collect().await;

    // Partition results into successes and failures
    let mut all_analyses: Vec<(i64, FrameAnalysis)> = Vec::new();
    let mut errors = Vec::new();
    let mut analyzed = 0usize;
    let mut failed = 0usize;
    let skipped;

    for result in results {
        match result {
            Ok(pair) => {
                all_analyses.push(pair);
                analyzed += 1;
            }
            Err(msg) => {
                errors.push(msg);
                failed += 1;
            }
        }
    }

    // Compute quality scores across all successful analyses
    let mut analyses: Vec<FrameAnalysis> = all_analyses.into_iter().map(|(_, a)| a).collect();
    analyzer::compute_quality_scores(&mut analyses, &analysis_config);

    // Persist all results
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        // If we analyzed some frames, we also need to recompute scores for
        // existing analyses in this set (to normalize across the full dataset)
        if !analyses.is_empty() && !force {
            // Load existing analyses for this frame set
            let existing = db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)
                .map_err(|e| e.to_string())?;

            // Combine new + existing, recompute scores for all
            let mut combined: Vec<FrameAnalysis> = Vec::new();
            let new_frame_ids: std::collections::HashSet<i64> = analyses.iter().map(|a| a.frame_id).collect();

            // Add existing analyses that weren't re-analyzed
            for existing_a in existing {
                if !new_frame_ids.contains(&existing_a.frame_id) {
                    combined.push(existing_a);
                }
            }
            // Add new analyses
            combined.append(&mut analyses);

            // Recompute quality scores across all
            analyzer::compute_quality_scores(&mut combined, &analysis_config);

            // Save all
            for a in &combined {
                db_analysis::upsert_frame_analysis(&conn, a).map_err(|e| e.to_string())?;
            }
        } else {
            for a in &analyses {
                db_analysis::upsert_frame_analysis(&conn, a).map_err(|e| e.to_string())?;
            }
        }
    }

    skipped = total.saturating_sub(analyzed + failed);

    Ok(AnalyzeFrameSetResult {
        analyzed,
        skipped,
        failed,
        errors,
    })
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

    let thread_pool = Some(Arc::clone(&state.ctx.image_pool));

    let cfg = analysis_config.clone();
    let path_owned = path.clone();
    let pool = thread_pool.clone();

    let analysis_future = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &cfg, pool)
    });

    let mut analysis = match timeout(Duration::from_secs(FRAME_ANALYSIS_TIMEOUT_SECS), analysis_future).await {
        Ok(Ok(Ok(a))) => a,
        Ok(Ok(Err(e))) => return Err(format!("Analysis failed: {}", e)),
        Ok(Err(e)) => return Err(format!("Analysis panicked: {}", e)),
        Err(_) => return Err(format!("Analysis timed out after {}s for {}", FRAME_ANALYSIS_TIMEOUT_SECS, path)),
    };
    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0); // Single frame gets perfect score

    // Persist
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        db_analysis::upsert_frame_analysis(&conn, &analysis).map_err(|e| e.to_string())?;
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
