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
//! Same shape as `api::stacking::start_stacking`'s `Arc<dyn ProgressEmitter>`
//! parameter: sync core fn + a boxed emitter, wrapper owns the blocking
//! boundary.

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

// ── The zero point rides the analysis (forced-list spec §5.4) ───────────────

/// What a request for the pass after a solve batch should do, as [`SolvePassGuard::request`] decides it.
#[cfg(feature = "solver")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassRequest {
    /// Start a pass now: the caller owns the guard until [`SolvePassGuard::finish`] or [`SolvePassGuard::abandon`].
    Start,
    /// One is running: the frames were added to its pending set and it takes them before it ends.
    Queued,
}

/// One pass after a solve batch at a time, and the frames asked for while one runs. Pure, so the decision is testable
/// without a thread.
#[cfg(feature = "solver")]
#[derive(Default)]
struct SolvePassGuard {
    running: bool,
    pending: std::collections::BTreeSet<i64>,
}

#[cfg(feature = "solver")]
impl SolvePassGuard {
    const fn new() -> Self {
        SolvePassGuard { running: false, pending: std::collections::BTreeSet::new() }
    }

    /// A pass over `ids` (the frames a batch has just solved) is wanted.
    fn request(&mut self, ids: &[i64]) -> PassRequest {
        if self.running {
            self.pending.extend(ids.iter().copied());
            return PassRequest::Queued;
        }
        self.running = true;
        PassRequest::Start
    }

    /// The running pass ended without finishing (an error, a panic): the guard is free again. The frames queued meanwhile
    /// are dropped with it: they read as "to update" and the next Analyze (or the next batch) measures them.
    fn abandon(&mut self) {
        self.running = false;
        self.pending.clear();
    }

    /// The running pass is done with its frames. The frames asked for meanwhile come back (the guard stays busy: the
    /// caller runs them next); with none, the guard is free and this returns `None`.
    fn finish(&mut self) -> Option<Vec<i64>> {
        if self.pending.is_empty() {
            self.running = false;
            return None;
        }
        Some(std::mem::take(&mut self.pending).into_iter().collect())
    }
}

/// Releases the running flag of the guard on EVERY exit path of the pass thread (a panic, an early return): without it a
/// dead pass would leave every later batch queueing behind it until restart. [`Self::disarm`] is called after
/// [`SolvePassGuard::finish`] has itself released the flag, so a normal end is not released twice.
#[cfg(feature = "solver")]
struct PassRelease<'a> {
    guard: &'a std::sync::Mutex<SolvePassGuard>,
    armed: bool,
}

#[cfg(feature = "solver")]
impl<'a> PassRelease<'a> {
    fn new(guard: &'a std::sync::Mutex<SolvePassGuard>) -> Self {
        PassRelease { guard, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(feature = "solver")]
impl Drop for PassRelease<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.guard.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).abandon();
        }
    }
}

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

/// A stored analysis is stale when it was written under another config, or carries no `measure_version` (a row
/// written between C5 and C6 keeps the current hash but the gate never trusts it, so it heals on the next batch).
/// A row of an older `MEASURE_VERSION` is stale only because `AnalysisConfig::config_hash` folds the version: keep it
/// folded there, or compare the version here.
fn row_is_stale(existing: &FrameAnalysis, config_hash: &str) -> bool {
    existing.config_hash.as_deref() != Some(config_hash) || existing.measure_version.is_none()
}

/// What the zero point needs of a solved frame: the solve (its id, to write only while it is still the frame's) as a
/// sky-to-pixel transform (or why it is unreadable), the exposure and the observation epoch.
#[cfg(feature = "solver")]
struct ZpWork {
    solve_id: i64,
    /// `Err`: a stored SIP table is unusable, or a forward SIP has no reverse one (the list cannot be projected).
    wcs: Result<solvemyastro::WcsSolution, String>,
    exptime_s: Option<f64>,
    epoch: f64,
}

#[cfg(feature = "solver")]
fn zp_work(record: &crate::plate_solve::storage::PlateSolveRecord, exptime_s: Option<f64>, date_obs: Option<&str>) -> Option<ZpWork> {
    let solve_id = record.id?;
    let wcs = record.try_solution().and_then(|wcs| {
        if wcs.sip_forward.is_some() && wcs.sip_reverse.is_none() {
            Err("the solve has a forward SIP and no reverse one".to_string())
        } else {
            Ok(wcs)
        }
    });
    let epoch = crate::plate_solve::hints::observation_epoch(&crate::models::Frame {
        date_obs: date_obs.and_then(crate::analysis::zero_point::parse_date_obs),
        ..crate::models::Frame::default()
    });
    Some(ZpWork { solve_id, wcs, exptime_s, epoch })
}

/// The solve and header facts of one frame, for the single-frame path.
#[cfg(feature = "solver")]
fn zp_job_of_frame(conn: &rusqlite::Connection, frame_id: i64) -> Result<Option<ZpWork>, ApiError> {
    let Some(record) = crate::plate_solve::storage::get_plate_solve(conn, frame_id)
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?
    else {
        return Ok(None);
    };
    let (exptime, date_obs): (Option<f64>, Option<String>) = conn
        .query_row("SELECT exptime, date_obs FROM frames WHERE id = ?1", rusqlite::params![frame_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    Ok(zp_work(&record, exptime, date_obs.as_deref()))
}

/// The catalogue a run measures the zero point against: its identity and its reader.
#[cfg(feature = "solver")]
pub(crate) struct ZpSource<'a> {
    pub identity: &'a str,
    pub cone: &'a analyzer::ConeFn<'a>,
}

/// The identity of the installed star catalogue, `None` when there is none.
#[cfg(feature = "solver")]
fn installed_identity(ctx: &ServiceContext) -> Option<String> {
    let dir = ctx.app_data_dir()?;
    crate::analysis::transparency::installed_catalog_identity(&dir)
}

/// One frame of an Analysis run: what the worker reads the file for, and (when the frame has a solve) what its zero
/// point needs.
struct FrameJob {
    frame_id: i64,
    file_id: i64,
    path: String,
    #[cfg(feature = "solver")]
    zp: Option<ZpWork>,
}

/// One frame's result, as the persist step takes it. `solve_id` is the solve the zero point was measured against (`None`:
/// none was attempted).
struct AnalyzedFrame {
    frame_id: i64,
    analysis: FrameAnalysis,
    stars: Vec<StarMetric>,
    solve_id: Option<i64>,
}

/// The frames of `frame_set_id` an Analysis run measures: its lights (a second copy of a frame the set already holds is
/// not measured: it is never stacked or exported, `db::frame_is_used_copy_sql`, and a zip copy would be extracted for
/// nothing). With `force` every one; otherwise the frames whose analysis is stale ([`row_is_stale`]) or has none, and the
/// solved frames whose zero point is not current ([`crate::analysis::zero_point::zp_outdated`], against the installed
/// catalogue's `catalog` identity; none installed, none selected for it). The solves and the header facts are read in
/// one statement beside the frame list, never one per frame.
fn select_frame_jobs(
    conn: &rusqlite::Connection,
    frame_set_id: i64,
    config_hash: &str,
    catalog: Option<&str>,
    force: bool,
) -> Result<Vec<FrameJob>, ApiError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT f.id as frame_id, fi.id as file_id, fi.path, f.exptime, f.date_obs
         FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         INNER JOIN session_members sm ON sm.frame_id = f.id
         INNER JOIN sessions s ON s.id = sm.session_id
         INNER JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE n.frames_set_id = ?1
           AND f.imagetyp = 'Light'
           AND {used_copy}",
        used_copy = crate::db::frame_is_used_copy_sql("n.frames_set_id"),
    ))?;
    let rows: Vec<(i64, i64, String, Option<f64>, Option<String>)> = stmt
        .query_map(rusqlite::params![frame_set_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    #[cfg(feature = "solver")]
    let mut solves = crate::plate_solve::storage::get_plate_solves_for_frames(conn, &ids)
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    #[cfg(not(feature = "solver"))]
    let _ = (&ids, catalog);

    let mut jobs = Vec::with_capacity(rows.len());
    for (frame_id, file_id, path, exptime, date_obs) in rows {
        #[cfg(feature = "solver")]
        let solve = solves.remove(&frame_id);
        #[cfg(feature = "solver")]
        let solved = solve.is_some();
        let run = force
            || match db_analysis::get_frame_analysis(conn, frame_id) {
                Ok(Some(existing)) => {
                    let outdated = {
                        #[cfg(feature = "solver")]
                        {
                            crate::analysis::zero_point::zp_outdated(&existing, solved, catalog)
                        }
                        #[cfg(not(feature = "solver"))]
                        {
                            false
                        }
                    };
                    row_is_stale(&existing, config_hash) || outdated
                }
                Ok(None) => true,
                Err(e) => {
                    // A read that failed is not a verdict that the frame is current: measure it again.
                    tracing::warn!(frame_id, error = %e, "analysis row unreadable; the frame is measured again");
                    true
                }
            };
        if !run {
            continue;
        }
        jobs.push(FrameJob {
            frame_id,
            file_id,
            path,
            #[cfg(feature = "solver")]
            zp: solve.and_then(|record| zp_work(&record, exptime, date_obs.as_deref())),
        });
        #[cfg(not(feature = "solver"))]
        let _ = (exptime, date_obs);
    }
    Ok(jobs)
}

/// Measure one frame's file: the Analysis rows and, for a solved frame against an installed catalogue, the forced-list
/// zero point in the same read. A frame without a solve or a catalogue stores none (`debug` says which); a solve that
/// cannot be turned into a transform stores `unreadable_solve`. Returns the file's `flip_vertical` and the solve id the zero point was judged against.
fn analyze_job_file(
    path: &str,
    job: &FrameJob,
    config: &AnalysisConfig,
    pool: &Arc<rayon::ThreadPool>,
    config_hash: &str,
    #[cfg(feature = "solver")] source: Option<&ZpSource<'_>>,
) -> anyhow::Result<(FrameAnalysis, Vec<StarMetric>, bool, Option<i64>)> {
    #[cfg(feature = "solver")]
    {
        match (&job.zp, source) {
            (Some(work), Some(source)) => {
                return match &work.wcs {
                    Ok(wcs) => {
                        let zp = analyzer::ZpFrame {
                            wcs,
                            exptime_s: work.exptime_s,
                            epoch: work.epoch,
                            identity: source.identity,
                            cone: source.cone,
                        };
                        let (analysis, stars, flip) =
                            analyzer::analyze_frame_with_zero_point(path, config, Some(pool), config_hash, Some(&zp))?;
                        Ok((analysis, stars, flip, Some(work.solve_id)))
                    }
                    Err(why) => {
                        tracing::debug!(frame_id = job.frame_id, reason = %why, "zero point not measured: the solve cannot be read");
                        let (mut analysis, stars, flip) = analyzer::analyze_frame(path, config, Some(pool), config_hash)?;
                        crate::db::analysis::StoredZeroPoint::code_only(
                            crate::analysis::zero_point::ZP_VERSION,
                            "unreadable_solve",
                        )
                        .apply_to(&mut analysis, source.identity);
                        Ok((analysis, stars, flip, Some(work.solve_id)))
                    }
                };
            }
            (None, Some(_)) => {
                tracing::debug!(frame_id = job.frame_id, zp_reason = "no_solve", "zero point not measured: the frame has no plate solve");
            }
            _ => {}
        }
    }
    #[cfg(not(feature = "solver"))]
    let _ = job;
    let (analysis, stars, flip) = analyzer::analyze_frame(path, config, Some(pool), config_hash)?;
    Ok((analysis, stars, flip, None))
}

/// A frame a run could not measure (its file is unreachable or unreadable): the caller says how loudly.
struct JobFailure {
    frame_id: i64,
    path: String,
    error: String,
}

/// How many frames a run measures at once: the configured `batch_concurrency` (0 = about one worker per 3 pool threads)
/// within `cap`.
fn worker_count(config: &AnalysisConfig, cap: usize) -> usize {
    let cap = cap.max(1);
    if config.batch_concurrency == 0 {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 3).max(2).min(cap)
    } else {
        (config.batch_concurrency as usize).clamp(1, cap)
    }
}

