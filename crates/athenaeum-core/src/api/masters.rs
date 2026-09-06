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
use crate::calibration_library::headers::{
    build_master_cards, load_header_inputs, measure_flat_channel_norms,
};
use crate::calibration_library::paths::{
    claim_collision_free, master_relative_path, release_claim, resolve_collision_free,
    MasterPathParams,
};
use crate::calibration_library::register::{member_hash, register_master};
use crate::events::{emit_event, ProgressEmitter};
use crate::fits_writer::write_fits_f32;
use crate::integration::combine::{IntegrationRecipe, Rejection};
use crate::integration::engine::{
    integrate_bias_like, integrate_flat, EngineProgress, FlatPrecal, IntegrationOutput,
};
use crate::integration::storage_class::StorageClass;
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
    /// None => Auto (per-type/per-N rule from spec §2/§9). Field name kept
    /// (`combine`) across the CombineMethod → IntegrationRecipe migration.
    pub combine: Option<IntegrationRecipe>,
    /// Constant-ADU fallback for flat pre-calibration when no darkflat/dark/bias master is linked.
    pub synthetic_bias: Option<f64>,
    /// Task 14: when true, chain `archive_originals` on the SOURCE
    /// calibration set right after a successful `New`-target build (i.e.
    /// after `register_master` supersedes it) — non-fatally: an archive
    /// failure is logged but never turns a successful master build into a
    /// reported failure. Has no effect on `BuildTarget::Rebuild` (that path
    /// never calls `register_master`, so there's no "just superseded" moment
    /// to chain from — see `rebuild_master`'s own recipe construction).
    #[serde(default)]
    pub archive_after: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterBuildPreview {
    pub set_id: i64,
    pub imagetyp: String,
    pub frame_count: i64,
    pub resolved_combine: IntegrationRecipe,
    /// Human description: "master darkflat #12" | "synthetic bias 500 ADU" | null.
    pub flat_precal: Option<String>,
    /// Absolute, collision-resolved.
    pub target_path: String,
    pub warnings: Vec<String>,
    /// Raw (non-master) sub-cal sets encountered while resolving flat
    /// pre-cal (see `select_flat_precal`'s DarkFlat -> Dark -> Bias
    /// fallback chain) — lets the caller offer a "build its master first"
    /// shortcut instead of just surfacing the warning string. Always empty
    /// for non-flat previews.
    pub raw_precal_sets: Vec<RawPrecalSetDto>,
}

/// Wire DTO for a raw sub-cal set a flat's pre-cal chain skipped over.
/// `cal_type` is the link type ("DarkFlat" | "Dark" | "Bias"), not the raw
/// set's own `imagetyp` — they're the same string in practice, but `cal_type`
/// is what the fallback chain was looking for at that step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct RawPrecalSetDto {
    pub set_id: i64,
    pub cal_type: String,
    pub frame_count: i64,
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
    /// `archived_in_operation` read off whichever source member file has it
    /// set — every member of a single archive-of-originals op shares the
    /// same op id and zip by construction (`planner::build_calibration_set_plan`
    /// produces exactly one `PlannedZip` per calibration set), so "any member"
    /// and "the" op id are the same thing here. `None` when `originals_archived`
    /// is false.
    pub archive_operation_id: Option<i64>,
    /// `archive_zip_path` read off the same member file as
    /// `archive_operation_id`. `None` when `originals_archived` is false.
    pub archive_zip_path: Option<String>,
    /// `true` when archive markers are present but `Path::exists(archive_zip_path)`
    /// is false — the zip was deleted (or moved) outside the app. Always
    /// `false` when `originals_archived` is false (nothing to be missing).
    /// The UI uses this to hide the restore button and show an actionable
    /// warning instead of letting the user kick off a restore doomed to fail.
    pub archive_zip_missing: bool,
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

/// What `delete_master` undid, for the confirmation the UI shows afterwards.
///
/// `links_repointed` is deliberately one field for two outcomes, matching
/// `MasterUnregisterSummary`: with a raw set to fall back to (i.e.
/// `restored_raw_set_id.is_some()`) the consumer links MOVED back onto it;
/// for an imported master there is no lineage, so the same count is links
/// DELETED. Callers must branch on `restored_raw_set_id` before wording it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMasterResult {
    pub master_set_id: i64,
    pub restored_raw_set_id: Option<i64>,
    pub links_repointed: usize,
    pub files_deleted: usize,
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
    /// Bytes of source read so far / in total. `current`/`total` count bands,
    /// which say nothing about size once bands are machine-sized.
    bytes_done: u64,
    bytes_total: u64,
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
///
/// `warning` is the non-fatal counterpart of `error`: the build SUCCEEDED but
/// something in the source data was worth telling the operator about (today:
/// non-finite samples excluded from the integration — audit C2). Always `None`
/// on a failed or cancelled build.
#[derive(Clone, serde::Serialize)]
struct MasterBuildCompleteEvent {
    set_id: i64,
    master_set_id: Option<i64>,
    success: bool,
    cancelled: bool,
    error: Option<String>,
    warning: Option<String>,
}

// ── Recipe resolution (pure, unit-tested) ────────────────────────────────────

/// spec §2/§9: bias-like N>=15 Average+Winsorized else Median; flat N>=15
/// Average+Winsorized else Average+Percentile. Explicit override always wins.
pub fn resolve_recipe(
    explicit: Option<IntegrationRecipe>,
    imagetyp: &str,
    n: i64,
) -> IntegrationRecipe {
    if let Some(recipe) = explicit {
        return recipe;
    }
    let is_flat = imagetyp == "Flat";
    if n >= 15 {
        IntegrationRecipe::average(Rejection::WinsorizedSigma {
            sigma_low: 3.0,
            sigma_high: 3.0,
        })
    } else if is_flat {
        IntegrationRecipe::average(Rejection::PercentileClip {
            low: 0.2,
            high: 0.02,
        })
    } else {
        IntegrationRecipe::median(Rejection::None)
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
            PrecalChoice::Master {
                set_id,
                imagetyp,
                cal_type,
                ..
            } => Some(format!(
                "{} master #{set_id} ({imagetyp})",
                cal_type.to_lowercase()
            )),
            PrecalChoice::Synthetic(b) => Some(format!("synthetic bias {b} ADU")),
            PrecalChoice::None => Option::None,
        }
    }
}

/// A raw (non-master) sub-cal set encountered while walking the flat's
/// DarkFlat -> Dark -> Bias fallback chain, before landing on a usable
/// master (or falling all the way through). `cal_type` is the link type the
/// chain was looking for at that step ("DarkFlat" | "Dark" | "Bias").
#[derive(Debug, Clone, PartialEq)]
struct RawPrecalCandidate {
    set_id: i64,
    cal_type: String,
    frame_count: i64,
}

/// Full result of `select_flat_precal`: the WHAT (`PrecalChoice`), any
/// warnings collected along the way, and every raw sub-cal set skipped en
/// route — the last of these is what lets `preview_master_build` offer a
/// "build its master first" shortcut instead of just the warning string.
struct PrecalSelection {
    choice: PrecalChoice,
    warnings: Vec<String>,
    raw_candidates: Vec<RawPrecalCandidate>,
}

/// Flat pre-cal selection per spec §9 fallback chain — pure DB, no pixel
/// I/O. Walks the sub-cal links of this flat set by type preference
/// (DarkFlat → exposure-matched Dark(±0.5s) → Bias), skipping raw
/// (non-master) links with a warning (and recording them as
/// `raw_candidates`). Falls back to the recipe's synthetic bias, then to
/// `PrecalChoice::None` + warning.
///
/// Called by `preview_master_build` (description + raw-candidate surfacing)
/// AND inside the build thread (so a just-built darkflat master — earlier in
/// a batch — is visible at build time, not preview time; the build thread
/// only uses `.choice`, it ignores `raw_candidates`).
fn select_flat_precal(
    conn: &rusqlite::Connection,
    set_id: i64,
    set_exptime: Option<f64>,
    synthetic_bias: Option<f64>,
) -> Result<PrecalSelection, ApiError> {
    let mut warnings = Vec::new();
    let mut raw_candidates = Vec::new();
    // sub-cal links of this flat set, by type preference
    for cal_type in ["DarkFlat", "Dark", "Bias"] {
        let row: Option<(i64, String, i64, Option<f64>, i64)> = conn
            .query_row(
                "SELECT cs.id, cs.imagetyp, cs.is_master_library, cs.exptime, cs.frame_count
             FROM calibration_set_to_frames l
             JOIN calibration_set cs ON cs.id = l.calibration_set_id
             WHERE l.source_id = ?1 AND l.source_type = 'calibration_set'
               AND l.calibration_type = ?2",
                rusqlite::params![set_id, cal_type],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((precal_set, imagetyp, is_master, precal_expt, precal_frame_count)) = row else {
            continue;
        };
        if is_master != 1 {
            warnings.push(format!(
                "linked {cal_type} set #{precal_set} is raw — build its master first (skipped)"
            ));
            raw_candidates.push(RawPrecalCandidate {
                set_id: precal_set,
                cal_type: cal_type.to_string(),
                frame_count: precal_frame_count,
            });
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
            [precal_set],
            |r| r.get(0),
        )?;
        return Ok(PrecalSelection {
            choice: PrecalChoice::Master {
                set_id: precal_set,
                imagetyp,
                cal_type,
                path,
            },
            warnings,
            raw_candidates,
        });
    }
    if let Some(b) = synthetic_bias {
        return Ok(PrecalSelection {
            choice: PrecalChoice::Synthetic(b),
            warnings,
            raw_candidates,
        });
    }
    warnings.push("no pre-calibration master linked and no synthetic bias set — flat combined un-pre-calibrated (vignetting zero level slightly off)".into());
    Ok(PrecalSelection {
        choice: PrecalChoice::None,
        warnings,
        raw_candidates,
    })
}

/// Materializes a `PrecalChoice` into engine-ready pixels. The Master arm
/// reads the ENTIRE precal master into RAM (single file, via the banded
/// reader in one band) — called ONLY from the build thread, never from
/// preview.
fn load_precal_pixels(choice: &PrecalChoice, scratch: &Path) -> Result<FlatPrecal, ApiError> {
    match choice {
        PrecalChoice::Master { path, .. } => {
            // One frame, no `IoPolicy` at this call site — nothing to
            // parallelize.
            let src =
                crate::integration::banded::BandSource::open(&[PathBuf::from(path)], scratch, 1)
                    .map_err(|e| ApiError::Internal(format!("pre-cal master unreadable: {e}")))?;
            let (w, h) = (src.width(), src.height());
            let mut planes = crate::integration::banded::BandPlanes::new(&src);
            src.read_band(0, h, &mut planes, 1)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            let mut data = vec![0f32; w * h];
            planes.decode_frame_into(0, &mut data);
            Ok(FlatPrecal::MasterFrame { data, width: w, height: h })
        }
        PrecalChoice::Synthetic(b) => Ok(FlatPrecal::SyntheticBias(*b as f32)),
        PrecalChoice::None => Ok(FlatPrecal::None),
    }
}

fn recipe_summary_string(recipe: IntegrationRecipe, n: i64) -> String {
    format!("{} n={n}", recipe.describe())
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
    let row: Option<(
        String,
        Option<i64>,
        i64,
        i64,
        Option<f64>,
        Option<String>,
        String,
    )> = conn
        .query_row(
            "SELECT imagetyp, superseded_by_set_id, is_master_library, frame_count,
                exptime, binning, date
         FROM calibration_set WHERE id = ?1",
            [set_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;

    let Some((imagetyp, superseded, is_master, frame_count, exptime, binning, date)) = row else {
        return Err(ApiError::NotFound(format!(
            "calibration set {set_id} not found"
        )));
    };

    validate_buildable_set(superseded, is_master, frame_count)?;

    Ok(SetRow {
        imagetyp,
        exptime,
        binning,
        date,
        frame_count,
    })
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
        return Err(ApiError::NotFound(format!(
            "calibration set {set_id} not found"
        )));
    };

    Ok(SetRow {
        imagetyp,
        exptime,
        binning,
        date,
        frame_count,
    })
}

/// Rebuild precondition (Task 13): every member frame of the source set must
/// still be present on disk — a rebuild re-reads and re-integrates the SAME
/// raw frames, it can't recombine frames that were archived into a ZIP or
/// otherwise removed. Pure DB + `Path::exists` — no `ServiceContext`
/// needed, so it's unit-testable directly (see tests below) the same way
/// `plan_batch` is. Distinguishes "archived" (actionable: restore first)
/// from "missing" (the file is just gone) when cheap to do so via
/// `files.archived_in_operation`.
fn check_rebuild_source_ready(
    conn: &rusqlite::Connection,
    source_set_id: i64,
) -> Result<(), ApiError> {
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
        Err(ApiError::Invalid(
            "originals are archived — restore them first".into(),
        ))
    } else {
        Err(ApiError::Invalid(
            "original source frames are missing on disk".into(),
        ))
    }
}

/// Important-2 fix round: the resolved dir (settings key or legacy scan
/// root) can point at a folder that was since renamed/deleted/unmounted
/// outside the app — the setting itself doesn't track disk state. Without
/// this check that surfaces only as an opaque I/O error partway through
/// `write_fits_f32` (or, worse, a silent `create_dir_all` recreating an
/// empty folder at the old path). Extracted so it's testable with a plain
/// path — no `ServiceContext` needed, unlike `library_dir_or_err` itself.
pub(crate) fn check_library_dir_exists(dir: &str) -> Result<(), ApiError> {
    if !Path::new(dir).is_dir() {
        return Err(ApiError::Invalid(format!(
            "Calibration folder no longer exists on disk: {dir} — reconfigure it in File Manager → Folders"
        )));
    }
    Ok(())
}

/// Effective master-write destination — the `calibration.library_dir`
/// settings key (folder nested inside a monitored directory) or the legacy
/// dedicated `calibration_library` scan root. See
/// `api::scan_roots::get_calibration_library_dir` for the precedence rules.
/// Shared by `preview_master_build`, `run_build`, and `start_master_build`.
///
/// Takes a `&rusqlite::Connection`, NOT `&ServiceContext` — it must reuse the
/// caller's already-checked-out pooled connection rather than checking out a
/// second one via `api::scan_roots::get_calibration_library_dir(ctx)`. That
/// used to be the signature here, and it deadlocked the pool: with N
/// concurrent `preview_master_build` calls (e.g. the batch Create-Masters
/// dialog firing one preview per calibration set via `Promise.allSettled`)
/// exceeding `Database`'s pool `max_size` (8, see `db::mod::Database::new`),
/// every task would hold its own conn from `preview_master_build`'s checkout
/// AND simultaneously block here trying to check out a second one — nobody
/// could progress, everyone timed out after the pool's 30s checkout wait,
/// and `Database::conn()` panicked (`db/mod.rs:124`). Call this ONLY with a
/// connection the caller already owns; never acquire a fresh one inside.
///
/// Every caller now lives in `api::masters` itself (preview, build, rebuild,
/// archive-of-originals), which all resolve the library root through this one
/// precedence + on-disk existence check instead of repeating it. The
/// `pub(crate)` is the wider visibility the removed standalone
/// light-calibration orchestration needed; it is kept because the root is a
/// crate-level concern, not because anything outside reaches for it today.
pub(crate) fn library_dir_or_err(
    conn: &rusqlite::Connection,
) -> Result<std::path::PathBuf, ApiError> {
    let dir = crate::api::scan_roots::resolve_calibration_library_dir(conn)?.ok_or_else(|| {
        ApiError::Invalid(
            "no calibration library folder configured — set one before building masters".into(),
        )
    })?;
    check_library_dir_exists(&dir)?;
    Ok(std::path::PathBuf::from(dir))
}

// ── Preview ───────────────────────────────────────────────────────────────

/// Validation + speculative recipe/precal/target-path resolution, no thread.
///
/// Invariant: acquires exactly ONE pooled connection (`conn`, below) for its
/// entire body. Every helper called from here must take `&rusqlite::Connection`
/// and must NOT touch `ServiceContext`/the pool itself — see `library_dir_or_err`'s
/// doc comment for the deadlock this guards against (many concurrent previews
/// from the batch Create-Masters dialog vs. a bounded connection pool).
pub fn preview_master_build(
    ctx: &ServiceContext,
    set_id: i64,
    recipe: &MasterRecipe,
) -> Result<MasterBuildPreview, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let set = load_and_validate_set(&conn, set_id)?;
    let library_dir = library_dir_or_err(&conn)?;

    let resolved_combine = resolve_recipe(recipe.combine, &set.imagetyp, set.frame_count);
    let is_flat = set.imagetyp == "Flat";
    // Selection only (pure DB) — preview never loads precal pixels.
    let (flat_precal, warnings, raw_precal_sets) = if is_flat {
        let sel = select_flat_precal(&conn, set_id, set.exptime, recipe.synthetic_bias)?;
        let raw_precal_sets = sel
            .raw_candidates
            .into_iter()
            .map(|c| RawPrecalSetDto {
                set_id: c.set_id,
                cal_type: c.cal_type,
                frame_count: c.frame_count,
            })
            .collect();
        (sel.choice.describe(), sel.warnings, raw_precal_sets)
    } else {
        (None, Vec::new(), Vec::new())
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
    let target_path = resolve_collision_free(&library_dir.join(&target_rel), &|p| {
        crate::db::file_exists(&conn, p).unwrap_or(false)
    })
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
        raw_precal_sets,
    })
}

