//! Shared `analysis` command-layer handlers — single business-logic source
//! for the Tauri (`commands/analysis.rs`) and web (`routes/analysis.rs`)
//! wrappers. See `.superpowers/sdd/p1-task-9-brief.md` for the conversion
//! recipe this module follows (reused verbatim by Tasks 10-12), and
//! `.superpowers/sdd/p1-task-12-report.md` for this module's specific drift
//! catalog (pilot 4/4 — the worst duplicate in the tree, `analyze_frame_set`
//! desktop :83-294 / web :155-375, near-verbatim except for the progress
//! transport).
//!
//! `analyze_frame_set`, `get_frame_star_metrics`, and
//! `compute_flat_contour_plot` all keep a `tokio::task::spawn_blocking` in
//! the picture (unchanged from pre-conversion — these are genuinely
//! CPU-bound: multi-threaded PSF fitting/star detection or per-pixel flat
//! processing), but only the latter two stay `pub async fn` with the
//! `spawn_blocking` INSIDE this shared function. `analyze_frame_set` cannot
//! do that: it takes `emitter: &dyn ProgressEmitter`, and a trait-object
//! reference borrowed from the caller can't be moved into a
//! `tokio::task::spawn_blocking` closure (which requires `'static`). So
//! `analyze_frame_set` is a plain, fully synchronous `pub fn` instead — the
//! `spawn_blocking` boundary moves OUT to each wrapper, which constructs its
//! concrete emitter (`TauriProgressEmitter`/`SseProgressEmitter`) *inside*
//! the blocking closure and calls this function synchronously from there.
//! This exactly mirrors the pre-existing (non-`api::*`)
//! `registration::register_frame_set` / `commands/registration.rs` +
//! `routes/registration.rs` pattern: sync core fn + `&dyn ProgressEmitter`,
//! wrapper owns the blocking boundary.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::analysis::analyzer;
use crate::analysis::config::{self, AnalysisConfig};
use crate::api::{db, ApiError};
use crate::db::analysis as db_analysis;
use crate::events::{emit_event, ProgressEmitter};
use crate::flat_analysis::{self, FlatContourOpts};
use crate::models::{FrameAnalysis, StarMetric, StarMetricsResponse};
use crate::services::ServiceContext;

/// Pre-conversion hardcoded concurrency ceiling for analyze_frame_set —
/// both the auto path and the manual batch_concurrency clamp used a
/// literal 16 before the api-layer conversion. Keep byte-for-byte
/// behavior; wiring a machine-derived value is a future product decision.
pub const ANALYZE_THREAD_CAP: usize = 16;

// ── Response DTOs / progress payloads (single-sourced) ──────────────────────

#[derive(Clone, serde::Serialize)]
pub struct AnalyzeFrameSetResult {
    pub analyzed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

/// `analysis-progress` event payload. Field names are byte-identical to
/// pre-conversion (NOT camelCase) — `src/types/helpers.ts`'s
/// `AnalysisProgressEvent` interface / `useAnalysisProgress.ts` consume
/// these verbatim (`payload.frame_set_id`, `payload.current_file`, ...).
/// Do not add `#[serde(rename_all = "camelCase")]` without updating the
/// frontend listener in lockstep — see payload-parity note in the Task 12
/// report.
#[derive(Clone, serde::Serialize)]
struct AnalysisProgressEvent {
    frame_set_id: i64,
    current: usize,
    total: usize,
    current_file: String,
    percent: f64,
}

/// `analysis-complete` event payload — same snake_case-on-the-wire
/// requirement as `AnalysisProgressEvent` above.
#[derive(Clone, serde::Serialize)]
struct AnalysisCompleteEvent {
    frame_set_id: i64,
    analyzed: usize,
    skipped: usize,
    failed: usize,
    errors: Vec<String>,
    cancelled: bool,
}

/// Wire response for `compute_flat_contour_plot`.
#[derive(Clone, serde::Serialize)]
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
    /// Base64 (STANDARD) of the 8-bit grayscale display buffer.
    /// Length == `width * height`. The frontend paints these directly.
    pub pixels_b64: String,
}

// ========== Config Commands ==========

pub fn get_analysis_config(ctx: &ServiceContext) -> Result<AnalysisConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(config::load_config(&conn))
}

