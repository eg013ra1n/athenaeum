//! Shared `masters` command-layer handlers — single business-logic source
//! for the Tauri (`commands/masters.rs`) and web (`routes/masters.rs`)
//! wrappers. See `.superpowers/sdd/task-12-brief.md` and
//! `.superpowers/sdd/task-13-brief.md` for the design this module follows.
//!
//! Wires together Tasks 4-11 (compute queue, banded integration engine,
//! combiners, master naming/headers, registration) into the user-facing
//! "build a master from this calibration set" feature. `start_master_build`
//! is a plain sync fn that validates + registers a cancel handle on the
//! CALLING thread, then spawns a dedicated named `std::thread` that does the
//! actual queue-admission + integration + write + register. That
//! validate-inline-then-hand-off-to-a-worker shape follows
//! `commands/archive.rs`'s `start_archive_operation` (plan + commit
//! synchronously, register the handle in the active map, hand the real work
//! to a background worker that removes the handle and emits a finished
//! event) — NOT `api::analysis::analyze_frame_set`, which stays synchronous
//! end-to-end inside the caller's `spawn_blocking` and only returns when the
//! work is done. `preview_master_build` is pure DB work with no pixel I/O
//! (see the `select_flat_precal` / `load_precal_pixels` split below); both
//! transports' preview wrappers still call it inside
//! `tokio::task::spawn_blocking` (the `analyze_frame_set` wrapper
//! precedent) so even its DB queries stay off the async executor.
//!
//! Task 13 adds two more entry points on top of the same build-thread
//! machinery (see `BuildTarget` below for how `run_build`/
//! `run_master_build_thread` branch on New-vs-Rebuild without duplicating
//! the integrate -> write -> finalize pipeline):
//! - `start_master_builds_batch`: dependency-ordered fan-out of
//!   `start_master_build` over many sets (`plan_batch` is the pure/testable
//!   planning step — see its doc comment).
//! - `rebuild_master`: re-integrates an EXISTING master's source frames in
//!   place (same file, atomic replace) and refreshes provenance instead of
//!   registering a brand-new master set.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
use crate::calibration_library::headers::{build_master_cards, load_header_inputs};
use crate::calibration_library::paths::{master_relative_path, resolve_collision, MasterPathParams};
use crate::calibration_library::register::{member_hash, register_master};
use crate::events::{emit_event, ProgressEmitter};
use crate::fits_writer::write_fits_f32;
use crate::integration::combine::CombineMethod;
use crate::integration::engine::{integrate_bias_like, integrate_flat, EngineProgress, FlatPrecal};
use crate::integration::IntegrationError;
use crate::services::compute_queue::ComputeJobKind;
use crate::services::{MasterBuildHandle, ServiceContext};

/// Minimum member-frame count required to build a master from a source
/// calibration set (spec §9 floor — below this the combine algorithms
/// degenerate and the result isn't trustworthy as a "master").
pub const MIN_MASTER_FRAMES: i64 = 3;

// ── DTOs (single-sourced; both wrapper crates import these) ─────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterRecipe {
    /// None => Auto (per-type/per-N rule from spec §9).
    pub combine: Option<CombineMethod>,
    /// Constant-ADU fallback for flat pre-calibration when no darkflat/dark/bias master is linked.
    pub synthetic_bias: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterBuildPreview {
    pub set_id: i64,
    pub imagetyp: String,
    pub frame_count: i64,
    pub resolved_combine: CombineMethod,
    /// Human description: "master darkflat #12" | "synthetic bias 500 ADU" | null.
    pub flat_precal: Option<String>,
    /// Absolute, collision-resolved.
    pub target_path: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterProvenanceInfo {
    pub master_set_id: i64,
    pub source_set_id: Option<i64>,
    pub recipe_json: String,
    pub member_count: usize,
    pub member_hash: String,
    pub created_at: String,
    /// Rebuild possible?
    pub source_frames_on_disk: bool,
    /// Any source file has archive markers.
    pub originals_archived: bool,
}

/// Result of [`start_master_builds_batch`]: which sets were actually
/// enqueued (in submission order — that order IS the dependency order, see
/// `plan_batch`) and which were skipped, with a per-set reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BatchBuildReport {
    pub started_set_ids: Vec<i64>,
    pub skipped: Vec<BatchSkip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BatchSkip {
    pub set_id: i64,
    pub reason: String,
}

// ── Progress / completion event payloads (snake_case, analysis precedent) ───

/// `master-build-progress` event payload. `stage` is one of "reading" |
/// "integrating" | "writing" | "registering" — "reading" is reserved for
/// future use (member-path enumeration is cheap enough today not to need
/// its own progress tick); the build thread currently emits "integrating"
/// (driven by the engine's per-band callback), "writing", and
/// "registering".
#[derive(Clone, serde::Serialize)]
struct MasterBuildProgressEvent {
    set_id: i64,
    stage: &'static str,
    current: usize,
    total: usize,
    percent: f64,
}

/// `master-build-complete` event payload. ALWAYS emitted exactly once per
/// `start_master_build` call, regardless of how the build thread ends
/// (success, error, cancelled-in-queue, cancelled-mid-integration, write
/// failure, register failure) — see `run_master_build_thread`.
///
/// Task 13 / `rebuild_master`: `set_id` here is the SOURCE calibration set
/// id (the one whose member frames got re-integrated), NOT `master_set_id`
/// — deliberate. The duplicate-build guard (`active_master_builds`) and
/// every existing consumer of `master-build-progress`/`master-build-complete`
/// already key an in-flight build by "the raw set being worked on"; a
/// rebuild is still working on the same source set, it just writes to a
/// different (pre-existing) target and updates provenance instead of
/// registering. `master_set_id` keeps its existing meaning: "the master
/// that resulted from this build" — for a rebuild that's always the master
/// that was passed in, echoed back.
#[derive(Clone, serde::Serialize)]
struct MasterBuildCompleteEvent {
    set_id: i64,
    master_set_id: Option<i64>,
    success: bool,
    cancelled: bool,
    error: Option<String>,
}

// ── Recipe resolution (pure, unit-tested) ────────────────────────────────────

/// spec §9: bias-like N>=15 winsorized else median; flat N>=15 winsorized
/// else percentile. Explicit override always wins.
pub fn resolve_combine(explicit: Option<CombineMethod>, imagetyp: &str, n: i64) -> CombineMethod {
    if let Some(m) = explicit {
        return m;
    }
    let is_flat = imagetyp == "Flat";
    if n >= 15 {
        CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 }
    } else if is_flat {
        CombineMethod::PercentileClip { low: 0.2, high: 0.02 }
    } else {
        CombineMethod::Median
    }
}

/// Dependency-build order for a batch (Task 13, spec: bias/darkflat before
/// dark before flat — flats resolve pre-cal at RUN time via
/// `select_flat_precal`, so a darkflat/dark/bias master built earlier in the
/// SAME batch is already visible on disk by the time a later flat build
/// runs, as long as submission order respects this rank). Anything not one
/// of the four calibration types (shouldn't normally reach here) sorts last
/// rather than erroring — `plan_batch` already only calls this on rows that
/// passed the buildability checks.
pub(crate) fn type_build_rank(imagetyp: &str) -> u8 {
    match imagetyp {
        "Bias" | "DarkFlat" => 0,
        "Dark" => 1,
        "Flat" => 2,
        _ => 3,
    }
}