/// Measure `jobs` on `concurrency` worker threads pulling from one queue: with par_iter every frame competes for the
/// pool and runs single-threaded, while a few workers give each frame more pool threads for its PSF fitting and
/// background estimation and let work-stealing fill the serial gaps between stages. Analyze and the pass after a solve
/// batch share it. A zip-resident frame is extracted here, per frame, so an unreachable zip fails only its own frames
/// (the pin keeps the cache file until the frame is measured). `on_done(path)` is called once per frame, measured or
/// failed; a set `cancel` stops the workers taking new frames.
fn measure_jobs(
    ctx: &ServiceContext,
    jobs: Vec<FrameJob>,
    config: &AnalysisConfig,
    config_hash: &str,
    concurrency: usize,
    cancel: &AtomicBool,
    #[cfg(feature = "solver")] source: Option<&ZpSource<'_>>,
    on_done: &(dyn Fn(&str) + Sync),
) -> Vec<Result<AnalyzedFrame, JobFailure>> {
    let pool = Arc::clone(&ctx.image_pool);
    let total = jobs.len();
    let work = std::sync::Mutex::new(jobs.into_iter());
    let results_lock: std::sync::Mutex<Vec<Result<AnalyzedFrame, JobFailure>>> =
        std::sync::Mutex::new(Vec::with_capacity(total));

    std::thread::scope(|s| {
        for _ in 0..concurrency {
            s.spawn(|| loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let item = work.lock().unwrap().next();
                let Some(job) = item else {
                    break;
                };
                let (frame_id, file_id, path) = (job.frame_id, job.file_id, job.path.clone());

                let result = match crate::zip_store::materialize::materialize_one(ctx, &path).and_then(|(local, pin)| {
                    let measured = analyze_job_file(
                        &local.to_string_lossy(),
                        &job,
                        config,
                        &pool,
                        config_hash,
                        #[cfg(feature = "solver")]
                        source,
                    );
                    drop(pin);
                    measured
                }) {
                    Ok((mut analysis, stars, _flip, solve_id)) => {
                        analysis.frame_id = frame_id;
                        analysis.file_id = file_id;
                        Ok(AnalyzedFrame { frame_id, analysis, stars, solve_id })
                    }
                    Err(e) => Err(JobFailure { frame_id, path: path.clone(), error: e.to_string() }),
                };
                on_done(&path);
                results_lock.lock().unwrap().push(result);
            });
        }
    });

    results_lock.into_inner().unwrap()
}

/// Keep a zero point only while the frame's solve is still the one it was measured against (`solve_id`, `None` = no
/// zero point was attempted): the read happens inside the caller's transaction, so a new or deleted solve landing
/// meanwhile has either cleared the frame's `zp_*` already or is seen here, and the nine fields go NULL.
fn settle_zero_point(
    conn: &rusqlite::Connection,
    frame_id: i64,
    analysis: &mut FrameAnalysis,
    solve_id: Option<i64>,
) -> Result<(), ApiError> {
    let Some(solve_id) = solve_id else {
        return Ok(());
    };
    let current: Option<i64> = rusqlite::OptionalExtension::optional(conn.query_row(
        "SELECT id FROM plate_solves WHERE frame_id = ?1",
        rusqlite::params![frame_id],
        |row| row.get(0),
    ))?;
    if current != Some(solve_id) {
        db_analysis::clear_zero_point_fields(analysis);
        tracing::debug!(frame_id, "zero point superseded");
    }
    Ok(())
}

/// Store a run's analyses, their stars and their zero points in ONE transaction. `BEGIN IMMEDIATE`: the solve is read and
/// then the row is written, so another writer must not slip a new solve in between.
fn persist_analyzed_frames(conn: &rusqlite::Connection, frames: &mut [AnalyzedFrame]) -> Result<(), ApiError> {
    if frames.is_empty() {
        return Ok(());
    }
    // Drop-rollback also covers a failed COMMIT (audit M11).
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    for f in frames.iter_mut() {
        settle_zero_point(conn, f.frame_id, &mut f.analysis, f.solve_id)?;
        let analysis_id = db_analysis::upsert_frame_analysis(conn, &f.analysis)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        db_analysis::upsert_star_metrics(conn, analysis_id, &f.stars)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    tx.commit()?;
    Ok(())
}

// ── The pass after a solve batch (forced-list spec §5.4, plan Task 5c, ruling R7) ───────────────────────────────

/// The frames among `ids` the pass after a solve batch measures: lights of a frame set (a second copy of a frame the set
/// already holds is never measured, as in Analyze) with a plate solve, an analysis that is current ([`row_is_stale`]:
/// a stale one is the next Analyze's whole job) and a zero point that is not current for `catalog`
/// ([`crate::analysis::zero_point::zp_outdated`]). Each comes with the id of the analysis row it was selected on: the
/// write lands only on that row. One statement for the frames and one for the solves.
#[cfg(feature = "solver")]
fn select_pass_jobs(
    conn: &rusqlite::Connection,
    ids: &[i64],
    config_hash: &str,
    catalog: &str,
) -> Result<Vec<(FrameJob, i64)>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT f.id, fi.id, fi.path, f.exptime, f.date_obs
         FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         INNER JOIN session_members sm ON sm.frame_id = f.id
         INNER JOIN sessions s ON s.id = sm.session_id
         INNER JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE f.id IN ({marks})
           AND f.imagetyp = 'Light'
           AND {used_copy}",
        used_copy = crate::db::frame_is_used_copy_sql("n.frames_set_id"),
    ))?;
    let rows: Vec<(i64, i64, String, Option<f64>, Option<String>)> = stmt
        .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let found: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let mut solves = crate::plate_solve::storage::get_plate_solves_for_frames(conn, &found)
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    let mut jobs = Vec::new();
    for (frame_id, file_id, path, exptime, date_obs) in rows {
        let Some(record) = solves.remove(&frame_id) else {
            continue;
        };
        let analysis = match db_analysis::get_frame_analysis(conn, frame_id) {
            Ok(Some(a)) => a,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(frame_id, error = %e, "analysis row unreadable; the zero point pass leaves the frame");
                continue;
            }
        };
        let (Some(analysis_id), false) = (analysis.id, row_is_stale(&analysis, config_hash)) else {
            continue;
        };
        if !crate::analysis::zero_point::zp_outdated(&analysis, true, Some(catalog)) {
            continue;
        }
        let Some(zp) = zp_work(&record, exptime, date_obs.as_deref()) else {
            continue;
        };
        jobs.push((FrameJob { frame_id, file_id, path, zp: Some(zp) }, analysis_id));
    }
    Ok(jobs)
}

/// Write the pass's judgements: for each measured frame only its nine `zp_*` columns, on the analysis row it was selected
/// on and while its solve is still the one it was judged against ([`db_analysis::set_zero_point`], compare-and-set on both
/// ids). A re-analysis or a new solve meanwhile wins: nothing is written for that frame. A run that could not judge a
/// frame (nothing to store) counts as failed and is left for the next Analyze.
#[cfg(feature = "solver")]
fn persist_pass_zero_points(
    conn: &rusqlite::Connection,
    frames: &[AnalyzedFrame],
    analysis_ids: &std::collections::HashMap<i64, i64>,
    stats: &mut crate::analysis::zero_point::ZpPassStats,
) -> Result<(), ApiError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    for f in frames {
        let (Some(figures), Some(catalog), Some(&analysis_id)) = (
            db_analysis::StoredZeroPoint::of_row(&f.analysis),
            f.analysis.zp_catalog.as_deref(),
            analysis_ids.get(&f.frame_id),
        ) else {
            stats.failed += 1;
            stats.first_error.get_or_insert_with(|| "the run could not judge the frame".to_string());
            tracing::debug!(frame_id = f.frame_id, "zero point not stored: the run could not judge the frame");
            continue;
        };
        match db_analysis::set_zero_point(conn, f.frame_id, analysis_id, f.solve_id, &figures, catalog) {
            Ok(true) => stats.stored += 1,
            Ok(false) => tracing::debug!(frame_id = f.frame_id, "zero point superseded"),
            Err(e) => {
                stats.failed += 1;
                stats.first_error.get_or_insert_with(|| format!("{e:#}"));
                tracing::debug!(frame_id = f.frame_id, error = %e, "zero point not stored: the write failed");
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// The pass after a solve batch, blocking: measure the zero point of the frames `frame_ids` the batch just solved
/// (Analyze's own worker, [`measure_jobs`], so there is one measurement and one set of `zp_*` figures), chunk by chunk of
/// [`crate::analysis::zero_point::PASS_CHUNK`] under the global compute queue (never beside Analyze or a stack). A frame
/// without a current analysis is left to Analyze; one already current is skipped. No star catalogue installed: nothing
/// to do (`debug`). A file that is offline or unreadable is a `debug` and counted; the pass says one
/// `warn_if_unmeasurable("solve")` for them and ends with one `info`.
#[cfg(feature = "solver")]
pub fn solve_zero_point_pass(
    ctx: &ServiceContext,
    frame_ids: &[i64],
) -> Result<crate::analysis::zero_point::ZpPassStats, ApiError> {
    solve_zero_point_pass_with(ctx, frame_ids, None)
}

/// [`solve_zero_point_pass`] with the catalogue given by the caller (`None` = the installed one): the tests give a
/// synthetic cone and identity.
#[cfg(feature = "solver")]
pub(crate) fn solve_zero_point_pass_with(
    ctx: &ServiceContext,
    frame_ids: &[i64],
    given: Option<&ZpSource<'_>>,
) -> Result<crate::analysis::zero_point::ZpPassStats, ApiError> {
    use crate::analysis::zero_point::{catalog_cone, zero_point_catalog, ZpCatalog, ZpPassStats, PASS_CHUNK};

    let started = std::time::Instant::now();
    let mut stats = ZpPassStats::default();
    let Some(identity) = given.map(|g| g.identity.to_owned()).or_else(|| installed_identity(ctx)) else {
        tracing::debug!(zp_reason = "no_catalogue", "zero point solve pass skipped: no star catalogue installed");
        return Ok(stats);
    };
    let span = tracing::info_span!("zero_point_solve_pass", catalog = %identity);
    let _in_span = span.enter();

    let (config, config_hash) = {
        let handle = db(ctx)?;
        let config = config::load_config(&handle.conn());
        let hash = config.config_hash();
        (config, hash)
    };
    let concurrency = worker_count(&config, ctx.image_pool.current_num_threads());
    // Never set: the pass has no cancel button, the queue ticket needs a flag.
    let cancel = Arc::new(AtomicBool::new(false));
    let mut opened: Option<ZpCatalog> = None;
    let mut frames = 0usize;

    for chunk in frame_ids.chunks(PASS_CHUNK) {
        // The compute queue is taken before the frames are read: what Analyze or a stack held it for is read fresh.
        let permit = match ctx.compute_queue.acquire(
            crate::services::compute_queue::ComputeJobKind::Analysis,
            "Analysis: zero points after a solve",
            cancel.clone(),
        ) {
            Ok((permit, _job_id)) => permit,
            Err(_cancelled) => {
                tracing::warn!("zero point solve pass cancelled in the compute queue");
                break;
            }
        };
        let selected = {
            let handle = db(ctx)?;
            let conn = handle.conn();
            select_pass_jobs(&conn, chunk, &config_hash, &identity)?
        };
        if selected.is_empty() {
            drop(permit);
            continue;
        }
        let analysis_ids: std::collections::HashMap<i64, i64> = selected.iter().map(|(j, a)| (j.frame_id, *a)).collect();
        let jobs: Vec<FrameJob> = selected.into_iter().map(|(job, _)| job).collect();
        frames += jobs.len();

        // The installed catalogue's tiers, opened once and only when a frame needs them.
        if given.is_none() && opened.is_none() {
            opened = ctx.app_data_dir().and_then(|dir| zero_point_catalog(&dir));
            if opened.is_none() {
                tracing::debug!("zero point solve pass ended: the star catalogue could not be opened");
                break;
            }
        }
        let own_cone;
        let own_source;
        let source: &ZpSource<'_> = match given {
            Some(g) => g,
            None => {
                let Some(catalog) = opened.as_ref() else { break };
                own_cone = catalog_cone(&catalog.layers);
                own_source = ZpSource { identity: &catalog.identity, cone: &own_cone };
                &own_source
            }
        };

        let results = measure_jobs(ctx, jobs, &config, &config_hash, concurrency, &cancel, Some(source), &|_: &str| {});
        let mut measured: Vec<AnalyzedFrame> = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(frame) => measured.push(frame),
                Err(failure) => {
                    stats.unreadable += 1;
                    stats.first_error.get_or_insert_with(|| format!("{}: {}", failure.path, failure.error));
                    tracing::debug!(
                        frame_id = failure.frame_id,
                        path = %failure.path,
                        error = %failure.error,
                        "zero point not measured: the file cannot be read"
                    );
                }
            }
        }
        {
            let handle = db(ctx)?;
            let conn = handle.conn();
            persist_pass_zero_points(&conn, &measured, &analysis_ids, &mut stats)?;
        }
        drop(permit);
    }

    stats.warn_if_unmeasurable("solve");
    tracing::info!(
        frames,
        zp_measured = stats.stored,
        duration_ms = started.elapsed().as_millis() as u64,
        "zero point solve pass done"
    );
    Ok(stats)
}