pub fn set_analysis_config(ctx: &ServiceContext, config: AnalysisConfig) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    config::save_config(&conn, &config).map_err(ApiError::Internal)
}

pub fn reset_analysis_config(ctx: &ServiceContext) -> Result<AnalysisConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let default_config = AnalysisConfig::default();
    config::save_config(&conn, &default_config).map_err(ApiError::Internal)?;
    Ok(default_config)
}

// ========== Frame-set analysis ==========

/// Analyze all LIGHT frames in a frame set.
///
/// Uses rayon par_iter inside pool.install() for natural work-stealing
/// across frames — actually N dedicated worker threads pulling from a
/// shared queue (see comment inline below); emits `analysis-progress`
/// during processing and `analysis-complete` at the end via `emitter`.
///
/// Fully synchronous — deliberately NOT `async` (see module doc for why:
/// the `&dyn ProgressEmitter` parameter can't cross a `spawn_blocking`
/// boundary, so each wrapper owns that boundary instead and calls this fn
/// synchronously from inside it).
///
/// `thread_budget` caps the worker-pool concurrency below (was a hardcoded
/// `16` pre-conversion on both platforms). It's threaded in as a plain
/// `usize` because it's read from `AppState`/`WebAppState::max_blink_threads`
/// (`min(vCPUs, 16)`), a platform-state field that has no `ServiceContext`
/// equivalent — same shape as `set_scan_root_monitor_enabled`'s
/// `monitor: &MonitorService` param (Task 9 report). Passing the real
/// per-machine cap here instead of a hardcoded `16` can only ever shrink
/// the effective ceiling (never raise it, since `max_blink_threads` is
/// itself `min(vCPUs, 16)`) — see the Task 12 report drift catalog for the
/// (bounded, disclosed) behavior-tightening this implies for machines with
/// fewer than 16 cores and a manually-configured `batch_concurrency` above
/// the core count.
pub fn analyze_frame_set(
    ctx: &ServiceContext,
    frame_set_id: i64,
    force: Option<bool>,
    thread_budget: usize,
    emitter: &dyn ProgressEmitter,
) -> Result<AnalyzeFrameSetResult, ApiError> {
    let force = force.unwrap_or(false);

    // Guard against concurrent analysis of same frame set. Classified as
    // `Conflict` (409) rather than the pre-conversion blanket 500 web
    // returned via its local `err()` helper — the same "already in
    // progress" -> `Conflict` fold already applied to
    // `start_scan_with_progress` in Task 9 (`api::scan_roots`); not a new
    // silent behavior fork, a precedented status-code improvement.
    {
        let analyses = ctx.active_analyses.lock().unwrap();
        if analyses.contains_key(&frame_set_id) {
            return Err(ApiError::Conflict("Analysis already in progress for this frame set".to_string()));
        }
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut analyses = ctx.active_analyses.lock().unwrap();
        analyses.insert(frame_set_id, crate::services::AnalysisHandle {
            cancel_flag: cancel_flag.clone(),
        });
    }

    // Global compute-queue admission: heavy jobs run one-at-a-time (default)
    // across analysis/master-build/light-calibration. Blocks this (already
    // spawn_blocking) thread until admitted; a cancel while queued surfaces
    // as a normal cancelled result, mirroring a cancel during the run.
    let queue_permit = match ctx.compute_queue.acquire(
        crate::services::compute_queue::ComputeJobKind::Analysis,
        &format!("Analysis: frame set {frame_set_id}"),
        cancel_flag.clone(),
    ) {
        Ok((permit, _job_id)) => permit,
        Err(_cancelled) => {
            let mut analyses = ctx.active_analyses.lock().unwrap();
            analyses.remove(&frame_set_id);
            drop(analyses);
            emit_event(emitter, "analysis-complete", &AnalysisCompleteEvent {
                frame_set_id,
                analyzed: 0,
                skipped: 0,
                failed: 0,
                errors: Vec::new(),
                cancelled: true,
            });
            return Ok(AnalyzeFrameSetResult {
                analyzed: 0, skipped: 0, failed: 0, errors: Vec::new(), cancelled: true,
            });
        }
    };

    // Load config and frame list under DB lock, then release
    let (analysis_config, frames_to_analyze) = {
        let db = db(ctx)?;
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
        )?;

        let frame_rows: Vec<(i64, i64, String)> = stmt.query_map(
            rusqlite::params![frame_set_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        )?
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
    let pool = Arc::clone(&ctx.image_pool);
    let img_analyzer = Arc::new(analyzer::build_analyzer(
        &analysis_config,
        Some(Arc::clone(&pool)),
    ));
    let config_hash = Arc::new(analysis_config.config_hash());
    let completed = Arc::new(AtomicUsize::new(0));
    let cancel_flag_worker = Arc::clone(&cancel_flag);
    let cap = thread_budget.max(1);
    let concurrency = if analysis_config.batch_concurrency == 0 {
        // Auto: ~1 worker per 3 pool threads
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 3).max(2).min(cap)
    } else {
        (analysis_config.batch_concurrency as usize).clamp(1, cap)
    };

    // Use N worker threads pulling from a shared queue instead of par_iter.
    // With par_iter, all frames compete for the pool — each gets ~1 thread and runs
    // single-threaded, causing batch-of-N behavior. With limited workers, each frame
    // gets more pool threads for internal parallelism (PSF fitting, background estimation),
    // and work-stealing fills serial gaps between pipeline stages.
    let work = std::sync::Mutex::new(frames_to_analyze.into_iter());
    let results_lock: std::sync::Mutex<Vec<Result<(i64, FrameAnalysis, Vec<StarMetric>), String>>> =
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
                    emit_event(emitter, "analysis-progress", &AnalysisProgressEvent {
                        frame_set_id,
                        current: done,
                        total,
                        current_file: filename,
                        percent: if total > 0 { (done as f64 / total as f64) * 100.0 } else { 100.0 },
                    });

                    results_lock.lock().unwrap().push(result);
                }
            });
        }
    });

    let results = results_lock.into_inner().unwrap();

    let was_cancelled = cancel_flag.load(Ordering::Relaxed);

    // Clean up active_analyses entry (always, even on early return)
    {
        let mut analyses = ctx.active_analyses.lock().unwrap();
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
    let mut stars_by_frame: HashMap<i64, Vec<StarMetric>> = HashMap::new();
    let mut analyses: Vec<FrameAnalysis> = Vec::new();
    for (frame_id, analysis, stars) in all_analyses {
        stars_by_frame.insert(frame_id, stars);
        analyses.push(analysis);
    }
    // Persist all results in a single transaction
    {
        let db = db(ctx)?;
        let conn = db.conn();

        if !analyses.is_empty() {
            // Drop-rollback replaces the manual ROLLBACK-in-map_err dance and
            // also covers the previously-unguarded COMMIT failure (audit M11).
            let tx = conn.unchecked_transaction()?;
            for a in &analyses {
                let analysis_id = db_analysis::upsert_frame_analysis(&conn, a)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                if let Some(stars) = stars_by_frame.get(&a.frame_id) {
                    db_analysis::upsert_star_metrics(&conn, analysis_id, stars)
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                }
            }
            tx.commit()?;
        }
    }

    let skipped = total.saturating_sub(analyzed + failed);

    // Release the compute-queue slot before emitting the completion event
    // (not after) so the next queued job is admitted the moment CPU work
    // ends, rather than waiting on event serialization.
    drop(queue_permit);

    emit_event(emitter, "analysis-complete", &AnalysisCompleteEvent {
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

/// Cancel an active analysis. Classified as `NotFound` — matches
/// `cancel_scan`'s identical-shape precedent in `api::scan_roots` — rather
/// than the pre-conversion blanket 500.
pub fn cancel_analysis(ctx: &ServiceContext, frame_set_id: i64) -> Result<(), ApiError> {
    let analyses = ctx.active_analyses.lock().unwrap();
    if let Some(handle) = analyses.get(&frame_set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err(ApiError::NotFound("No active analysis for this frame set".to_string()))
    }
}

/// Get all stored analysis results for a frame set.
pub fn get_analysis_for_frame_set(ctx: &ServiceContext, frame_set_id: i64) -> Result<Vec<FrameAnalysis>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)?)
}