/// Outcome of the flat pre-calibration fallback chain — the WHAT, with no
/// pixels attached. `select_flat_precal` (pure DB) produces it; the build
/// thread turns it into a `FlatPrecal` via `load_precal_pixels`, and
/// `preview_master_build` only ever calls `describe()` on it. This split is
/// deliberate: preview must never do full-image I/O just to render a
/// description string.
#[derive(Debug, Clone, PartialEq)]
enum PrecalChoice {
    Master {
        set_id: i64,
        imagetyp: String,
        cal_type: &'static str,
        /// On-disk path of the master's (single) member file — resolved at
        /// selection time so `load_precal_pixels` needs no DB connection.
        path: String,
    },
    Synthetic(f64),
    None,
}

impl PrecalChoice {
    /// Human description for previews / provenance JSON — e.g.
    /// "darkflat master #12 (MasterDarkFlat)" or "synthetic bias 500 ADU".
    fn describe(&self) -> Option<String> {
        match self {
            PrecalChoice::Master { set_id, imagetyp, cal_type, .. } => {
                Some(format!("{} master #{set_id} ({imagetyp})", cal_type.to_lowercase()))
            }
            PrecalChoice::Synthetic(b) => Some(format!("synthetic bias {b} ADU")),
            PrecalChoice::None => Option::None,
        }
    }
}