// ── Build-thread error plumbing ──────────────────────────────────────────────

/// Internal-only error type for the build thread: collapses every failure
/// mode into "cancelled" (queue-cancel or mid-integration cancel — both
/// surface identically to the caller) or "other" (a message suitable for
/// the `master-build-complete` event's `error` field).
#[derive(Debug)]
enum BuildStepError {
    Cancelled,
    Other(String),
}

impl From<ApiError> for BuildStepError {
    fn from(e: ApiError) -> Self {
        BuildStepError::Other(e.to_string())
    }
}
impl From<rusqlite::Error> for BuildStepError {
    fn from(e: rusqlite::Error) -> Self {
        BuildStepError::Other(e.to_string())
    }
}
impl From<anyhow::Error> for BuildStepError {
    fn from(e: anyhow::Error) -> Self {
        BuildStepError::Other(format!("{e:#}"))
    }
}
impl From<crate::fits_writer::FitsWriteError> for BuildStepError {
    fn from(e: crate::fits_writer::FitsWriteError) -> Self {
        BuildStepError::Other(e.to_string())
    }
}
impl From<std::io::Error> for BuildStepError {
    fn from(e: std::io::Error) -> Self {
        BuildStepError::Other(e.to_string())
    }
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

/// Build-lifecycle log lines. Named functions rather than inline macros so a
/// test can pin their message text without a multi-gigabyte build fixture —
/// a multi-minute operation that logged nothing is why a running build was
/// indistinguishable from a hung one (spec §8). The message strings are the
/// contract: `docs/logging/README.md` recipes and the log-mcp queries match
/// on them.
fn log_build_started(
    set_id: i64,
    imagetyp: &str,
    count: i64,
    recipe: &str,
    budget_bytes: usize,
    read_threads: usize,
    storage: StorageClass,
) {
    tracing::info!(
        set_id,
        imagetyp,
        count,
        recipe,
        budget_mb = budget_bytes / (1024 * 1024),
        // Read concurrency is `image_pool`'s size, i.e. derived from core
        // count — a CPU number applied to an I/O problem (spec §8). Logged
        // alongside `storage` so the pending SSD/NAS measurement can be read
        // against the policy that actually produced it, instead of guessing.
        read_threads,
        storage = storage.as_str(),
        "master build started"
    );
}

fn log_build_finished(set_id: i64, duration: std::time::Duration, bytes_read: u64, out: &IntegrationOutput) {
    let read_s = out.read_duration.as_secs_f64();
    tracing::info!(
        set_id,
        duration_ms = duration.as_millis() as u64,
        read_ms = out.read_duration.as_millis() as u64,
        combine_ms = out.combine_duration.as_millis() as u64,
        read_mb_s = if read_s > 0.0 { (bytes_read as f64 / read_s / 1e6).round() as u64 } else { 0 },
        band_rows = out.band_rows,
        bands = out.bands,
        "master build finished"
    );
}

/// Owns the zero-byte placeholder [`claim_collision_free`] mints for a `New`
/// build's output name (audit I7), from the moment the name is claimed until
/// the real bytes replace it. Every exit in that window releases the claim —
/// the `?` on `write_fits_f32` AND an unwinding panic inside it, which no
/// explicit `release_claim` call could cover (the panic is only caught one
/// frame up, in `run_master_build_thread`, where the path no longer exists).
/// A stranded placeholder is not cosmetic: it sits on disk under the master's
/// name, so it would shift every future build of that set to `_2`, `_3`… and
/// present an unparseable 0-byte "master" to the next library scan.
///
/// [`ClaimGuard::disarm`] is called the instant the write succeeds, so from
/// there on nothing can remove the file — a strictly stronger form of the
/// "never delete a fully built master" stance (audit F4b) that
/// `release_claim`'s empty-only rule already guarantees on its own.
struct ClaimGuard(Option<PathBuf>);

impl ClaimGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            release_claim(&p);
        }
    }
}