#[cfg(feature = "solver")]
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Start the pass after a solve batch on its own thread (`zero-point-solve-pass`) over the frames the batch solved. One
/// pass runs at a time; frames asked for while it runs join its pending set and it takes them before it ends
/// ([`SolvePassGuard`]). A panic or an error ends that round, never the thread unannounced, and the guard is released on
/// every exit path.
#[cfg(feature = "solver")]
pub fn spawn_solve_zero_point_pass(ctx: Arc<ServiceContext>, frame_ids: Vec<i64>) {
    static GUARD: std::sync::Mutex<SolvePassGuard> = std::sync::Mutex::new(SolvePassGuard::new());
    let lock = || GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if frame_ids.is_empty() {
        return;
    }
    match lock().request(&frame_ids) {
        PassRequest::Start => {}
        PassRequest::Queued => {
            tracing::debug!(frames = frame_ids.len(), "zero point solve pass queued behind the running one");
            return;
        }
    }
    let spawned = std::thread::Builder::new().name("zero-point-solve-pass".into()).spawn(move || {
        let mut release = PassRelease::new(&GUARD);
        let mut ids = frame_ids;
        loop {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| solve_zero_point_pass(&ctx, &ids))) {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "zero point solve pass failed"),
                Err(payload) => tracing::warn!(error = %panic_text(payload.as_ref()), "zero point solve pass panicked"),
            }
            match lock().finish() {
                Some(next) => ids = next,
                None => {
                    release.disarm();
                    break;
                }
            }
        }
    });
    if let Err(e) = spawned {
        // Nothing runs: the guard is free for the next batch.
        lock().abandon();
        tracing::warn!(error = %e, "zero point solve pass thread did not start");
    }
}

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
/// `thread_budget` caps the worker-pool concurrency below. It is read from
/// `AppState`/`WebAppState::max_blink_threads` (the image pool's size,
/// `machine::pool_threads`, which has no 16 cap since Task 13c), a
/// platform-state field that has no `ServiceContext` equivalent: same shape
/// as `set_scan_root_monitor_enabled`'s `monitor: &MonitorService` param.
/// The old hardcoded `16` ceiling is gone, so on a machine with more than 16
/// pool threads a configured `batch_concurrency` above 16 is now honoured.
pub fn analyze_frame_set(
    ctx: &ServiceContext,
    frame_set_id: i64,
    force: Option<bool>,
    thread_budget: usize,
    emitter: &dyn ProgressEmitter,
) -> Result<AnalyzeFrameSetResult, ApiError> {
    analyze_frame_set_with(
        ctx,
        frame_set_id,
        force,
        thread_budget,
        emitter,
        #[cfg(feature = "solver")]
        None,
    )
}

/// [`analyze_frame_set`] with the catalogue the zero point is measured against given by the caller (`source`; `None` =
/// the installed one). The tests give a synthetic cone and identity.
pub(crate) fn analyze_frame_set_with(
    ctx: &ServiceContext,
    frame_set_id: i64,
    force: Option<bool>,
    thread_budget: usize,
    emitter: &dyn ProgressEmitter,
    #[cfg(feature = "solver")] source: Option<&ZpSource<'_>>,
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
            return Err(ApiError::Conflict(
                "Analysis already in progress for this frame set".to_string(),
            ));
        }
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut analyses = ctx.active_analyses.lock().unwrap();
        analyses.insert(
            frame_set_id,
            crate::services::AnalysisHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
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
            emit_event(
                emitter,
                "analysis-complete",
                &AnalysisCompleteEvent {
                    frame_set_id,
                    analyzed: 0,
                    skipped: 0,
                    failed: 0,
                    errors: Vec::new(),
                    cancelled: true,
                },
            );
            // Auto-publish trigger (collab v3 wave 2, Task 10). Nothing was
            // analyzed here (cancelled before admission), but a coalesced
            // run over this set is cheap when nothing changed (Task 7), and
            // keeping one call site per `analysis-complete` emission is
            // simpler than special-casing the early-cancel branch.
            #[cfg(feature = "solver")]
            crate::api::collab_autopublish::request_auto_publish_for_sets(&[frame_set_id]);
            return Ok(AnalyzeFrameSetResult {
                analyzed: 0,
                skipped: 0,
                failed: 0,
                errors: Vec::new(),
                cancelled: true,
            });
        }
    };

    // Load config and frame list under DB lock, then release. The catalogue is looked at once for the whole run: its
    // identity (a stat of the tier files) decides which solved frames are not current, and its tiers are opened only
    // when a frame will be measured against them.
    let (analysis_config, frames_to_analyze) = {
        let db = db(ctx)?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);
        let config_hash = analysis_config.config_hash();
        #[cfg(feature = "solver")]
        let identity = source.map(|s| s.identity.to_owned()).or_else(|| installed_identity(ctx));
        #[cfg(not(feature = "solver"))]
        let identity: Option<String> = None;
        let jobs = select_frame_jobs(&conn, frame_set_id, &config_hash, identity.as_deref(), force)?;
        (analysis_config, jobs)
    };

    // The installed catalogue's tiers, opened once and only when a frame of this run has a solve to measure against.
    #[cfg(feature = "solver")]
    if source.is_none() && frames_to_analyze.iter().any(|j| j.zp.is_some()) && installed_identity(ctx).is_none() {
        tracing::debug!(zp_reason = "no_catalogue", "zero point not measured: no star catalogue installed");
    }
    #[cfg(feature = "solver")]
    let opened = match source {
        Some(_) => None,
        None if frames_to_analyze.iter().any(|j| j.zp.is_some()) => ctx
            .app_data_dir()
            .and_then(|dir| crate::analysis::zero_point::zero_point_catalog(&dir)),
        None => None,
    };
    #[cfg(feature = "solver")]
    let opened_cone = opened.as_ref().map(|c| crate::analysis::zero_point::catalog_cone(&c.layers));
    #[cfg(feature = "solver")]
    let source: Option<ZpSource<'_>> = match (source, opened.as_ref(), opened_cone.as_ref()) {
        (Some(given), _, _) => Some(ZpSource { identity: given.identity, cone: given.cone }),
        (None, Some(catalog), Some(cone)) => Some(ZpSource { identity: &catalog.identity, cone }),
        _ => None,
    };

    let total = frames_to_analyze.len();

    let config_hash = analysis_config.config_hash();
    let completed = AtomicUsize::new(0);
    let concurrency = worker_count(&analysis_config, thread_budget);
    let results = measure_jobs(
        ctx,
        frames_to_analyze,
        &analysis_config,
        &config_hash,
        concurrency,
        &cancel_flag,
        #[cfg(feature = "solver")]
        source.as_ref(),
        &|path: &str| {
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            let filename = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string());
            emit_event(
                emitter,
                "analysis-progress",
                &AnalysisProgressEvent {
                    frame_set_id,
                    current: done,
                    total,
                    current_file: filename,
                    percent: if total > 0 { (done as f64 / total as f64) * 100.0 } else { 100.0 },
                },
            );
        },
    );

    let was_cancelled = cancel_flag.load(Ordering::Relaxed);

    // Clean up active_analyses entry (always, even on early return)
    {
        let mut analyses = ctx.active_analyses.lock().unwrap();
        analyses.remove(&frame_set_id);
    }

    // Partition results
    let mut all_analyses: Vec<AnalyzedFrame> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for result in results {
        match result {
            Ok(frame) => all_analyses.push(frame),
            Err(failure) => {
                tracing::warn!(
                    frame_id = failure.frame_id,
                    path = %failure.path,
                    error = %failure.error,
                    "frame analysis failed"
                );
                errors.push(format!("{}: {}", failure.path, failure.error));
            }
        }
    }
    let analyzed = all_analyses.len();
    let failed = errors.len();

    // Persist all results in a single transaction: the analysis row, its stars and its zero point commit together.
    // The zero point is written only while the frame's solve is still the one it was measured against.
    {
        let db = db(ctx)?;
        let conn = db.conn();
        persist_analyzed_frames(&conn, &mut all_analyses)?;
    }

    let skipped = total.saturating_sub(analyzed + failed);

    // Release the compute-queue slot before emitting the completion event
    // (not after) so the next queued job is admitted the moment CPU work
    // ends, rather than waiting on event serialization.
    drop(queue_permit);

    emit_event(
        emitter,
        "analysis-complete",
        &AnalysisCompleteEvent {
            frame_set_id,
            analyzed,
            skipped,
            failed,
            errors: errors.clone(),
            cancelled: was_cancelled,
        },
    );
    // Auto-publish trigger (collab v3 wave 2, Task 10): fresh analysis can
    // move a frame's per-frame gate verdict (eccentricity, trail rejection).
    #[cfg(feature = "solver")]
    crate::api::collab_autopublish::request_auto_publish_for_sets(&[frame_set_id]);

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
        Err(ApiError::NotFound(
            "No active analysis for this frame set".to_string(),
        ))
    }
}

/// Get all stored analysis results for a frame set. Each row's `zp_outdated` says whether its zero point is not current
/// ([`crate::analysis::zero_point::zp_outdated`]: a catalogue is installed, the frame is solved, and the row carries
/// another zero-point version or catalogue): the installed identity is a stat of the tier files and the solves are one
/// chunked query, so the listing reads no pixel and no star.
pub fn get_analysis_for_frame_set(
    ctx: &ServiceContext,
    frame_set_id: i64,
) -> Result<Vec<FrameAnalysis>, ApiError> {
    #[cfg(feature = "solver")]
    let identity = installed_identity(ctx);
    let db = db(ctx)?;
    let conn = db.conn();
    #[allow(unused_mut)]
    let mut rows = db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)?;
    #[cfg(feature = "solver")]
    if let Some(identity) = identity.as_deref() {
        let ids: Vec<i64> = rows.iter().map(|a| a.frame_id).collect();
        let solved = crate::plate_solve::storage::frames_with_solve(&conn, &ids)
            .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
        for a in &mut rows {
            a.zp_outdated =
                crate::analysis::zero_point::zp_outdated(a, solved.contains(&a.frame_id), Some(identity));
        }
    }
    Ok(rows)
}

/// The one count of "N to update" and its reasons for a frame set's zero points, for the Analysis tab (plan Task 6a/6b):
/// [`crate::analysis::transparency::count_zero_point_updates`] over the set's analysis rows, with the installed
/// catalogue's identity and the set's solved frames. `to_update` is exactly what a run of Analyze (without Re-Analyze)
/// measures for the zero point; frames that wait on a catalogue that is not installed, or whose measurement failed,
/// are reported apart and never counted in it. Reads no pixel and no star: a stat of the tier files, one chunked solve query.
#[cfg(feature = "solver")]
pub fn zero_point_updates(
    ctx: &ServiceContext,
    frame_set_id: i64,
) -> Result<crate::analysis::transparency::ZeroPointUpdates, ApiError> {
    let identity = installed_identity(ctx);
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)?;
    let ids: Vec<i64> = rows.iter().map(|a| a.frame_id).collect();
    let solved = crate::plate_solve::storage::frames_with_solve(&conn, &ids)
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    Ok(crate::analysis::transparency::count_zero_point_updates(&rows, &solved, identity.as_deref()))
}