/// Delegates to the central orientation helper — see
/// `crate::orientation::flip_vertical_for_path` doc for the astronomical
/// (not TOP-DOWN/BOTTOM-UP-literal) convention this applies.
fn detect_flip_vertical(path: &str) -> bool {
    crate::orientation::flip_vertical_for_path(std::path::Path::new(path))
}

/// Get star metrics for a frame. Returns from DB if fresh, otherwise
/// analyzes on-demand.
///
/// `async` (unlike most `api::*` handlers, and unlike `analyze_frame_set`
/// above) because the on-demand path wraps a single-frame analysis in
/// `tokio::task::spawn_blocking` — CPU-bound PSF fitting/star detection,
/// unchanged from pre-conversion. No `ProgressEmitter` is involved here, so
/// there's no lifetime conflict: the `spawn_blocking` stays inside this
/// shared function. The `Database` connection guard is dropped before the
/// `.await` per `api::db`'s doc-comment precondition (mirrors
/// pre-conversion's explicit `drop(conn); let _ = db;`).
pub async fn get_frame_star_metrics(ctx: &ServiceContext, frame_id: i64) -> Result<StarMetricsResponse, ApiError> {
    // Named `db_handle` (not `db`) throughout this fn — a local binding
    // named `db` would shadow the `db(ctx)` helper fn for the rest of this
    // scope, breaking the second acquisition below (after the on-demand
    // analysis `.await`).
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();

    let analysis_config = config::load_config(&conn);
    let current_hash = analysis_config.config_hash();

    // Always need the file path for flip_vertical detection and possible on-demand analysis
    let (file_id, path): (i64, String) = conn.query_row(
        "SELECT fi.id, fi.path FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         WHERE f.id = ?1",
        rusqlite::params![frame_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(|e| ApiError::NotFound(format!("Frame not found: {}", e)))?;

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
    let _ = db_handle;

    let pool = Arc::clone(&ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = current_hash.clone();
    let path_owned = path.clone();

    let (mut analysis, mut stars, flip_vertical) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| ApiError::Internal(format!("Analysis panicked: {}", e)))?
        .map_err(|e| ApiError::Internal(format!("Analysis failed: {}", e)))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;

    // Persist
    let db = db(ctx)?;
    let conn = db.conn();
    let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis)?;

    for s in &mut stars {
        s.frame_analysis_id = analysis_id;
    }
    db_analysis::upsert_star_metrics(&conn, analysis_id, &stars)?;

    Ok(StarMetricsResponse {
        image_width: analysis.width,
        image_height: analysis.height,
        metrics: analysis,
        stars,
        flip_vertical,
    })
}

// ========== Flat Contour Plot ==========
//
// PixInsight FlatContourPlot v1.3.1 port. Reads a flat (or master flat) at
// `file_id`, resamples + clips + Gaussian-blurs + quantizes per the user's
// opts, and returns the band-per-pixel buffer (as base64-LE bytes) plus
// the per-band central values for the legend strip on the frontend.

/// `async` for the same reason as `get_frame_star_metrics` above — wraps
/// the pixel computation in `tokio::task::spawn_blocking`; no
/// `ProgressEmitter` involved.
pub async fn compute_flat_contour_plot(
    ctx: &ServiceContext,
    file_id: i64,
    opts: FlatContourOpts,
) -> Result<FlatContourPlotResponse, ApiError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    // Resolve file path under the DB lock, then drop it before the (slow)
    // pixel work to avoid holding the connection across an await.
    let path = {
        let db = db(ctx)?;
        let conn = db.conn();
        conn.query_row(
            "SELECT path FROM files WHERE id = ?1",
            rusqlite::params![file_id],
            |row| row.get::<_, String>(0),
        ).map_err(|e| ApiError::NotFound(format!("File not found (id={}): {}", file_id, e)))?
    };

    let result = tokio::task::spawn_blocking(move || {
        flat_analysis::compute_flat_contour_plot(&path, opts)
    }).await
        .map_err(|e| ApiError::Internal(format!("Flat contour analysis panicked: {}", e)))?
        .map_err(|e| ApiError::Internal(format!("Flat contour analysis failed: {}", e)))?;

    let pixels_b64 = STANDARD.encode(&result.pixels);

    Ok(FlatContourPlotResponse {
        width: result.width,
        height: result.height,
        peak: result.peak,
        mean: result.mean,
        min: result.min,
        min_quantile: result.min_quantile,
        max_quantile: result.max_quantile,
        contours: result.contours,
        pixels_b64,
    })
}