/// The whole build: acquire queue slot -> load member paths/set row ->
/// resolve combine/precal -> integrate -> write -> register (or, for a
/// rebuild, update-in-place — see `BuildTarget`). Every early exit is a
/// plain `?`/`Err` return — `run_master_build_thread` (the only caller)
/// always removes the handle and always emits `master-build-complete`
/// afterward, so there's no cleanup duty here beyond the filename claim,
/// which `ClaimGuard` releases by RAII on every exit (`?`, `Err`, panic).
///
/// On success returns `(master_set_id, build_warning)` — the warning is the
/// non-fatal "the data had undefined pixels" note (audit C2) that the single
/// exit path puts on `master-build-complete`.
#[allow(clippy::too_many_arguments)]
fn run_build(
    ctx: &ServiceContext,
    emitter: &dyn ProgressEmitter,
    app_version: &str,
    set_id: i64,
    recipe: &MasterRecipe,
    cancel_flag: &Arc<AtomicBool>,
    target: BuildTarget,
) -> Result<(i64, Option<String>), BuildStepError> {
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
    let resolved_combine = resolve_recipe(recipe.combine, &set.imagetyp, set.frame_count);
    // The build thread only cares about the resolved choice — `raw_candidates`
    // is a preview-only affordance (the "build its master first" checkboxes);
    // by build time any raw dependency the operator wanted addressed was
    // already submitted earlier in the same batch (see `plan_batch`/dependency
    // ordering), so there's nothing actionable left to do with it here.
    let precal_choice = if is_flat {
        select_flat_precal(&conn, set_id, set.exptime, recipe.synthetic_bias)?.choice
    } else {
        PrecalChoice::None
    };
    let precal_desc = precal_choice.describe();

    // Both I/O knobs, resolved from the machine AND from the storage the
    // frames actually live on — the same set may sit on a local disk today
    // and a NAS tomorrow.
    let io = crate::integration::io_policy::resolve(&conn, &ctx.settings, &paths, ctx.image_pool.current_num_threads())?;

    // A multi-minute build that logs nothing is indistinguishable from a
    // hung one (spec §8) — this is the one line an operator has to go on
    // until the first progress event arrives.
    let build_started_at = std::time::Instant::now();
    log_build_started(
        set_id,
        &set.imagetyp,
        set.frame_count,
        &resolved_combine.describe(),
        io.band_budget_bytes,
        io.read_concurrency,
        io.storage,
    );

    // Release the pooled connection before the (potentially long) pixel
    // work below — precal load + banded integration do no DB work, and the
    // pool only has a handful of slots shared with every other in-flight
    // request.
    drop(conn);

    let pool = ctx.image_pool.as_ref();
    let scratch = std::env::temp_dir();
    let on_band = |current: usize, total: usize, bytes_read_so_far: u64, bytes_total: u64| {
        let percent = if total > 0 {
            (current as f64 / total as f64) * 100.0
        } else {
            100.0
        };
        emit_event(
            emitter,
            "master-build-progress",
            &MasterBuildProgressEvent {
                set_id,
                stage: "integrating",
                current,
                total,
                percent,
                bytes_done: bytes_read_so_far,
                bytes_total,
            },
        );
    };
    let progress = EngineProgress { on_band: &on_band };

    let out = if is_flat {
        // Pixel materialization of the selected precal happens HERE, on the
        // build thread — the only place `load_precal_pixels` is called.
        let precal = load_precal_pixels(&precal_choice, &scratch)?;
        integrate_flat(
            &paths,
            &precal,
            resolved_combine,
            pool,
            &scratch,
            cancel_flag.as_ref(),
            progress,
            io,
        )?
    } else {
        integrate_bias_like(
            &paths,
            resolved_combine,
            pool,
            &scratch,
            cancel_flag.as_ref(),
            progress,
            io,
        )?
    };
    log_build_finished(set_id, build_started_at.elapsed(), out.bytes_read, &out);

    // Audit C2: the engine drops non-finite samples ("undefined pixels") from
    // the per-pixel stack instead of panicking or baking NaN into the master.
    // That is never a build failure — but it IS data loss the operator must
    // hear about, per frame in the log and once in the completion event.
    // `bad_samples_per_frame` is indexed by `paths` order (the same slice the
    // engine was handed), so index i names the file at paths[i].
    let bad_total: usize = out.bad_samples_per_frame.iter().sum();
    let build_warning = if bad_total > 0 || out.all_bad_pixels > 0 {
        for (i, &count) in out.bad_samples_per_frame.iter().enumerate() {
            if count > 0 {
                tracing::warn!(
                    set_id, path = %paths[i].display(), count,
                    "non-finite samples excluded from integration"
                );
            }
        }
        if out.all_bad_pixels > 0 {
            tracing::warn!(
                set_id,
                count = out.all_bad_pixels,
                "pixels with no valid sample written as 0"
            );
        }
        let frames_touched = out.bad_samples_per_frame.iter().filter(|&&c| c > 0).count();
        Some(format!(
            "{bad_total} undefined sample(s) excluded across {frames_touched} frame(s){}",
            if out.all_bad_pixels > 0 {
                format!("; {} pixel(s) had no valid data", out.all_bad_pixels)
            } else {
                String::new()
            }
        ))
    } else {
        None
    };

    emit_event(
        emitter,
        "master-build-progress",
        &MasterBuildProgressEvent {
            set_id,
            stage: "writing",
            current: 0,
            total: 0,
            percent: 0.0,
            // Reading is over by this stage — report it fully done rather
            // than reset to 0/0, which would read as a regression in a UI
            // that renders bytes_done/bytes_total as a fraction.
            bytes_done: out.bytes_read,
            bytes_total: out.bytes_read,
        },
    );

    let db_handle = db(ctx)?;
    let conn = db_handle.conn();

    let inputs = load_header_inputs(&conn, set_id)?;
    let (member_hash_str, _uuids) = member_hash(&conn, set_id)?;
    let recipe_summary = recipe_summary_string(resolved_combine, set.frame_count);
    // Per-channel flat constants (ATH_FNR/G/B) describe the same measurement
    // ATH_FNRM does, so they are a flat-only affair — and a CFA-only one: a
    // mono set declares no pattern, so this returns None and its master is
    // stamped exactly as before. Measured off `out.data`, the very plane
    // written a few lines down and the one ATH_FNRM came from, so all four
    // constants share their units. The engine's flux equalization is a global
    // per-frame scale, so it cannot have moved the channels relative to one
    // another.
    let channel_norms = if is_flat {
        measure_flat_channel_norms(&inputs, &out.data, out.width, out.height)
    } else {
        None
    };
    let cards = build_master_cards(
        &inputs,
        app_version,
        &recipe_summary,
        &member_hash_str,
        out.flat_norm,
        channel_norms,
    )?;

    let (target_abs, mut claim) = match &target {
        BuildTarget::New => {
            let library_dir = library_dir_or_err(&conn)?;
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
            let desired = library_dir.join(&target_rel);
            // create_dir_all runs BEFORE name resolution, not after: claiming
            // a name means CREATING the placeholder file, which needs its
            // directory to exist.
            if let Some(parent) = desired.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Claim, don't just look: two builds running concurrently
            // (compute.max_concurrent > 1) used to resolve to the same free
            // name and the loser's rename silently replaced the winner's
            // master (audit I7). The claim is held by `ClaimGuard` until the
            // write lands.
            let claimed = claim_collision_free(&desired, &|p| {
                crate::db::file_exists(&conn, p).unwrap_or(false)
            })?;
            (claimed.clone(), ClaimGuard(Some(claimed)))
        }
        // Same path the master already lives at — no collision resolution and
        // nothing to claim (the name was claimed by the build that created
        // it): `write_fits_f32` replaces the existing file atomically.
        BuildTarget::Rebuild { target_path, .. } => {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            (target_path.clone(), ClaimGuard(None))
        }
    };
    write_fits_f32(&target_abs, out.width, out.height, 1, &out.data, &cards)?;
    // The master's real bytes are on disk now (atomic rename inside
    // `write_fits_f32` replaced the placeholder), so the name belongs to the
    // file, not to the claim: nothing below this line may delete it.
    claim.disarm();

    emit_event(
        emitter,
        "master-build-progress",
        &MasterBuildProgressEvent {
            set_id,
            stage: "registering",
            current: 0,
            total: 0,
            percent: 0.0,
            bytes_done: out.bytes_read,
            bytes_total: out.bytes_read,
        },
    );

    let recipe_json = serde_json::json!({
        "combine": resolved_combine,
        "syntheticBias": recipe.synthetic_bias,
        "precal": precal_desc,
        "rejectedFraction": out.rejected_fraction,
        "engine": "athenaeum",
        "version": app_version,
    })
    .to_string();

    match target {
        BuildTarget::New => match register_master(&conn, set_id, &target_abs, &recipe_json) {
            Ok(reg) => Ok((reg.master_set_id, build_warning)),
            Err(e) => {
                // Keep the written master on disk: deleting it on a catalog
                // conflict re-created the exact divergence that caused the
                // failure (permanent build loop, audit F4b). The next library
                // scan ingests it as an imported master instead — visible, not
                // lost. The path in the error makes user reports actionable.
                // The filename claim was disarmed right after the write, so
                // nothing on this path can remove the file either.
                Err(BuildStepError::Other(format!(
                    "register master at {}: {e:#}",
                    target_abs.display()
                )))
            }
        },
        BuildTarget::Rebuild {
            master_set_id,
            master_file_id,
            ..
        } => {
            if let Err(e) = finalize_rebuild(
                &conn,
                master_set_id,
                master_file_id,
                &target_abs,
                &recipe_json,
                &member_hash_str,
            ) {
                // METADATA-DRIFT WINDOW: the on-disk master file HAS already
                // been replaced (atomic rename inside `write_fits_f32`), but
                // the catalog metadata was NOT refreshed — the
                // master_provenance row (recipe/hash/created_at), the files
                // row (size/modified_at) and the frames row + stored header
                // still describe the PREVIOUS build, and the
                // `master-build-complete` event will report failure. The
                // drift is metadata-only (pixels on disk are the new, valid
                // rebuild) and self-heals on the next successful rebuild of
                // the same master — or on the next library scan, which sees
                // the size/mtime drift and re-parses the file in place.
                // Logged loudly here — this is the one state where DB and
                // disk disagree.
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
                     the file now holds the rebuilt pixels but master_provenance/files/frames still \
                     describe the previous build; a subsequent successful rebuild self-heals this drift"
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
            Ok((master_set_id, build_warning))
        }
    }
}

/// Everything a rebuild does AFTER `write_fits_f32`'s atomic rename, in ONE
/// function and ONE transaction so every possible failure in that window (tx
/// open, provenance update, re-parse, commit) funnels through the single
/// metadata-drift error log at the call site, and so no half-refreshed catalog
/// state can be observed.
///
/// A rebuild rewrites the master's HEADER as well as its pixels — the cards
/// are rebuilt from the current member consensus, so BAYERPAT/XBAYROFF/
/// YBAYROFF/ROWORDER/ATH_FNRM can all differ from what the original
/// registration parsed. Refreshing only `files.size`/`modified_at` therefore
/// used to leave the `frames` row and the `fits_header` snapshot describing
/// the PREVIOUS build's header; light-calibration copy-through reads that
/// stored header, so the drift was user-visible in the calibrated output's
/// CFA cards. `scanner::resync_catalog_rows_from_disk` re-parses the file that
/// was just written and UPDATEs files/frames/fits_header in place, preserving
/// `files.id` and `frames.id` — the master's calibration_set membership and
/// every consumer link in `calibration_set_to_frames` are keyed on those ids
/// and stay untouched.
fn finalize_rebuild(
    conn: &rusqlite::Connection,
    master_set_id: i64,
    master_file_id: i64,
    target_abs: &Path,
    recipe_json: &str,
    member_hash_str: &str,
) -> Result<(), BuildStepError> {
    let tx = conn.unchecked_transaction()?;
    crate::db::master_provenance::update_rebuild(&tx, master_set_id, recipe_json, member_hash_str)?;
    crate::scanner::resync_catalog_rows_from_disk(&tx, master_file_id, target_abs)?;
    tx.commit()?;
    // After the commit: the refresh is only a fact once it is durable, and
    // this is the counterpart of the drift `error!` at the call site — the
    // pair tells the whole story of the post-write window at debug level.
    tracing::debug!(
        master_set_id,
        master_file_id,
        path = %target_abs.display(),
        "rebuild resynced catalog rows from disk"
    );
    Ok(())
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
    // Captured before `run_build` consumes `target` — only a `New` build
    // just called `register_master`, which is the "just superseded" moment
    // `archive_after` chains from. A `Rebuild` never supersedes anything (its
    // source set was already superseded by a prior build), so it never
    // chains regardless of `recipe.archive_after`.
    let was_new_build = matches!(target, BuildTarget::New);

    // Wrap the fallible body so a panic inside `run_build` can never unwind past
    // the handle removal / `master-build-complete` emission below, which would
    // leak the `active_master_builds` handle and leave the UI showing a build
    // that is no longer running. A caught panic is folded into
    // `BuildStepError::Other` so the SINGLE exit path (handle removal +
    // error-outcome event) handles it identically to any other build failure.
    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_build(
            &ctx,
            emitter.as_ref(),
            &app_version,
            set_id,
            &recipe,
            &cancel_flag,
            target,
        )
    })) {
        Ok(r) => r,
        Err(panic) => {
            let detail = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".to_string());
            let msg = format!("master build panicked: {detail}");
            tracing::error!(set_id, error = %msg, "master build thread panicked");
            Err(BuildStepError::Other(msg))
        }
    };

    // Task 14: archive_after chaining. Deliberately happens BEFORE the handle
    // removal / master-build-complete emission below, but its outcome is
    // NEVER folded into `result` — archiving is a follow-up to a successful
    // build, not part of the build itself, so a failure here must never turn
    // a successful master build into a reported failure. Log-and-continue.
    if was_new_build && recipe.archive_after {
        if result.is_ok() {
            // Same catch_unwind discipline as run_build above: a panic in the
            // archive chain must never skip handle removal / completion emission.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                archive_originals(ctx.clone(), emitter.clone(), set_id)
            })) {
                Ok(Ok(archive_op_id)) => {
                    tracing::info!(
                        set_id, archive_op_id,
                        "archive_after: queued archive-of-originals for the just-superseded source set"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        set_id, error = %e,
                        "archive_after: failed to queue archive-of-originals for the just-superseded \
                         source set — the master build itself still succeeded; originals were left in place"
                    );
                }
                Err(panic) => {
                    let detail = panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown".to_string());
                    tracing::error!(
                        set_id, error = %detail,
                        "archive_after: PANICKED — build still reported as succeeded; originals left in place"
                    );
                }
            }
        }
    }

    ctx.active_master_builds.lock().unwrap().remove(&set_id);

    let (master_set_id, success, cancelled, error, warning) = match result {
        Ok((id, warning)) => (Some(id), true, false, None, warning),
        Err(BuildStepError::Cancelled) => (None, false, true, None, None),
        Err(BuildStepError::Other(msg)) => {
            tracing::error!(set_id, error = %msg, "master build failed");
            (None, false, false, Some(msg), None)
        }
    };

    emit_event(
        emitter.as_ref(),
        "master-build-complete",
        &MasterBuildCompleteEvent {
            set_id,
            master_set_id,
            success,
            cancelled,
            error,
            warning,
        },
    );
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
        library_dir_or_err(&conn)?;
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut active = ctx.active_master_builds.lock().unwrap();
        if active.contains_key(&set_id) {
            return Err(ApiError::Conflict(format!(
                "a master build is already in progress for calibration set {set_id}"
            )));
        }
        active.insert(
            set_id,
            MasterBuildHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    let thread_ctx = ctx.clone();
    let spawn_result = std::thread::Builder::new()
        .name(format!("master-build-{set_id}"))
        .spawn(move || {
            run_master_build_thread(
                thread_ctx,
                emitter,
                app_version,
                set_id,
                recipe,
                cancel_flag,
                BuildTarget::New,
            );
        });

    if let Err(e) = spawn_result {
        // The thread never started, so nothing will ever remove this handle
        // or emit master-build-complete — clean up right here instead.
        ctx.active_master_builds.lock().unwrap().remove(&set_id);
        return Err(ApiError::Internal(format!(
            "failed to spawn master-build thread: {e}"
        )));
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
        let row: Option<(String, i64, Option<i64>, i64)> = conn
            .query_row(
                "SELECT imagetyp, frame_count, superseded_by_set_id, is_master_library
             FROM calibration_set WHERE id = ?1",
                [set_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        let Some((imagetyp, frame_count, superseded_by_set_id, is_master_library)) = row else {
            skipped.push(BatchSkip {
                set_id,
                reason: "unknown set".into(),
            });
            continue;
        };

        if superseded_by_set_id.is_some() {
            skipped.push(BatchSkip {
                set_id,
                reason: "already has a master".into(),
            });
            continue;
        }
        if is_master_library != 0 {
            skipped.push(BatchSkip {
                set_id,
                reason: "is itself a master".into(),
            });
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
        match start_master_build(
            ctx.clone(),
            emitter.clone(),
            app_version.clone(),
            set_id,
            recipe.clone(),
        ) {
            Ok(()) => started_set_ids.push(set_id),
            Err(e) => skipped.push(BatchSkip {
                set_id,
                reason: e.to_string(),
            }),
        }
    }

    Ok(BatchBuildReport {
        started_set_ids,
        skipped,
    })
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
/// `Option<IntegrationRecipe>` input), so treating it as a future override would
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

        let prov = crate::db::master_provenance::get(&conn, master_set_id)?.ok_or_else(|| {
            ApiError::Invalid(
                "master was not built by Athenaeum — no provenance recorded, cannot rebuild".into(),
            )
        })?;
        let source_set_id = prov.source_set_id.ok_or_else(|| {
            ApiError::Invalid(
                "master was not built by Athenaeum — no source set recorded, cannot rebuild".into(),
            )
        })?;

        check_rebuild_source_ready(&conn, source_set_id)?;

        // LIMIT 1: a master is a 1:1 calibration_set by invariant (exactly
        // one member frame), so this never actually needs to disambiguate —
        // matches `select_flat_precal`'s defensive `LIMIT 1` on the same
        // shape of query.
        let master_file: Option<(i64, String)> = conn
            .query_row(
                "SELECT fi.id, fi.path FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1 LIMIT 1",
                [master_set_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
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
        active.insert(
            source_set_id,
            MasterBuildHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    let recipe = MasterRecipe {
        combine: None,
        synthetic_bias: None,
        archive_after: false,
    };
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
                thread_ctx,
                emitter,
                app_version,
                source_set_id,
                recipe,
                cancel_flag,
                target,
            );
        });

    if let Err(e) = spawn_result {
        ctx.active_master_builds
            .lock()
            .unwrap()
            .remove(&source_set_id);
        return Err(ApiError::Internal(format!(
            "failed to spawn master-build thread: {e}"
        )));
    }

    Ok(())
}

// ── Archive-of-originals (Task 14) ───────────────────────────────────────────

/// Plan + commit + enqueue archiving of a SUPERSEDED calibration set's
/// original member frames into a single ZIP, on the shared disk worker
/// (`ctx.operation_queue` — the same serialized queue frame-set archiving and
/// file-ops share, so nothing competes for disk bandwidth). Mirrors
/// `commands/archive.rs::start_archive_operation`'s shape (plan+commit
/// synchronously, register the cancel handle in `ctx.active_archives`, hand
/// the real work to the queue with a closure that runs → updates status →
/// rolls back on error → emits `archive-finished` → cleans up the handle) but
/// lives in core so both transports AND the `archive_after` build-chain in
/// `run_master_build_thread` below can call it without a Tauri/Axum
/// dependency.
///
/// No `archive_root_path` parameter (unlike the frame-set flow's UI-driven
/// override) — this is an automatic follow-up to a build, so it always uses
/// the configured default archive root (`archive::resolve_archive_root` with
/// `requested: None`) and the user's configured compression mode. Conflict
/// resolution is always `AddSuffix`: a follow-up archive is not the moment to
/// silently clobber an existing zip that happens to share the predicted name.
///
/// Returns the new `archive_operations.id` as soon as it's queued — this does
/// NOT wait for the zip to actually be built. Progress/completion arrive via
/// the existing `archive-progress` / `archive-finished` events, unchanged —
/// the frame-set archive UI already listens for these; a calibration-set op
/// is just another row flowing through the same events.
pub fn archive_originals(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    calibration_set_id: i64,
) -> Result<i64, ApiError> {
    use crate::archive::models::{ArchiveCompression, ArchiveStatus, ConflictResolution};
    use crate::archive::{db as adb, executor, planner, rollback};
    use crate::services::operation_queue::{OperationKind, QueuedJob};
    use crate::services::ArchiveHandle;

    let op_id = {
        let db_handle = db(&ctx)?;
        let conn = db_handle.conn();

        let root = crate::archive::resolve_archive_root(&conn, &ctx.settings, None)?;
        let compression = ctx
            .settings
            .get_archive_compression(&conn)
            .ok()
            .and_then(|s| ArchiveCompression::from_str(&s))
            .unwrap_or(ArchiveCompression::Store);

        let plan = planner::build_calibration_set_plan(
            &conn,
            calibration_set_id,
            Path::new(&root),
            compression,
        )?;
        planner::commit_plan(&conn, &plan, ConflictResolution::AddSuffix)?
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(
            op_id,
            ArchiveHandle {
                operation_id: op_id,
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    let ctx_for_worker = ctx.clone();
    let emitter_for_worker = emitter.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let Some(db_handle) = ctx_for_worker.db.get() else {
                tracing::error!(operation_id = op_id, "db not initialized for calibration archive worker");
                ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
                return;
            };
            let conn = db_handle.conn();
            let result = executor::run_operation(&conn, op_id, &cancel_flag, emitter_for_worker.as_ref());
            let outcome = match result {
                Ok(()) => {
                    tracing::info!(operation_id = op_id, "calibration archive operation completed");
                    "completed"
                }
                Err(e) => {
                    let outcome = if executor::was_cancelled(&e) {
                        let _ = adb::update_operation_status(&conn, op_id, ArchiveStatus::Cancelled, None);
                        "cancelled"
                    } else {
                        tracing::error!(operation_id = op_id, error = ?e, "calibration archive operation failed");
                        let msg = format!("{:#}", e);
                        let _ = adb::update_operation_status(&conn, op_id, ArchiveStatus::Failed, Some(&msg));
                        "failed"
                    };
                    if let Err(rb_err) = rollback::rollback_operation(&conn, op_id, emitter_for_worker.as_ref()) {
                        tracing::error!(
                            operation_id = op_id, error = ?rb_err,
                            "rollback after failed calibration archive operation also failed, \
                             operation may be left in an inconsistent state"
                        );
                    }
                    outcome
                }
            };
            emit_event(
                emitter_for_worker.as_ref(),
                "archive-finished",
                &serde_json::json!({ "operation_id": op_id, "outcome": outcome }),
            );
            ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
        }),
    });

    Ok(op_id)
}

/// Cancel an active master build (queued-in-compute-queue or running).
pub fn cancel_master_build(ctx: &ServiceContext, set_id: i64) -> Result<(), ApiError> {
    let active = ctx.active_master_builds.lock().unwrap();
    if let Some(handle) = active.get(&set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err(ApiError::NotFound(format!(
            "no active master build for calibration set {set_id}"
        )))
    }
}

// ── Delete a master (audit C3) ───────────────────────────────────────────────

/// Delete a built or imported master: un-supersede its raw source set and
/// repoint every consumer back onto it (Task 7's `unregister_master_set`),
/// drop the master's own catalog rows, and remove its file from disk.
///
/// The audit's C3: without this there is no way back from a bad master — the
/// raw set stays superseded (invisible to the matcher) forever, so its whole
/// lineage is locked.
///
/// Known, intended side effect: calibrated lights already exported with this
/// master keep the deleted master's uuid/path in their `ATH_C*` cards. Nothing
/// tracks them (calibrated-export v2 removed the tracking table), so they are
/// simply stale files on disk until the next export regenerates them.
pub fn delete_master(
    ctx: &ServiceContext,
    master_set_id: i64,
) -> Result<DeleteMasterResult, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    // Refuse up front on the two user-level errors, so the web boundary can
    // answer 404/400 instead of turning them into a 500 via the catch-all
    // mapping of `unregister_master_set`'s anyhow error below.
    let is_master: i64 = conn
        .query_row(
            "SELECT COALESCE(is_master_library, 0) FROM calibration_set WHERE id = ?1",
            rusqlite::params![master_set_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| ApiError::NotFound(format!("calibration set {master_set_id} not found")))?;
    if is_master == 0 {
        return Err(ApiError::Invalid(format!(
            "calibration set {master_set_id} is not a master — only masters can be deleted this way"
        )));
    }

    // Duplicate-work guard. `active_master_builds` is keyed by the SOURCE set
    // id, never by the master's: a build that is about to PRODUCE this master
    // and a `rebuild_master` OF it both register under the raw set (see
    // `rebuild_master`'s comment), so checking `master_set_id` alone would
    // miss precisely the case this guard exists for — a rebuild finishing
    // after the catalog rows are gone, leaving an untracked file in the
    // library root. A TOCTOU window remains (a build could start between this
    // check and the transaction below); it is the same accepted shape as
    // `api::scan_roots::check_library_root_uniqueness`, and its worst outcome
    // is that untracked file, not catalog corruption.
    let mut busy_keys = vec![master_set_id];
    if let Some(src) = conn
        .query_row(
            "SELECT source_set_id FROM master_provenance WHERE master_set_id = ?1",
            rusqlite::params![master_set_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
    {
        busy_keys.push(src);
    }
    if let Some(raw) = conn
        .query_row(
            "SELECT id FROM calibration_set WHERE superseded_by_set_id = ?1 ORDER BY id LIMIT 1",
            rusqlite::params![master_set_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        busy_keys.push(raw);
    }
    {
        let active = ctx.active_master_builds.lock().unwrap();
        if let Some(busy) = busy_keys.iter().find(|k| active.contains_key(k)) {
            return Err(ApiError::Conflict(format!(
                "a master build is in progress for calibration set {busy} — cancel it first"
            )));
        }
    }

    let tx = conn.unchecked_transaction()?;

    let summary =
        crate::db::master_unregister::unregister_master_set(&tx, master_set_id).map_err(|e| {
            tracing::error!(master_set_id, error = %e, "master unregister failed");
            ApiError::Internal(format!("{e:#}"))
        })?;

    // Resolve paths, then drop the file rows (frames / fits_header / the rest
    // cascade off `files`) inside the same transaction — so a failure here
    // rolls the un-supersede back too rather than half-deleting a master.
    let mut paths: Vec<String> = Vec::new();
    for fid in &summary.file_ids {
        match tx
            .query_row(
                "SELECT path FROM files WHERE id = ?1",
                rusqlite::params![fid],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            Some(p) => paths.push(p),
            // A master set whose member frame has no `files` row is a catalog
            // inconsistency, not a reason to abort the delete — say so and
            // carry on removing the rows.
            None => tracing::warn!(
                master_set_id,
                file_id = fid,
                "master member has no files row"
            ),
        }
        tx.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![fid])?;
    }
    tx.commit()?;

    // Disk last, deliberately: the catalog is the source of truth, so the
    // benign failure mode is an orphan file on disk (logged, visible), not a
    // catalog row pointing at a file that is already gone.
    let mut files_deleted = 0usize;
    for p in &paths {
        match std::fs::remove_file(p) {
            Ok(()) => files_deleted += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(path = %p, "master file already absent on disk");
            }
            Err(e) => tracing::error!(
                path = %p,
                error = %e,
                "master file delete failed, catalog rows already removed"
            ),
        }
    }

    tracing::info!(
        master_set_id,
        restored_raw_set_id = ?summary.restored_raw_set_id,
        links_repointed = summary.links_repointed,
        files_deleted,
        "master deleted, lineage restored"
    );
    Ok(DeleteMasterResult {
        master_set_id,
        restored_raw_set_id: summary.restored_raw_set_id,
        links_repointed: summary.links_repointed,
        files_deleted,
    })
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

    let (
        originals_archived,
        source_frames_on_disk,
        archive_operation_id,
        archive_zip_path,
        archive_zip_missing,
    ) = if let Some(src_id) = prov.source_set_id {
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
        let source_frames_on_disk =
            !paths.is_empty() && paths.iter().all(|p| Path::new(p).exists());
        drop(stmt);

        // Any ONE member's markers — see the field doc comments above for
        // why every member shares the same op id / zip path.
        let marker: Option<(Option<i64>, Option<String>)> = conn
            .query_row(
                "SELECT fi.archived_in_operation, fi.archive_zip_path
                 FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id
                 JOIN files fi ON fi.id = f.file_id
                 WHERE csf.set_id = ?1 AND fi.archived_in_operation IS NOT NULL
                 LIMIT 1",
                [src_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (archive_operation_id, archive_zip_path) = marker.unwrap_or((None, None));
        let archive_zip_missing = archive_zip_path
            .as_deref()
            .map(|p| !Path::new(p).exists())
            .unwrap_or(false);

        (
            archived_count > 0,
            source_frames_on_disk,
            archive_operation_id,
            archive_zip_path,
            archive_zip_missing,
        )
    } else {
        (false, false, None, None, false)
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
        archive_operation_id,
        archive_zip_path,
        archive_zip_missing,
    }))
}

/// Common ancestor directory of a set of absolute file paths. Used by
/// [`restore_originals`] to synthesize a `target_root_path` for
/// `archive::restore::run_restore` that is GUARANTEED to be a string prefix
/// of every member's `source_path` — which is exactly what
/// `restore::classify_target` checks to pick "restore to original location"
/// mode. Any common ancestor works for that purpose (the value itself is
/// never used as an actual restore destination in Original mode — see
/// `RestoreTargetMode::Original`), so this doesn't need to be the TIGHTEST
/// possible ancestor, just A valid one; comparing path components pairwise
/// happens to produce the tightest one for free. Deliberately NOT a hardcoded
/// `/` (or `C:\`): that only works on Unix-style absolute paths, and this
/// needs to work identically on Windows.
fn common_source_dir(paths: &[PathBuf]) -> PathBuf {
    match paths.split_first() {
        None => PathBuf::new(),
        Some((first, rest)) if rest.is_empty() => first
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| first.clone()),
        Some((first, rest)) => {
            let first_components: Vec<_> = first.components().collect();
            let mut common_len = first_components.len();
            for p in rest {
                let matched = first_components
                    .iter()
                    .zip(p.components())
                    .take_while(|(a, b)| **a == *b)
                    .count();
                common_len = common_len.min(matched);
            }
            first_components[..common_len].iter().collect()
        }
    }
}

/// Restore a SUPERSEDED calibration set's archived originals from their zip
/// (the reverse of `archive_originals`, sharing its enqueue-on-shared-worker
/// shape). Resolves the archive operation id + zip path from the source
/// set's member files (same query `get_master_provenance` uses to populate
/// `archive_operation_id`/`archive_zip_path` — see those field doc comments
/// for why "any member" is enough), validates the zip still exists on disk
/// (returns an actionable `ApiError::Invalid` — never an io panic — if it
/// doesn't; the UI's `archive_zip_missing` flag is meant to keep the operator
/// from ever reaching this path via the button, but the API itself must
/// degrade gracefully if called directly), then enqueues
/// `archive::restore::run_restore` on the same shared disk-op worker
/// `archive_originals` uses.
///
/// Restores to each file's own recorded `source_path` (Original mode — see
/// `common_source_dir`) with `overwrite_existing: false,
/// keep_zip_after_restore: true`: never clobber something already sitting at
/// the original path, and don't delete the only other known-good copy (the
/// zip) until the operator has separately confirmed the restore stuck. The
/// frame-set restore dialog exposes both of these as user choices; this
/// one-click calibration flow doesn't ask, so it defaults to the safer
/// option on both axes.
///
/// Reuses `archive::restore::run_restore` — the exact engine
/// `start_restore_operation` (Tauri) / its web twin drive for frame-set
/// restores, already proven subject-agnostic end-to-end by the Task 14
/// `calibration_archive_executor_and_restore_round_trip` test — rather than
/// duplicating any extract/reconcile/catalog-update logic here.
pub fn restore_originals(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    calibration_set_id: i64,
) -> Result<i64, ApiError> {
    use crate::archive::restore;
    use crate::services::operation_queue::{OperationKind, QueuedJob};
    use crate::services::ArchiveHandle;

    {
        let map = ctx.active_archives.lock().unwrap();
        if !map.is_empty() {
            return Err(ApiError::Conflict(
                "another archive/restore operation is already in progress".into(),
            ));
        }
    }

    let (op_id, target_root) = {
        let db_handle = db(&ctx)?;
        let conn = db_handle.conn();

        let mut stmt = conn.prepare(
            "SELECT fi.path, fi.archived_in_operation, fi.archive_zip_path
             FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1",
        )?;
        let rows: Vec<(String, Option<i64>, Option<String>)> = stmt
            .query_map([calibration_set_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        if rows.is_empty() {
            return Err(ApiError::Invalid(
                "calibration set has no member frames on record".into(),
            ));
        }

        let marker = rows.iter().find_map(|(_, op, zip)| match (op, zip) {
            (Some(op), Some(zip)) => Some((*op, zip.clone())),
            _ => None,
        });
        let Some((op_id, zip_path)) = marker else {
            return Err(ApiError::Invalid(
                "calibration set's originals are not archived — nothing to restore".into(),
            ));
        };

        if !Path::new(&zip_path).exists() {
            return Err(ApiError::Invalid(format!(
                "Archive zip is missing on disk: {zip_path} — it may have been deleted; \
                 you can use 'Forget archive' in the master's provenance panel to clear \
                 the stale markers"
            )));
        }

        let paths: Vec<PathBuf> = rows.into_iter().map(|(p, _, _)| PathBuf::from(p)).collect();
        let target_root = common_source_dir(&paths);

        (op_id, target_root)
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut map = ctx.active_archives.lock().unwrap();
        map.insert(
            op_id,
            ArchiveHandle {
                operation_id: op_id,
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    let ctx_for_worker = ctx.clone();
    let emitter_for_worker = emitter.clone();
    ctx.operation_queue.enqueue(QueuedJob {
        kind: OperationKind::ZipArchive,
        operation_id: op_id,
        run: Box::new(move || {
            let Some(db_handle) = ctx_for_worker.db.get() else {
                tracing::error!(operation_id = op_id, "db not initialized for calibration restore worker");
                ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
                return;
            };
            let conn = db_handle.conn();
            let (outcome, conflicts) = match restore::run_restore(
                &conn, op_id, &target_root, false, true, &cancel_flag, emitter_for_worker.as_ref(),
            ) {
                Ok(result) if result.has_conflicts() => {
                    tracing::warn!(
                        operation_id = op_id,
                        conflicts = result.conflicts.len(),
                        "calibration originals restore completed with conflicts"
                    );
                    ("completed_with_conflicts", result.conflicts.len())
                }
                Ok(_) => ("completed", 0),
                Err(e) => {
                    tracing::error!(operation_id = op_id, error = ?e, "calibration originals restore failed");
                    let outcome = if format!("{:#}", e).contains("cancelled") { "cancelled" } else { "failed" };
                    (outcome, 0)
                }
            };
            emit_event(
                emitter_for_worker.as_ref(),
                "archive-finished",
                &serde_json::json!({
                    "operation_id": op_id,
                    "outcome": outcome,
                    "kind": "restore",
                    "conflicts": conflicts,
                }),
            );
            ctx_for_worker.active_archives.lock().unwrap().remove(&op_id);
        }),
    });

    Ok(op_id)
}

/// Escape hatch for the stuck missing-zip state: clear the archive markers
/// (`archived_in_operation` / `archive_zip_path` / `archive_path_in_zip`) on
/// every member file of a SUPERSEDED calibration set whose archive zip has
/// been deleted outside the app. Without this, `archive_zip_missing` is a
/// dead end — the originals are gone (they were Move-disposition into the
/// zip), the markers say "archived", and neither restore (zip gone) nor
/// re-archive (`build_calibration_set_plan` bails on already-archived
/// members) can ever run. After clearing, provenance honestly reports the
/// files as not-archived and missing on disk (`originals_archived: false`,
/// `source_frames_on_disk: false`).
///
/// **Gated on the zip actually being missing** — re-checked here, not
/// trusted from the UI's earlier `get_master_provenance` snapshot. If the
/// zip still exists this returns `Conflict`: forgetting a live archive would
/// orphan a perfectly restorable zip, so the only supported path in that
/// state is `restore_originals`.
///
/// Marker clearing goes through the same `unmark_file_archived` helper the
/// restore reconcile path uses (path/mtime/size args all `None` — the files
/// are gone from disk, there is nothing to sync), wrapped in one transaction
/// so a crash mid-loop can't leave a half-forgotten set. The
/// `archive_operations` row is deliberately left untouched: a successful
/// restore also leaves the op row as-is (status stays `completed` — see
/// `restore::run_restore`, which never calls `update_operation_status` on
/// success), so the honest end-state here mirrors that — the op row is
/// history ("this archive happened"), the file markers are current state
/// ("these bytes live in that zip"), and only the latter is now a lie worth
/// fixing.
///
/// Pool discipline (the 27eff86b invariant): acquires exactly ONE pooled
/// connection for the whole request — every query below runs on `conn`, and
/// no helper called from here touches `ServiceContext`/the pool itself.
///
/// Returns the number of member files whose markers were cleared.
pub fn clear_stale_archive_markers(
    ctx: &ServiceContext,
    calibration_set_id: i64,
) -> Result<usize, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();

    let mut stmt = conn.prepare(
        "SELECT fi.id, fi.archive_zip_path
         FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE csf.set_id = ?1 AND fi.archived_in_operation IS NOT NULL",
    )?;
    let marked: Vec<(i64, Option<String>)> = stmt
        .query_map([calibration_set_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    if marked.is_empty() {
        return Err(ApiError::Invalid(
            "calibration set's originals are not archived — no markers to clear".into(),
        ));
    }

    // The gate: if ANY marked member's zip still exists on disk, this is not
    // the stuck state — the archive is intact and restorable. (All members
    // share one zip by construction — see `MasterProvenanceInfo::archive_operation_id`'s
    // doc comment — but checking every row costs nothing and stays correct
    // even if that invariant ever bends.)
    if marked.iter().any(|(_, zip)| {
        zip.as_deref()
            .map(|p| Path::new(p).exists())
            .unwrap_or(false)
    }) {
        return Err(ApiError::Conflict(
            "archive zip still exists on disk — use Restore originals instead".into(),
        ));
    }

    let tx = conn.unchecked_transaction()?;
    let cleared = marked.len();
    for (file_id, _) in &marked {
        crate::archive::db::unmark_file_archived(&tx, *file_id, None, None, None)?;
    }
    tx.commit()?;

    tracing::info!(
        calibration_set_id,
        cleared,
        "cleared stale archive markers for a calibration set whose zip is missing on disk"
    );

    Ok(cleared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::combine::Rejection;
    use rusqlite::Connection;

    #[test]
    fn auto_recipe_rules() {
        // spec §2/§9: bias-like N>=15 Average+Winsorized else Median; flat
        // N>=15 Average+Winsorized else Average+Percentile.
        assert_eq!(
            resolve_recipe(None, "Dark", 20),
            IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 3.0,
                sigma_high: 3.0
            })
        );
        assert_eq!(
            resolve_recipe(None, "Dark", 5),
            IntegrationRecipe::median(Rejection::None)
        );
        assert_eq!(
            resolve_recipe(None, "Bias", 14),
            IntegrationRecipe::median(Rejection::None)
        );
        assert_eq!(
            resolve_recipe(None, "Flat", 20),
            IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 3.0,
                sigma_high: 3.0
            })
        );
        assert_eq!(
            resolve_recipe(None, "Flat", 6),
            IntegrationRecipe::average(Rejection::PercentileClip {
                low: 0.2,
                high: 0.02
            })
        );
        // explicit override wins
        let override_recipe = IntegrationRecipe::average(Rejection::None);
        assert_eq!(
            resolve_recipe(Some(override_recipe), "Flat", 6),
            override_recipe
        );
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
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// A precal calibration set with one member file — the member row is
    /// what `select_flat_precal`'s path lookup resolves against (the path
    /// itself is a dummy string; selection never touches disk).
    fn seed_precal_set(
        conn: &Connection,
        imagetyp: &str,
        is_master: i64,
        exptime: Option<f64>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, exptime, date, is_master_library, frame_count)
             VALUES (?1, ?2, '2026-06-28', ?3, 1)",
            rusqlite::params![imagetyp, exptime, is_master],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 10, '2026-06-28', 'FITS')",
            rusqlite::params![
                format!("/library/{imagetyp}_{set_id}.fits"),
                format!("{imagetyp}_{set_id}.fits")
            ],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
            rusqlite::params![file_id, imagetyp],
        )
        .unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        )
        .unwrap();
        set_id
    }

    fn link(conn: &Connection, flat_set: i64, precal_set: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'calibration_set', ?2, ?3)",
            rusqlite::params![flat_set, precal_set, cal_type],
        )
        .unwrap();
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

        let sel = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(sel.warnings.is_empty(), "{:?}", sel.warnings);
        match &sel.choice {
            PrecalChoice::Master {
                set_id, cal_type, ..
            } => {
                assert_eq!(*set_id, df);
                assert_eq!(*cal_type, "DarkFlat");
            }
            other => panic!("expected darkflat master, got {other:?}"),
        }
        assert!(
            sel.choice.describe().unwrap().contains("darkflat master"),
            "{:?}",
            sel.choice.describe()
        );
        assert!(sel.raw_candidates.is_empty(), "{:?}", sel.raw_candidates);
    }

    #[test]
    fn precal_exposure_matched_dark_master_is_used() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let dark = seed_precal_set(&conn, "MasterDark", 1, Some(2.3)); // within ±0.5s
        link(&conn, flat, dark, "Dark");

        let sel = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(sel.warnings.is_empty(), "{:?}", sel.warnings);
        assert!(
            matches!(
                sel.choice,
                PrecalChoice::Master {
                    cal_type: "Dark",
                    ..
                }
            ),
            "{:?}",
            sel.choice
        );
        assert!(sel.raw_candidates.is_empty(), "{:?}", sel.raw_candidates);
    }

    #[test]
    fn precal_exposure_mismatched_dark_is_skipped_with_warning() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let dark = seed_precal_set(&conn, "MasterDark", 1, Some(4.0)); // off by 2s
        link(&conn, flat, dark, "Dark");

        let sel = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(matches!(sel.choice, PrecalChoice::None), "{:?}", sel.choice);
        assert!(
            sel.warnings
                .iter()
                .any(|w| w.contains("exposure does not match")),
            "{:?}",
            sel.warnings
        );
        // fell all the way through: also carries the un-pre-calibrated warning
        assert!(
            sel.warnings.iter().any(|w| w.contains("un-pre-calibrated")),
            "{:?}",
            sel.warnings
        );
        // a mismatched-exposure dark master is skipped for a different reason
        // than "raw" — it's not a build-a-master-first candidate.
        assert!(sel.raw_candidates.is_empty(), "{:?}", sel.raw_candidates);
    }

    #[test]
    fn precal_raw_darkflat_skipped_falls_to_bias() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let raw_df = seed_precal_set(&conn, "DarkFlat", 0, Some(2.0)); // NOT a master
        link(&conn, flat, raw_df, "DarkFlat");
        // Give the raw set a realistic member count (seed_precal_set hardcodes
        // 1, since it's normally a 1:1 MASTER set) so the reported
        // `frame_count` is actually exercised.
        conn.execute(
            "UPDATE calibration_set SET frame_count = 12 WHERE id = ?1",
            [raw_df],
        )
        .unwrap();
        let bias = seed_precal_set(&conn, "MasterBias", 1, None);
        link(&conn, flat, bias, "Bias");

        let PrecalSelection {
            choice,
            warnings,
            raw_candidates,
        } = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("build its master first")),
            "{warnings:?}"
        );
        match &choice {
            PrecalChoice::Master {
                set_id, cal_type, ..
            } => {
                assert_eq!(*set_id, bias);
                assert_eq!(*cal_type, "Bias");
            }
            other => panic!("expected bias master fallback, got {other:?}"),
        }
        // The skipped raw darkflat is reported as a "build its master first"
        // candidate, not silently dropped.
        assert_eq!(
            raw_candidates,
            vec![RawPrecalCandidate {
                set_id: raw_df,
                cal_type: "DarkFlat".to_string(),
                frame_count: 12,
            }]
        );
    }

    #[test]
    fn precal_bias_master_only() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);
        let bias = seed_precal_set(&conn, "MasterBias", 1, None);
        link(&conn, flat, bias, "Bias");

        let sel = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(sel.warnings.is_empty(), "{:?}", sel.warnings);
        assert!(
            matches!(
                sel.choice,
                PrecalChoice::Master {
                    cal_type: "Bias",
                    ..
                }
            ),
            "{:?}",
            sel.choice
        );
        assert!(
            sel.choice.describe().unwrap().contains("bias master"),
            "{:?}",
            sel.choice.describe()
        );
        assert!(sel.raw_candidates.is_empty(), "{:?}", sel.raw_candidates);
    }

    #[test]
    fn precal_synthetic_bias_fallback() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);

        let sel = select_flat_precal(&conn, flat, Some(2.0), Some(500.0)).unwrap();
        assert!(sel.warnings.is_empty(), "{:?}", sel.warnings);
        assert_eq!(sel.choice, PrecalChoice::Synthetic(500.0));
        assert_eq!(
            sel.choice.describe().as_deref(),
            Some("synthetic bias 500 ADU")
        );
        assert!(sel.raw_candidates.is_empty(), "{:?}", sel.raw_candidates);
    }

    #[test]
    fn precal_nothing_yields_none_with_warning() {
        let conn = test_conn();
        let flat = seed_flat_set(&conn, 2.0);

        let sel = select_flat_precal(&conn, flat, Some(2.0), None).unwrap();
        assert!(matches!(sel.choice, PrecalChoice::None), "{:?}", sel.choice);
        assert_eq!(sel.choice.describe(), Option::None);
        assert!(
            sel.warnings.iter().any(|w| w.contains("un-pre-calibrated")),
            "{:?}",
            sel.warnings
        );
        assert!(sel.raw_candidates.is_empty(), "{:?}", sel.raw_candidates);
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

        let (ready, skipped) =
            plan_batch(&conn, &[flat, dark, darkflat_b, bias, darkflat_a]).unwrap();
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
        assert_eq!(
            skipped,
            vec![BatchSkip {
                set_id: raw,
                reason: "already has a master".into()
            }]
        );
    }

    #[test]
    fn plan_batch_skips_too_few_frames() {
        let conn = test_conn();
        let raw = seed_set(&conn, "Dark", 2, None, 0);

        let (ready, skipped) = plan_batch(&conn, &[raw]).unwrap();
        assert!(ready.is_empty());
        assert_eq!(
            skipped,
            vec![BatchSkip {
                set_id: raw,
                reason: "only 2 frames (minimum 3)".into()
            }],
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
        assert_eq!(
            skipped,
            vec![BatchSkip {
                set_id: master,
                reason: "is itself a master".into()
            }]
        );
    }

    #[test]
    fn plan_batch_skips_unknown_set() {
        let conn = test_conn();
        let (ready, skipped) = plan_batch(&conn, &[999_999]).unwrap();
        assert!(ready.is_empty());
        assert_eq!(
            skipped,
            vec![BatchSkip {
                set_id: 999_999,
                reason: "unknown set".into()
            }]
        );
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
        assert!(skipped
            .iter()
            .any(|s| s.set_id == master && s.reason == "is itself a master"));
        assert!(skipped
            .iter()
            .any(|s| s.set_id == too_small && s.reason.contains("minimum 3")));
        assert!(skipped
            .iter()
            .any(|s| s.set_id == 424242 && s.reason == "unknown set"));
    }

    // ── check_rebuild_source_ready (pure DB + Path::exists) ─────────────────

    /// A source calibration_set with `n` member frames, each pointing at a
    /// real (but possibly not actually created) path so `Path::exists` has
    /// something concrete to check. `archived` marks every member's
    /// `files.archived_in_operation` as set (non-NULL) or not.
    fn seed_source_with_files(
        conn: &Connection,
        dir: &std::path::Path,
        n: usize,
        archived: bool,
    ) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, frame_count) VALUES ('Dark', '2026-06-28', ?1)",
            [n as i64],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        for i in 0..n {
            let p = dir.join(format!("raw{i}.fits"));
            std::fs::write(
                &p,
                b"not a real fits file, existence is all that matters here",
            )
            .unwrap();
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
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            )
            .unwrap();
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

    // ── Important-2: check_library_dir_exists ────────────────────────────

    #[test]
    fn library_dir_check_passes_for_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_library_dir_exists(&dir.path().to_string_lossy()).is_ok());
    }

    #[test]
    fn library_dir_check_fails_for_deleted_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        drop(dir); // simulates the folder being removed/unmounted outside the app
        let err = check_library_dir_exists(&path).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)));
        assert!(err.to_string().contains(&path), "{err}");
        assert!(
            err.to_string().contains("reconfigure it in File Manager"),
            "{err}"
        );
    }

    #[test]
    fn library_dir_check_fails_for_a_file_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, b"hello").unwrap();
        let err = check_library_dir_exists(&file_path.to_string_lossy()).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)));
    }

    // ── Regression: concurrent preview_master_build must not deadlock the
    // pool ────────────────────────────────────────────────────────────────
    //
    // Pins the fix for the production panic this module's `library_dir_or_err`
    // doc comment describes: `preview_master_build` must acquire exactly ONE
    // pooled connection for its whole body. Before the fix, `library_dir_or_err`
    // took `&ServiceContext` and re-entered the pool via
    // `scan_roots::get_calibration_library_dir(ctx)` while the caller's own
    // connection (acquired at the top of `preview_master_build`) was still
    // held — so N concurrent previews exceeding `Database`'s pool `max_size`
    // (8, see `db::Database::new`) each grabbed one connection and then
    // deadlocked trying to grab a second, exactly reproducing what the batch
    // Create-Masters dialog did in production (one `preview_master_build`
    // call per calibration set, all fired concurrently via
    // `Promise.allSettled`). This spawns pool_size + 4 real OS threads, each
    // running a genuine `preview_master_build` call against a seeded catalog
    // + configured library dir, and requires them all to finish well under
    // the pool's connection-checkout timeout — pre-fix this test hangs for
    // ~30s per stranded task and then panics (`Database::conn()`'s `.expect`);
    // post-fix it completes in well under a second.
    #[test]
    fn preview_master_build_survives_pool_sized_concurrency() {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, RwLock};
        use std::time::{Duration, Instant};

        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();

        let lib_dir = tmp.path().join("library");
        std::fs::create_dir_all(&lib_dir).unwrap();

        // Seed one buildable Bias set (not a Flat, so no select_flat_precal
        // lookups are in play — keeps this focused on the library-dir
        // resolution path that used to double-acquire) plus the calibration
        // library setting `resolve_calibration_library_dir` reads.
        let set_id = {
            let conn = db.conn();
            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date, frame_count) VALUES ('Bias', '2026-06-28', 5)",
                [],
            ).unwrap();
            let set_id = conn.last_insert_rowid();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &lib_dir.to_string_lossy(),
            )
            .unwrap();
            set_id
        };

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
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });

        // Pool max_size is 8 — exceed it so, pre-fix, every task grabs one
        // connection and then blocks forever on a second.
        const N: usize = 12;
        let recipe = MasterRecipe {
            combine: None,
            synthetic_bias: None,
            archive_after: false,
        };
        let start = Instant::now();
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let ctx = ctx.clone();
                let recipe = recipe.clone();
                std::thread::spawn(move || preview_master_build(&ctx, set_id, &recipe))
            })
            .collect();

        for h in handles {
            let result = h
                .join()
                .expect("preview_master_build thread panicked — pool-checkout deadlock regression");
            result.expect("preview_master_build returned an error");
        }

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(20),
            "12 previews against an 8-conn pool took {elapsed:?} — \
             looks like pool contention/near-timeout, not the near-instant pure-DB path"
        );
    }

    // ── Shakedown gap: deleted archive zip must be detected, not panic ──────
    //
    // Lives here rather than alongside the Task 14
    // `calibration_archive_executor_and_restore_round_trip` test in
    // `archive/planner.rs` because both functions under test
    // (`get_master_provenance`, `restore_originals`) need a full
    // `ServiceContext` (`operation_queue`, `active_archives`) — borrows the
    // construction precedent from `preview_master_build_survives_pool_sized_concurrency`
    // above rather than `archive/planner.rs`'s bare-`Connection` tests.
    #[test]
    fn archive_zip_missing_is_detected_and_restore_is_actionable() {
        use crate::archive::models::{ArchiveCompression, ConflictResolution};
        use crate::archive::planner::{build_calibration_set_plan, commit_plan};
        use crate::cache::MemoryImageCache;
        use crate::events::NullEmitter;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, RwLock};

        let tmp = tempfile::tempdir().unwrap();
        let archive_dir = tmp.path().join("archive");
        std::fs::create_dir_all(&archive_dir).unwrap();
        let raw_dir = tmp.path().join("raw");
        std::fs::create_dir_all(&raw_dir).unwrap();

        // Named `catalog_db` (not `db`) — a local binding named `db` would
        // shadow the `db(ctx)` helper fn for the rest of this scope (see
        // `run_build`'s `db_handle` precedent above for the same pitfall).
        let catalog_db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();

        // Same fixture shape as the Task 14 round-trip test: a raw Dark set
        // superseded by a master, one member file, a master_provenance row
        // linking them.
        let calibration_set_id = {
            let conn = catalog_db.conn();
            conn.execute(
                "INSERT INTO scan_roots (path) VALUES (?1)",
                [raw_dir.to_string_lossy()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
                 VALUES (999, 'MasterDark', '2026-06-28', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set
                    (imagetyp, date, instrume, gain, exptime, date_start, date_end, superseded_by_set_id)
                 VALUES ('Dark','2026-06-28','TestCam',100.0,300.0,
                         '2026-06-28T20:00:00Z','2026-06-28T21:00:00Z', 999)",
                [],
            ).unwrap();
            let set_id = conn.last_insert_rowid();

            let f = raw_dir.join("d1.fits");
            std::fs::write(&f, b"data").unwrap();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, 'd1.fits', 4, '2026-06-28', 'FITS')",
                [f.to_string_lossy()],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Dark')",
                [file_id],
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO master_provenance
                    (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
                 VALUES (999, ?1, '{}', '[]', 'abc123', '2026-06-28T00:00:00Z')",
                [set_id],
            ).unwrap();

            set_id
        };
        let master_set_id: i64 = 999;

        let db_cell = OnceLock::new();
        let _ = db_cell.set(catalog_db);
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
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });

        // Archive the raw set's originals for real (plan -> commit -> execute) —
        // the same sequence `api::masters::archive_originals` drives — so the
        // fixture matches exactly what production writes to `files`.
        let op_id = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            let plan = build_calibration_set_plan(
                &conn,
                calibration_set_id,
                &archive_dir,
                ArchiveCompression::Store,
            )
            .unwrap();
            let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            crate::archive::executor::run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
            op_id
        };

        // Sanity: provenance sees it archived and NOT missing yet.
        let prov = get_master_provenance(&ctx, master_set_id).unwrap().unwrap();
        assert!(prov.originals_archived);
        assert_eq!(prov.archive_operation_id, Some(op_id));
        let zip_path = prov.archive_zip_path.clone().expect("zip path recorded");
        assert!(!prov.archive_zip_missing, "zip still on disk at this point");

        // Forget-archive is GATED on the zip actually being missing: while
        // the zip is intact the only supported path is Restore originals,
        // so this must refuse with Conflict (not clear anything).
        let err = clear_stale_archive_markers(&ctx, calibration_set_id).unwrap_err();
        assert!(
            matches!(&err, ApiError::Conflict(m) if m.contains("use Restore originals")),
            "expected Conflict while zip exists, got {err:?}"
        );

        // Delete the zip out from under the catalog — simulates the user
        // removing it (or the drive it lived on going away) outside the app.
        std::fs::remove_file(&zip_path).unwrap();

        // (a) provenance now reports the gap instead of silently claiming the
        // originals are still one click away from being restored.
        let prov_after = get_master_provenance(&ctx, master_set_id).unwrap().unwrap();
        assert!(
            prov_after.archive_zip_missing,
            "must detect the deleted zip"
        );
        assert!(
            prov_after.originals_archived,
            "markers are still present — only the zip vanished"
        );

        // (b) a restore attempt fails with an actionable Invalid, not an io
        // panic — this is what the UI's `archiveZipMissing` gate exists to
        // pre-empt, but the API itself must degrade gracefully even if
        // called directly (e.g. from a script, or a future UI regression).
        let emitter: Arc<dyn ProgressEmitter> = Arc::new(NullEmitter);
        let err = restore_originals(ctx.clone(), emitter, calibration_set_id).unwrap_err();
        match err {
            ApiError::Invalid(msg) => {
                assert!(msg.contains("missing on disk"), "{msg}");
                assert!(msg.contains(&zip_path), "{msg}");
                // The message must point at the affordance that actually
                // exists — the provenance panel's Forget-archive button.
                assert!(msg.contains("Forget archive"), "{msg}");
            }
            other => panic!("expected ApiError::Invalid, got {other:?}"),
        }

        // No operation was ever enqueued for the failed attempt — the
        // handle map must be exactly as empty as it started.
        assert!(ctx.active_archives.lock().unwrap().is_empty());

        // (c) the exit from the stuck state: Forget archive clears the
        // markers (zip genuinely missing now), and provenance flips to the
        // honest end-state — not archived, originals missing on disk.
        let cleared = clear_stale_archive_markers(&ctx, calibration_set_id).unwrap();
        assert_eq!(cleared, 1, "exactly the one member file's markers cleared");

        let prov_final = get_master_provenance(&ctx, master_set_id).unwrap().unwrap();
        assert!(
            !prov_final.originals_archived,
            "markers gone — no longer 'archived'"
        );
        assert!(
            !prov_final.archive_zip_missing,
            "nothing archived => nothing missing"
        );
        assert_eq!(prov_final.archive_operation_id, None);
        assert_eq!(prov_final.archive_zip_path, None);
        assert!(
            !prov_final.source_frames_on_disk,
            "the files themselves are still gone — provenance must say so, not pretend they're back"
        );

        // Second call: nothing left to clear — Invalid, not a silent 0.
        let err = clear_stale_archive_markers(&ctx, calibration_set_id).unwrap_err();
        assert!(
            matches!(&err, ApiError::Invalid(m) if m.contains("no markers to clear")),
            "expected Invalid on re-run, got {err:?}"
        );
    }

    // ── common_source_dir (pure — pins the target_root_path synthesis) ──────
    //
    // `restore_originals` feeds this into `restore::classify_target`, which
    // picks Original mode iff every file's source_path starts with the
    // target root. Even the degenerate all-the-way-divergent case therefore
    // still resolves to Original mode (a root like "/" prefixes every
    // absolute path) — these cases pin `common_source_dir` itself.
    #[test]
    fn common_source_dir_multi_path_cases() {
        // Different subtrees under a shared ancestor → the shared ancestor.
        assert_eq!(
            common_source_dir(&[
                PathBuf::from("/data/roots/a/f1.fits"),
                PathBuf::from("/data/other/b/f2.fits"),
            ]),
            PathBuf::from("/data"),
        );

        // Identical directories → that directory (not its parent).
        assert_eq!(
            common_source_dir(&[
                PathBuf::from("/data/roots/a/f1.fits"),
                PathBuf::from("/data/roots/a/f2.fits"),
            ]),
            PathBuf::from("/data/roots/a"),
        );

        // Fully divergent absolute paths → the degenerate ancestor (the
        // filesystem root) — still a valid Original-mode target root, per
        // the note above.
        assert_eq!(
            common_source_dir(&[
                PathBuf::from("/data/a/f1.fits"),
                PathBuf::from("/var/b/f2.fits"),
            ]),
            PathBuf::from("/"),
        );

        // Single path → its parent directory (not the file itself).
        assert_eq!(
            common_source_dir(&[PathBuf::from("/data/roots/a/f1.fits")]),
            PathBuf::from("/data/roots/a"),
        );

        // Empty input → empty path (restore_originals bails on empty member
        // lists long before calling this; pinned for totality).
        assert_eq!(common_source_dir(&[]), PathBuf::new());
    }

    // ── delete_master (audit C3) ────────────────────────────────────────────
    //
    // The end-to-end reversal: Task 7's `unregister_master_set` puts the
    // catalog back, `delete_master` adds the two things the primitive
    // deliberately leaves to its caller — the `files` rows and the file on
    // disk. Fixture shape mirrors `db::master_unregister`'s
    // `seed_registered_master`, except the master's `files.path` points at a
    // REAL file in a temp dir (the whole point of testing at this layer).
    #[test]
    fn delete_master_removes_the_file_and_restores_the_raw_lineage() {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::services::MasterBuildHandle;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, RwLock};

        let tmp = tempfile::tempdir().unwrap();
        let lib_dir = tmp.path().join("library");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let master_path = lib_dir.join("master_dark_300s_-10C_g100_bin1x1_2026-08-01.fits");
        std::fs::write(&master_path, b"master pixels").unwrap();

        // Named `catalog_db` so it doesn't shadow the `db(ctx)` helper fn —
        // same pitfall noted in `archive_zip_missing_is_detected_...` above.
        let catalog_db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();

        let (raw_set, master_set, master_file_id, raw_file_id, consumer_frame_id) = {
            let conn = catalog_db.conn();

            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
                [],
            )
            .unwrap();
            let raw_set = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date, is_master_library)
                 VALUES ('MasterDark', '2026-08-01', 1)",
                [],
            )
            .unwrap();
            let master_set = conn.last_insert_rowid();
            conn.execute(
                "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
                rusqlite::params![master_set, raw_set],
            )
            .unwrap();

            // The master's own file — on disk, in the catalog, in the set.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, 'master.fits', 13, '2026-08-01', 'FITS')",
                [master_path.to_string_lossy()],
            )
            .unwrap();
            let master_file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MASTERDARK', 1)",
                [master_file_id],
            )
            .unwrap();
            let master_frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![master_set, master_frame_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO master_provenance
                    (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
                 VALUES (?1, ?2, '{}', '[]', 'h', '2026-08-01T00:00:00Z')",
                rusqlite::params![master_set, raw_set],
            )
            .unwrap();

            // A raw member of the source set — its file is on disk too and
            // must survive untouched (deleting the master must never eat the
            // frames it was integrated from).
            let raw_path = tmp.path().join("d1.fits");
            std::fs::write(&raw_path, b"raw pixels").unwrap();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, 'd1.fits', 10, '2026-08-01', 'FITS')",
                [raw_path.to_string_lossy()],
            )
            .unwrap();
            let raw_file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Dark')",
                [raw_file_id],
            )
            .unwrap();
            let raw_frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![raw_set, raw_frame_id],
            )
            .unwrap();

            // A Light frame whose calibration link register_master moved onto
            // the master — this is what has to come back to the raw set.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES ('/lights/l1.fits', 'l1.fits', 10, '2026-08-01', 'FITS')",
                [],
            )
            .unwrap();
            let light_file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'LIGHT')",
                [light_file_id],
            )
            .unwrap();
            let consumer_frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_to_frames
                    (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
                 VALUES (?1, 'frame', ?2, 'Dark', 0.9, 1)",
                rusqlite::params![consumer_frame_id, master_set],
            )
            .unwrap();

            (
                raw_set,
                master_set,
                master_file_id,
                raw_file_id,
                consumer_frame_id,
            )
        };

        let db_cell = OnceLock::new();
        let _ = db_cell.set(catalog_db);
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
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });

        // (a) An in-flight build blocks the delete. `active_master_builds` is
        // keyed by the SOURCE set id (see `rebuild_master`'s comment) — a
        // rebuild of THIS master registers under `raw_set`, not `master_set`,
        // so the guard has to look under both or it misses the very case it
        // exists for.
        for busy_key in [raw_set, master_set] {
            ctx.active_master_builds.lock().unwrap().insert(
                busy_key,
                MasterBuildHandle {
                    cancel_flag: Arc::new(AtomicBool::new(false)),
                },
            );
            let err = delete_master(&ctx, master_set).unwrap_err();
            assert!(
                matches!(&err, ApiError::Conflict(m) if m.contains("in progress")),
                "expected a Conflict while a build is registered under {busy_key}, got {err:?}"
            );
            assert!(
                master_path.exists(),
                "a refused delete must not touch the file on disk"
            );
            ctx.active_master_builds.lock().unwrap().remove(&busy_key);
        }

        // (b) The real delete.
        let res = delete_master(&ctx, master_set).unwrap();
        assert_eq!(res.master_set_id, master_set);
        assert_eq!(res.restored_raw_set_id, Some(raw_set));
        assert_eq!(res.links_repointed, 1);
        assert_eq!(res.files_deleted, 1);

        assert!(!master_path.exists(), "the master file is gone from disk");

        let conn = db(&ctx).unwrap().conn();
        let master_files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                [master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(master_files, 0, "the master's files row is gone");
        let master_frames: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE file_id = ?1",
                [master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(master_frames, 0, "its frames row cascaded away with it");

        let sup: Option<i64> = conn
            .query_row(
                "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
                [raw_set],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sup, None, "the raw set is matchable again");

        let (target, manual): (i64, i64) = conn
            .query_row(
                "SELECT calibration_set_id, is_manual_override FROM calibration_set_to_frames
                  WHERE source_id = ?1 AND source_type = 'frame'",
                [consumer_frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            target, raw_set,
            "the consumer link came back to the raw set"
        );
        assert_eq!(manual, 1, "the manual-override flag rode along");

        let raw_files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                [raw_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw_files, 1, "the source frames are untouched");
        assert!(
            tmp.path().join("d1.fits").exists(),
            "and still on disk — only the master's own file is deleted"
        );

        // (c) Second call: the set is gone, so this is a plain NotFound-shaped
        // failure, not a silent success that pretends to have deleted something.
        assert!(
            delete_master(&ctx, master_set).is_err(),
            "deleting an already-deleted master must fail loudly"
        );
    }

    // ── rebuild finalize (provenance + catalog resync) ─────────────────────

    /// Write a master FITS whose CFA cards are the caller's choice. `side`
    /// varies the geometry so a rewrite also moves `files.size`/`naxis1`.
    fn write_master_file(
        path: &std::path::Path,
        bayer: crate::fits_writer::keywords::Bayer,
        xoff: i64,
        roworder: &str,
        side: usize,
    ) {
        use crate::fits_writer::keywords::{FrameKind, HeaderBuilder};
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .instrume("TestCam")
            .exptime(300.0)
            .bayer(bayer, xoff, 0)
            .roworder(roworder)
            .build()
            .unwrap();
        write_fits_f32(path, side, side, 1, &vec![100.0f32; side * side], &cards).unwrap();
    }

    /// A raw Dark set for `register_master` to supersede.
    fn seed_raw_dark_set(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn stored_header(conn: &Connection, file_id: i64) -> String {
        conn.query_row(
            "SELECT header FROM fits_header WHERE file_id = ?1",
            [file_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn frame_bayerpat(conn: &Connection, file_id: i64) -> Option<String> {
        conn.query_row(
            "SELECT bayerpat FROM frames WHERE file_id = ?1",
            [file_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Collect `(level, message)` of every event emitted on THIS thread inside
    /// `f`. Same scoped-`with_default` + custom-`Layer` pattern
    /// `logging::config`'s tests use, including the interest-cache rebuild
    /// (macro callsites cache their verdict per process, so a callsite first
    /// hit with no subscriber would otherwise stay disabled).
    fn capture_events<T>(f: impl FnOnce() -> T) -> (T, Vec<(String, String)>) {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone, Default)]
        struct Seen(Arc<Mutex<Vec<(String, String)>>>);
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Seen {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut msg = Message(String::new());
                event.record(&mut msg);
                self.0
                    .lock()
                    .unwrap()
                    .push((event.metadata().level().to_string(), msg.0));
            }
        }

        let seen = Seen::default();
        let subscriber = tracing_subscriber::registry().with(seen.clone());
        let out = tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        let events = seen.0.lock().unwrap().clone();
        (out, events)
    }

    /// A rebuild rewrites the master's HEADER as well as its pixels — since
    /// the Bayer cards are built from the member consensus, BAYERPAT /
    /// XBAYROFF / YBAYROFF / ROWORDER can all differ from what the original
    /// registration parsed. `finalize_rebuild` therefore re-reads the file it
    /// just wrote: light-calibration copy-through reads the STORED header, and
    /// a stale non-blank card there wins over the frames-column fallback.
    #[test]
    fn rebuild_finalize_syncs_frames_and_stored_header_with_the_rewritten_file() {
        use crate::fits_writer::keywords::Bayer;

        let tmp = tempfile::tempdir().unwrap();
        let master_path = tmp.path().join("master_dark_300s.fits");
        let conn = test_conn();
        let raw_set = seed_raw_dark_set(&conn);

        // ── Build #1: RGGB, no phase offsets, BOTTOM-UP, 4x4.
        write_master_file(&master_path, Bayer::Rggb, 0, "BOTTOM-UP", 4);
        let reg = register_master(&conn, raw_set, &master_path, r#"{"combine":"median"}"#).unwrap();

        let stored = stored_header;
        assert!(
            stored(&conn, reg.master_file_id).contains("BAYERPAT= 'RGGB"),
            "sanity: registration stored the first build's header"
        );

        // ── Rebuild: SAME path, different consensus cards, different geometry
        // (so files.size moves too). Only the file is rewritten here — the
        // catalog refresh is entirely `finalize_rebuild`'s job.
        write_master_file(&master_path, Bayer::Grbg, 1, "TOP-DOWN", 64);

        finalize_rebuild(
            &conn,
            reg.master_set_id,
            reg.master_file_id,
            &master_path,
            r#"{"combine":"mean","rebuilt":true}"#,
            "hash-after-rebuild",
        )
        .unwrap();

        // frames row: same id (every junction link is keyed on it), new cards.
        let (frame_id, bayerpat, xbayroff, ybayroff, roworder, naxis1): (
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT id, bayerpat, xbayroff, ybayroff, roworder, naxis1
                 FROM frames WHERE file_id = ?1",
                [reg.master_file_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            frame_id, reg.master_frame_id,
            "rebuild must preserve frames.id"
        );
        assert_eq!(bayerpat.as_deref(), Some("GRBG"));
        assert_eq!(xbayroff, Some(1));
        assert_eq!(ybayroff, Some(0));
        assert_eq!(roworder.as_deref(), Some("TOP-DOWN"));
        assert_eq!(naxis1, Some(64), "geometry follows the rewritten file");

        // Stored header: the blob light-cal copy-through reads.
        let header = stored(&conn, reg.master_file_id);
        assert!(
            header.contains("BAYERPAT= 'GRBG"),
            "stored header must follow disk, got: {header}"
        );
        assert!(
            !header.contains("BAYERPAT= 'RGGB"),
            "the pre-rebuild CFA card must not survive in the blob"
        );
        let header_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fits_header WHERE file_id = ?1",
                [reg.master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            header_rows, 1,
            "the refresh must not duplicate the blob row"
        );

        // files row: size follows disk — that is what makes the next library
        // scan classify the rebuilt master as unchanged instead of re-parsing.
        let size: i64 = conn
            .query_row(
                "SELECT size FROM files WHERE id = ?1",
                [reg.master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            size,
            std::fs::metadata(&master_path).unwrap().len() as i64,
            "files.size must follow the rewritten file"
        );

        // Set membership + provenance: untouched / refreshed as before.
        let member: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1 AND frame_id = ?2",
                rusqlite::params![reg.master_set_id, reg.master_frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            member, 1,
            "the master's calibration_set membership survives"
        );
        let prov = crate::db::master_provenance::get(&conn, reg.master_set_id)
            .unwrap()
            .unwrap();
        assert_eq!(prov.member_hash, "hash-after-rebuild");
        assert_eq!(
            prov.source_set_id,
            Some(raw_set),
            "rebuild must not relink to a different source"
        );
    }

    /// The chosen precedence when the user has edited a master's metadata:
    /// `frames.override = 1` keeps the frames columns, the stored header still
    /// follows the rewritten file. This INVERTS the usual drift (blob new,
    /// columns old), and it is reachable —
    /// `bulk_update_calibration_metadata` sets override = 1 on every member of
    /// an edited calibration set, and a master is the sole member of its own
    /// set — so the skip must be announced, not silent.
    #[test]
    fn rebuild_finalize_keeps_user_override_columns_and_warns_about_the_skip() {
        use crate::fits_writer::keywords::Bayer;

        let tmp = tempfile::tempdir().unwrap();
        let master_path = tmp.path().join("master_dark_300s.fits");
        let conn = test_conn();
        let raw_set = seed_raw_dark_set(&conn);

        write_master_file(&master_path, Bayer::Rggb, 0, "BOTTOM-UP", 4);
        let reg = register_master(&conn, raw_set, &master_path, r#"{"combine":"median"}"#).unwrap();

        // The user edited this master's metadata (Equipment page → bulk
        // calibration edit), so the scanner — and the rebuild resync with it —
        // must not overwrite the frames columns.
        conn.execute(
            "UPDATE frames SET override = 1 WHERE id = ?1",
            [reg.master_frame_id],
        )
        .unwrap();

        write_master_file(&master_path, Bayer::Grbg, 1, "TOP-DOWN", 64);
        let (result, events) = capture_events(|| {
            finalize_rebuild(
                &conn,
                reg.master_set_id,
                reg.master_file_id,
                &master_path,
                r#"{"combine":"mean","rebuilt":true}"#,
                "hash-after-rebuild",
            )
        });
        result.unwrap();

        assert_eq!(
            frame_bayerpat(&conn, reg.master_file_id).as_deref(),
            Some("RGGB"),
            "user edits outrank the rewritten header — frames columns stay put"
        );
        let (override_flag, roworder): (i64, Option<String>) = conn
            .query_row(
                "SELECT override, roworder FROM frames WHERE file_id = ?1",
                [reg.master_file_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(override_flag, 1, "the override flag itself survives");
        assert_eq!(
            roworder.as_deref(),
            Some("BOTTOM-UP"),
            "no header-derived column is refreshed under override"
        );

        // The snapshot still describes the bytes now on disk — that is what
        // per-field revert and move detection read.
        let header = stored_header(&conn, reg.master_file_id);
        assert!(
            header.contains("BAYERPAT= 'GRBG"),
            "the stored header still follows disk under override, got: {header}"
        );

        assert!(
            events.iter().any(|(level, msg)| level == "WARN"
                && msg.contains("rebuild resync skipped frames columns")),
            "the inverted drift must be announced; captured: {events:?}"
        );
    }

    /// All-or-nothing: if anything in the post-write window fails, the catalog
    /// must look exactly as it did before — provenance included. The resync's
    /// parse runs BEFORE it touches a row, so a file that cannot be read back
    /// takes the whole transaction down with it (`update_rebuild` included).
    #[test]
    fn rebuild_finalize_failure_leaves_provenance_frames_and_header_untouched() {
        use crate::fits_writer::keywords::Bayer;

        let tmp = tempfile::tempdir().unwrap();
        let master_path = tmp.path().join("master_dark_300s.fits");
        let conn = test_conn();
        let raw_set = seed_raw_dark_set(&conn);

        write_master_file(&master_path, Bayer::Rggb, 0, "BOTTOM-UP", 4);
        let reg = register_master(&conn, raw_set, &master_path, r#"{"combine":"median"}"#).unwrap();
        let before = crate::db::master_provenance::get(&conn, reg.master_set_id)
            .unwrap()
            .unwrap();
        let size_before: i64 = conn
            .query_row(
                "SELECT size FROM files WHERE id = ?1",
                [reg.master_file_id],
                |r| r.get(0),
            )
            .unwrap();

        // What is at the master's path can no longer be read back as FITS.
        std::fs::write(&master_path, b"NOT A FITS FILE").unwrap();

        let err = finalize_rebuild(
            &conn,
            reg.master_set_id,
            reg.master_file_id,
            &master_path,
            r#"{"combine":"mean","rebuilt":true}"#,
            "hash-that-must-not-land",
        )
        .expect_err("an unreadable master must fail the finalize, not pass silently");
        assert!(
            matches!(err, BuildStepError::Other(_)),
            "expected a plain failure, got {err:?}"
        );

        let after = crate::db::master_provenance::get(&conn, reg.master_set_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            after.member_hash, before.member_hash,
            "provenance rolled back"
        );
        assert_eq!(after.recipe_json, before.recipe_json);
        assert_eq!(after.created_at, before.created_at);

        assert_eq!(
            frame_bayerpat(&conn, reg.master_file_id).as_deref(),
            Some("RGGB"),
            "frames row untouched"
        );
        assert!(
            stored_header(&conn, reg.master_file_id).contains("BAYERPAT= 'RGGB"),
            "stored header untouched"
        );
        let size_after: i64 = conn
            .query_row(
                "SELECT size FROM files WHERE id = ?1",
                [reg.master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            size_after, size_before,
            "files row untouched — the failure must not half-apply"
        );
    }

    // ── CFA master flat, end-to-end through `run_build` (Task 10) ────────────

    /// The mosaic colour index of pixel `(x, y)` for RGGB at CFA phase (1, 0).
    /// Spelled out rather than routed through `cfa_channel_at` so the fixture
    /// states the layout INDEPENDENTLY of the production mapping it is used to
    /// check. `0` = R, `1` = G, `2` = B. (`api::lights`' orchestration tests
    /// carry the same three lines for their own fixtures; sharing them would
    /// mean exporting a test helper across two modules to save six lines.)
    fn rggb_at_phase_1_0(x: usize, y: usize) -> usize {
        // grid[y % 2][(x + 1) % 2] over [[R, G], [G, B]].
        match (x % 2 == 1, y % 2 == 0) {
            (true, true) => 0,
            (false, false) => 2,
            _ => 1,
        }
    }

    fn build_test_ctx(db: crate::db::Database) -> Arc<ServiceContext> {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, RwLock};

        let cell = OnceLock::new();
        let _ = cell.set(db);
        Arc::new(ServiceContext {
            db: cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    /// A master flat built from real mosaic subs whose headers declare
    /// `RGGB`/`XBAYROFF=1`/`YBAYROFF=0`/`BOTTOM-UP`, driven through the REAL
    /// `run_build` and asserted against the FILE it wrote.
    ///
    /// The consensus rule and the per-channel measurement are unit-pinned in
    /// `calibration_library::headers`; what this adds is the composition —
    /// `is_flat` gating `measure_flat_channel_norms`, the consensus reaching
    /// `build_master_cards`, and the writer emitting `XBAYROFF` as a real
    /// INTEGER card. A phase-1 sensor whose master claims phase 0 swaps R and
    /// B in every light it ever calibrates, and nothing about the pixels looks
    /// wrong while it happens.
    #[test]
    fn cfa_master_flat_build_writes_consensus_bayer_cards_and_channel_constants() {
        use crate::fits_parser::FitsHeader;
        use crate::fits_writer::keywords::{FrameKind, HeaderBuilder};
        use crate::fits_writer::{Card, CardValue};

        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let library_dir = tmp.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();

        let (w, h) = (6usize, 6usize);
        // Channel levels chosen so the three central-third constants are all
        // different: a build that mixed the channels (the pre-cycle global
        // scalar) would land on their blend, 2750, for all three.
        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = [2000.0f32, 4000.0, 1000.0][rggb_at_phase_1_0(x, y)];
            }
        }

        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let set_id = {
            let conn = database.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set
                 (imagetyp, exptime, date, date_start, date_end, instrume, binning, frame_count)
                 VALUES ('Flat', 3.0, '2026-06-28', '2026-06-28T20:00:00Z',
                         '2026-06-28T20:10:00Z', 'TestCam', '1x1', 3)",
                [],
            )
            .unwrap();
            let set_id = conn.last_insert_rowid();

            for i in 0..3 {
                let p = src.join(format!("flat{i}.fits"));
                let mut cards = HeaderBuilder::new(FrameKind::Flat)
                    .instrume("TestCam")
                    .exptime(3.0)
                    .binning(1, 1)
                    .build()
                    .unwrap();
                // The four CFA cards a real OSC capture writes. Phase (1, 0),
                // not (0, 0): a fixture at phase 0 cannot tell a consensus
                // offset from a fabricated one.
                cards.extend([
                    Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
                    Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
                    Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
                    Card::new("ROWORDER", CardValue::Str("BOTTOM-UP".into())).unwrap(),
                ]);
                crate::fits_writer::write_fits_f32(&p, w, h, 1, &data, &cards).unwrap();

                // Ingested through the real parser, so the Bayer columns the
                // consensus reads are exactly what a scan of these files
                // produces — not values hand-typed to match the assertion.
                let (parsed, header_text) =
                    crate::fits_parser::parse_fits_with_header(&p, 0).unwrap();
                assert_eq!(parsed.xbayroff, Some(1), "fixture sanity: parsed phase");
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format)
                     VALUES (?1, ?2, 100, '2026-06-28', 'FITS')",
                    rusqlite::params![p.to_string_lossy(), format!("flat{i}.fits")],
                )
                .unwrap();
                let file_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO frames (file_id, imagetyp, instrume, exptime, binning, date_obs,
                                         bayerpat, xbayroff, ybayroff, roworder)
                     VALUES (?1, 'Flat', 'TestCam', 3.0, '1x1', '2026-06-28T20:05:00Z',
                             ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        file_id,
                        parsed.bayerpat,
                        parsed.xbayroff,
                        parsed.ybayroff,
                        parsed.roworder
                    ],
                )
                .unwrap();
                let frame_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                    rusqlite::params![set_id, frame_id],
                )
                .unwrap();
                crate::db::insert_fits_header(&conn, file_id, &header_text).unwrap();
            }
            set_id
        };

        let ctx = build_test_ctx(database);
        let recipe = MasterRecipe {
            // Explicit, not Auto: at n = 3 the flat rule resolves to
            // percentile-clip, and this test is about metadata, not about
            // which rejection an identical stack happens to survive.
            combine: Some(IntegrationRecipe::average(Rejection::None)),
            synthetic_bias: None,
            archive_after: false,
        };
        let (master_set_id, _warning) = run_build(
            &ctx,
            &crate::events::NullEmitter,
            "0.5.1-test",
            set_id,
            &recipe,
            &Arc::new(AtomicBool::new(false)),
            BuildTarget::New,
        )
        .expect("the master flat must build");

        let master_path: PathBuf = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            let p: String = conn
                .query_row(
                    "SELECT fi.path FROM calibration_set_frames csf
                     JOIN frames f ON f.id = csf.frame_id
                     JOIN files fi ON fi.id = f.file_id
                     WHERE csf.set_id = ?1",
                    [master_set_id],
                    |r| r.get(0),
                )
                .unwrap();
            PathBuf::from(p)
        };

        // ── The pin: the master FILE's own header, re-read from disk. ──
        let header = FitsHeader::from_path(&master_path).unwrap();
        assert_eq!(header.get_str("BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(
            header.get_i32("XBAYROFF"),
            Some(1),
            "the members' REAL phase must survive, not a fabricated 0"
        );
        assert_eq!(header.get_i32("YBAYROFF"), Some(0));
        assert_eq!(header.get_str("ROWORDER").as_deref(), Some("BOTTOM-UP"));

        // XBAYROFF must be an INTEGER card. The header reader strips quotes, so
        // `get_i32` alone cannot tell `1` from `'1'` — and a quoted offset is
        // exactly the spelling third-party debayerers refuse to read.
        let raw = header.to_header_text();
        let xcard = raw
            .lines()
            .find(|l| l.starts_with("XBAYROFF"))
            .expect("XBAYROFF card in the written master");
        assert!(
            !xcard.contains('\''),
            "XBAYROFF must be an integer card, got {xcard:?}"
        );

        // Per-channel constants, measured on the very plane that was written.
        let near = |kw: &str, want: f64| {
            let got = header
                .get_f64(kw)
                .unwrap_or_else(|| panic!("{kw} card in the written master"));
            assert!((got - want).abs() < 1e-3, "{kw} = {got}, want {want}");
        };
        near("ATH_FNR", 2000.0);
        near("ATH_FNG", 4000.0);
        near("ATH_FNB", 1000.0);
        // The global constant stays what it always was — the mosaic-mixed
        // central-third mean — so a consumer that knows only ATH_FNRM is
        // unaffected by the three above.
        near("ATH_FNRM", 2750.0);
    }

    /// An `IntegrationOutput` with zeroed pixel data and plausible timing/size
    /// fields — enough for `log_build_finished` to compute `read_mb_s` without
    /// a real multi-frame FITS fixture on disk.
    fn nop_output() -> IntegrationOutput {
        IntegrationOutput {
            width: 0,
            height: 0,
            data: Vec::new(),
            rejected_fraction: 0.0,
            flat_norm: None,
            bad_samples_per_frame: Vec::new(),
            all_bad_pixels: 0,
            read_duration: std::time::Duration::from_millis(30_000),
            combine_duration: std::time::Duration::from_millis(14_400),
            band_rows: 20,
            bands: 3,
            bytes_read: 5_200_000_000,
        }
    }

    /// A multi-minute operation that logs nothing is why a running build was
    /// indistinguishable from a hung one (research §8). The message strings
    /// are the contract — `docs/logging/README.md` recipes and the log-mcp
    /// queries match on them.
    #[test]
    fn build_lifecycle_lines_are_emitted_at_info() {
        let (_, events) = capture_events(|| {
            log_build_started(
                42,
                "Dark",
                30,
                "average+winsorized-3.0/3.0",
                4 * 1024 * 1024 * 1024,
                10,
                StorageClass::Local,
            );
        });
        assert!(
            events.iter().any(|(lvl, m)| lvl == "INFO" && m == "master build started"),
            "no start line; got {events:?}"
        );

        let (_, events) = capture_events(|| {
            log_build_finished(42, std::time::Duration::from_millis(44_400), 5_200_000_000, &nop_output());
        });
        assert!(
            events.iter().any(|(lvl, m)| lvl == "INFO" && m == "master build finished"),
            "no finish line; got {events:?}"
        );
    }
}