/// Delegates to the central orientation helper — see
/// `crate::orientation::flip_vertical_for_path` doc for the astronomical
/// (not TOP-DOWN/BOTTOM-UP-literal) convention this applies.
fn detect_flip_vertical(path: &str) -> bool {
    crate::orientation::flip_vertical_for_path(std::path::Path::new(path))
}

/// Store one frame's analysis row and its star rows in ONE transaction (as the batch path does): a reader (the gate)
/// can never see the row with a partial star list. Sets each star's `frame_analysis_id`; on any failure the transaction
/// rolls back and no analysis row is left behind. The zero point rides in the same upsert, only while the frame's solve
/// is still `solve_id` ([`settle_zero_point`]); otherwise its nine fields are cleared on `analysis` first.
fn persist_frame_analysis(
    conn: &rusqlite::Connection,
    analysis: &mut FrameAnalysis,
    stars: &mut [StarMetric],
    solve_id: Option<i64>,
) -> Result<i64, ApiError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    settle_zero_point(conn, analysis.frame_id, analysis, solve_id)?;
    let analysis_id = db_analysis::upsert_frame_analysis(conn, analysis)?;
    for s in stars.iter_mut() {
        s.frame_analysis_id = analysis_id;
    }
    db_analysis::upsert_star_metrics(conn, analysis_id, stars)?;
    tx.commit()?;
    Ok(analysis_id)
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
pub async fn get_frame_star_metrics(
    ctx: &ServiceContext,
    frame_id: i64,
) -> Result<StarMetricsResponse, ApiError> {
    // Named `db_handle` (not `db`) throughout this fn — a local binding
    // named `db` would shadow the `db(ctx)` helper fn for the rest of this
    // scope, breaking the second acquisition below (after the on-demand
    // analysis `.await`).
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();

    let analysis_config = config::load_config(&conn);
    let current_hash = analysis_config.config_hash();

    // Always need the file path for flip_vertical detection and possible on-demand analysis
    let (file_id, path): (i64, String) = conn
        .query_row(
            "SELECT fi.id, fi.path FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         WHERE f.id = ?1",
            rusqlite::params![frame_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| ApiError::NotFound(format!("Frame not found: {}", e)))?;

    // Reads the ROWORDER header, so a zip-resident frame is extracted (or reused from the cache) first.
    // An unreachable zip is an error here: a silent `false` would flip the stored stars the wrong way.
    drop(conn);
    // `block_in_place`: the extraction is a full entry decompress (possibly over a share); it must not pin an async
    // worker (both hosts run the multi-thread runtime).
    let (flip_local, flip_pin) =
        tokio::task::block_in_place(|| crate::zip_store::materialize::materialize_one(ctx, &path)).map_err(|e| {
            tracing::warn!(frame_id, path = %path, error = %e, "frame is not readable for star metrics");
            ApiError::Internal(format!("{}: {}", path, e))
        })?;
    let flip_vertical = detect_flip_vertical(&flip_local.to_string_lossy());
    let conn = db_handle.conn();

    // Check if we have fresh analysis data with stars
    if let Ok(Some(existing)) = db_analysis::get_frame_analysis(&conn, frame_id) {
        if !row_is_stale(&existing, &current_hash) {
            if let Ok(stars) = db_analysis::get_star_metrics_by_frame_id(&conn, frame_id) {
                // A trailed / defocused / no-stars frame stores no star (PC-6), so its verdict also makes the row
                // servable as it is.
                if !stars.is_empty() || existing.frame_verdict.is_some() {
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

    // Stale or missing — analyze on-demand. A solved frame is measured for its zero point in the same read when a
    // catalogue is installed (the batch's own entry); the solve and header facts are read here, under the connection.
    #[cfg(feature = "solver")]
    let (identity, zp) = {
        let identity = installed_identity(ctx);
        let zp = match identity {
            Some(_) => zp_job_of_frame(&conn, frame_id)?,
            None => None,
        };
        (identity, zp)
    };
    drop(conn);
    let _ = db_handle;

    let pool = Arc::clone(&ctx.image_pool);
    let config_hash = current_hash.clone();
    // `flip_pin` (taken above) keeps the cache file of a zip-resident frame alive across the analysis.
    let path_owned = flip_local.to_string_lossy().into_owned();
    #[cfg(feature = "solver")]
    let app_data_dir = ctx.app_data_dir();

    // The pin moves into the closure: a dropped handler future (client disconnect) must not release the cache file
    // while the blocking thread still reads it.
    let analysed = tokio::task::spawn_blocking(move || {
        let _pin = flip_pin;
        #[cfg(feature = "solver")]
        {
            let catalog = match (&zp, &identity, &app_data_dir) {
                (Some(_), Some(_), Some(dir)) => crate::analysis::zero_point::zero_point_catalog(dir),
                _ => None,
            };
            let cone = catalog.as_ref().map(|c| crate::analysis::zero_point::catalog_cone(&c.layers));
            let source = match (&catalog, &cone) {
                (Some(c), Some(cone)) => Some(ZpSource { identity: &c.identity, cone }),
                _ => None,
            };
            let job = FrameJob { frame_id, file_id, path: path_owned.clone(), zp };
            analyze_job_file(&path_owned, &job, &analysis_config, &pool, &config_hash, source.as_ref())
        }
        #[cfg(not(feature = "solver"))]
        {
            let job = FrameJob { frame_id, file_id, path: path_owned.clone() };
            analyze_job_file(&path_owned, &job, &analysis_config, &pool, &config_hash)
        }
    })
    .await;
    let (mut analysis, mut stars, flip_vertical, solve_id) = analysed
        .map_err(|e| ApiError::Internal(format!("Analysis panicked: {}", e)))?
        .map_err(|e| ApiError::Internal(format!("Analysis failed: {}", e)))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;

    // Persist: the row, its stars and its zero point commit together, so no reader sees an analysis with a partial star
    // list. The response carries what was stored.
    {
        let db = db(ctx)?;
        let conn = db.conn();
        persist_frame_analysis(&conn, &mut analysis, &mut stars, solve_id)?;
    }

    Ok(StarMetricsResponse {
        image_width: analysis.width,
        image_height: analysis.height,
        metrics: analysis,
        stars,
        flip_vertical,
    })
}

/// Star metrics for a project frame in Blink: the file Blink shows for it
/// (`collab_blink::resolve_collab_frame_path`, the same rule as
/// `get_collab_frame_image`), analysed on demand with the Analysis tab's
/// config. A project frame — a published copy or a member's replica — is no
/// catalog frame, so nothing is stored: `frame_id` and `file_id` of the
/// answer are 0, and Blink keeps the answer for as long as it is open.
pub async fn get_collab_frame_star_metrics(
    ctx: &ServiceContext,
    project_id: &str,
    frame: &crate::api::collab_blink::CollabFrameRef,
) -> Result<StarMetricsResponse, ApiError> {
    let (path, _) = crate::api::collab_blink::resolve_collab_frame_path(ctx, project_id, frame)?;
    let path = path.to_string_lossy().into_owned();
    let (analysis_config, config_hash) = {
        let db_handle = db(ctx)?;
        let conn = db_handle.conn();
        let analysis_config = config::load_config(&conn);
        let config_hash = analysis_config.config_hash();
        (analysis_config, config_hash)
    };
    // An attested own frame resolves to its raw original, which may sit inside a zip.
    let (local, pin) =
        tokio::task::block_in_place(|| crate::zip_store::materialize::materialize_one(ctx, &path)).map_err(|e| {
            tracing::warn!(project_id, path = %path, error = %e, "project frame is not readable for star metrics");
            ApiError::Internal(format!("{}: {}", path, e))
        })?;
    let pool = Arc::clone(&ctx.image_pool);
    let local = local.to_string_lossy().into_owned();
    let analysed = tokio::task::spawn_blocking(move || {
        let _pin = pin;
        analyzer::analyze_frame(&local, &analysis_config, Some(&pool), &config_hash)
    })
    .await;
    let (analysis, stars, flip_vertical) = analysed
        .map_err(|e| ApiError::Internal(format!("Analysis panicked: {}", e)))?
        .map_err(|e| ApiError::Internal(format!("Analysis failed: {}", e)))?;
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
// Port of the ERP's flat-contour-plot script v1.3.1. Reads a flat (or master flat) at
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
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    // Resolve file path under the DB lock, then drop it before the (slow)
    // pixel work to avoid holding the connection across an await.
    let path = {
        let db = db(ctx)?;
        let conn = db.conn();
        conn.query_row(
            "SELECT path FROM files WHERE id = ?1",
            rusqlite::params![file_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| ApiError::NotFound(format!("File not found (id={}): {}", file_id, e)))?
    };

    let (local, pin) =
        tokio::task::block_in_place(|| crate::zip_store::materialize::materialize_one(ctx, &path)).map_err(|e| {
            tracing::warn!(file_id, path = %path, error = %e, "flat is not readable for the contour plot");
            ApiError::Internal(format!("Flat contour analysis failed: {}: {}", path, e))
        })?;
    let local = local.to_string_lossy().into_owned();
    let computed = tokio::task::spawn_blocking(move || {
        let _pin = pin;
        flat_analysis::compute_flat_contour_plot(&local, opts)
    })
    .await;
    let result = computed
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{FrameAnalysis, FrameVerdict};

    /// Seeds frame 1 (file absent on disk, so a re-analysis fails) with a trailed-verdict row; `tweak` edits the row
    /// before it is stored, and receives the current config hash.
    fn seed_verdict_row(ctx: &ServiceContext, tweak: impl FnOnce(&mut FrameAnalysis)) -> i64 {
        let handle = db(ctx).unwrap();
        let conn = handle.conn();
        conn.execute(
            "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
             VALUES (1,'/nowhere/t.fits','t.fits',0,'2025-01-01','FITS','2025-01-01')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO frames (id,file_id) VALUES (1,1)", []).unwrap();
        let hash = config::load_config(&conn).config_hash();
        let mut a = FrameAnalysis {
            id: None,
            frame_id: 1,
            file_id: 1,
            stars_detected: 400,
            median_fwhm: 0.0,
            median_eccentricity: 0.0,
            median_snr: 0.0,
            median_hfr: 0.0,
            frame_snr: 30.0,
            snr_weight: 0.0,
            psf_signal: 0.0,
            background: 100.0,
            noise: 5.0,
            detection_threshold: 6.0,
            width: 600,
            height: 450,
            source_channels: 1,
            trail_r_squared: 0.0,
            possibly_trailed: true,
            median_beta: None,
            quality_score: None,
            config_hash: Some(hash),
            analyzed_at: "2026-10-24T00:00:00Z".into(),
            measure_version: Some(crate::star_measure::MEASURE_VERSION),
            stars_refused: Some(25),
            frame_verdict: Some(FrameVerdict::Trailed),
            zero_point: None,
            zp_stars: None,
            zp_sigma: None,
            zp_version: None,
            zp_catalog: None,
            zp_refusal: None,
            zp_list_stars: None,
            zp_window_bright: None,
            zp_window_slope: None,
            zp_outdated: false,
        };
        tweak(&mut a);
        db_analysis::upsert_frame_analysis(&conn, &a).unwrap();
        1
    }

    /// C5 review I1: a trailed / defocused / no-stars frame stores no `StarMetric`, so its verdict alone makes a fresh
    /// row servable. The file does not exist: a re-analysis would fail, so an `Ok` proves none ran.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_fresh_row_with_a_verdict_and_no_stars_is_served_without_reanalysis() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let frame_id = seed_verdict_row(&ctx, |_| {});
        let r = get_frame_star_metrics(&ctx, frame_id).await.expect("served from the stored row");
        assert!(r.stars.is_empty());
        assert_eq!(r.metrics.frame_verdict, Some(FrameVerdict::Trailed));
        assert_eq!((r.image_width, r.image_height), (600, 450));
    }

    /// C6 review m6: a STALE row (another config hash) that carries a verdict is not served; the re-analysis is
    /// attempted, and with the file absent it fails.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_row_with_a_verdict_is_reanalyzed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let frame_id = seed_verdict_row(&ctx, |a| a.config_hash = Some("another-hash".into()));
        let r = get_frame_star_metrics(&ctx, frame_id).await;
        assert!(r.is_err(), "a stale row must be recomputed, not served");
    }

    /// C6 review m7, batch rule: current hash + `measure_version` None is stale; current hash + current version is not.
    #[test]
    fn the_batch_treats_a_legacy_version_row_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        seed_verdict_row(&ctx, |_| {});
        let handle = db(&ctx).unwrap();
        let conn = handle.conn();
        let hash = config::load_config(&conn).config_hash();
        let mut a = db_analysis::get_frame_analysis(&conn, 1).unwrap().unwrap();
        assert!(!row_is_stale(&a, &hash));
        a.measure_version = None;
        assert!(row_is_stale(&a, &hash));
        a.measure_version = Some(crate::star_measure::MEASURE_VERSION);
        assert!(row_is_stale(&a, "another-hash"));
    }

    /// C6 review m7: a row with the current hash but no `measure_version` (written between C5 and C6) is stale in the
    /// on-demand path.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_row_with_the_current_hash_but_no_measure_version_is_reanalyzed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let frame_id = seed_verdict_row(&ctx, |a| a.measure_version = None);
        let r = get_frame_star_metrics(&ctx, frame_id).await;
        assert!(r.is_err(), "a legacy-version row must be recomputed, not served");
    }

    // ---- zip-resident frames (ZIP archives in the catalog, Task 7) ----

    /// A 16-bit light frame (768x512, stars on a grid) as FITS bytes; `seed` varies the header so fingerprints differ.
    fn star_fits(seed: u64) -> Vec<u8> {
        let (data, w, h) = crate::test_support::fixture_field(seed, 1.0, 0.002);
        let mut out = Vec::new();
        for c in [
            "SIMPLE  =                    T".to_string(),
            "BITPIX  =                   16".to_string(),
            "NAXIS   =                    2".to_string(),
            format!("NAXIS1  = {w:>20}"),
            format!("NAXIS2  = {h:>20}"),
            "BZERO   =                32768".to_string(),
            "BSCALE  =                    1".to_string(),
            "OBJECT  = 'M33     '".to_string(),
            "TELESCOP= 'T       '".to_string(),
            "INSTRUME= 'C       '".to_string(),
            "IMAGETYP= 'Light   '".to_string(),
            format!("SEQNUM  = {seed:>20}"),
            "END".to_string(),
        ] {
            let mut card = vec![b' '; 80];
            card[..c.len()].copy_from_slice(c.as_bytes());
            out.extend(card);
        }
        while out.len() % 2880 != 0 {
            out.push(b' ');
        }
        for v in data {
            let u = (v.clamp(0.0, 1.0) * 65535.0).round() as i32;
            out.extend(((u - 32768) as i16).to_be_bytes());
        }
        while out.len() % 2880 != 0 {
            out.push(0);
        }
        out
    }

    struct ZipRig {
        tmp: tempfile::TempDir,
        ctx: ServiceContext,
    }

    fn zip_rig() -> ZipRig {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("db")).unwrap();
        std::fs::create_dir_all(tmp.path().join("astro")).unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("db").join("catalog.db"));
        {
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [tmp.path().join("astro").to_str().unwrap()]).unwrap();
            conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M33')", []).unwrap();
            conn.execute(
                "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2026-07-01', '2026-07-02')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        }
        ZipRig { tmp, ctx }
    }

    impl ZipRig {
        /// Zips one frame as `<name>.zip`, catalogs the zip, puts its frame into the analysed set. Returns (zip, frame id).
        fn zipped_light(&self, name: &str, seed: u64) -> (std::path::PathBuf, i64) {
            use crate::zip_store::scan::test_support::make_zip;
            let zip = make_zip(
                &self.tmp.path().join("astro"),
                &format!("{name}.zip"),
                &[("Lights/L_001.fits", star_fits(seed))],
                crate::archive::models::ArchiveCompression::Deflate,
            );
            let handle = db(&self.ctx).unwrap();
            let conn = handle.conn();
            crate::zip_store::scan::scan_zip(&conn, &zip, 1, false).unwrap();
            let virtual_path = crate::zip_store::scan::virtual_path(&zip, "Lights/L_001.fits");
            let frame_id: i64 = conn
                .query_row(
                    "SELECT f.id FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
                    [&virtual_path],
                    |r| r.get(0),
                )
                .expect("the zipped light has a frame row");
            conn.execute("UPDATE frames SET imagetyp = 'Light' WHERE id = ?1", [frame_id]).unwrap();
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [frame_id]).unwrap();
            (zip, frame_id)
        }
    }

    #[test]
    fn analyze_frame_set_measures_a_zip_resident_frame() {
        let rig = zip_rig();
        let (_zip, frame_id) = rig.zipped_light("night", 1);
        let r = analyze_frame_set(&rig.ctx, 1, Some(true), 2, &crate::events::NullEmitter).unwrap();
        assert_eq!((r.analyzed, r.failed), (1, 0), "errors: {:?}", r.errors);
        let handle = db(&rig.ctx).unwrap();
        let row = db_analysis::get_frame_analysis(&handle.conn(), frame_id).unwrap().expect("an analysis row was written");
        assert_eq!((row.width, row.height), (768, 512));
        assert!(row.stars_detected > 50, "the stars of the zipped frame were measured, got {}", row.stars_detected);
        assert!(rig.ctx.zip_cache.extraction_count() >= 1, "the frame was read through the zip cache");
    }

    #[test]
    fn analyze_frame_set_reports_unreachable_zip_per_frame_and_finishes_the_rest() {
        let rig = zip_rig();
        let (gone_zip, gone_frame) = rig.zipped_light("gone", 1);
        let (_ok_zip, ok_frame) = rig.zipped_light("here", 2);
        std::fs::remove_file(&gone_zip).unwrap();
        let r = analyze_frame_set(&rig.ctx, 1, Some(true), 2, &crate::events::NullEmitter).unwrap();
        assert_eq!((r.analyzed, r.failed), (1, 1), "errors: {:?}", r.errors);
        assert!(r.errors[0].contains("gone.zip") && r.errors[0].contains("not reachable"), "{:?}", r.errors);
        let handle = db(&rig.ctx).unwrap();
        let conn = handle.conn();
        assert!(db_analysis::get_frame_analysis(&conn, ok_frame).unwrap().is_some());
        assert!(db_analysis::get_frame_analysis(&conn, gone_frame).unwrap().is_none(), "the unreachable frame got no row");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_frame_star_metrics_and_the_flat_plot_read_a_zip_resident_frame() {
        let rig = zip_rig();
        let (_zip, frame_id) = rig.zipped_light("night", 1);
        let r = get_frame_star_metrics(&rig.ctx, frame_id).await.expect("on-demand analysis through the zip cache");
        assert_eq!((r.image_width, r.image_height), (768, 512));
        assert!(!r.stars.is_empty());
        // The second call is served from the stored row and still reads the ROWORDER header through the cache.
        let again = get_frame_star_metrics(&rig.ctx, frame_id).await.unwrap();
        assert_eq!(again.flip_vertical, r.flip_vertical);

        let file_id: i64 = db(&rig.ctx).unwrap().conn().query_row("SELECT file_id FROM frames WHERE id = ?1", [frame_id], |r| r.get(0)).unwrap();
        let plot = compute_flat_contour_plot(&rig.ctx, file_id, FlatContourOpts::default()).await.expect("flat plot through the zip cache");
        assert_eq!((plot.width > 0, plot.height > 0), (true, true));
    }

    /// Review Focus 2: no star catalogue installed (a fresh install, the Docker build). The analysis is saved, no
    /// error surfaces, and the zero point columns stay NULL. The catalogue is looked for once per run (a stat of the tier
    /// files for the selection; the tiers are opened only when a solved frame needs them), so the skip is logged once at
    /// `debug`, not per frame; a log capture is not asserted here because a per-thread subscriber is unreliable in a
    /// parallel test binary.
    #[cfg(feature = "solver")]
    #[test]
    fn no_catalogue_leaves_the_analysis_saved_and_logs_once() {
        let rig = zip_rig();
        let (_zip_a, frame_a) = rig.zipped_light("night-a", 1);
        let (_zip_b, frame_b) = rig.zipped_light("night-b", 2);
        let r = analyze_frame_set(&rig.ctx, 1, Some(true), 2, &crate::events::NullEmitter)
            .expect("the analysis succeeds without a catalogue");
        assert_eq!((r.analyzed, r.failed), (2, 0), "errors: {:?}", r.errors);
        let handle = db(&rig.ctx).unwrap();
        let conn = handle.conn();
        for id in [frame_a, frame_b] {
            let a = db_analysis::get_frame_analysis(&conn, id).unwrap().expect("the analysis row exists");
            assert_eq!(
                (
                    a.zero_point,
                    a.zp_stars,
                    a.zp_sigma,
                    a.zp_version,
                    a.zp_catalog,
                    a.zp_refusal,
                    a.zp_list_stars,
                    a.zp_window_bright,
                    a.zp_window_slope
                ),
                (None, None, None, None, None, None, None, None, None)
            );
        }
    }

    /// One pass at a time: a request while one runs joins its pending set (duplicates folded), the running thread takes
    /// the pending frames when it finishes a round, and the guard is free only when nothing is pending.
    #[cfg(feature = "solver")]
    #[test]
    fn the_solve_pass_runs_one_at_a_time_and_takes_queued_frames() {
        let mut guard = SolvePassGuard::new();
        assert_eq!(guard.request(&[1, 2]), PassRequest::Start);
        assert_eq!(guard.request(&[3, 2]), PassRequest::Queued);
        assert_eq!(guard.request(&[4]), PassRequest::Queued);
        assert_eq!(guard.finish(), Some(vec![2, 3, 4]), "the queued frames come back once, in order");
        assert!(guard.running, "still busy: the second round owns the guard");
        assert_eq!(guard.request(&[9]), PassRequest::Queued, "a request during the second round queues too");
        assert_eq!(guard.finish(), Some(vec![9]));
        assert_eq!(guard.finish(), None, "nothing pending: the guard is free");
        assert!(!guard.running);
        assert_eq!(guard.request(&[5]), PassRequest::Start);
    }

    /// A pass that dies (an error, a panic) frees the guard and drops what was queued behind it: the next batch starts
    /// a pass instead of queueing until restart. Pure: the guard's own method and the drop guard the thread wears.
    #[cfg(feature = "solver")]
    #[test]
    fn a_solve_pass_that_dies_releases_the_guard() {
        let mut guard = SolvePassGuard::new();
        assert_eq!(guard.request(&[1]), PassRequest::Start);
        assert_eq!(guard.request(&[2]), PassRequest::Queued);
        guard.abandon();
        assert!(!guard.running && guard.pending.is_empty(), "nothing runs and nothing is queued");
        assert_eq!(guard.request(&[3]), PassRequest::Start, "a later batch is not told to wait");

        // The drop guard: a panic on the thread releases the flag; a normal end (disarmed) does not release twice.
        let shared = std::sync::Mutex::new(SolvePassGuard::new());
        assert_eq!(shared.lock().unwrap().request(&[1]), PassRequest::Start);
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _release = PassRelease::new(&shared);
            panic!("the pass died");
        }));
        assert!(died.is_err());
        assert!(!shared.lock().unwrap().running, "released on the unwinding path");
        assert_eq!(shared.lock().unwrap().request(&[2]), PassRequest::Start);
        {
            let mut release = PassRelease::new(&shared);
            release.disarm();
        }
        assert!(shared.lock().unwrap().running, "a disarmed guard leaves the flag alone");
    }

    /// The analysis row and its stars commit together: a failing star write leaves no analysis row behind.
    #[test]
    fn a_single_frame_analysis_commits_its_stars_with_its_row() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
             VALUES (1,'/x/a.fits','a.fits',0,'2025-01-01','FITS','2025-01-01')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO frames (id,file_id) VALUES (1,1)", []).unwrap();
        let mut analysis = FrameAnalysis {
            id: None,
            frame_id: 1,
            file_id: 1,
            stars_detected: 1,
            median_fwhm: 3.0,
            median_eccentricity: 0.1,
            median_snr: 20.0,
            median_hfr: 1.5,
            frame_snr: 30.0,
            snr_weight: 1.0,
            psf_signal: 100.0,
            background: 100.0,
            noise: 5.0,
            detection_threshold: 6.0,
            width: 600,
            height: 450,
            source_channels: 1,
            trail_r_squared: 0.0,
            possibly_trailed: false,
            median_beta: None,
            quality_score: None,
            config_hash: None,
            analyzed_at: "2026-10-24T00:00:00Z".into(),
            measure_version: Some(crate::star_measure::MEASURE_VERSION),
            stars_refused: None,
            frame_verdict: None,
            zero_point: None,
            zp_stars: None,
            zp_sigma: None,
            zp_version: None,
            zp_catalog: None,
            zp_refusal: None,
            zp_list_stars: None,
            zp_window_bright: None,
            zp_window_slope: None,
            zp_outdated: false,
        };
        let star = StarMetric {
            id: None,
            frame_analysis_id: 0,
            x: 10.0,
            y: 20.0,
            peak: 1000.0,
            flux: 5000.0,
            fwhm: 3.0,
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            eccentricity: 0.2,
            snr: Some(30.0),
            hfr: Some(1.5),
            theta: 0.0,
            beta: None,
            fit_method: "gaussian".into(),
            fit_residual: 0.01,
        };
        let mut stars = vec![star];

        // One transaction: the row is there with its stars linked to it.
        let id = persist_frame_analysis(&conn, &mut analysis, &mut stars, None).unwrap();
        assert_eq!(stars[0].frame_analysis_id, id);
        assert_eq!(db_analysis::get_star_metrics_by_frame_id(&conn, 1).unwrap().len(), 1);

        // A failing star write rolls the row back with it: no analysis without its stars.
        conn.execute("DELETE FROM frame_analysis", []).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_stars BEFORE INSERT ON star_metrics BEGIN SELECT RAISE(ABORT, 'no stars'); END;",
        )
        .unwrap();
        assert!(persist_frame_analysis(&conn, &mut analysis, &mut stars, None).is_err());
        assert!(db_analysis::get_frame_analysis(&conn, 1).unwrap().is_none(), "no analysis row without its stars");
        assert!(conn.is_autocommit(), "the transaction ended");
    }

    // ---- the zero point rides the analysis (forced-list spec §5.4, plan Task 5b) ----

    #[cfg(feature = "solver")]
    mod zero_point_rides_the_analysis {
        use super::*;
        use crate::plate_solve::storage::{insert_plate_solve, PlateSolveRecord};
        use crate::star_measure::MEASURE_VERSION;
        use crate::test_support::{add_noise, moffat_field, MoffatStar};
        use solvemyastro::{CatalogStar, WcsSolution};

        const ZP_TRUE: f64 = 24.0;
        const T_EXP: f64 = 10.0;

        fn wcs_1as(w: usize, h: usize) -> WcsSolution {
            let s = 1.0 / 3600.0;
            WcsSolution {
                crpix: ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0),
                crval: (150.0, 20.0),
                cd: [[-s, 0.0], [0.0, s]],
                sip_forward: None,
                sip_reverse: None,
            }
        }

        /// `n` isolated Moffat stars (FWHM 3.5, beta 4) on a jittered grid, amplitudes 500..2000.
        fn grid_stars(n: usize, w: usize, h: usize, seed: u64) -> Vec<MoffatStar> {
            let mut rng = crate::geometry::ransac::SplitMix64(seed);
            let cols = ((n as f64 * w as f64 / h as f64).sqrt().ceil() as usize).max(1);
            let rows = n.div_ceil(cols);
            let (cw, ch) = (w as f64 / cols as f64, h as f64 / rows as f64);
            let alpha = 3.5 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
            (0..n)
                .map(|i| {
                    let (c, r) = ((i % cols) as f64, (i / cols) as f64);
                    MoffatStar {
                        x: c * cw + 12.0 + rng.next_f64() * (cw - 24.0),
                        y: r * ch + 12.0 + rng.next_f64() * (ch - 24.0),
                        amp: 500.0 + rng.next_f64() * 1500.0,
                        alpha_x: alpha,
                        alpha_y: alpha,
                        theta: 0.0,
                    }
                })
                .collect()
        }

        /// A mono scene with the catalogue that matches it (G from each star's flux at the true zero point).
        fn scene(n: usize, w: usize, h: usize, seed: u64) -> (Vec<f32>, Vec<CatalogStar>, WcsSolution) {
            let stars = grid_stars(n, w, h, seed);
            let wcs = wcs_1as(w, h);
            let cat = stars
                .iter()
                .map(|s| {
                    let total = std::f64::consts::PI * s.amp * s.alpha_x * s.alpha_y / 3.0;
                    let (ra, dec) = wcs.pixel_to_sky(s.x, s.y);
                    CatalogStar { ra, dec, mag: (ZP_TRUE - 2.5 * (total / T_EXP).log10()) as f32 }
                })
                .collect();
            let mut plane = moffat_field(w, h, &stars, 4.0, 1000.0);
            add_noise(&mut plane, 5.0, seed + 1);
            (plane, cat, wcs)
        }

        /// A 16-bit FITS (BZERO 32768) of a plane in ADU (the exposure rides in the catalogue's `frames.exptime`).
        fn fits_of(plane: &[f32], w: usize, h: usize) -> Vec<u8> {
            let mut out = Vec::new();
            for c in [
                "SIMPLE  =                    T".to_string(),
                "BITPIX  =                   16".to_string(),
                "NAXIS   =                    2".to_string(),
                format!("NAXIS1  = {w:>20}"),
                format!("NAXIS2  = {h:>20}"),
                "BZERO   =                32768".to_string(),
                "BSCALE  =                    1".to_string(),
                "IMAGETYP= 'Light   '".to_string(),
                "END".to_string(),
            ] {
                let mut card = vec![b' '; 80];
                card[..c.len()].copy_from_slice(c.as_bytes());
                out.extend(card);
            }
            while out.len() % 2880 != 0 {
                out.push(b' ');
            }
            for v in plane {
                let u = v.clamp(0.0, 65535.0).round() as i32;
                out.extend(((u - 32768) as i16).to_be_bytes());
            }
            while out.len() % 2880 != 0 {
                out.push(0);
            }
            out
        }

        fn solve_record(frame_id: i64, wcs: &WcsSolution, forward_only_sip: bool) -> PlateSolveRecord {
            let table = |c: f64| serde_json::to_string(&vec![vec![c; 3]; 3]).unwrap();
            PlateSolveRecord {
                id: None,
                frame_id,
                crpix1: wcs.crpix.0,
                crpix2: wcs.crpix.1,
                crval1: wcs.crval.0,
                crval2: wcs.crval.1,
                cd1_1: wcs.cd[0][0],
                cd1_2: wcs.cd[0][1],
                cd2_1: wcs.cd[1][0],
                cd2_2: wcs.cd[1][1],
                sip_order: forward_only_sip.then_some(2),
                sip_a_coeffs: forward_only_sip.then(|| table(1e-9)),
                sip_b_coeffs: forward_only_sip.then(|| table(2e-9)),
                sip_ap_coeffs: None,
                sip_bp_coeffs: None,
                matched_stars: 300,
                total_detected: 400,
                rms_residual_px: 0.3,
                rms_residual_arcsec: 0.3,
                pixel_scale_arcsec: 1.0,
                field_rotation_deg: 0.0,
                solve_time_ms: 10,
                catalog_used: "test".into(),
                algorithm_used: "test".into(),
                solved_at: "2026-03-01T23:00:00Z".into(),
                expected_catalog_stars_in_fov: None,
                inlier_ratio: None,
            }
        }

        /// One frame set with plain files on disk.
        struct Rig {
            tmp: tempfile::TempDir,
            ctx: ServiceContext,
            next: std::cell::Cell<i64>,
        }

        impl Rig {
            fn new() -> Rig {
                let tmp = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(tmp.path().join("astro")).unwrap();
                let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
                {
                    let handle = db(&ctx).unwrap();
                    let conn = handle.conn();
                    conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M33')", []).unwrap();
                    conn.execute(
                        "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
                         VALUES (10, 1, '2026-07-01', '2026-07-02')",
                        [],
                    )
                    .unwrap();
                    conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
                }
                Rig { tmp, ctx, next: std::cell::Cell::new(1) }
            }

            /// A light in the set: the file written to disk, `exptime` and the solve as given.
            fn add_frame(&self, bytes: &[u8], exptime: Option<f64>, solve: Option<(&WcsSolution, bool)>) -> i64 {
                let id = self.next.get();
                self.next.set(id + 1);
                let path = self.tmp.path().join("astro").join(format!("L_{id:03}.fits"));
                std::fs::write(&path, bytes).unwrap();
                let handle = db(&self.ctx).unwrap();
                let conn = handle.conn();
                conn.execute(
                    "INSERT INTO files (id, path, filename, size, modified_at, format, created_at)
                     VALUES (?1, ?2, ?3, ?4, '2026-07-01', 'FITS', '2026-07-01')",
                    rusqlite::params![id, path.to_str().unwrap(), format!("L_{id:03}.fits"), bytes.len() as i64],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO frames (id, file_id, imagetyp, exptime, date_obs)
                     VALUES (?1, ?1, 'Light', ?2, '2026-07-01T22:00:00+00:00')",
                    rusqlite::params![id, exptime],
                )
                .unwrap();
                conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [id]).unwrap();
                if let Some((wcs, forward_only)) = solve {
                    insert_plate_solve(&conn, &solve_record(id, wcs, forward_only)).unwrap();
                }
                id
            }

            fn row(&self, frame_id: i64) -> FrameAnalysis {
                let handle = db(&self.ctx).unwrap();
                let row = db_analysis::get_frame_analysis(&handle.conn(), frame_id).unwrap().expect("an analysis row");
                row
            }

            fn run(&self, force: bool, catalogue: &(&str, Vec<CatalogStar>)) -> AnalyzeFrameSetResult {
                let cone = |_: &WcsSolution, _: usize, _: usize, _: f64, _: f64| Ok(catalogue.1.clone());
                let source = ZpSource { identity: catalogue.0, cone: &cone };
                analyze_frame_set_with(&self.ctx, 1, Some(force), 2, &crate::events::NullEmitter, Some(&source)).unwrap()
            }
        }

        fn zp_columns(
            a: &FrameAnalysis,
        ) -> (Option<f64>, Option<i64>, Option<f64>, Option<u32>, Option<String>, Option<String>, Option<i64>, Option<f64>, Option<f64>)
        {
            (
                a.zero_point,
                a.zp_stars,
                a.zp_sigma,
                a.zp_version,
                a.zp_catalog.clone(),
                a.zp_refusal.clone(),
                a.zp_list_stars,
                a.zp_window_bright,
                a.zp_window_slope,
            )
        }

        #[test]
        fn analysis_with_a_solve_stores_a_current_zero_point() {
            let rig = Rig::new();
            let (plane, cat, wcs) = scene(500, 2000, 1500, 31);
            let frame = rig.add_frame(&fits_of(&plane, 2000, 1500), Some(T_EXP), Some((&wcs, false)));
            let r = rig.run(false, &("CAT-A", cat.clone()));
            assert_eq!((r.analyzed, r.failed), (1, 0), "errors: {:?}", r.errors);
            let a = rig.row(frame);
            let (zp, stars, sigma, version, catalog, refusal, list, g_b, _slope) = zp_columns(&a);
            let zp = zp.unwrap_or_else(|| panic!("a measured zero point, refusal {refusal:?}"));
            assert!((zp - ZP_TRUE).abs() < 0.05, "zero point {zp}");
            assert!(stars.is_some_and(|n| n >= 50), "members {stars:?}");
            assert!(sigma.is_some_and(f64::is_finite));
            assert_eq!((version, catalog.as_deref(), refusal), (Some(crate::analysis::zero_point::ZP_VERSION), Some("CAT-A"), None));
            assert!(list.is_some_and(|n| n >= 50), "list {list:?}");
            assert!(g_b.is_some_and(f64::is_finite), "window edge {g_b:?}");
            assert!(!a.zp_outdated);

            // The same catalogue and config: nothing to do. Another identity (a tier installed): the frame is selected
            // again, and the run replaces the nine columns with the new identity; a forced run replaces them too.
            let analysis_id = a.id;
            assert_eq!(rig.run(false, &("CAT-A", cat.clone())).analyzed, 0, "current: not selected");
            assert_eq!(rig.run(false, &("CAT-B", cat.clone())).analyzed, 1, "another catalogue: selected");
            let b = rig.row(frame);
            assert_eq!(b.zp_catalog.as_deref(), Some("CAT-B"));
            assert!(b.zero_point.is_some() && b.zp_version == Some(crate::analysis::zero_point::ZP_VERSION));
            assert_ne!(b.id, analysis_id, "the analysis row was replaced");
            assert_eq!(rig.run(true, &("CAT-B", cat)).analyzed, 1, "force measures it again");
            assert!((rig.row(frame).zero_point.unwrap() - ZP_TRUE).abs() < 0.05);
        }

        #[test]
        fn a_frame_without_a_solve_stores_no_zero_point() {
            let rig = Rig::new();
            let (plane, cat, _wcs) = scene(150, 1000, 750, 5);
            let frame = rig.add_frame(&fits_of(&plane, 1000, 750), Some(T_EXP), None);
            let r = rig.run(false, &("CAT-A", cat.clone()));
            assert_eq!((r.analyzed, r.failed), (1, 0), "errors: {:?}", r.errors);
            let a = rig.row(frame);
            assert!(a.stars_detected > 50, "the analysis itself ran: {}", a.stars_detected);
            assert_eq!(zp_columns(&a), (None, None, None, None, None, None, None, None, None));
            assert_eq!(rig.run(false, &("CAT-A", cat)).analyzed, 0, "an unsolved frame is not selected for the zero point");
        }

        #[test]
        fn a_forward_only_sip_stores_unreadable_solve() {
            let rig = Rig::new();
            let (plane, cat, wcs) = scene(150, 1000, 750, 6);
            let frame = rig.add_frame(&fits_of(&plane, 1000, 750), Some(T_EXP), Some((&wcs, true)));
            let r = rig.run(false, &("CAT-A", cat.clone()));
            assert_eq!((r.analyzed, r.failed), (1, 0), "errors: {:?}", r.errors);
            assert_eq!(
                zp_columns(&rig.row(frame)),
                (
                    None,
                    None,
                    None,
                    Some(crate::analysis::zero_point::ZP_VERSION),
                    Some("CAT-A".to_string()),
                    Some("unreadable_solve".to_string()),
                    None,
                    None,
                    None
                )
            );
            assert_eq!(rig.run(false, &("CAT-A", cat)).analyzed, 0, "the refusal does not change on a re-read");
        }

        #[test]
        fn a_missing_exposure_stores_a_refusal() {
            let rig = Rig::new();
            let (plane, cat, wcs) = scene(150, 1000, 750, 7);
            let frame = rig.add_frame(&fits_of(&plane, 1000, 750), None, Some((&wcs, false)));
            let r = rig.run(false, &("CAT-A", cat));
            assert_eq!((r.analyzed, r.failed), (1, 0), "errors: {:?}", r.errors);
            let a = rig.row(frame);
            assert_eq!(a.zp_refusal.as_deref(), Some("no_exposure"));
            assert_eq!((a.zero_point, a.zp_version, a.zp_catalog.as_deref()), (None, Some(3), Some("CAT-A")));
            assert_eq!((a.zp_stars, a.zp_list_stars, a.zp_window_bright), (None, None, None));
        }

        /// What `select_frame_jobs` picks, as frame ids.
        fn selected(conn: &rusqlite::Connection, hash: &str, catalog: Option<&str>, force: bool) -> Vec<i64> {
            let mut ids: Vec<i64> =
                select_frame_jobs(conn, 1, hash, catalog, force).unwrap().iter().map(|j| j.frame_id).collect();
            ids.sort_unstable();
            ids
        }

        /// A set of five solved or unsolved lights with analysis rows (no files: only the selection is read).
        fn seeded_selection() -> (tempfile::TempDir, ServiceContext, String) {
            let rig = Rig::new();
            let wcs = wcs_1as(100, 100);
            for _ in 0..5 {
                rig.add_frame(b"x", Some(T_EXP), None);
            }
            let handle = db(&rig.ctx).unwrap();
            let conn = handle.conn();
            let hash = config::load_config(&conn).config_hash();
            // 1 current, 2 version 2, 3 another catalogue, 4 unsolved, 5 current zero point under a stale config.
            for id in [1, 2, 3, 5] {
                insert_plate_solve(&conn, &solve_record(id, &wcs, false)).unwrap();
            }
            let row = |id: i64, config_hash: &str, zp_version: Option<u32>, catalog: Option<&str>| {
                let mut a = seed_analysis_row(id);
                a.config_hash = Some(config_hash.to_string());
                a.zp_version = zp_version;
                a.zp_catalog = catalog.map(str::to_owned);
                a.zero_point = zp_version.map(|_| 21.0);
                db_analysis::upsert_frame_analysis(&conn, &a).unwrap();
            };
            row(1, &hash, Some(3), Some("A"));
            row(2, &hash, Some(2), Some("A"));
            row(3, &hash, Some(3), Some("Z"));
            row(4, &hash, None, None);
            row(5, "another-hash", Some(3), Some("A"));
            drop(conn);
            let Rig { tmp, ctx, .. } = rig;
            (tmp, ctx, hash)
        }

        fn seed_analysis_row(id: i64) -> FrameAnalysis {
            FrameAnalysis {
                id: None,
                frame_id: id,
                file_id: id,
                stars_detected: 200,
                median_fwhm: 4.0,
                median_eccentricity: 0.1,
                median_snr: 100.0,
                median_hfr: 2.0,
                frame_snr: 30.0,
                snr_weight: 1.0,
                psf_signal: 100.0,
                background: 100.0,
                noise: 5.0,
                detection_threshold: 6.0,
                width: 600,
                height: 450,
                source_channels: 1,
                trail_r_squared: 0.0,
                possibly_trailed: false,
                median_beta: None,
                quality_score: None,
                config_hash: None,
                analyzed_at: "2026-10-24T00:00:00Z".into(),
                measure_version: Some(MEASURE_VERSION),
                stars_refused: None,
                frame_verdict: None,
                zero_point: None,
                zp_stars: None,
                zp_sigma: None,
                zp_version: None,
                zp_catalog: None,
                zp_refusal: None,
                zp_list_stars: None,
                zp_window_bright: None,
                zp_window_slope: None,
                zp_outdated: false,
            }
        }

        #[test]
        fn analyze_selects_solved_frames_whose_zero_point_is_not_current() {
            let (_tmp, ctx, hash) = seeded_selection();
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            // Exactly the version-2, the other-catalogue and the stale-config frames; the current and the unsolved stay.
            assert_eq!(selected(&conn, &hash, Some("A"), false), vec![2, 3, 5]);
            // `force` takes every light.
            assert_eq!(selected(&conn, &hash, Some("A"), true), vec![1, 2, 3, 4, 5]);
        }

        #[test]
        fn analyze_selects_nothing_for_the_zero_point_without_a_catalogue() {
            let (_tmp, ctx, hash) = seeded_selection();
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            // No catalogue installed: only the stale-config frame (its own staleness) is selected.
            assert_eq!(selected(&conn, &hash, None, false), vec![5]);
        }

        #[test]
        fn the_selection_carries_each_solved_frames_solve_and_header_facts() {
            let (_tmp, ctx, hash) = seeded_selection();
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            let jobs = select_frame_jobs(&conn, 1, &hash, Some("A"), true).unwrap();
            let solved: Vec<i64> = jobs.iter().filter(|j| j.zp.is_some()).map(|j| j.frame_id).collect();
            assert_eq!(solved.len(), 4, "the four solved frames carry their solve and header facts: {solved:?}");
            assert!(jobs.iter().filter(|j| j.zp.is_some()).all(|j| j.zp.as_ref().unwrap().exptime_s == Some(T_EXP)));
        }

        /// Installs a stand-in star catalogue under the context's app-data folder (the identity reads the tier file's name,
        /// size and time only) and returns its identity.
        fn install_fake_catalogue(tmp: &tempfile::TempDir) -> String {
            let dir = tmp.path().join("catalogs").join("smac_gaia");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("stars.smac"), b"x").unwrap();
            crate::analysis::transparency::installed_catalog_identity(tmp.path()).expect("the fake catalogue reads")
        }

        #[test]
        fn the_listing_flags_rows_whose_zero_point_is_not_current() {
            let (tmp, ctx, _hash) = seeded_selection();
            // No catalogue installed: nothing is outdated.
            let none = get_analysis_for_frame_set(&ctx, 1).unwrap();
            assert!(none.iter().all(|a| !a.zp_outdated), "no catalogue, no flag");
            let identity = install_fake_catalogue(&tmp);
            {
                // Frame 1 now carries the installed catalogue's identity: the one current, solved frame.
                let handle = db(&ctx).unwrap();
                handle
                    .conn()
                    .execute("UPDATE frame_analysis SET zp_catalog = ?1 WHERE frame_id IN (1, 5)", [&identity])
                    .unwrap();
            }
            let rows = get_analysis_for_frame_set(&ctx, 1).unwrap();
            let flag = |id: i64| rows.iter().find(|a| a.frame_id == id).unwrap().zp_outdated;
            // 1 current; 2 version 2; 3 another catalogue; 4 unsolved (never flagged); 5 current zero point.
            assert_eq!((flag(1), flag(2), flag(3), flag(4), flag(5)), (false, true, true, false, false));
            // The flag is never in the stored row.
            let handle = db(&ctx).unwrap();
            assert!(!db_analysis::get_frame_analysis(&handle.conn(), 2).unwrap().unwrap().zp_outdated);
        }

        /// The count of the Analysis tab is Analyze's own selection: the same frames, no catalogue is "install it", never
        /// "to update".
        #[test]
        fn the_update_count_matches_what_analyze_selects() {
            let (tmp, ctx, hash) = seeded_selection();
            // No catalogue: nothing to update; the four solved frames wait for the catalogue.
            let none = zero_point_updates(&ctx, 1).unwrap();
            assert_eq!((none.to_update, none.no_catalogue, none.measurement_failed), (0, 4, 0));
            let identity = install_fake_catalogue(&tmp);
            {
                let handle = db(&ctx).unwrap();
                handle
                    .conn()
                    .execute("UPDATE frame_analysis SET zp_catalog = ?1 WHERE frame_id IN (1, 5)", [&identity])
                    .unwrap();
            }
            let got = zero_point_updates(&ctx, 1).unwrap();
            // 2 (version 2) and 3 (another catalogue) are to update; 1 and 5 are current; 4 is unsolved.
            assert_eq!((got.to_update, got.no_catalogue, got.measurement_failed), (2, 0, 0));
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            let analyze: Vec<i64> = select_frame_jobs(&conn, 1, &hash, Some(&identity), false)
                .unwrap()
                .iter()
                .map(|j| j.frame_id)
                .filter(|id| *id != 5) // 5 is selected for its stale config, not for its zero point
                .collect();
            assert_eq!(analyze.len(), got.to_update, "{analyze:?}");
        }

        /// Blink never re-reads a file for an outdated zero point: the row is served as it is stored. The file does not
        /// exist, so a re-analysis would fail and an `Ok` proves none ran.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_fresh_row_with_an_outdated_zero_point_is_served_without_reanalysis() {
            let tmp = tempfile::tempdir().unwrap();
            let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
            let frame_id = seed_verdict_row(&ctx, |a| {
                a.zp_version = Some(2);
                a.zp_catalog = Some("old".into());
                a.zero_point = Some(20.0);
            });
            install_fake_catalogue(&tmp);
            {
                let handle = db(&ctx).unwrap();
                insert_plate_solve(&handle.conn(), &solve_record(frame_id, &wcs_1as(600, 450), false)).unwrap();
                // A new solve clears the zero point; write the outdated value again as an older build had stored it.
                handle
                    .conn()
                    .execute(
                        "UPDATE frame_analysis SET zp_version = 2, zp_catalog = 'old', zero_point = 20.0 WHERE frame_id = ?1",
                        [frame_id],
                    )
                    .unwrap();
            }
            let r = get_frame_star_metrics(&ctx, frame_id).await.expect("served from the stored row");
            assert_eq!(r.metrics.zp_version, Some(2), "the row is served as stored");
            assert_eq!(r.metrics.zero_point, Some(20.0));
        }

        /// A new or replaced solve between the read and the write wins: the zero point of the old solve is dropped and
        /// the analysis row is still stored.
        #[test]
        fn a_new_solve_during_the_analysis_drops_the_zero_point() {
            let (_tmp, ctx, _hash) = seeded_selection();
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            let solve_id = |id: i64| -> i64 {
                conn.query_row("SELECT id FROM plate_solves WHERE frame_id = ?1", [id], |r| r.get(0)).unwrap()
            };
            let judged = |id: i64| -> AnalyzedFrame {
                let mut analysis = seed_analysis_row(id);
                db_analysis::StoredZeroPoint {
                    zero_point: Some(21.5),
                    stars: Some(80),
                    sigma: Some(0.03),
                    version: crate::analysis::zero_point::ZP_VERSION,
                    refusal: None,
                    list_stars: Some(400),
                    window_bright: Some(12.5),
                    window_slope: Some(-0.02),
                }
                .apply_to(&mut analysis, "A");
                AnalyzedFrame { frame_id: id, analysis, stars: Vec::new(), solve_id: Some(solve_id(id)) }
            };
            // Frame 1's solve is unchanged: the zero point is written. Frame 2 is re-solved after the read.
            let mut batch = vec![judged(1), judged(2)];
            insert_plate_solve(&conn, &solve_record(2, &wcs_1as(100, 100), false)).unwrap();
            // Frame 3's solve is deleted after the read.
            batch.push(judged(3));
            crate::plate_solve::storage::delete_plate_solve(&conn, 3).unwrap();
            persist_analyzed_frames(&conn, &mut batch).unwrap();
            let zp = |id: i64| zp_columns(&db_analysis::get_frame_analysis(&conn, id).unwrap().unwrap());
            assert_eq!(zp(1).0, Some(21.5), "the unchanged solve keeps its zero point");
            assert_eq!(zp(1).8, Some(-0.02));
            assert_eq!(zp(2), (None, None, None, None, None, None, None, None, None), "a re-solved frame drops it");
            assert_eq!(zp(3), (None, None, None, None, None, None, None, None, None), "so does a frame whose solve is gone");
            for id in [1, 2, 3] {
                assert!(db_analysis::get_frame_analysis(&conn, id).unwrap().is_some(), "the analysis row is stored");
            }
        }

        #[test]
        fn a_poor_solve_is_stored_once_and_a_new_solve_measures_again() {
            let (_tmp, ctx, hash) = seeded_selection();
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            let wcs = wcs_1as(100, 100);
            // Frame 2 (outdated) judged `poor_solve` by this build: current, no longer selected.
            let sid: i64 = conn.query_row("SELECT id FROM plate_solves WHERE frame_id = 2", [], |r| r.get(0)).unwrap();
            let aid = db_analysis::get_frame_analysis(&conn, 2).unwrap().unwrap().id.unwrap();
            let poor = db_analysis::StoredZeroPoint {
                zero_point: None,
                stars: None,
                sigma: None,
                version: crate::analysis::zero_point::ZP_VERSION,
                refusal: Some("poor_solve".into()),
                list_stars: Some(400),
                window_bright: Some(12.5),
                window_slope: None,
            };
            assert!(db_analysis::set_zero_point(&conn, 2, aid, Some(sid), &poor, "A").unwrap());
            assert_eq!(selected(&conn, &hash, Some("A"), false), vec![3, 5]);
            // A new solve clears the nine columns: the frame is outdated again and Analyze measures it.
            insert_plate_solve(&conn, &solve_record(2, &wcs, false)).unwrap();
            assert_eq!(selected(&conn, &hash, Some("A"), false), vec![2, 3, 5]);
        }
        // ---- the pass after a solve batch (plan Task 5c) ----

        fn pass(rig: &Rig, ids: &[i64], catalogue: &(&str, Vec<CatalogStar>)) -> crate::analysis::zero_point::ZpPassStats {
            let cone = |_: &WcsSolution, _: usize, _: usize, _: f64, _: f64| Ok(catalogue.1.clone());
            let source = ZpSource { identity: catalogue.0, cone: &cone };
            solve_zero_point_pass_with(&rig.ctx, ids, Some(&source)).unwrap()
        }

        #[test]
        fn the_solve_pass_measures_solved_frames_with_a_current_analysis() {
            let rig = Rig::new();
            let (plane, cat, wcs) = scene(500, 2000, 1500, 31);
            // Frame 1: analysed with a solve under CAT-A. Frames 2 and 3 are solved too: 2's analysis is stale (another
            // config), 3 has none: both are the next Analyze's, never the pass's.
            let one = rig.add_frame(&fits_of(&plane, 2000, 1500), Some(T_EXP), Some((&wcs, false)));
            assert_eq!(rig.run(false, &("CAT-A", cat.clone())).analyzed, 1);
            let two = rig.add_frame(b"x", Some(T_EXP), Some((&wcs, false)));
            let three = rig.add_frame(b"x", Some(T_EXP), Some((&wcs, false)));
            {
                let handle = db(&rig.ctx).unwrap();
                let mut a = seed_analysis_row(two);
                a.config_hash = Some("another-hash".into());
                db_analysis::upsert_frame_analysis(&handle.conn(), &a).unwrap();
            }
            let before = rig.row(one);
            assert_eq!(before.zp_catalog.as_deref(), Some("CAT-A"));

            let ids = [one, two, three];
            let current = pass(&rig, &ids, &("CAT-A", cat.clone()));
            assert_eq!(current.stored, 0, "a zero point that is already current is skipped");
            assert_eq!(zp_columns(&rig.row(one)), zp_columns(&before), "the row is untouched");

            // A tier installed since: the identity changes, the frame's own nine columns are rewritten and nothing else.
            let stats = pass(&rig, &ids, &("CAT-B", cat));
            assert_eq!((stats.stored, stats.unreadable, stats.failed), (1, 0, 0));
            let after = rig.row(one);
            assert_eq!(after.id, before.id, "the analysis row is not replaced");
            assert_eq!((after.stars_detected, &after.analyzed_at), (before.stars_detected, &before.analyzed_at));
            assert_eq!(after.zp_catalog.as_deref(), Some("CAT-B"));
            assert_eq!(after.zp_version, Some(crate::analysis::zero_point::ZP_VERSION));
            assert!((after.zero_point.unwrap() - ZP_TRUE).abs() < 0.05, "zero point {:?}", after.zero_point);
            assert_eq!(rig.row(two).zp_version, None, "a stale analysis is skipped");
            let handle = db(&rig.ctx).unwrap();
            assert!(db_analysis::get_frame_analysis(&handle.conn(), three).unwrap().is_none(), "no analysis, no row made");
        }

        #[test]
        fn the_solve_pass_skips_an_offline_file_and_warns_once() {
            let rig = Rig::new();
            let wcs = wcs_1as(100, 100);
            let offline = [rig.add_frame(b"x", Some(T_EXP), Some((&wcs, false))), rig.add_frame(b"x", Some(T_EXP), Some((&wcs, false)))];
            {
                let handle = db(&rig.ctx).unwrap();
                let conn = handle.conn();
                let hash = config::load_config(&conn).config_hash();
                for id in offline {
                    let mut a = seed_analysis_row(id);
                    a.config_hash = Some(hash.clone());
                    db_analysis::upsert_frame_analysis(&conn, &a).unwrap();
                }
            }
            for id in offline {
                std::fs::remove_file(rig.tmp.path().join("astro").join(format!("L_{id:03}.fits"))).unwrap();
            }
            let stats = pass(&rig, &offline, &("CAT-A", Vec::new()));
            assert_eq!((stats.stored, stats.unreadable, stats.failed), (0, 2, 0));
            assert!(stats.first_error.is_some(), "the one warn carries the first failure");
            for id in offline {
                assert_eq!(rig.row(id).zp_version, None, "nothing was written for an offline file");
            }
        }

        #[test]
        fn the_pass_selection_takes_only_outdated_frames_with_a_current_analysis() {
            let (_tmp, ctx, hash) = seeded_selection();
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            // 1 current, 2 version 2, 3 another catalogue, 4 unsolved, 5 outdated under a stale config; 6 is not asked.
            let pick = |ids: &[i64]| -> Vec<i64> {
                let mut got: Vec<i64> = select_pass_jobs(&conn, ids, &hash, "A").unwrap().iter().map(|(j, _)| j.frame_id).collect();
                got.sort_unstable();
                got
            };
            assert_eq!(pick(&[1, 2, 3, 4, 5, 6]), vec![2, 3]);
            assert_eq!(pick(&[2]), vec![2], "only the frames asked for");
            assert!(pick(&[]).is_empty());
            let jobs = select_pass_jobs(&conn, &[2], &hash, "A").unwrap();
            let analysis_id = db_analysis::get_frame_analysis(&conn, 2).unwrap().unwrap().id.unwrap();
            assert_eq!(jobs[0].1, analysis_id, "the job carries the analysis row it was selected on");
            assert!(jobs[0].0.zp.is_some());
        }

        /// A re-analysis or a new solve between the selection and the write wins: nothing of the pass is stored for that
        /// frame; the frame the pass read alone is written.
        #[test]
        fn a_reanalysis_during_the_pass_wins() {
            let (_tmp, ctx, hash) = seeded_selection();
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            let selection = select_pass_jobs(&conn, &[2, 3], &hash, "A").unwrap();
            let analysis_ids: std::collections::HashMap<i64, i64> = selection.iter().map(|(j, a)| (j.frame_id, *a)).collect();
            let solve_id = |id: i64| -> i64 {
                conn.query_row("SELECT id FROM plate_solves WHERE frame_id = ?1", [id], |r| r.get(0)).unwrap()
            };
            let judged = |id: i64| -> AnalyzedFrame {
                let mut analysis = seed_analysis_row(id);
                db_analysis::StoredZeroPoint {
                    zero_point: Some(21.5),
                    stars: Some(80),
                    sigma: Some(0.03),
                    version: crate::analysis::zero_point::ZP_VERSION,
                    refusal: None,
                    list_stars: Some(400),
                    window_bright: Some(12.5),
                    window_slope: Some(-0.02),
                }
                .apply_to(&mut analysis, "A");
                AnalyzedFrame { frame_id: id, analysis, stars: Vec::new(), solve_id: Some(solve_id(id)) }
            };
            let frames = [judged(2), judged(3)];
            // Frame 2 is re-analysed (the row gets a new id) and frame 3 re-solved, both after the selection.
            let mut reanalysed = seed_analysis_row(2);
            reanalysed.config_hash = Some(hash.clone());
            db_analysis::upsert_frame_analysis(&conn, &reanalysed).unwrap();
            insert_plate_solve(&conn, &solve_record(3, &wcs_1as(100, 100), false)).unwrap();
            let mut stats = crate::analysis::zero_point::ZpPassStats::default();
            persist_pass_zero_points(&conn, &frames, &analysis_ids, &mut stats).unwrap();
            assert_eq!((stats.stored, stats.failed), (0, 0), "both were superseded");
            let none = (None, None, None, None, None, None, None, None, None);
            assert_eq!(zp_columns(&db_analysis::get_frame_analysis(&conn, 2).unwrap().unwrap()), none);
            assert_eq!(zp_columns(&db_analysis::get_frame_analysis(&conn, 3).unwrap().unwrap()), none);

            // The same judgement on an untouched frame is written, the nine columns alone.
            let selection = select_pass_jobs(&conn, &[2], &hash, "A").unwrap();
            let ids: std::collections::HashMap<i64, i64> = selection.iter().map(|(j, a)| (j.frame_id, *a)).collect();
            let mut stats = crate::analysis::zero_point::ZpPassStats::default();
            persist_pass_zero_points(&conn, &[judged(2)], &ids, &mut stats).unwrap();
            assert_eq!(stats.stored, 1);
            let a = db_analysis::get_frame_analysis(&conn, 2).unwrap().unwrap();
            assert_eq!((a.zero_point, a.zp_window_slope, a.zp_catalog.as_deref()), (Some(21.5), Some(-0.02), Some("A")));
        }
    }
}