/// Flat pre-cal selection per spec §9 fallback chain — pure DB, no pixel
/// I/O. Walks the sub-cal links of this flat set by type preference
/// (DarkFlat → exposure-matched Dark(±0.5s) → Bias), skipping raw
/// (non-master) links with a warning. Falls back to the recipe's synthetic
/// bias, then to `PrecalChoice::None` + warning.
///
/// Called by `preview_master_build` (description only) AND inside the build
/// thread (so a just-built darkflat master — earlier in a batch — is
/// visible at build time, not preview time).
fn select_flat_precal(
    conn: &rusqlite::Connection,
    set_id: i64,
    set_exptime: Option<f64>,
    synthetic_bias: Option<f64>,
) -> Result<(PrecalChoice, Vec<String>), ApiError> {
    let mut warnings = Vec::new();
    // sub-cal links of this flat set, by type preference
    for cal_type in ["DarkFlat", "Dark", "Bias"] {
        let row: Option<(i64, String, i64, Option<f64>)> = conn.query_row(
            "SELECT cs.id, cs.imagetyp, cs.is_master_library, cs.exptime
             FROM calibration_set_to_frames l
             JOIN calibration_set cs ON cs.id = l.calibration_set_id
             WHERE l.source_id = ?1 AND l.source_type = 'calibration_set'
               AND l.calibration_type = ?2",
            rusqlite::params![set_id, cal_type],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional()?;
        let Some((precal_set, imagetyp, is_master, precal_expt)) = row else { continue };
        if is_master != 1 {
            warnings.push(format!(
                "linked {cal_type} set #{precal_set} is raw — build its master first (skipped)"));
            continue;
        }
        if cal_type == "Dark" {
            // exposure-matched dark only (spec §9): the matcher enforced
            // exptime at link time, re-verify defensively.
            match (set_exptime, precal_expt) {
                (Some(a), Some(b)) if (a - b).abs() <= 0.5 => {}
                _ => {
                    warnings.push(format!(
                        "linked dark master #{precal_set} exposure does not match the flats — skipped"));
                    continue;
                }
            }
        }
        let path: String = conn.query_row(
            "SELECT fi.path FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1 LIMIT 1",
            [precal_set], |r| r.get(0),
        )?;
        return Ok((
            PrecalChoice::Master { set_id: precal_set, imagetyp, cal_type, path },
            warnings,
        ));
    }
    if let Some(b) = synthetic_bias {
        return Ok((PrecalChoice::Synthetic(b), warnings));
    }
    warnings.push("no pre-calibration master linked and no synthetic bias set — flat combined un-pre-calibrated (vignetting zero level slightly off)".into());
    Ok((PrecalChoice::None, warnings))
}

/// Materializes a `PrecalChoice` into engine-ready pixels. The Master arm
/// reads the ENTIRE precal master into RAM (single file, via the banded
/// reader in one band) — called ONLY from the build thread, never from
/// preview.
fn load_precal_pixels(choice: &PrecalChoice, scratch: &Path) -> Result<FlatPrecal, ApiError> {
    match choice {
        PrecalChoice::Master { path, .. } => {
            let mut src = crate::integration::banded::BandSource::open(
                &[PathBuf::from(path)], scratch,
            ).map_err(|e| ApiError::Internal(format!("pre-cal master unreadable: {e}")))?;
            let (w, h) = (src.width(), src.height());
            let mut bufs = vec![Vec::new()];
            src.read_band(0, h, &mut bufs).map_err(|e| ApiError::Internal(e.to_string()))?;
            Ok(FlatPrecal::MasterFrame { data: std::mem::take(&mut bufs[0]), width: w, height: h })
        }
        PrecalChoice::Synthetic(b) => Ok(FlatPrecal::SyntheticBias(*b as f32)),
        PrecalChoice::None => Ok(FlatPrecal::None),
    }
}

fn recipe_summary_string(method: CombineMethod, n: i64) -> String {
    match method {
        CombineMethod::Mean => format!("mean n={n}"),
        CombineMethod::Median => format!("median n={n}"),
        CombineMethod::WinsorizedSigmaClip { sigma_low, sigma_high } => {
            format!("winsorized({sigma_low},{sigma_high}) n={n}")
        }
        CombineMethod::PercentileClip { low, high } => format!("percentile({low},{high}) n={n}"),
    }
}

// ── Shared validation (preview and start must reject IDENTICALLY) ───────────

/// The calibration_set fields needed to drive a build. `instrume` / `filter`
/// / `gain` / `ccd_temp` are deliberately NOT carried here — the build and
/// preview paths both re-derive those (already averaged/consolidated across
/// member frames) from `load_header_inputs` instead, so they'd be dead
/// weight on this struct.
struct SetRow {
    imagetyp: String,
    exptime: Option<f64>,
    binning: Option<String>,
    date: String,
    frame_count: i64,
}

/// exists AND not already superseded AND not itself a master AND has enough
/// member frames. Same checks power `preview_master_build`,
/// `start_master_build`'s pre-spawn validation, and the build thread's own
/// (defensive, re-checked) load — so preview can never green-light a build
/// that start would then reject.
fn validate_buildable_set(
    superseded_by_set_id: Option<i64>,
    is_master_library: i64,
    frame_count: i64,
) -> Result<(), ApiError> {
    if let Some(master_id) = superseded_by_set_id {
        return Err(ApiError::Conflict(format!(
            "calibration set is already superseded by master set {master_id}"
        )));
    }
    if is_master_library != 0 {
        return Err(ApiError::Invalid(
            "calibration set is already a master — cannot build a master from a master".into(),
        ));
    }
    if frame_count < MIN_MASTER_FRAMES {
        return Err(ApiError::Invalid(format!(
            "calibration set has {frame_count} frames — at least {MIN_MASTER_FRAMES} are required to build a master"
        )));
    }
    Ok(())
}

fn load_and_validate_set(conn: &rusqlite::Connection, set_id: i64) -> Result<SetRow, ApiError> {
    let row: Option<(String, Option<i64>, i64, i64, Option<f64>, Option<String>, String)> = conn.query_row(
        "SELECT imagetyp, superseded_by_set_id, is_master_library, frame_count,
                exptime, binning, date
         FROM calibration_set WHERE id = ?1",
        [set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
    ).optional()?;

    let Some((imagetyp, superseded, is_master, frame_count, exptime, binning, date)) = row else {
        return Err(ApiError::NotFound(format!("calibration set {set_id} not found")));
    };

    validate_buildable_set(superseded, is_master, frame_count)?;

    Ok(SetRow { imagetyp, exptime, binning, date, frame_count })
}

/// Same field set as `load_and_validate_set`, but WITHOUT
/// `validate_buildable_set` — used only by the `BuildTarget::Rebuild` path.
/// A rebuild's source set is, by definition, already
/// `superseded_by_set_id`-marked (that's exactly what `register_master` did
/// when the master it built was first registered), so running the normal
/// buildability check against it would always reject with "already
/// superseded". `rebuild_master`'s own preconditions (provenance row with a
/// `source_set_id`, every source file on disk) are the real gate here; this
/// helper only fetches the fields `run_build` needs to resolve combine/
/// precal/header inputs for that source set.
fn load_set_row(conn: &rusqlite::Connection, set_id: i64) -> Result<SetRow, ApiError> {
    let row: Option<(String, Option<f64>, Option<String>, String, i64)> = conn.query_row(
        "SELECT imagetyp, exptime, binning, date, frame_count FROM calibration_set WHERE id = ?1",
        [set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).optional()?;

    let Some((imagetyp, exptime, binning, date, frame_count)) = row else {
        return Err(ApiError::NotFound(format!("calibration set {set_id} not found")));
    };

    Ok(SetRow { imagetyp, exptime, binning, date, frame_count })
}

/// Rebuild precondition (Task 13): every member frame of the source set must
/// still be present on disk — a rebuild re-reads and re-integrates the SAME
/// raw frames, it can't recombine frames that were archived into a ZIP or
/// otherwise removed. Pure DB + `Path::exists` — no `ServiceContext`
/// needed, so it's unit-testable directly (see tests below) the same way
/// `plan_batch` is. Distinguishes "archived" (actionable: restore first)
/// from "missing" (the file is just gone) when cheap to do so via
/// `files.archived_in_operation`.
fn check_rebuild_source_ready(conn: &rusqlite::Connection, source_set_id: i64) -> Result<(), ApiError> {
    let mut stmt = conn.prepare(
        "SELECT fi.path, fi.archived_in_operation FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE csf.set_id = ?1",
    )?;
    let rows: Vec<(String, Option<i64>)> = stmt
        .query_map([source_set_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    if rows.is_empty() {
        return Err(ApiError::Invalid(
            "source calibration set has no member frames on record".into(),
        ));
    }
    if rows.iter().all(|(p, _)| Path::new(p).exists()) {
        return Ok(());
    }
    if rows.iter().any(|(_, archived)| archived.is_some()) {
        Err(ApiError::Invalid("originals are archived — restore them first".into()))
    } else {
        Err(ApiError::Invalid("original source frames are missing on disk".into()))
    }
}

fn library_root_or_err(ctx: &ServiceContext) -> Result<crate::models::ScanRoot, ApiError> {
    crate::api::scan_roots::get_calibration_library_root(ctx)?
        .ok_or_else(|| ApiError::Invalid(
            "no calibration library root configured — set one before building masters".into(),
        ))
}

// ── Preview ───────────────────────────────────────────────────────────────

/// Validation + speculative recipe/precal/target-path resolution, no thread.
pub fn preview_master_build(
    ctx: &ServiceContext,
    set_id: i64,
    recipe: &MasterRecipe,
) -> Result<MasterBuildPreview, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let set = load_and_validate_set(&conn, set_id)?;
    let library_root = library_root_or_err(ctx)?;

    let resolved_combine = resolve_combine(recipe.combine, &set.imagetyp, set.frame_count);
    let is_flat = set.imagetyp == "Flat";
    // Selection only (pure DB) — preview never loads precal pixels.
    let (flat_precal, warnings) = if is_flat {
        let (choice, warnings) = select_flat_precal(&conn, set_id, set.exptime, recipe.synthetic_bias)?;
        (choice.describe(), warnings)
    } else {
        (None, Vec::new())
    };

    let inputs = load_header_inputs(&conn, set_id)?;
    let target_rel = master_relative_path(&MasterPathParams {
        instrume: inputs.instrume.as_deref(),
        master_kind: inputs.kind,
        filter: inputs.filter.as_deref(),
        exptime: inputs.exptime,
        ccd_temp: inputs.temp_mean,
        gain: inputs.gain,
        binning: set.binning.as_deref(),
        date: &set.date,
    });
    let target_path = resolve_collision(&Path::new(&library_root.path).join(&target_rel))
        .to_string_lossy()
        .to_string();

    Ok(MasterBuildPreview {
        set_id,
        imagetyp: set.imagetyp,
        frame_count: set.frame_count,
        resolved_combine,
        flat_precal,
        target_path,
        warnings,
    })
}

// ── Build-thread error plumbing ──────────────────────────────────────────────

/// Internal-only error type for the build thread: collapses every failure
/// mode into "cancelled" (queue-cancel or mid-integration cancel — both
/// surface identically to the caller) or "other" (a message suitable for
/// the `master-build-complete` event's `error` field).
enum BuildStepError {
    Cancelled,
    Other(String),
}

impl From<ApiError> for BuildStepError {
    fn from(e: ApiError) -> Self { BuildStepError::Other(e.to_string()) }
}
impl From<rusqlite::Error> for BuildStepError {
    fn from(e: rusqlite::Error) -> Self { BuildStepError::Other(e.to_string()) }
}
impl From<anyhow::Error> for BuildStepError {
    fn from(e: anyhow::Error) -> Self { BuildStepError::Other(format!("{e:#}")) }
}
impl From<crate::fits_writer::FitsWriteError> for BuildStepError {
    fn from(e: crate::fits_writer::FitsWriteError) -> Self { BuildStepError::Other(e.to_string()) }
}
impl From<std::io::Error> for BuildStepError {
    fn from(e: std::io::Error) -> Self { BuildStepError::Other(e.to_string()) }
}
impl From<IntegrationError> for BuildStepError {
    fn from(e: IntegrationError) -> Self {
        match e {
            IntegrationError::Cancelled => BuildStepError::Cancelled,
            other => BuildStepError::Other(other.to_string()),
        }
    }
}

/// Where a build's output goes and what happens to the DB afterward (Task
/// 13). `New` is the Task 12 path unchanged: a fresh, collision-resolved
/// path under the library root, then `register_master` (new master
/// `calibration_set` + provenance + relink + supersede the source). `Rebuild`
/// is Task 13: the master's OWN existing file path (no collision
/// resolution — `write_fits_f32` replaces it atomically), then
/// `master_provenance::update_rebuild` + a direct refresh of that file's
/// `size`/`modified_at` row. Rebuild never touches `calibration_set`,
/// `frames`, or any `calibration_set_to_frames` link — the master set/frame
/// identity and every consumer relink from the original registration stay
/// exactly as they were; only the pixels on disk and the provenance
/// bookkeeping change.
enum BuildTarget {
    New,
    Rebuild {
        master_set_id: i64,
        /// `files.id` for the master's existing file row — the direct SQL
        /// UPDATE's key.
        master_file_id: i64,
        /// The master's existing `files.path` — write target, unchanged.
        target_path: PathBuf,
    },
}

/// The whole build: acquire queue slot -> load member paths/set row ->
/// resolve combine/precal -> integrate -> write -> register (or, for a
/// rebuild, update-in-place — see `BuildTarget`). Every early exit is a
/// plain `?`/`Err` return — `run_master_build_thread` (the only caller)
/// always removes the handle and always emits `master-build-complete`
/// afterward, so there's no cleanup duty here.
#[allow(clippy::too_many_arguments)]
fn run_build(
    ctx: &ServiceContext,
    emitter: &dyn ProgressEmitter,
    app_version: &str,
    set_id: i64,
    recipe: &MasterRecipe,
    cancel_flag: &Arc<AtomicBool>,
    target: BuildTarget,
) -> Result<i64, BuildStepError> {
    let label = format!("Master build: calibration set {set_id}");
    // Bound (not discarded with `_`) so the permit's Drop — which releases
    // the concurrency slot — fires at the end of THIS function's scope,
    // before `run_master_build_thread` removes the handle / emits the
    // completion event. Prefixed with `_` only to silence the "never read"
    // lint; it's still held, just never read.
    let (_permit, _job_id) = ctx
        .compute_queue
        .acquire(ComputeJobKind::MasterBuild, &label, cancel_flag.clone())
        .map_err(|_queue_cancelled| BuildStepError::Cancelled)?;

    // Named `db_handle` (not `db`) — a local binding named `db` would shadow
    // the `db(ctx)` helper fn for the rest of this scope, breaking the
    // second acquisition below (see `api::analysis::get_frame_star_metrics`
    // for the same precedent/comment).
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();

    let set = match &target {
        BuildTarget::New => load_and_validate_set(&conn, set_id)?,
        BuildTarget::Rebuild { .. } => {
            // Defensive re-check (same spirit as `load_and_validate_set` for
            // the New path) — `rebuild_master` already checked this before
            // spawning, but the thread re-verifies rather than trusting a
            // check that ran on a possibly-stale snapshot.
            check_rebuild_source_ready(&conn, set_id)?;
            load_set_row(&conn, set_id)?
        }
    };

    let mut stmt = conn.prepare(
        "SELECT fi.path FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE csf.set_id = ?1 ORDER BY fi.path",
    )?;
    let paths: Vec<PathBuf> = stmt
        .query_map([set_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(PathBuf::from)
        .collect();
    drop(stmt);

    let is_flat = set.imagetyp == "Flat";
    let resolved_combine = resolve_combine(recipe.combine, &set.imagetyp, set.frame_count);
    let (precal_choice, _warnings) = if is_flat {
        select_flat_precal(&conn, set_id, set.exptime, recipe.synthetic_bias)?
    } else {
        (PrecalChoice::None, Vec::new())
    };
    let precal_desc = precal_choice.describe();

    // Release the pooled connection before the (potentially long) pixel
    // work below — precal load + banded integration do no DB work, and the
    // pool only has a handful of slots shared with every other in-flight
    // request.
    drop(conn);

    let pool = ctx.image_pool.as_ref();
    let scratch = std::env::temp_dir();
    let on_band = |current: usize, total: usize| {
        let percent = if total > 0 { (current as f64 / total as f64) * 100.0 } else { 100.0 };
        emit_event(emitter, "master-build-progress", &MasterBuildProgressEvent {
            set_id, stage: "integrating", current, total, percent,
        });
    };
    let progress = EngineProgress { on_band: &on_band };

    let out = if is_flat {
        // Pixel materialization of the selected precal happens HERE, on the
        // build thread — the only place `load_precal_pixels` is called.
        let precal = load_precal_pixels(&precal_choice, &scratch)?;
        integrate_flat(&paths, &precal, resolved_combine, pool, &scratch, cancel_flag.as_ref(), progress)?
    } else {
        integrate_bias_like(&paths, resolved_combine, pool, &scratch, cancel_flag.as_ref(), progress)?
    };

    emit_event(emitter, "master-build-progress", &MasterBuildProgressEvent {
        set_id, stage: "writing", current: 0, total: 0, percent: 0.0,
    });

    let db_handle = db(ctx)?;
    let conn = db_handle.conn();

    let inputs = load_header_inputs(&conn, set_id)?;
    let (member_hash_str, _uuids) = member_hash(&conn, set_id)?;
    let recipe_summary = recipe_summary_string(resolved_combine, set.frame_count);
    let cards = build_master_cards(&inputs, app_version, &recipe_summary, &member_hash_str, out.flat_norm)?;

    let target_abs = match &target {
        BuildTarget::New => {
            let library_root = library_root_or_err(ctx)?;
            let target_rel = master_relative_path(&MasterPathParams {
                instrume: inputs.instrume.as_deref(),
                master_kind: inputs.kind,
                filter: inputs.filter.as_deref(),
                exptime: inputs.exptime,
                ccd_temp: inputs.temp_mean,
                gain: inputs.gain,
                binning: set.binning.as_deref(),
                date: &set.date,
            });
            resolve_collision(&Path::new(&library_root.path).join(&target_rel))
        }
        // Same path the master already lives at — no collision resolution:
        // `write_fits_f32` replaces the existing file atomically.
        BuildTarget::Rebuild { target_path, .. } => target_path.clone(),
    };
    if let Some(parent) = target_abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_fits_f32(&target_abs, out.width, out.height, 1, &out.data, &cards)?;

    emit_event(emitter, "master-build-progress", &MasterBuildProgressEvent {
        set_id, stage: "registering", current: 0, total: 0, percent: 0.0,
    });

    let recipe_json = serde_json::json!({
        "combine": resolved_combine,
        "syntheticBias": recipe.synthetic_bias,
        "precal": precal_desc,
        "rejectedFraction": out.rejected_fraction,
        "engine": "athenaeum",
        "version": app_version,
    }).to_string();

    match target {
        BuildTarget::New => match register_master(&conn, set_id, &target_abs, &recipe_json) {
            Ok(reg) => Ok(reg.master_set_id),
            Err(e) => {
                // Registration failed AFTER the file was written — remove it
                // so no orphan master sits in the library unregistered.
                let _ = std::fs::remove_file(&target_abs);
                Err(BuildStepError::Other(format!("{e:#}")))
            }
        },
        BuildTarget::Rebuild { master_set_id, master_file_id, .. } => {
            // Everything after `write_fits_f32`'s atomic rename, in ONE
            // closure so every possible failure in this window (tx open,
            // update_rebuild, metadata read, files UPDATE, commit) funnels
            // through the single metadata-drift error log below.
            let finalize = || -> Result<(), BuildStepError> {
                let tx = conn.unchecked_transaction()?;
                crate::db::master_provenance::update_rebuild(&tx, master_set_id, &recipe_json, &member_hash_str)?;
                let meta = std::fs::metadata(&target_abs)?;
                let modified_at = chrono::DateTime::<chrono::Utc>::from(meta.modified()?);
                tx.execute(
                    "UPDATE files SET size = ?1, modified_at = ?2 WHERE id = ?3",
                    rusqlite::params![meta.len() as i64, modified_at.to_rfc3339(), master_file_id],
                )?;
                tx.commit()?;
                Ok(())
            };
            if let Err(e) = finalize() {
                // METADATA-DRIFT WINDOW: the on-disk master file HAS already
                // been replaced (atomic rename inside `write_fits_f32`), but
                // the catalog metadata was NOT refreshed — the
                // master_provenance row (recipe/hash/created_at) and the
                // files row (size/modified_at) still describe the PREVIOUS
                // build, and the `master-build-complete` event will report
                // failure. The drift is metadata-only (pixels on disk are
                // the new, valid rebuild) and self-heals on the next
                // successful rebuild of the same master. Logged loudly here
                // — this is the one state where DB and disk disagree.
                let err_msg = match &e {
                    BuildStepError::Cancelled => "cancelled".to_string(),
                    BuildStepError::Other(m) => m.clone(),
                };
                tracing::error!(
                    source_set_id = set_id,
                    master_set_id,
                    master_path = %target_abs.display(),
                    error = %err_msg,
                    "rebuild metadata refresh failed AFTER the on-disk master was atomically replaced — \
                     the file now holds the rebuilt pixels but master_provenance/files still describe \
                     the previous build; a subsequent successful rebuild self-heals this drift"
                );
                return Err(e);
            }
            // Deliberately NO remove_file on any error above (unlike the New
            // arm): by this point `write_fits_f32` already atomically
            // replaced the PRE-EXISTING master file with the freshly
            // rebuilt pixels/header — there is no orphan to clean up, only
            // a DB update that may have failed. Deleting the file here
            // would turn a provenance-refresh failure into total data loss
            // (no master file at all, where there was a perfectly good one
            // moments ago); leaving the rebuilt file in place is strictly
            // better, and the `tx` above is one transaction so
            // update_rebuild + the files UPDATE can't half-apply. The COST
            // of keeping the file is the metadata-drift window logged
            // above: new pixels on disk, stale provenance/files rows in the
            // DB until a later rebuild succeeds. That trade (stale metadata
            // over destroyed data) is intentional.
            Ok(master_set_id)
        }
    }
}

/// Runs on the dedicated `master-build-{set_id}` thread. The single exit
/// path for the whole build: handle removal and `master-build-complete`
/// ALWAYS happen here, exactly once, regardless of how `run_build` ended.
#[allow(clippy::too_many_arguments)]
fn run_master_build_thread(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    set_id: i64,
    recipe: MasterRecipe,
    cancel_flag: Arc<AtomicBool>,
    target: BuildTarget,
) {
    let result = run_build(&ctx, emitter.as_ref(), &app_version, set_id, &recipe, &cancel_flag, target);

    ctx.active_master_builds.lock().unwrap().remove(&set_id);

    let (master_set_id, success, cancelled, error) = match result {
        Ok(id) => (Some(id), true, false, None),
        Err(BuildStepError::Cancelled) => (None, false, true, None),
        Err(BuildStepError::Other(msg)) => (None, false, false, Some(msg)),
    };

    emit_event(emitter.as_ref(), "master-build-complete", &MasterBuildCompleteEvent {
        set_id, master_set_id, success, cancelled, error,
    });
}

// ── Public start/batch/rebuild/cancel/provenance API ─────────────────────────

/// Validates, registers the cancel handle, spawns the detached build thread
/// (queue admission happens INSIDE the thread), returns immediately.
pub fn start_master_build(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    set_id: i64,
    recipe: MasterRecipe,
) -> Result<(), ApiError> {
    {
        let db = db(&ctx)?;
        let conn = db.conn();
        load_and_validate_set(&conn, set_id)?;
    }
    library_root_or_err(&ctx)?;

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut active = ctx.active_master_builds.lock().unwrap();
        if active.contains_key(&set_id) {
            return Err(ApiError::Conflict(format!(
                "a master build is already in progress for calibration set {set_id}"
            )));
        }
        active.insert(set_id, MasterBuildHandle { cancel_flag: cancel_flag.clone() });
    }

    let thread_ctx = ctx.clone();
    let spawn_result = std::thread::Builder::new()
        .name(format!("master-build-{set_id}"))
        .spawn(move || {
            run_master_build_thread(
                thread_ctx, emitter, app_version, set_id, recipe, cancel_flag, BuildTarget::New,
            );
        });

    if let Err(e) = spawn_result {
        // The thread never started, so nothing will ever remove this handle
        // or emit master-build-complete — clean up right here instead.
        ctx.active_master_builds.lock().unwrap().remove(&set_id);
        return Err(ApiError::Internal(format!("failed to spawn master-build thread: {e}")));
    }

    Ok(())
}

/// Pure planning step for [`start_master_builds_batch`] (Task 13): loads
/// `(imagetyp, frame_count, superseded_by_set_id, is_master_library)` for
/// every requested id, buckets each into "ready to start" or "skipped" using
/// the SAME predicates as `validate_buildable_set` (in the SAME order —
/// superseded, then already-a-master, then too-few-frames — so a set that
/// happens to match more than one, e.g. a master itself always has
/// `frame_count == 1 < MIN_MASTER_FRAMES`, gets the more specific reason),
/// then sorts the ready set by `(type_build_rank, id)` — dependency order,
/// ties broken by id for determinism. No `ServiceContext`, no thread, no
/// side effects — just DB reads — so it's unit-testable directly against an
/// in-memory DB (see tests below). `start_master_builds_batch` is exactly
/// this plus a loop of `start_master_build`.
fn plan_batch(
    conn: &rusqlite::Connection,
    set_ids: &[i64],
) -> Result<(Vec<(i64, String)>, Vec<BatchSkip>), ApiError> {
    let mut ready: Vec<(i64, String)> = Vec::new();
    let mut skipped: Vec<BatchSkip> = Vec::new();

    for &set_id in set_ids {
        let row: Option<(String, i64, Option<i64>, i64)> = conn.query_row(
            "SELECT imagetyp, frame_count, superseded_by_set_id, is_master_library
             FROM calibration_set WHERE id = ?1",
            [set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional()?;

        let Some((imagetyp, frame_count, superseded_by_set_id, is_master_library)) = row else {
            skipped.push(BatchSkip { set_id, reason: "unknown set".into() });
            continue;
        };

        if superseded_by_set_id.is_some() {
            skipped.push(BatchSkip { set_id, reason: "already has a master".into() });
            continue;
        }
        if is_master_library != 0 {
            skipped.push(BatchSkip { set_id, reason: "is itself a master".into() });
            continue;
        }
        if frame_count < MIN_MASTER_FRAMES {
            skipped.push(BatchSkip {
                set_id,
                reason: format!("only {frame_count} frames (minimum {MIN_MASTER_FRAMES})"),
            });
            continue;
        }

        ready.push((set_id, imagetyp));
    }

    ready.sort_by_key(|(id, imagetyp)| (type_build_rank(imagetyp), *id));
    Ok((ready, skipped))
}

/// Enqueue builds for many sets, dependency-ordered: Bias & DarkFlat first,
/// then Dark, then Flat (flats resolve pre-cal at RUN time inside the build
/// thread — see `select_flat_precal` — so a master built earlier in THIS
/// batch is already visible on disk to a later flat build, as long as it was
/// submitted first; `plan_batch`'s sort plus the compute queue's FIFO
/// admission is what guarantees that). Sets already superseded / too small /
/// themselves masters / unknown are skipped with a per-set reason instead of
/// failing the whole batch; likewise, a per-set `start_master_build` error
/// (e.g. a Conflict from a concurrent duplicate-build click) is caught and
/// folded into `skipped` rather than aborting the remaining sets.
///
/// **Concurrency caveat:** the FIFO admission order only *serializes
/// execution* when `compute.max_concurrent == 1` (the default). At
/// `max_concurrent > 1` a flat build can be admitted while its
/// darkflat/dark/bias dependency is still running, in which case
/// `select_flat_precal` sees the dependency's source set still raw and
/// **degrades gracefully**: it skips the raw set with a "build its master
/// first" warning and falls through the spec §9 chain (next precal type ->
/// synthetic bias -> un-pre-calibrated) — no corruption, just a lesser
/// recipe recorded in the flat's provenance/warnings. When a batch mixes a
/// Flat with any rank<2 dependency AND the queue is running concurrent, a
/// `tracing::warn!` flags the weakened guarantee. Queue semantics are
/// deliberately NOT changed here.
pub fn start_master_builds_batch(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    set_ids: Vec<i64>,
    recipe: MasterRecipe,
) -> Result<BatchBuildReport, ApiError> {
    let (ready, mut skipped) = {
        let db = db(&ctx)?;
        let conn = db.conn();
        plan_batch(&conn, &set_ids)?
    };

    // See the doc comment's concurrency caveat: submission order only
    // implies completion order at max_concurrent == 1. Only worth flagging
    // when the batch actually has a flat that could want a rank<2 master
    // built earlier in the same batch.
    let has_flat = ready.iter().any(|(_, t)| type_build_rank(t) == 2);
    let has_precal_rank = ready.iter().any(|(_, t)| type_build_rank(t) < 2);
    let max_concurrent = ctx.compute_queue.max_concurrent();
    if has_flat && has_precal_rank && max_concurrent > 1 {
        tracing::warn!(
            max_concurrent,
            batch_size = ready.len(),
            "batch mixes flats with bias/darkflat/dark builds but compute.max_concurrent > 1 — \
             dependency ordering is only guaranteed at max_concurrent=1; a flat admitted before its \
             precal master finishes will fall back to a lesser precal (raw set skipped with a \
             'build its master first' warning, then synthetic bias / un-pre-calibrated)"
        );
    }

    let mut started_set_ids = Vec::new();
    for (set_id, _imagetyp) in ready {
        match start_master_build(ctx.clone(), emitter.clone(), app_version.clone(), set_id, recipe.clone()) {
            Ok(()) => started_set_ids.push(set_id),
            Err(e) => skipped.push(BatchSkip { set_id, reason: e.to_string() }),
        }
    }

    Ok(BatchBuildReport { started_set_ids, skipped })
}

/// Re-integrate an existing Athenaeum-built master IN PLACE from the SAME
/// source frames that originally built it (Task 13) — same target file
/// (atomic replace via `write_fits_f32`, no collision suffix), refreshed
/// `master_provenance` + `files` row instead of a new registration. See
/// `BuildTarget::Rebuild` for exactly what changes vs. a normal build, and
/// the `MasterBuildCompleteEvent` doc comment for why events key on the
/// SOURCE set id rather than `master_set_id`.
///
/// Recipe: unlike `start_master_build`/`start_master_builds_batch`, this
/// takes no `MasterRecipe` argument — matches the spec'd interface. A
/// rebuild always resolves a fresh Auto recipe
/// (`MasterRecipe { combine: None, synthetic_bias: None }`) rather than
/// replaying whatever explicit override built the original: the persisted
/// `recipe_json.combine` is already-RESOLVED (not the original
/// `Option<CombineMethod>` input), so treating it as a future override would
/// freeze the recipe forever instead of picking up a since-built precal
/// master (spec §9 fallback chain) or a frame-count change that shifts the
/// Auto combine method — exactly the kind of drift a rebuild exists to
/// correct. An operator wanting a specific non-Auto override has no path
/// through this command in v1 (out of scope for Task 13).
pub fn rebuild_master(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    master_set_id: i64,
) -> Result<(), ApiError> {
    let (source_set_id, master_file_id, target_path) = {
        let db = db(&ctx)?;
        let conn = db.conn();

        let prov = crate::db::master_provenance::get(&conn, master_set_id)?
            .ok_or_else(|| ApiError::Invalid(
                "master was not built by Athenaeum — no provenance recorded, cannot rebuild".into(),
            ))?;
        let source_set_id = prov.source_set_id.ok_or_else(|| ApiError::Invalid(
            "master was not built by Athenaeum — no source set recorded, cannot rebuild".into(),
        ))?;

        check_rebuild_source_ready(&conn, source_set_id)?;

        // LIMIT 1: a master is a 1:1 calibration_set by invariant (exactly
        // one member frame), so this never actually needs to disambiguate —
        // matches `select_flat_precal`'s defensive `LIMIT 1` on the same
        // shape of query.
        let master_file: Option<(i64, String)> = conn.query_row(
            "SELECT fi.id, fi.path FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1 LIMIT 1",
            [master_set_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let (master_file_id, target_path) = master_file.ok_or_else(|| {
            ApiError::NotFound(format!("master set {master_set_id} has no file on record"))
        })?;

        (source_set_id, master_file_id, target_path)
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        // Keyed by the SOURCE set id (reusing `active_master_builds`, same
        // map `start_master_build` uses) — a rebuild is, mechanically,
        // still "a build of this source set's frames", so it shares the
        // same duplicate-build guard rather than a parallel one keyed by
        // `master_set_id`.
        let mut active = ctx.active_master_builds.lock().unwrap();
        if active.contains_key(&source_set_id) {
            return Err(ApiError::Conflict(format!(
                "a master build is already in progress for calibration set {source_set_id}"
            )));
        }
        active.insert(source_set_id, MasterBuildHandle { cancel_flag: cancel_flag.clone() });
    }

    let recipe = MasterRecipe { combine: None, synthetic_bias: None };
    let target = BuildTarget::Rebuild {
        master_set_id,
        master_file_id,
        target_path: PathBuf::from(target_path),
    };

    let thread_ctx = ctx.clone();
    let spawn_result = std::thread::Builder::new()
        .name(format!("master-build-{source_set_id}"))
        .spawn(move || {
            run_master_build_thread(
                thread_ctx, emitter, app_version, source_set_id, recipe, cancel_flag, target,
            );
        });

    if let Err(e) = spawn_result {
        ctx.active_master_builds.lock().unwrap().remove(&source_set_id);
        return Err(ApiError::Internal(format!("failed to spawn master-build thread: {e}")));
    }

    Ok(())
}

/// Cancel an active master build (queued-in-compute-queue or running).
pub fn cancel_master_build(ctx: &ServiceContext, set_id: i64) -> Result<(), ApiError> {
    let active = ctx.active_master_builds.lock().unwrap();
    if let Some(handle) = active.get(&set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err(ApiError::NotFound(format!("no active master build for calibration set {set_id}")))
    }
}

/// Provenance + rebuildability info for a master set. `None` if the set
/// isn't a master built by Athenaeum (no `master_provenance` row).
pub fn get_master_provenance(
    ctx: &ServiceContext,
    master_set_id: i64,
) -> Result<Option<MasterProvenanceInfo>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let Some(prov) = crate::db::master_provenance::get(&conn, master_set_id)? else {
        return Ok(None);
    };

    let member_count = serde_json::from_str::<Vec<String>>(&prov.member_frame_uuids)
        .map(|v| v.len())
        .unwrap_or(0);

    let (originals_archived, source_frames_on_disk) = if let Some(src_id) = prov.source_set_id {
        let archived_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1 AND fi.archived_in_operation IS NOT NULL",
            [src_id],
            |r| r.get(0),
        )?;

        let mut stmt = conn.prepare(
            "SELECT fi.path FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1",
        )?;
        let paths: Vec<String> = stmt
            .query_map([src_id], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let source_frames_on_disk = !paths.is_empty() && paths.iter().all(|p| Path::new(p).exists());

        (archived_count > 0, source_frames_on_disk)
    } else {
        (false, false)
    };

    Ok(Some(MasterProvenanceInfo {
        master_set_id: prov.master_set_id,
        source_set_id: prov.source_set_id,
        recipe_json: prov.recipe_json,
        member_count,
        member_hash: prov.member_hash,
        created_at: prov.created_at,
        source_frames_on_disk,
        originals_archived,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::combine::CombineMethod;
    use rusqlite::Connection;

    #[test]
    fn auto_recipe_rules() {
        // spec §9: bias-like N>=15 winsorized else median; flat N>=15 winsorized
        // else percentile
        assert_eq!(resolve_combine(None, "Dark", 20),
            CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 });
        assert_eq!(resolve_combine(None, "Dark", 5), CombineMethod::Median);
        assert_eq!(resolve_combine(None, "Bias", 14), CombineMethod::Median);
        assert_eq!(resolve_combine(None, "Flat", 20),
            CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 });
        assert_eq!(resolve_combine(None, "Flat", 6),
            CombineMethod::PercentileClip { low: 0.2, high: 0.02 });
        // explicit override wins
        assert_eq!(resolve_combine(Some(CombineMethod::Mean), "Flat", 6), CombineMethod::Mean);
    }

    #[test]
    fn batch_order_bias_darkflat_dark_flat() {
        let order = |t: &str| type_build_rank(t);
        assert!(order("Bias") < order("Dark"));
        assert!(order("DarkFlat") < order("Dark"));
        assert!(order("Dark") < order("Flat"));
        // Bias and DarkFlat are co-equal rank 0 (either order between them is
        // fine — neither can be the OTHER's flat pre-cal).
        assert_eq!(order("Bias"), order("DarkFlat"));
        // anything unrecognized sorts after every real calibration type.
        assert!(order("Flat") < order("Light"));
    }

    // ── select_flat_precal fallback chain (pure DB — no pixel I/O) ──────────

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn seed_flat_set(conn: &Connection, exptime: f64) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, exptime, date, frame_count)
             VALUES ('Flat', ?1, '2026-06-28', 5)",
            [exptime],
        ).unwrap();
        conn.last_insert_rowid()
    }

    /// A precal calibration set with one member file — the member row is
    /// what `select_flat_precal`'s path lookup resolves against (the path
    /// itself is a dummy string; selection never touches disk).
    fn seed_precal_set(conn: &Connection, imagetyp: &str, is_master: i64, exptime: Option<f64>) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, exptime, date, is_master_library, frame_count)
             VALUES (?1, ?2, '2026-06-28', ?3, 1)",
            rusqlite::params![imagetyp, exptime, is_master],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 10, '2026-06-28', 'FITS')",
            rusqlite::params![
                format!("/library/{imagetyp}_{set_id}.fits"),
                format!("{imagetyp}_{set_id}.fits")
            ],
        ).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
            rusqlite::params![file_id, imagetyp],
        ).unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        ).unwrap();
        set_id
    }

    fn link(conn: &Connection, flat_set: i64, precal_set: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'calibration_set', ?2, ?3)",
            rusqlite::params![flat_set, precal_set, cal_type],
        ).unwrap();
    }

    #[test]
    fn precal_prefers_darkflat_master_over_bias() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let df = seed_precal_set(&conn, "MasterDarkFlat", 1, Some(2.0));
        link(&conn, flat, df, "DarkFlat");
        // bias also linked — must lose to the darkflat
        let bias = seed_precal_set(&conn, "MasterBias", 1, None);
        link(&conn, flat, bias, "Bias");

        let (choice, warnings) = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        match &choice {
            PrecalChoice::Master { set_id, cal_type, .. } => {
                assert_eq!(*set_id, df);
                assert_eq!(*cal_type, "DarkFlat");
            }
            other => panic!("expected darkflat master, got {other:?}"),
        }
        assert!(choice.describe().unwrap().contains("darkflat master"),
            "{:?}", choice.describe());
    }

    #[test]
    fn precal_exposure_matched_dark_master_is_used() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let dark = seed_precal_set(&conn, "MasterDark", 1, Some(2.3)); // within ±0.5s
        link(&conn, flat, dark, "Dark");

        let (choice, warnings) = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(matches!(choice, PrecalChoice::Master { cal_type: "Dark", .. }), "{choice:?}");
    }

    #[test]
    fn precal_exposure_mismatched_dark_is_skipped_with_warning() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let dark = seed_precal_set(&conn, "MasterDark", 1, Some(4.0)); // off by 2s
        link(&conn, flat, dark, "Dark");

        let (choice, warnings) = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(matches!(choice, PrecalChoice::None), "{choice:?}");
        assert!(warnings.iter().any(|w| w.contains("exposure does not match")), "{warnings:?}");
        // fell all the way through: also carries the un-pre-calibrated warning
        assert!(warnings.iter().any(|w| w.contains("un-pre-calibrated")), "{warnings:?}");
    }

    #[test]
    fn precal_raw_darkflat_skipped_falls_to_bias() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let raw_df = seed_precal_set(&conn, "DarkFlat", 0, Some(2.0)); // NOT a master
        link(&conn, flat, raw_df, "DarkFlat");
        let bias = seed_precal_set(&conn, "MasterBias", 1, None);
        link(&conn, flat, bias, "Bias");

        let (choice, warnings) = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(warnings.iter().any(|w| w.contains("build its master first")), "{warnings:?}");
        match &choice {
            PrecalChoice::Master { set_id, cal_type, .. } => {
                assert_eq!(*set_id, bias);
                assert_eq!(*cal_type, "Bias");
            }
            other => panic!("expected bias master fallback, got {other:?}"),
        }
    }

    #[test]
    fn precal_bias_master_only() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let bias = seed_precal_set(&conn, "MasterBias", 1, None);
        link(&conn, flat, bias, "Bias");

        let (choice, warnings) = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(matches!(choice, PrecalChoice::Master { cal_type: "Bias", .. }), "{choice:?}");
        assert!(choice.describe().unwrap().contains("bias master"), "{:?}", choice.describe());
    }

    #[test]
    fn precal_synthetic_bias_fallback() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);

        let (choice, warnings) = select_flat_precal(&conn, flat, Some(2.0), Some(500.0)).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(choice, PrecalChoice::Synthetic(500.0));
        assert_eq!(choice.describe().as_deref(), Some("synthetic bias 500 ADU"));
    }

    #[test]
    fn precal_nothing_yields_none_with_warning() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);

        let (choice, warnings) = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(matches!(choice, PrecalChoice::None), "{choice:?}");
        assert_eq!(choice.describe(), Option::None);
        assert!(warnings.iter().any(|w| w.contains("un-pre-calibrated")), "{warnings:?}");
    }

    // ── plan_batch (pure DB — Task 13 dependency-ordering + skip reasons) ───

    /// A minimal buildable (or not) calibration_set row — only the columns
    /// `plan_batch` reads. `superseded_by_set_id` doesn't need to point at a
    /// real row for these tests (no FK enforcement on that column in
    /// sqlite's default mode), so a dummy id is fine.
    fn seed_set(
        conn: &Connection,
        imagetyp: &str,
        frame_count: i64,
        superseded_by_set_id: Option<i64>,
        is_master_library: i64,
    ) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, frame_count, superseded_by_set_id, is_master_library)
             VALUES (?1, '2026-06-28', ?2, ?3, ?4)",
            rusqlite::params![imagetyp, frame_count, superseded_by_set_id, is_master_library],
        ).unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn plan_batch_orders_by_type_then_id() {
        let conn = test_conn();
        // Seeded out of dependency order and out of id order within a rank,
        // to make sure both the rank AND the id tie-break are exercised.
        let flat = seed_set(&conn, "Flat", 5, None, 0);
        let dark = seed_set(&conn, "Dark", 5, None, 0);
        let darkflat_b = seed_set(&conn, "DarkFlat", 5, None, 0);
        let bias = seed_set(&conn, "Bias", 5, None, 0);
        let darkflat_a = seed_set(&conn, "DarkFlat", 5, None, 0);

        let (ready, skipped) = plan_batch(&conn, &[flat, dark, darkflat_b, bias, darkflat_a]).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        let ordered_ids: Vec<i64> = ready.iter().map(|(id, _)| *id).collect();
        // rank 0 (Bias/DarkFlat) sorted by id, then rank 1 (Dark), then rank 2 (Flat).
        let mut expected_rank0 = vec![bias, darkflat_a, darkflat_b];
        expected_rank0.sort();
        let mut expected = expected_rank0;
        expected.push(dark);
        expected.push(flat);
        assert_eq!(ordered_ids, expected);
    }

    #[test]
    fn plan_batch_skips_already_superseded() {
        let conn = test_conn();
        let master = seed_set(&conn, "MasterDark", 1, None, 1);
        let raw = seed_set(&conn, "Dark", 5, Some(master), 0);

        let (ready, skipped) = plan_batch(&conn, &[raw]).unwrap();
        assert!(ready.is_empty());
        assert_eq!(skipped, vec![BatchSkip { set_id: raw, reason: "already has a master".into() }]);
    }

    #[test]
    fn plan_batch_skips_too_few_frames() {
        let conn = test_conn();
        let raw = seed_set(&conn, "Dark", 2, None, 0);

        let (ready, skipped) = plan_batch(&conn, &[raw]).unwrap();
        assert!(ready.is_empty());
        assert_eq!(
            skipped,
            vec![BatchSkip { set_id: raw, reason: "only 2 frames (minimum 3)".into() }],
        );
    }

    #[test]
    fn plan_batch_skips_set_that_is_itself_a_master() {
        let conn = test_conn();
        // A 1:1 master set: frame_count == 1, which is ALSO < MIN_MASTER_FRAMES —
        // the reason must still come out as "is itself a master", not the
        // frame-count reason, because the is-master check runs first.
        let master = seed_set(&conn, "MasterDark", 1, None, 1);

        let (ready, skipped) = plan_batch(&conn, &[master]).unwrap();
        assert!(ready.is_empty());
        assert_eq!(skipped, vec![BatchSkip { set_id: master, reason: "is itself a master".into() }]);
    }

    #[test]
    fn plan_batch_skips_unknown_set() {
        let conn = test_conn();
        let (ready, skipped) = plan_batch(&conn, &[999_999]).unwrap();
        assert!(ready.is_empty());
        assert_eq!(skipped, vec![BatchSkip { set_id: 999_999, reason: "unknown set".into() }]);
    }

    #[test]
    fn plan_batch_mixed_batch_ready_and_skipped_independently() {
        let conn = test_conn();
        let dark = seed_set(&conn, "Dark", 5, None, 0);
        let master = seed_set(&conn, "MasterBias", 1, None, 1);
        let too_small = seed_set(&conn, "Flat", 1, None, 0);

        let (ready, skipped) = plan_batch(&conn, &[dark, master, too_small, 424242]).unwrap();
        assert_eq!(ready, vec![(dark, "Dark".to_string())]);
        assert_eq!(skipped.len(), 3);
        assert!(skipped.iter().any(|s| s.set_id == master && s.reason == "is itself a master"));
        assert!(skipped.iter().any(|s| s.set_id == too_small && s.reason.contains("minimum 3")));
        assert!(skipped.iter().any(|s| s.set_id == 424242 && s.reason == "unknown set"));
    }

    // ── check_rebuild_source_ready (pure DB + Path::exists) ─────────────────

    /// A source calibration_set with `n` member frames, each pointing at a
    /// real (but possibly not actually created) path so `Path::exists` has
    /// something concrete to check. `archived` marks every member's
    /// `files.archived_in_operation` as set (non-NULL) or not.
    fn seed_source_with_files(conn: &Connection, dir: &std::path::Path, n: usize, archived: bool) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, frame_count) VALUES ('Dark', '2026-06-28', ?1)",
            [n as i64],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        for i in 0..n {
            let p = dir.join(format!("raw{i}.fits"));
            std::fs::write(&p, b"not a real fits file, existence is all that matters here").unwrap();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, archived_in_operation)
                 VALUES (?1, ?2, 10, '2026-06-28', 'FITS', ?3)",
                rusqlite::params![
                    p.to_string_lossy(), format!("raw{i}.fits"), archived.then_some(1i64),
                ],
            ).unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Dark')",
                [file_id],
            ).unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            ).unwrap();
        }
        set_id
    }

    #[test]
    fn rebuild_source_ready_when_all_files_present() {
        let dir = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let set_id = seed_source_with_files(&conn, dir.path(), 3, false);
        assert!(check_rebuild_source_ready(&conn, set_id).is_ok());
    }

    #[test]
    fn rebuild_source_not_ready_when_archived() {
        let dir = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let set_id = seed_source_with_files(&conn, dir.path(), 3, true);
        // Remove the files on disk too — an archived original really is gone
        // from its recorded path (moved into a ZIP).
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        let err = check_rebuild_source_ready(&conn, set_id).unwrap_err();
        assert!(err.to_string().contains("archived"), "{err}");
    }

    #[test]
    fn rebuild_source_not_ready_when_missing_but_not_archived() {
        let dir = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let set_id = seed_source_with_files(&conn, dir.path(), 3, false);
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        let err = check_rebuild_source_ready(&conn, set_id).unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
        assert!(!err.to_string().contains("archived"), "{err}");
    }

    #[test]
    fn rebuild_source_not_ready_when_no_member_frames() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, frame_count) VALUES ('Dark', '2026-06-28', 0)",
            [],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        let err = check_rebuild_source_ready(&conn, set_id).unwrap_err();
        assert!(err.to_string().contains("no member frames"), "{err}");
    }
}
