//! The stacking run thread (spec §8: "the permit is acquired first, so a
//! queued run waits behind an analysis or a master build"; ruling 2:
//! `start_stacking` returns `{ runId }` only). [`start_stacking`] validates
//! the plan, inserts the run + its group rows, registers a cancel handle on
//! [`crate::services::ServiceContext::active_stacks`] and spawns a dedicated
//! `stacking-run-<id>` thread; [`run_thread`] is that thread's single body —
//! catch-unwind around [`run_pipeline`], handle removal, `finish_run`,
//! writing `runs/run-<id>.json`, the terminal log lines, and exactly one
//! `stacking-complete` event, in that order, regardless of how the pipeline
//! ended. [`cancel_stacking`] just flips the registered cancel flag — the
//! thread notices it (inside the queue wait, or at [`RunContext::check_cancel`]
//! between frames/groups) and unwinds through [`RunError::Cancelled`].
//!
//! Plan 5a Task 6 lands the run thread itself, the progress/complete events,
//! and stage 1 (calibrate) — the calibrated-lights export's own generator
//! (`export::calibrated_generator`), reused verbatim, with per-frame
//! `stacking_artifacts` reuse keyed by [`crate::stacking::plan::calibration_hash_for`]'s
//! SAME hash the plan gate computes (ruling: a run and the plan that
//! preceded it must never disagree about what "fresh" means). Tasks 7-8 add
//! stages 3-9 to the SAME [`run_pipeline`]/[`RunContext`] this task defines —
//! see each item's doc comment for exactly what is provisioned now and
//! consumed later.
//!
//! The stages are this folder's files, each with its tests beside it in `<file>_tests.rs`: `masters` (stage 0.5),
//! `calibrate` and `prepare` (stage 1), `solve` and `measure` (stage 3), `reference` (stage 4), `register`,
//! `two_pass` and `reweigh` (stage 5), `normalize` (stage 6), `output` (stages 7 to 9) and `crop`. `fan_out` is the
//! worker pool and the memory admission the stages share; `frames` is the frame rows and the frame-level file
//! names. This file keeps the run thread, [`RunContext`], the in-memory frame entries and the events; `tests.rs`
//! keeps the rig every stage's tests use and the tests of a run as a whole.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::api::stacking::MasterLightKind;
use crate::api::{db, ApiError, PathPolicy};
use crate::calibration_library::cosmetic::{hot_pixel_map_from_dark, HotPixelMapOutcome};
use crate::calibration_library::light_cal::{preload_master_plane, PreloadedMasters};
use crate::db::stacking::{
    finish_run, get_run, insert_group, insert_master_light, insert_run,
    set_frame_rejected_fraction, set_run_reference, set_run_status, update_group, upsert_artifact,
    upsert_frame_row, ArtifactKind, GroupUpdate, NewArtifact, NewFrameRow, NewGroup, NewMasterLight,
    NewRun, StackSubject,
};
use crate::events::{emit_event, ProgressEmitter};
use crate::export::{
    execute_generation, CalibratedLightOptions, GeneratedLight,
    GenerationSpec,
};
use crate::fits_parser::FitsHeader;
use crate::fits_writer::wcs::scale_plate_solve;
use crate::fits_writer::{Card, CardValue};
use crate::geometry::{Linear, PixelMap};
use crate::integration::band_budget::total_ram_bytes;
use crate::integration::engine::EngineProgress;
use crate::integration::io_policy::IoPolicy;
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::{NormalizationPair, RejectionNormalization};
use crate::integration::IntegrationError;
use crate::plate_solve::storage::PlateSolveRecord;
use crate::registration::db::RegistrationRecord;
use crate::resample::Interpolation;
use crate::services::compute_queue::ComputeJobKind;
use crate::services::machine;
use crate::services::{ServiceContext, StackHandle};
#[cfg(test)]
use crate::stacking::config::config_hash;
use crate::stacking::config::{CleanupPolicy, ReferenceMode, StackingConfig};
use crate::stacking::common_set;
use crate::stacking::crop::{restate_crpix, union_rects, CropRect};
use crate::stacking::drizzle::{
    drizzle_group, DrizzleError, DrizzleFrame, DrizzleInput, DrizzleProgress, DrizzleStats,
};
use crate::stacking::fits_artifact;
use crate::stacking::grid_map::{
    compose_to_grid, crop_plate_solve, grid_registered_hash, subject_dims, GridRun, ReferenceWcs,
};
use crate::stacking::groups::{frame_source, ColorMode, GroupFrame, IntegrationGroup};
use crate::stacking::integrate::{
    included_after_min_weight, integrate_group, large_scale_passes, GroupInput, GroupProgress,
    GroupStats, StackFrame,
};
use crate::stacking::ln::{
    self, build_reference as build_ln_reference, build_reference_saturation, normalize_frame,
    read_reference, read_reference_saturation_masks, write_reference, write_reference_saturation,
    BackgroundModel, LnError, LnFrameGrids, LnReference, LnReferenceForDetection, LnScaleSeeds,
    SaturationMask, TileTable, CLEANING,
};
use crate::stacking::master_cards::{
    build_drizzle_cards, build_master_light_cards, cards_row_order_is_bottom_up, exposure_label,
    master_file_name, remove_partial_outputs, stamp_grid_cards, write_cropped_drizzle,
    write_cropped_master, write_drizzled_master, write_master_light, GridStamp, MasterCardInputs,
    WrittenDrizzle, WrittenMaster,
};
use crate::stacking::measure::{
    channel_c_inputs, channel_c_summary, substitute_group_c, ChannelMeasurement, check_pass1_head, measure_frame_pass1, measure_frame_pass2,
    measure_frame_with_fits, FrameMeasurement, FramePass1, MeasureOptions,
};
use crate::stacking::pass1_artifact::{
    read_pass1, read_pass1_head, write_pass1, Pass1ArtifactError, Pass1Head,
};
use crate::stacking::origin::{cfa_geometry, origin_of, stored_solve, FrameOrigin, FrameOrigins};
use crate::stacking::paths::{
    cleanup_work, cleanup_work_with, subject_layout, CleanupWhat, WorkingLayout,
    LN_REFERENCE_SATURATION_FILE,
};
use crate::stacking::plan::{
    build_plan, fits_artifact_fresh, fits_artifact_fresh_for_planes, is_fresh,
    measurement_hash_for, normalization_hash_for, pass1_hash_for, pass1_own_betas,
    registration_hash_for,
    registration_row_is_fresh, seeds_calibration_hash_for, store_registration,
    stored_reference_frame, stored_registrations, wants_cfa_mosaic, HashMemo, LnReferencePayload,
    MasterWork, PlanMaster, Stage,
};
use crate::stacking::provenance::{
    MasterBuilt, RunSummary, SummaryFrame, SummaryGroup, SummaryMeasurement, SummaryReference,
};
use crate::stacking::psf_signal;
use crate::stacking::rebase::RebaseView;
use crate::stacking::register::align::{self, SeedKind, SeedPolicy};
use crate::stacking::register::detect::stars_from_fits;
use crate::stacking::register::frame::{
    detect_frame_stars, identity_registration, reference_stars, register_detected, to_record,
    DetectedStars, ReferenceStars,
};
use crate::stacking::register::wcs_seed::{ratio_wants_seed, seed_from_solves};
use crate::stacking::register::writer::{
    build_registered_cards, source_cards_from_file, write_registered_frame, RegisteredCards,
};
use crate::stacking::register::{
    low_inlier_frames, scale_gate_for, scale_ratio_for, RegistrationGeometry,
};
use crate::stacking::rej::RejBitmapSet;
use crate::stacking::weights::{
    best_by_weight, compute_weights, defers_weight_gate, reference_coverage, select_frames,
    select_frames_deferring_weight_gate, sky_penalized_order, two_pass_deviation_px,
    two_pass_pick, two_pass_pick_among, weight_gate, FrameWeight, TwoPassCandidate, WeightInput,
    WeightMode, MIN_REFERENCE_COVERAGE, TWO_PASS_MIN_GAIN_PX, TWO_PASS_PICK_VERSION,
};

// The stages of the run, one file each. Every file opens with `use super::*`, and this file takes their items back
// through the globs below, so a name resolves as it did when all of it was one file.
mod frames;
mod fan_out;
mod masters;
mod calibrate;
mod prepare;
mod solve;
mod measure;
mod reference;
mod register;
mod two_pass;
mod reweigh;
mod normalize;
mod output;
mod crop;
pub(crate) use frames::*;
pub(crate) use fan_out::*;
use masters::*;
pub(crate) use calibrate::*;
use prepare::*;
use solve::*;
pub(crate) use measure::*;
use reference::*;
pub(crate) use register::*;
pub(crate) use two_pass::*;
use reweigh::*;
pub(crate) use normalize::*;
use output::*;
pub(crate) use crop::*;

/// Wire event for `stacking-progress`. `percent` is `100 * current / total`
/// (`100.0` when `total == 0`); `bytes_done`/`bytes_total` describe the
/// current STAGE's byte footprint, not the whole run's.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingProgressEvent {
    pub run_id: i64,
    // Exactly one of `set_id` / `project_id` is set (plan 6b-2 C1).
    pub set_id: Option<i64>,
    pub project_id: Option<String>,
    pub stage: Stage,
    pub group_key: Option<String>,
    pub current: usize,
    pub total: usize,
    pub percent: f64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub frame_id: Option<i64>,
    pub message: Option<String>,
}

/// One group's written master, for `StackingCompleteEvent::masters` (ruling
/// 15: per-group masters, listing every group that actually wrote one).
/// Populated by Task 8's Output stage — always empty in this task, since
/// stage 1 (calibrate) never writes a master.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingMasterRef {
    pub group_key: String,
    pub path: String,
    pub drizzle_path: Option<String>,
}

/// Wire event for `stacking-complete` — emitted exactly once per run, from
/// [`run_thread`]'s single exit path, regardless of success/cancel/failure/
/// panic.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingCompleteEvent {
    pub run_id: i64,
    // Exactly one of `set_id` / `project_id` is set (plan 6b-2 C1).
    pub set_id: Option<i64>,
    pub project_id: Option<String>,
    pub success: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub masters: Vec<StackingMasterRef>,
    /// Keys of the groups the run summary lists without a master (skipped or
    /// failed); wire-only, never stored.
    pub groups_without_master: Vec<String>,
}

pub const STACKING_PROGRESS_EVENT: &str = "stacking-progress";
pub const STACKING_COMPLETE_EVENT: &str = "stacking-complete";
pub const PROGRESS_THROTTLE_MS: u64 = 300;

/// [`start_stacking`]'s success payload. Ruling 2: the `ComputeQueue` permit
/// is acquired INSIDE the run thread, so no queue job id exists yet when
/// this returns — the sidebar finds the job by its `"Stacking · <set name>"`
/// label in the queue snapshot instead.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StartedStacking {
    pub run_id: i64,
}

/// A stacking run's terminal outcome, as far as `run_pipeline`/its stages are
/// concerned. `Other`'s `String` is the human-readable failure — already
/// formatted (`{e:#}`-style) by whichever stage produced it, since
/// [`RunContext`] never carries the original error TYPE across a stage
/// boundary, only its text.
#[derive(Debug)]
pub(crate) enum RunError {
    Cancelled,
    Other(String),
    /// M2 Task 7 (carry-over (b) from Task 5's re-review): a DB error while
    /// persisting a stage-6 exclusion (`exclude_frame_and_persist`), raised
    /// AFTER that call already flipped the frame's in-memory
    /// `MeasuredFrame::included` — the frame's state is now inconsistent
    /// (excluded in memory, possibly not on disk), so the caller must fail
    /// the group outright rather than treat this like any other recoverable
    /// local-normalization error and continue with global normalization as
    /// if the frame were still a normal member. Constructed nowhere else;
    /// never escapes `process_group_output`, which converts it to a
    /// `fail_group` call — the arm on the top-level status match below is
    /// exhaustiveness only.
    ExclusionPersistFailed(String),
}

impl From<ApiError> for RunError {
    fn from(e: ApiError) -> Self {
        RunError::Other(e.to_string())
    }
}

impl From<anyhow::Error> for RunError {
    fn from(e: anyhow::Error) -> Self {
        RunError::Other(format!("{e:#}"))
    }
}

/// One stacking run's whole mutable state, held by the dedicated
/// `stacking-run-<id>` thread for its entire life. Built once in
/// [`start_stacking`] (or, in tests, by [`test_context`]) and threaded
/// through every stage function by `&mut` — this is the "RunContext" Tasks
/// 7-8 extend with their own stages, reading/writing the SAME fields.
///
/// Fields beyond the task-6 brief's literal interface (documented individually
/// below): `group_ids`, `memo`, `hot_maps`, `runtime_exclusions`, and the
/// `#[cfg(test)]` `fail_after_stage` hook.
pub(crate) struct RunContext {
    pub(crate) ctx: Arc<ServiceContext>,
    pub(crate) emitter: Arc<dyn ProgressEmitter>,
    pub(crate) run_id: i64,
    /// What this run stacks; every persistence call keys on it.
    pub(crate) subject: StackSubject,
    pub(crate) set_name: String,
    pub(crate) config: StackingConfig,
    pub(crate) hash: String,
    pub(crate) plan_groups: Vec<IntegrationGroup>,
    /// Stage 0.5's work list (spec §2 row 0.5, owner requirement 2026-09-09),
    /// as resolved by the plan gate at `start_stacking` time — the SAME list
    /// `stage_masters` executes, in order. Empty when there is nothing to
    /// build or rebuild.
    pub(crate) masters_to_build: Vec<PlanMaster>,
    /// Manually-excluded frame ids (spec §9.1's `stacking_set_config.excluded_frame_ids`,
    /// already resolved by the plan gate) — a frame in here is skipped by
    /// every stage, never calibrated/measured/registered/integrated.
    pub(crate) excluded: Vec<i64>,
    /// `group_key -> stacking_run_groups.id`, populated by [`start_stacking`]
    /// right after [`insert_run`] so every later stage (`upsert_frame_row`'s
    /// `group_id`, `update_group`) has the row id without a second DB
    /// round-trip. Read by stages 3 and 5 (Task 7's `stage_measure`/
    /// `write_frame_rows`).
    pub(crate) group_ids: HashMap<String, i64>,
    pub(crate) layout: WorkingLayout,
    /// The run's output folder — every group's master (and, when requested,
    /// its rejection maps) lands directly here (Task 8's Output stage).
    pub(crate) output_dir: PathBuf,
    pub(crate) cancel: Arc<AtomicBool>,
    /// Seeds `summary.app_version` at construction; Task 8's master-card
    /// headers (`MasterCardInputs::app_version`, the `SWCREATE` card) read it
    /// again.
    pub(crate) app_version: String,
    pub(crate) warnings: Vec<String>,
    pub(crate) timings: Vec<crate::stacking::provenance::StageTiming>,
    pub(crate) summary: RunSummary,
    pub(crate) last_emit: Instant,
    pub(crate) rerun_from: Option<Stage>,
    /// This run's ONE [`HashMemo`] (decision 3): the calibrate stage's stage-1
    /// hash lookups AND its `resolve_generation_cached` calls share the same
    /// [`crate::export::DivisorCache`] via [`HashMemo::divisors_mut`].
    pub(crate) memo: HashMemo,
    /// Where each frame of this run comes from (see [`crate::stacking::origin`]):
    /// every per-frame catalog read after the frame source dispatches on it.
    pub(crate) origins: FrameOrigins,
    /// The Solve sub-step's solver (plan 6b-2 C14): the installed catalog,
    /// opened on first use. Tests inject a fake.
    pub(crate) solver: Arc<dyn crate::stacking::solve::FrameSolver>,
    /// A project run's reference candidates, set by stage 4 (spec §5.4).
    /// `None` for a frame set, which has no WCS rule.
    pub(crate) wcs_rule: Option<WcsRule>,
    /// A project run's canonical grid (spec §5.5, plan 6b-3): every group is
    /// delivered in its geometry, through one composed map per frame.
    /// `None` for a frame set.
    pub(crate) grid: Option<Arc<crate::stacking::grid_map::GridRun>>,
    /// This run's ONE hot-pixel-map cache, keyed by resolved master dark path
    /// (decision 4) — a dark shared by every frame in a group pays the
    /// measurement once.
    pub(crate) hot_maps: HashMap<PathBuf, Arc<HotPixelMapOutcome>>,
    /// `frame_id -> whether stage 1 REUSED an existing `calibrated` artifact
    /// for it` (Task 8's own addition — the brief's `SummaryFrame.cached_calibrated`
    /// has no field on [`MeasuredFrame`] to read it from otherwise, since
    /// `CalibrateResolved` is transient and stage 3 builds `MeasuredFrame`
    /// fresh from the DB, blind to which run wrote the artifact it found).
    /// Populated once per frame by [`stage_calibrate`]; read by
    /// [`stage_measure`] when it builds each frame's [`MeasuredFrame`].
    pub(crate) cached_calibrated: HashMap<i64, bool>,
    /// Frames excluded AT RUN TIME (as opposed to `excluded`'s manual,
    /// pre-run exclusions), with why — stage 1 pushes `(frame_id,
    /// "calibration failed: …")` here; stages 3 and 5 (Task 7) push their
    /// own failures the same way; `write_frame_rows` (end of stage 5) reads
    /// the union back to build `stacking_run_frames` rows.
    pub(crate) runtime_exclusions: Vec<(i64, String)>,
    /// Group key -> every one of that group's frames' stage 3-5 lifecycle
    /// (Task 7's own addition — the brief's `MeasuredFrame`/
    /// `RegisteredFrameOutcome` shapes, see their doc comments below for the
    /// deviations from the brief's literal, non-`Option` field types). Built
    /// once per group at the end of stage 3, mutated in place by stage 5 with
    /// each frame's registration outcome. Task 8's integrate stage reads this
    /// directly: the per-group INCLUDED frames' calibrated paths,
    /// measurements and `PixelMap`s all live here.
    pub(crate) measured: HashMap<String, Vec<MeasuredFrame>>,
    /// Group key -> that group's stage 3 summary beyond the per-frame list
    /// above (Tier C Task 1) — today just the resolved group β. Built at
    /// the same point as the `measured` entry for that group; a group
    /// never in this map measured no frame at all (or the run never
    /// reached stage 3).
    pub(crate) measured_groups: HashMap<String, MeasuredGroup>,
    /// The run's chosen reference (stage 4) — `None` until stage 4 runs.
    pub(crate) reference_frame_id: Option<i64>,
    /// The reference frame's own calibrated file (stage 4 looks this up from
    /// `measured`, once, so stage 5 does not need to search for it again).
    pub(crate) reference_calibrated: Option<PathBuf>,
    /// The RUN-wide reference's own measured geometry — set by stage 5 from
    /// `reference_stars`'s own read of that frame's calibrated file (the
    /// natural place: stage 5 is what calls `reference_stars`; stage 4 only
    /// picks WHICH frame is the reference, not its pixel geometry).
    ///
    /// M4b (ruling R-M4b-5): no stage reads this pair for its own geometry
    /// any more — every consumer goes through
    /// [`RunContext::geometry_of`] instead. It survives as the run-wide
    /// fact [`adopt_co_registered_geometry`] copies into every group's
    /// entry in co-registered mode, and as the geometry of whichever group
    /// holds the run's reference in native mode.
    pub(crate) reference_width: usize,
    pub(crate) reference_height: usize,
    /// Every group's own registration reference and delivered geometry
    /// (M4b ruling R-M4b-5) — one entry per `plan_groups` group, inserted
    /// by [`resolve_group_geometry`] at the end of stage 4 and refreshed
    /// from each reference's own `reference_stars` in stage 5.
    ///
    /// In `coRegistered` mode every entry names the SAME (run-wide)
    /// reference and the same geometry, so every consumer below reads one
    /// number whichever mode the run is in and the M1–M4a behaviour is
    /// reproduced exactly; in `native` mode each group carries its own.
    pub(crate) group_geometry: HashMap<String, GroupGeometry>,
    /// Perf tier C Task 2 fix round 4 (ruling C-14 item 2): each group's
    /// measured seeds calibration, recorded by stage 6 and read by
    /// [`push_summary_group`] so the run's provenance says what `k` its
    /// sidecars were written under. Absent for a group whose LN never ran,
    /// or one whose calibration could not be measured at all.
    pub(crate) group_seeds_calibration: HashMap<String, ln::SeedsCalibration>,
    /// Plan B B0 (PB-2, R-PB0-7): each group's LN reference aperture correction `c` per plane, recorded by stage 6 from
    /// the prepared reference channels and read by [`push_summary_group`] (`SummaryGroup::ln_reference_aperture_correction`).
    /// A plane at floor (i) holds `None` (`null` in the summary), never the applied `1.0`. Absent for a group whose LN
    /// never reached the reference.
    pub(crate) group_ln_reference_c: HashMap<String, Vec<Option<f64>>>,
    /// Plan D D1w: each reweighed group's frame-common star set, per plane, in group coordinates
    /// ([`crate::stacking::common_set::CommonOutcome::Measured::positions`]), recorded by [`reweigh_groups`] at the end of
    /// stage 5 for D1b's SNR gain. A plane that fell back (PD-6) holds `None`, and a group whose every plane fell back
    /// still holds its record (`[None, ...]`); a group never reweighed (no aligned member) is absent. Recorded in every
    /// weight mode (PD-19 b): the set is a property of the group's frames, not of the weights.
    pub(crate) common_stars: HashMap<String, Vec<Option<Vec<(f64, f64)>>>>,
    /// Per group, the LN scale the start plan reported (spec 2026-10-04 §2.3,
    /// P-14); a run whose own resolution differs warns once.
    pub(crate) planned_ln_scales: HashMap<String, u32>,
    /// M3 Task 5, fix round 1 (Minor M6): incremented once per group that
    /// actually reaches an attempt at `drizzle_group` (i.e. drizzle is on
    /// AND the group's `RejBitmapSet` — when wanted — was created
    /// successfully), regardless of whether the attempt then succeeds,
    /// fails, or is refused by the `output_pairs` shape check. `stage_output`
    /// reads it after the group loop to decide whether to push a
    /// `Stage::Drizzle` `StageTiming` — an EXACT count, unlike the `any_master`
    /// proxy that used to gate it (a run where every group's bitmap set
    /// failed to create would push a `0 ms` `Drizzle` timing under that
    /// proxy despite `drizzle_group` never having run at all).
    pub(crate) drizzle_attempted: usize,
    /// Plan 6b-3 D10: the groups a crop run has written to its scratch
    /// folder, finalized by [`finish_crop`] after the last group. Always
    /// empty when `output.cropToCoverage` is 0.
    pub(crate) crop_items: Vec<CropItem>,
    /// Perf tier 1 Task 9: the two-pass dry pass's own detections
    /// (`frame_id -> Arc<DetectedStars>`), kept only long enough for the
    /// persisting pass right after it to reuse them instead of re-reading
    /// and re-detecting the same frame. [`register_group_pass`] inserts
    /// into this when it runs `persist = false`, and removes an entry as
    /// the PERSISTING pass builds its `RegisterItem`s: every frame it adds
    /// to `to_register` consumes its own entry there, the reference's
    /// included (whether or not the two-pass pick switched to it), so it
    /// drains itself as the persisting pass runs. [`stage_register`]
    /// clears whatever is left once the whole stage is done (belt and
    /// braces) — two real cases leave something behind: a frame whose
    /// stored registration row is already fresh skips `to_register`
    /// entirely and is REUSED instead of rebuilt, so nothing ever consumes
    /// its dry-pass entry (including a reused reference row — the frame
    /// the two-pass pick just switched to may already carry a fresh row
    /// from an earlier run); and a group that never reaches a persisting
    /// pass at all — cancelled mid-stage — would otherwise leak whatever
    /// the dry pass cached for it.
    pub(crate) dry_pass_stars: HashMap<i64, Arc<DetectedStars>>,
    /// Test-only fault injection: [`run_pipeline`] panics right after the
    /// named stage completes, so [`run_thread`]'s catch-unwind/single-exit-path
    /// contract can be exercised without a real failure anywhere in the
    /// pipeline itself.
    #[cfg(test)]
    pub(crate) fail_after_stage: Option<Stage>,
}

/// A project run's reference candidates (spec §5.4, plan 6b-2 C15).
#[derive(Debug, Clone, Default)]
pub(crate) struct WcsRule {
    /// Frames that may be the reference: those with a manifest WCS or, only
    /// when no included frame has one, those the run solved locally.
    pub(crate) candidates: HashSet<i64>,
    /// Every measured frame with any WCS (what a manual pin needs).
    pub(crate) with_wcs: HashSet<i64>,
}

/// One group's registration reference and the geometry everything from
/// stage 5 on works in (M4b ruling R-M4b-5): the frame every member of the
/// group is warped onto, the pixel grid the master is delivered in, that
/// reference's own calibrated file and its stage-1 hash (the two inputs
/// `registration_hash_for` needs beyond the frame's own).
#[derive(Debug, Clone)]
pub(crate) struct GroupGeometry {
    pub(crate) reference_frame_id: i64,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) calibrated: PathBuf,
    pub(crate) hash: String,
}

impl RunContext {
    /// This group's [`GroupGeometry`]. Panics — with the key — when the
    /// group has no entry: [`resolve_group_geometry`] inserts one for EVERY
    /// `plan_groups` group at the end of stage 4 (including a group with no
    /// included frame, which falls back to the run-wide reference), so a
    /// miss is a programming error in the pipeline's own wiring, not a
    /// condition any run can reach.
    pub(crate) fn geometry_of(&self, key: &str) -> &GroupGeometry {
        self.group_geometry
            .get(key)
            .unwrap_or_else(|| panic!("no group geometry resolved for group {key}"))
    }

    /// `Err(RunError::Cancelled)` iff the run's cancel flag is set. Called
    /// between frames and between groups so a cancel lands promptly without
    /// polling inside the hot per-frame loop.
    fn check_cancel(&self) -> Result<(), RunError> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(RunError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// One Output-stage tick of the crop pass (`current` of `total` groups
    /// finalized; `group_key` empty on the closing tick). Same throttle as
    /// every stage; the first and last always go out.
    fn crop_progress(&mut self, group_key: &str, current: usize, total: usize) {
        let key = (!group_key.is_empty()).then(|| group_key.to_string());
        self.progress(
            Stage::Output,
            key,
            current,
            total,
            0,
            0,
            None,
            Some("cropping".to_string()),
        );
    }

    /// Emit `stacking-progress`, throttled (decision 6): `current == 0`,
    /// `current == total`, or at least [`PROGRESS_THROTTLE_MS`] since the
    /// last emission for ANY stage — so the first and last events of every
    /// stage always go out even if the whole stage runs in under 300 ms.
    #[allow(clippy::too_many_arguments)]
    fn progress(
        &mut self,
        stage: Stage,
        group_key: Option<String>,
        current: usize,
        total: usize,
        bytes_done: u64,
        bytes_total: u64,
        frame_id: Option<i64>,
        message: Option<String>,
    ) {
        let percent = if total == 0 {
            100.0
        } else {
            100.0 * current as f64 / total as f64
        };
        let now = Instant::now();
        let force = current == 0 || current == total;
        if !force
            && now.duration_since(self.last_emit) < Duration::from_millis(PROGRESS_THROTTLE_MS)
        {
            return;
        }
        self.last_emit = now;
        let (set_id, project_id) = wire_subject(&self.subject);
        emit_event(
            self.emitter.as_ref(),
            STACKING_PROGRESS_EVENT,
            &StackingProgressEvent {
                run_id: self.run_id,
                set_id,
                project_id,
                stage,
                group_key,
                current,
                total,
                percent,
                bytes_done,
                bytes_total,
                frame_id,
                message,
            },
        );
    }
}

fn color_mode_wire(mode: ColorMode) -> &'static str {
    match mode {
        ColorMode::Mono => "mono",
        ColorMode::Osc => "osc",
    }
}

fn reference_mode_wire(mode: ReferenceMode) -> &'static str {
    match mode {
        ReferenceMode::Auto => "auto",
        ReferenceMode::Manual => "manual",
    }
}

/// `stacking_run_groups.width`/`height` at insert time (owner decision
/// 2026-09-10: `IntegrationGroup` no longer carries a group-level
/// width/height — a group's members can differ in native geometry now that
/// camera/geometry are not grouping keys). The run's actual reference
/// geometry (`RunContext::reference_width`/`height`) is not known this
/// early — it is only resolved once stage 5 (register) picks the ONE
/// run-wide reference frame — so this reads the group's own
/// reference-anchor member's native `NAXIS1`/`NAXIS2` instead: the same
/// "first member, `(date_obs, id)` order" convention `IntegrationGroup.instrume`
/// already uses for its own display value. `None` only for an (unreachable
/// in practice) empty group — the DB column stays nullable either way.
///
/// `pub(crate)` since M3 Task 5: `stacking::plan`'s own `PlanGroup.anchor_width`/
/// `anchor_height` reuse this SAME convention rather than re-deriving it — a
/// plan and the run it precedes must never disagree about which member
/// anchors a group's geometry.
pub(crate) fn group_anchor_geometry(g: &IntegrationGroup) -> (Option<i64>, Option<i64>) {
    match g.frames.first() {
        Some(f) => (Some(f.width), Some(f.height)),
        None => (None, None),
    }
}

/// `(size, modified_at)` for a just-written file, in the scanner's own shape
/// (`scanner/mod.rs`'s `modified_dt.to_rfc3339()`) — the same identity
/// [`crate::stacking::plan::calibration_hash_for`] reads back off a
/// `stacking_artifacts` row's `size`/`modified_at`. Only the size decides freshness
/// ([`crate::stacking::plan::file_matches_identity`]); the time is what the working-folder rebase compares (size and
/// time within a tolerance, [`crate::stacking::rebase`]) to recognise a moved folder's file, so it is still recorded.
fn file_identity(path: &Path) -> anyhow::Result<(i64, String)> {
    let meta = std::fs::metadata(path)?;
    let modified_at = crate::stacking::plan::render_modified_at(meta.modified()?);
    Ok((meta.len() as i64, modified_at))
}

/// The subject pair every wire payload carries — `setId` for a frame set,
/// `projectId` for a project, never both (plan 6b-2 C1). The frontend keys
/// its maps by `set:<id>` / `project:<uuid>` (spec §9.2, wave 6c).
pub(crate) fn wire_subject(subject: &StackSubject) -> (Option<i64>, Option<String>) {
    (
        subject.frame_set_id(),
        subject.project_id().map(str::to_string),
    )
}

/// Heal every `stacking_runs` row a crashed or killed process left
/// `planning`/`running` forever (final fix wave, Critical item 1): such a
/// row has no live cancel handle in [`ServiceContext::active_stacks`] (that
/// map is process-memory only — a restart always starts it empty), so
/// [`start_stacking`]'s own "already active" check and
/// [`crate::api::stacking::cleanup_stacking_work`]'s `Conflict` guard would
/// otherwise refuse forever with no command able to clear the row. Finishes
/// each stuck row as `"failed"` (no summary JSON — the run's own thread is
/// long gone, there is nothing to snapshot) with
/// `error = "interrupted by a restart"`, one `warn!` per row.
///
/// Deliberately entry-point-driven, not host-startup-driven (unlike
/// [`crate::api::sync_prepare::heal_interrupted_preparations`], which the
/// two hosts call once at boot): a stuck stacking run only ever matters to a
/// caller about to read or act on its frame set, and every such caller
/// already goes through one of the `api::stacking` handlers this is wired
/// into (the frame-set ones: `get_stacking_plan`, `start_stacking`,
/// `get_stacking_runs`, `get_stacking_run`, `cleanup_stacking_work`; and the
/// project ones: `get_project_stacking_plan`, `start_project_stacking`,
/// `get_project_stacking_runs`, `cleanup_project_stacking_work`) — a
/// process-wide wire-up
/// would heal rows nobody is about to look at, for no benefit over healing
/// on demand.
///
/// A row WITH a live handle (the normal case — this process's own run still
/// actually running) is left alone: `active` is checked before anything is
/// touched, and only ids missing from it are ever finished.
pub(crate) fn heal_interrupted_runs(
    ctx: &ServiceContext,
    conn: &rusqlite::Connection,
) -> Result<usize, ApiError> {
    let unfinished = crate::db::stacking::list_unfinished_runs(conn)?;
    if unfinished.is_empty() {
        return Ok(0);
    }

    let stuck: Vec<(i64, StackSubject)> = {
        let active = ctx.active_stacks.lock().unwrap();
        unfinished
            .into_iter()
            .filter(|(run_id, _)| !active.contains_key(run_id))
            .collect()
    };

    let mut healed = 0usize;
    for (run_id, subject) in stuck {
        match &subject {
            StackSubject::FrameSet(frames_set_id) => tracing::warn!(
                run_id,
                frames_set_id = *frames_set_id,
                "stacking run interrupted by a restart"
            ),
            StackSubject::Project(project_id) => tracing::warn!(
                run_id,
                project_id = %project_id,
                "stacking run interrupted by a restart"
            ),
        }
        // B12 (M3 final fix wave, M9): a crashed run's `rej/run-<id>` is
        // worthless — nothing will ever finish writing it, and no future
        // run reads a PRIOR run's bitmaps — so it is swept here too,
        // unless the run's own config explicitly asked to keep everything.
        // Fetched BEFORE `finish_run` below (which only ever touches
        // `status`/`finished_at`/`summary_json`/`error`, never `config_
        // json`/`working_dir` — order does not matter, but reads the row
        // in its most complete state regardless).
        if let Err(e) = remove_rej_dir_for_interrupted_run(conn, run_id, &subject) {
            match &subject {
                StackSubject::FrameSet(frames_set_id) => tracing::warn!(
                    run_id,
                    frames_set_id = *frames_set_id,
                    error = %e,
                    "stacking run heal: failed to remove rej/run-<id>"
                ),
                StackSubject::Project(project_id) => tracing::warn!(
                    run_id,
                    project_id = %project_id,
                    error = %e,
                    "stacking run heal: failed to remove rej/run-<id>"
                ),
            }
        }
        warn_leftover_uncropped(conn, run_id, &subject);
        finish_run(
            conn,
            run_id,
            "failed",
            None,
            Some("interrupted by a restart"),
        )?;
        healed += 1;
    }
    Ok(healed)
}

/// Plan 6b-3 D10 (fix round 1): a run killed mid-crop can leave its
/// `runs/run-<id>-uncropped` folder holding the only copy of a master. It is
/// never removed (cleanup skips it): this names it so the user can find it.
fn warn_leftover_uncropped(conn: &rusqlite::Connection, run_id: i64, subject: &StackSubject) {
    let row = match crate::db::stacking::get_run(conn, run_id) {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(run_id, error = %error, "stacking run heal: reading the run failed");
            return;
        }
    };
    let layout = match subject_layout(conn, Path::new(&row.working_dir), subject) {
        Ok(layout) => layout,
        Err(error) => {
            tracing::warn!(
                run_id,
                error = %error,
                "stacking run heal: resolving the layout failed"
            );
            return;
        }
    };
    let dir = layout.uncropped_dir(run_id);
    if dir.exists() {
        tracing::warn!(
            run_id,
            path = %dir.display(),
            "interrupted crop run left masters in its scratch folder; never removed automatically"
        );
    }
}

/// [`heal_interrupted_runs`]'s own rej-sweep for one stuck row: resolve the
/// run's `cleanup` policy from whichever of `summary_json`/`config_json` the
/// row carries (a stuck row normally has no `summary_json` yet — it never
/// reached `finish_run` — so `config_json`, the frozen per-run config
/// `start_stacking` wrote, is the usual source), and remove `rej/run-<id>`
/// unless that policy is explicitly `KeepAll`. Unparseable/missing config on
/// BOTH fields is treated as "unavailable, remove it" (B12's own ruling: a
/// crashed run's bitmaps are worthless either way). Never fails the heal
/// itself — every error here is a caller-side `warn!`, not a propagated
/// `Err`.
fn remove_rej_dir_for_interrupted_run(
    conn: &rusqlite::Connection,
    run_id: i64,
    subject: &StackSubject,
) -> anyhow::Result<()> {
    let Some(row) = crate::db::stacking::get_run(conn, run_id)? else {
        return Ok(()); // the row vanished between listing and healing — nothing to sweep
    };

    let cleanup = row
        .summary_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<RunSummary>(s).ok())
        .map(|s| s.config.output.cleanup)
        .or_else(|| {
            serde_json::from_str::<StackingConfig>(&row.config_json)
                .ok()
                .map(|c| c.output.cleanup)
        });

    // `None` (neither field parsed) is "unavailable" per B12's ruling —
    // falls through to the removal below, same as any non-`KeepAll` policy.
    if cleanup == Some(CleanupPolicy::KeepAll) {
        return Ok(());
    }

    let layout = subject_layout(conn, Path::new(&row.working_dir), subject)?;
    let dir = layout.rej_run_dir(run_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("removing {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// RAII guard for [`start_stacking`]'s `active_stacks` handle (Plan 5b final
/// fix wave, review finding B2) — same shape as
/// [`crate::api::masters::ActiveBuildGuard`]. Armed the instant the handle is
/// registered, covering everything from there through the thread spawn:
/// `get_run`, the per-group `insert_group` loop, and the spawn itself can
/// all still fail. Before this guard existed, such a failure returned via
/// `?` straight past the handle-removal / row-finishing cleanup the
/// spawn-failure path already had — the handle stayed in `active_stacks`
/// and the `stacking_runs` row stayed `"planning"` forever, so
/// [`heal_interrupted_runs`] (which only heals rows ABSENT from
/// `active_stacks`) would never touch it and every later `start_stacking`
/// for the same set answered `Conflict` until the process restarted.
///
/// Dropped while still armed: removes the handle and finishes the run row
/// as `"failed"`, with [`Self::fail`]'s message when one was recorded (every
/// call site on the risky path calls it right before returning `Err`) or a
/// generic fallback otherwise. Disarmed once `spawn_result` is `Ok` — from
/// there, [`run_thread`]'s own single exit path owns both.
struct StartStackingGuard<'a> {
    ctx: &'a ServiceContext,
    run_id: i64,
    armed: bool,
    error: Option<String>,
}

impl<'a> StartStackingGuard<'a> {
    fn new(ctx: &'a ServiceContext, run_id: i64) -> Self {
        Self {
            ctx,
            run_id,
            armed: true,
            error: None,
        }
    }

    /// Record the error that is about to unwind past this guard via `?` —
    /// `Drop::drop` has no other way to see it.
    fn fail(&mut self, error: impl std::fmt::Display) {
        self.error = Some(error.to_string());
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StartStackingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.ctx.active_stacks.lock().unwrap().remove(&self.run_id);
        let error = self.error.clone().unwrap_or_else(|| {
            "stacking run setup failed before the run thread could start".to_string()
        });
        match db(self.ctx) {
            Ok(database) => {
                let conn = database.conn();
                if let Err(e) = finish_run(&conn, self.run_id, "failed", None, Some(&error)) {
                    tracing::warn!(
                        run_id = self.run_id,
                        error = %e,
                        "failed to mark stacking run failed after a setup failure"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    run_id = self.run_id,
                    error = %e,
                    "failed to open a connection to mark stacking run failed after a setup failure"
                );
            }
        }
    }
}

/// Start a stacking run for a subject (a frame set or a project): build the
/// plan, refuse on the first blocker or an already-active run, insert the
/// run + its group rows, register a cancel handle, and spawn the dedicated
/// thread. Returns as soon as the thread is spawned — the queue permit is
/// acquired INSIDE the thread (ruling 2).
pub fn start_stacking(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    policy: &PathPolicy,
    app_version: String,
    subject: StackSubject,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, ApiError> {
    let solver: Arc<dyn crate::stacking::solve::FrameSolver> =
        Arc::new(crate::stacking::solve::CatalogSolver::new(ctx.clone()));
    start_stacking_with_solver(
        ctx,
        emitter,
        policy,
        app_version,
        subject,
        config,
        rerun_from,
        solver,
    )
}

/// [`start_stacking`] with the Solve sub-step's solver given — the seam the
/// fake-hub e2e uses (spec §15.2: a synthetic star field cannot be solved
/// against a real catalog). Plan 6b-3 D13: a project starts like a frame
/// set; its run stacks onto the plan's confirmed grid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_stacking_with_solver(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    policy: &PathPolicy,
    app_version: String,
    subject: StackSubject,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
    solver: Arc<dyn crate::stacking::solve::FrameSolver>,
) -> Result<StartedStacking, ApiError> {
    let db_handle = db(&ctx)?;
    let conn = db_handle.conn();

    // Critical fix, item 1: heal any row a crashed/killed process left
    // `planning`/`running` BEFORE the plan's own `active_run_id` read below —
    // otherwise a stuck row from a previous process would read as "already
    // active" forever, with no command able to clear it.
    heal_interrupted_runs(&ctx, &conn)?;

    let plan = build_plan(&conn, &ctx.settings, policy, &subject, config)?;
    let planned_ln_scales: HashMap<String, u32> = plan
        .groups
        .iter()
        .filter_map(|g| g.ln_scale_px.map(|s| (g.key.clone(), s)))
        .collect();

    if let Some(blocker) = plan.blockers.first() {
        return Err(ApiError::Invalid(blocker.message.clone()));
    }
    // Advisory only (fix round 1, item 3): catches a row left `planning`/
    // `running` by a crashed process. The REAL guard against a same-process
    // double start is the `active_stacks` lock below — this read can still
    // race against another `start_stacking` call reaching that lock first
    // (both could pass this check), and that is fine: the lock is what
    // actually serializes the run-row insert and the handle registration.
    if let Some(active_id) = plan.active_run_id {
        return Err(ApiError::Conflict(format!(
            "a stacking run (id {active_id}) is already active for {subject}"
        )));
    }

    let working_dir = plan.working_dir.clone().ok_or_else(|| {
        ApiError::Internal("stacking plan reported no blockers but no working folder".to_string())
    })?;
    let output_dir_str = plan.output_dir.clone().ok_or_else(|| {
        ApiError::Internal("stacking plan reported no blockers but no output folder".to_string())
    })?;

    // Final fix wave, item 5: the plan itself validates in `ValidateMode::Plan`
    // — no filesystem write, since `build_plan` above runs on every debounced
    // config edit, not just a real start. A run that is actually about to
    // write into these folders needs them to exist, so `start_stacking` is
    // the ONE place that calls `ValidateMode::Save` — creating both folders
    // (and probing them writable) right before the thread spawns. From here
    // on, `working_dir`/`output_dir_str` are Save mode's own canonicalized,
    // post-creation paths, not the plan's lexical ones.
    let validated_dirs = crate::stacking::paths::validate_dirs(
        &conn,
        policy,
        &working_dir,
        &output_dir_str,
        crate::stacking::paths::ValidateMode::Save,
    )?;
    let working_dir = validated_dirs.working.to_string_lossy().into_owned();
    let output_dir_str = validated_dirs.output.to_string_lossy().into_owned();

    let (plan_groups, origins) = frame_source(&conn, &subject, &plan.config)?;

    // Plan 6b-3 D13: a project run stacks onto the confirmed grid. The plan's
    // `grid` blocker guarantees one; a frame set has none.
    let grid = plan.grid.as_ref().and_then(|g| g.current);
    if subject.project_id().is_some() && grid.is_none() {
        return Err(ApiError::Internal(
            "the project plan reported no blockers but no confirmed grid".to_string(),
        ));
    }

    // The folder the run writes its masters into: the output root for a
    // frame set, `<output>/<title>` for a project. The run row names the
    // same folder, so what the row says is what the run does.
    let output_dir = crate::stacking::paths::subject_output_dir(
        Path::new(&output_dir_str),
        &subject,
        &plan.set_name,
    );
    let output_dir_row = output_dir.to_string_lossy().into_owned();

    let config_json = serde_json::to_string(&plan.config)
        .map_err(|e| ApiError::Internal(format!("failed to serialize stacking config: {e}")))?;

    // Fix round 1, item 3: the "already running" check AND the run-row
    // insert + handle registration happen under ONE `active_stacks` lock —
    // lock; scan for a handle already covering this set → `Conflict`;
    // `insert_run` (a short insert) while still holding the lock; insert
    // the handle; unlock. Two concurrent `start_stacking` calls for the
    // SAME set can never both pass: whichever reaches the lock second sees
    // the first's handle before it can insert its own run row.
    let cancel = Arc::new(AtomicBool::new(false));
    let run_id = {
        let mut active = ctx.active_stacks.lock().unwrap();
        if active.values().any(|h| h.subject == subject) {
            return Err(ApiError::Conflict(format!(
                "a stacking run is already active for {subject}"
            )));
        }
        let run_id = insert_run(
            &conn,
            &NewRun {
                subject: &subject,
                config_json: &config_json,
                config_hash: &plan.config_hash,
                reference_frame_id: plan.reference.frame_id,
                reference_mode: reference_mode_wire(plan.reference.mode),
                working_dir: &working_dir,
                output_dir: &output_dir_row,
            },
        )?;
        active.insert(
            run_id,
            StackHandle {
                cancel_flag: cancel.clone(),
                subject: subject.clone(),
            },
        );
        run_id
    };

    // Fix round: Plan 5b final fix wave, review finding B2. Armed from here
    // through the spawn below — see `StartStackingGuard`'s own doc comment.
    let mut guard = StartStackingGuard::new(&ctx, run_id);

    let started_at = match get_run(&conn, run_id) {
        Ok(row) => row
            .map(|r| r.started_at)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        Err(e) => {
            guard.fail(&e);
            return Err(e.into());
        }
    };

    let included_by_key: HashMap<&str, usize> = plan
        .groups
        .iter()
        .map(|g| (g.key.as_str(), g.included_count))
        .collect();

    let mut group_ids: HashMap<String, i64> = HashMap::with_capacity(plan_groups.len());
    for g in &plan_groups {
        let included_count = included_by_key
            .get(g.key.as_str())
            .copied()
            .unwrap_or(g.frames.len());
        let (anchor_width, anchor_height) = group_anchor_geometry(g);
        let group_id = match insert_group(
            &conn,
            &NewGroup {
                run_id,
                group_key: &g.key,
                instrume: g.instrume.as_deref(),
                color_mode: color_mode_wire(g.color_mode),
                filter: g.filter.as_deref(),
                binning: Some(g.binning),
                width: anchor_width,
                height: anchor_height,
                exposure: g.exposure_s,
                exposure_class: Some(&g.exposure_class),
                frame_count: g.frames.len() as i64,
                included_count: included_count as i64,
            },
        ) {
            Ok(id) => id,
            Err(e) => {
                guard.fail(&e);
                return Err(e.into());
            }
        };
        group_ids.insert(g.key.clone(), group_id);
    }

    let layout = match subject_layout(&conn, Path::new(&working_dir), &subject) {
        Ok(layout) => layout,
        Err(e) => {
            guard.fail(&e);
            return Err(e.into());
        }
    };
    let (set_id, project_id) = wire_subject(&subject);
    let summary = RunSummary {
        run_id,
        set_id,
        project_id,
        set_name: plan.set_name.clone(),
        app_version: app_version.clone(),
        started_at,
        finished_at: None,
        status: "planning".to_string(),
        config: plan.config.clone(),
        config_hash: plan.config_hash.clone(),
        grid,
        reference: SummaryReference {
            frame_id: plan.reference.frame_id,
            filename: plan.reference.filename.clone(),
            mode: plan.reference.mode,
            weight: None,
            switched_from: None,
        },
        measurement: SummaryMeasurement {
            seed_source: plan.config.measurement.seed_source.as_str().to_string(),
            scale_estimator: plan.config.normalization.scale_estimator,
        },
        groups: Vec::new(),
        masters_built: Vec::new(),
        stages: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };

    let rc = RunContext {
        ctx: ctx.clone(),
        emitter: emitter.clone(),
        run_id,
        subject: subject.clone(),
        set_name: plan.set_name,
        config: plan.config,
        hash: plan.config_hash,
        plan_groups,
        masters_to_build: plan.masters_to_build,
        excluded: plan.excluded_frame_ids,
        group_ids,
        layout,
        output_dir,
        cancel,
        app_version,
        warnings: Vec::new(),
        timings: Vec::new(),
        summary,
        last_emit: Instant::now(),
        rerun_from,
        memo: HashMemo::for_run(origins.clone()),
        origins,
        solver,
        wcs_rule: None,
        grid: grid.map(|g| Arc::new(crate::stacking::grid_map::GridRun::new(g))),
        hot_maps: HashMap::new(),
        cached_calibrated: HashMap::new(),
        runtime_exclusions: Vec::new(),
        measured: HashMap::new(),
        measured_groups: HashMap::new(),
        reference_frame_id: None,
        reference_calibrated: None,
        reference_width: 0,
        reference_height: 0,
        group_geometry: HashMap::new(),
        group_seeds_calibration: HashMap::new(),
        group_ln_reference_c: HashMap::new(),
        common_stars: HashMap::new(),
        planned_ln_scales,
        drizzle_attempted: 0,
        crop_items: Vec::new(),
        dry_pass_stars: HashMap::new(),
        #[cfg(test)]
        fail_after_stage: None,
    };

    let spawn_result = std::thread::Builder::new()
        .name(format!("stacking-run-{run_id}"))
        .spawn(move || {
            run_thread(rc);
        });

    match spawn_result {
        Ok(_) => {
            // The thread started — from here on it owns handle removal /
            // `finish_run` / `stacking-complete` via its own single exit
            // path (`run_thread`). Disarm so this guard's `Drop` is a no-op.
            guard.disarm();
        }
        Err(e) => {
            // The thread never started, so nothing will ever remove the
            // handle, finish the run row, or emit stacking-complete —
            // `guard`'s `Drop` (still armed) does that cleanup below.
            let msg = format!("failed to spawn stacking thread: {e}");
            guard.fail(&msg);
            return Err(ApiError::Internal(msg));
        }
    }

    Ok(StartedStacking { run_id })
}

/// Whether a stacking run of a subject OTHER than `subject` is active (registered in `active_stacks`). Two subjects
/// can share a working root, and a run writes a pass-1 / fits file before its row, so a sweep for `subject` must not
/// run meanwhile (ruling R8 (c)).
pub(crate) fn other_stack_active(ctx: &ServiceContext, subject: &StackSubject) -> bool {
    ctx.active_stacks
        .lock()
        .unwrap()
        .values()
        .any(|h| &h.subject != subject)
}

/// Cancel an active stacking run (queued-in-compute-queue or running).
pub fn cancel_stacking(ctx: &ServiceContext, run_id: i64) -> Result<(), ApiError> {
    let active = ctx.active_stacks.lock().unwrap();
    if let Some(handle) = active.get(&run_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err(ApiError::NotFound(format!(
            "no active stacking run {run_id}"
        )))
    }
}

/// Runs on the dedicated `stacking-run-{run_id}` thread. The single exit
/// path for the whole run: `finish_run`, handle removal, the provenance
/// snapshot, the terminal log lines, and `stacking-complete` ALWAYS happen
/// here, exactly once, regardless of how [`run_pipeline`] ended (including a
/// panic inside it). Fix round 1, item 4: `finish_run` runs BEFORE the
/// handle is removed from `active_stacks` — a `cancel_stacking` call that
/// lands in between sees the row already terminal (never `NotFound` while
/// the row still reads `running`); its flag-set is simply never read by
/// anything afterward, which is harmless. The old order (handle removed
/// first) could show a run nowhere in `active_stacks` while its DB row
/// still said `running`, which is the confusing state this avoids.
fn run_thread(mut rc: RunContext) {
    let run_id = rc.run_id;
    let (set_id, project_id) = wire_subject(&rc.subject);
    let run_started = Instant::now();

    // The de-registration itself is RAII (M4a Task 4 fix round 1, item 1):
    // it runs after `finish_run`, the provenance snapshot, the
    // rejection-bitmap cleanup and the `stacking-complete` event — the
    // ordering the tail needs (see this function's own doc) — and it still
    // happens if any of those panics. An explicit removal at the tail would
    // have been skipped by such a panic, leaking the handle:
    // `heal_interrupted_runs` only heals rows ABSENT from `active_stacks`,
    // so every later `start_stacking` for that set would answer `Conflict`
    // until the process restarted — the same failure mode
    // [`StartStackingGuard`] exists to prevent on the setup path.
    //
    // Declaring it first is NOT what makes it last (M4c Task 4 fix round 3,
    // finding 1 — the previous wording said so and was wrong): `rc` is a
    // function PARAMETER and parameters drop after the body's locals, so
    // the guard would have gone first and freed the run's registration
    // while `rc` was still alive. The explicit `drop(rc)` at the tail is
    // what orders them.
    let _handle = RunHandleGuard {
        ctx: rc.ctx.clone(),
        run_id,
    };

    let result =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_pipeline(&mut rc))) {
            Ok(r) => r,
            Err(panic) => {
                let detail = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown".to_string());
                let msg = format!("stacking run panicked: {detail}");
                tracing::error!(run_id, error = %msg, "stacking run thread panicked");
                // The panic skipped `stage_output`'s wrapper: name a crop
                // scratch folder it may have left holding masters.
                let scratch = rc.layout.uncropped_dir(run_id);
                if scratch.exists() {
                    tracing::warn!(
                        run_id,
                        path = %scratch.display(),
                        "panicked crop run left masters in its scratch folder"
                    );
                    rc.warnings.push(format!(
                        "the run panicked; masters may remain in {}",
                        scratch.display()
                    ));
                }
                Err(RunError::Other(msg))
            }
        };

    let (status, success, cancelled, error): (&str, bool, bool, Option<String>) = match &result {
        Ok(()) => ("done", true, false, None),
        Err(RunError::Cancelled) => {
            tracing::info!(run_id, "stacking run cancelled");
            ("cancelled", false, true, None)
        }
        Err(RunError::Other(msg)) | Err(RunError::ExclusionPersistFailed(msg)) => {
            tracing::error!(run_id, error = %msg, "stacking run failed");
            ("failed", false, false, Some(msg.clone()))
        }
    };

    // Ruling 15 / brief: `masters` lists every group that actually wrote one
    // (`SummaryGroup.master_path` is `Some` only then); `success` additionally
    // requires at least one — `stage_output` already fails the whole run when
    // no group ever wrote a master, so this is a redundant-but-cheap safety
    // net, not the primary gate.
    let masters: Vec<StackingMasterRef> = rc
        .summary
        .groups
        .iter()
        .filter_map(|g| {
            g.master_path.clone().map(|path| StackingMasterRef {
                group_key: g.key.clone(),
                path,
                drizzle_path: g.drizzle_path.clone(),
            })
        })
        .collect();
    let groups_without_master: Vec<String> = rc
        .summary
        .groups
        .iter()
        .filter(|g| g.master_path.is_none())
        .map(|g| g.key.clone())
        .collect();
    let success = success && !masters.is_empty();

    rc.summary.status = status.to_string();
    rc.summary.finished_at =
        Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    rc.summary.warnings = rc.warnings.clone();
    rc.summary.stages = rc.timings.clone();
    rc.summary.error = error.clone();

    let summary_json = match serde_json::to_string(&rc.summary) {
        Ok(json) => Some(json),
        Err(e) => {
            tracing::warn!(run_id, error = %e, "failed to serialize stacking run summary");
            None
        }
    };

    match db(&rc.ctx) {
        Ok(db_handle) => {
            let conn = db_handle.conn();
            if let Err(e) = finish_run(
                &conn,
                run_id,
                status,
                summary_json.as_deref(),
                error.as_deref(),
            ) {
                tracing::warn!(run_id, error = %e, "failed to persist stacking run completion");
            }
        }
        Err(e) => {
            tracing::warn!(run_id, error = %e, "failed to persist stacking run completion: database unavailable");
        }
    }

    if let Some(json) = &summary_json {
        if let Err(e) = std::fs::create_dir_all(rc.layout.runs_dir()) {
            tracing::warn!(run_id, error = %e, "failed to create the runs folder for the provenance snapshot");
        } else if let Err(e) = std::fs::write(rc.layout.run_json(run_id), json) {
            tracing::warn!(
                run_id,
                path = %rc.layout.run_json(run_id).display(),
                error = %e,
                "failed to write the stacking run provenance snapshot"
            );
        }
    }

    for t in &rc.timings {
        tracing::info!(
            run_id,
            stage = t.stage.as_str(),
            duration_ms = t.duration_ms,
            "stacking stage finished"
        );
    }

    let duration_ms = run_started.elapsed().as_millis() as u64;
    tracing::info!(
        run_id,
        set_id,
        project_id = project_id.as_deref(),
        duration_ms,
        outcome = status,
        "stacking run finished"
    );

    // M3 Task 5 (ruling R-M3-8): `.rej` bitmaps — and, since M4c Task 3,
    // the `.rejl` siblings the large-scale filter writes next to them —
    // are per-run temporaries, removed HERE, at the run's single exit path,
    // for EVERY outcome (success, cancel, failure, panic-recovery all reach
    // this line) unless the user asked to keep everything. Both kinds live
    // in the same `rej/run-<id>/<group>/` directory, so the one
    // `remove_dir_all` below takes them together. A missing dir (neither
    // drizzle nor large-scale rejection ran for this run) is not an error;
    // a removal failure is a `warn!`, never fails the already-decided run.
    if rc.config.output.cleanup != CleanupPolicy::KeepAll {
        let rej_run_dir = rc.layout.rej_run_dir(run_id);
        match std::fs::remove_dir_all(&rej_run_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    run_id,
                    path = %rej_run_dir.display(),
                    error = %e,
                    "failed to remove the run's rejection-bitmap temporaries"
                );
            }
        }
    }

    emit_event(
        rc.emitter.as_ref(),
        STACKING_COMPLETE_EVENT,
        &StackingCompleteEvent {
            run_id,
            set_id,
            project_id: project_id.clone(),
            success,
            cancelled,
            error,
            warnings: rc.warnings.clone(),
            masters,
            groups_without_master,
        },
    );

    // M4c Task 4 fix round 3, finding 1: `rc` is a PARAMETER, and Rust
    // drops parameters AFTER the body's locals — so `_handle` would
    // otherwise go first and de-register the run while `rc` (every group's
    // `PixelMap`, its displacement grids, the readers) is still alive.
    // Every observer that wakes on the de-registration — `wait_for_run`
    // above all — would then be racing the teardown it thinks it waited
    // for. Dropping `rc` explicitly here is what actually puts the handle
    // last.
    drop(rc);

    // `_handle`'s `Drop` de-registers the run from `active_stacks` here, as
    // the last thing the thread does — see its construction at the top.
}

/// De-registers a running stack from [`ServiceContext::active_stacks`] when
/// [`run_thread`] ends, however it ends (M4a Task 4 fix round 1, item 1).
///
/// `active_stacks` is what every observer — `start_stacking`'s double-start
/// refusal, [`cancel_stacking`], `heal_interrupted_runs`, and the tests' own
/// `wait_for_run` — reads as "is this run still in flight?", so the entry
/// has to outlive everything the run still has to do: `finish_run`,
/// `runs/run-<id>.json`, the rejection-bitmap cleanup and the
/// `stacking-complete` event. The removal used to sit right after
/// `finish_run`, which made "gone from the registry" mean only "the DB row
/// is final" — an observer that then looked at the provenance file or the
/// working folder could legitimately find the run still writing them. The
/// cost of holding the entry those few extra ms is that a cancel arriving
/// in the window sets a flag nobody reads any more, and a new run for the
/// same set queued in the window is refused — both already true for the
/// whole `finish_run`-to-removal gap this replaces.
///
/// Owns an `Arc<ServiceContext>` clone rather than borrowing `rc.ctx`: the
/// guard has to outlive the `&mut rc` the pipeline call takes.
struct RunHandleGuard {
    ctx: Arc<ServiceContext>,
    run_id: i64,
}

impl Drop for RunHandleGuard {
    fn drop(&mut self) {
        self.ctx.active_stacks.lock().unwrap().remove(&self.run_id);
    }
}

/// The `stacking budget` line: what this run sized itself from (profile,
/// memory now, the budget and why, the pool and why, `compute.max_concurrent`).
/// Unknown `Option` values log as `-1`.
fn log_stacking_budget(rc: &RunContext) {
    // The width the pool really has; the why is the process-wide resolved one (no env re-read).
    let pool_threads = rc.ctx.image_pool.current_num_threads();
    let (_, pool_why) = machine::pool_threads(machine::profile());
    let budget = fan_budget(rc);
    let max_concurrent = match db(&rc.ctx) {
        Ok(d) => match rc.ctx.settings.get_compute_max_concurrent(&d.conn()) {
            Ok(n) => n as i64,
            Err(e) => {
                tracing::warn!(error = %e, "compute.max_concurrent unreadable for the budget line");
                -1
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "catalog unavailable for the budget line's compute.max_concurrent");
            -1
        }
    };
    machine::machine_fields_info!(
        "stacking budget",
        run_id = rc.run_id,
        memory_budget_bytes = machine::opt_i64(budget.bytes),
        budget_why = budget.why,
        pool_threads,
        pool_why,
        max_concurrent
    );
}

/// The whole pipeline, run in order. Queue admission happens FIRST (ruling
/// 2 / decision 1): a `QueueCancelled` while queued becomes
/// [`RunError::Cancelled`] before `set_run_status(running)` is ever called,
/// so a run cancelled while still waiting in line never shows as `running`
/// (see the `cancel_before_admission_finishes_cancelled` test). Tasks 7-8
/// append their own stages after stage 1's `#[cfg(test)]` fault-injection
/// check below.
fn run_pipeline(rc: &mut RunContext) -> Result<(), RunError> {
    let label = format!("Stacking · {}", rc.set_name);
    let (_permit, _job_id) = rc
        .ctx
        .compute_queue
        .acquire(ComputeJobKind::Stacking, &label, rc.cancel.clone())
        .map_err(|_queue_cancelled| RunError::Cancelled)?;

    log_stacking_budget(rc);

    {
        let conn = db(&rc.ctx)?.conn();
        set_run_status(&conn, rc.run_id, "running")?;
    }

    stage_masters(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Masters) {
        panic!("injected test failure after stage masters");
    }

    stage_calibrate(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Calibrate) {
        panic!("injected test failure after stage calibrate");
    }

    stage_measure(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Measure) {
        panic!("injected test failure after stage measure");
    }

    stage_reference(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Reference) {
        panic!("injected test failure after stage reference");
    }

    stage_register(rc)?;

    #[cfg(test)]
    if rc.fail_after_stage == Some(Stage::Register) {
        panic!("injected test failure after stage register");
    }

    stage_output(rc)?;

    Ok(())
}

/// One frame's stage 3-5 lifecycle, tracked per group
/// (`RunContext::measured`, keyed by `group_key`) across those three stages
/// — Task 8's integrate stage reads this directly: every INCLUDED frame's
/// calibrated path, measurement and (after stage 5) `PixelMap` all live
/// here.
///
/// Deviation from the brief's literal shape (`calibrated: PathBuf`,
/// `measurement: FrameMeasurement`, both non-`Option`): a manually- or
/// stage-1-excluded frame never reaches stage 3 at all (calibrate never
/// wrote it a file — Task 6), and a frame whose own measurement failed has
/// none either, so both are `Option` here. One entry exists for EVERY frame
/// of every group, included or not, so `write_frame_rows` (end of stage 5)
/// has one place to read every frame's whole outcome from — including the
/// ones excluded before stage 3 (or stage 3 itself) ever ran.
pub(crate) struct MeasuredFrame {
    frame: GroupFrame,
    calibrated: Option<PathBuf>,
    #[allow(dead_code)] // read by Task 8 (admission sizing for later stages)
    planes: usize,
    measurement: Option<FrameMeasurement>,
    weight: Option<FrameWeight>,
    included: bool,
    reason: Option<String>,
    registration: Option<RegisteredFrameOutcome>,
    /// Whether stage 1 REUSED an existing `calibrated` artifact for this
    /// frame rather than writing a fresh one (Task 8's own addition, from
    /// `RunContext::cached_calibrated` — `false` for a frame excluded before
    /// reaching that lookup, which is never wrong for those since they never
    /// had anything to reuse). Feeds `SummaryFrame::cached_calibrated`.
    cached_calibrated: bool,
    /// Whether stage 3 reused an existing `metrics` artifact for this frame
    /// rather than measuring it fresh (Task 8's own addition — set inline,
    /// in the same branch that already decides "reused", right below).
    /// Feeds `SummaryFrame::cached_metrics`.
    cached_metrics: bool,
    /// Stage 6 (local normalization, M2): this frame's own relative scale
    /// (mean across channels), `None` until `run_group_normalization` sets
    /// it (or forever, when LN never ran for this group). Feeds
    /// `SummaryFrame::ln_scale`.
    ln_scale: Option<f64>,
    /// Whether stage 6 REUSED an existing `ln` artifact for this frame
    /// rather than normalizing it fresh. Feeds `SummaryFrame::cached_ln`.
    cached_ln: bool,
    /// Plan B B0 (PB-2): F1's growth-curve factor, its pair count (the minimum over the planes) and the target side's
    /// `c` (the mean over the planes) from this run's own `normalize_frame`; `None` until [`set_ln_summary`] sets
    /// them, for a frame LN did not normalize, and on an LN cache hit. Feed `SummaryFrame::ln_growth_factor`,
    /// `ln_growth_pairs` and `ln_aperture_correction`.
    ln_growth_factor: Option<f64>,
    ln_growth_pairs: Option<usize>,
    ln_aperture_correction: Option<f64>,
    /// Plan D D1w (PD-18): under a mode that defers the weight floor ([`defers_weight_gate`]), stage 3 sets this when the
    /// frame's Measure-time weight fails [`weight_gate`] - the frame stays included, is registered and is reweighed, but
    /// no reference pick reads it (stage 4's group choice and candidates, the two-pass candidates, `group_can_switch`;
    /// see [`MeasuredFrame::in_reference_pick`]). A stored flag, because the reweigh later overwrites `weight`. Never
    /// set under any other weight mode.
    measure_floored: bool,
}

impl MeasuredFrame {
    /// Whether a reference pick may see this frame: included, and not under the Measure-time floor that the PSF-family
    /// modes defer (PD-18). The viability counts (3 frames to register or to integrate) read `included` alone.
    fn in_reference_pick(&self) -> bool {
        self.included && !self.measure_floored
    }
}

/// Stage 3's per-group summary beyond the per-frame [`MeasuredFrame`] list
/// (Tier C Task 1, ruling C-1/C-1a) — one entry per group in
/// `RunContext::measured_groups`, built at the same point stage 3 inserts
/// into `RunContext::measured`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MeasuredGroup {
    /// The group β: the lower median of every measured member's own `Auto`
    /// pick ([`psf_signal::group_beta`]), always one of their own βs
    /// (continuous from Plan A Task 9, F-4; one of
    /// [`psf_signal::AUTO_BETAS`] before it). `None` when the group measured no
    /// frame at all. Stage 6 fits its reference and every member's seeds at
    /// it (`PsfModel::Fixed`), and from Plan A Task 11b (owner decision
    /// R11b-12) stage 3 fitted every member at it under `Auto`
    /// ([`measure_two_pass_prepare`]) — one value, resolved once, read by both;
    /// every later stage's measurement hash folds it
    /// ([`group_measurement_hash`]).
    pub beta: Option<f64>,
}

/// One frame's stage 5 outcome. `cached: true` means an existing
/// `registration_results` row was reused verbatim (ruling 10) — no
/// `register_frame` call, no `upsert_registration` write; `record` is the
/// EXISTING row in that case.
pub(crate) enum RegisteredFrameOutcome {
    Aligned {
        #[allow(dead_code)] // read by Task 8 (per-frame resampling)
        map: PixelMap,
        record: RegistrationRecord,
        #[allow(dead_code)] // read by Task 8/9 (provenance)
        cached: bool,
        /// The hash this frame's `registered` artifact is cached under:
        /// its registration hash, or the composed one on a grid (plan 6b-3
        /// D2). Integration's materialized lookup and LN key on it.
        registered_hash: String,
    },
    /// The failure text — kept alongside (not read back today: the SAME
    /// text is already stored on the frame's `MeasuredFrame::reason`, which
    /// is what `write_frame_rows` and the caller's own `warn!` read) so a
    /// later consumer of `RegisteredFrameOutcome` on its own (Task 8/9,
    /// without also holding the `MeasuredFrame`) still has it.
    Failed(#[allow(dead_code)] String),
}

/// Build a [`RunContext`] directly from fixture data, bypassing
/// [`start_stacking`]'s DB/plan machinery — the calibrate-stage tests below
/// (and Task 7's own stage 3-5 tests) call [`stage_calibrate`] (or their own
/// stage functions) straight off a context built this way. `run_id` is the
/// caller's to choose: pass a real `stacking_runs.id` (from
/// [`crate::db::stacking::insert_run`]) for a test that also asserts on the
/// DB row or the provenance file; a fixed dummy otherwise — the calibrate
/// stage's reuse logic never depends on it, since `stacking_artifacts` is
/// keyed by `(frames_set_id, group_key, kind, frame_id)`, never by `run_id`.
///
/// `group_ids` (Task 7's own addition to this signature): stages 3 and 5
/// write `stacking_run_groups`/`stacking_run_frames` rows, both FK'd to a
/// real `stacking_runs` row (this crate's pooled connections run with
/// `PRAGMA foreign_keys = ON`) — a calibrate-only test can still pass
/// `HashMap::new()` (calibrate never reads it), but a test that runs stage 3
/// or 5 needs one `stacking_run_groups` row per group, inserted against the
/// SAME real `run_id`, with THIS map built from the returned ids
/// (`run_stages_for_test`'s own `seed_run_and_groups` helper does exactly
/// that).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn test_context(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    run_id: i64,
    set_id: i64,
    set_name: &str,
    config: StackingConfig,
    plan_groups: Vec<IntegrationGroup>,
    layout: WorkingLayout,
    output_dir: PathBuf,
    group_ids: HashMap<String, i64>,
) -> RunContext {
    let hash = config_hash(&config);
    let solver: Arc<dyn crate::stacking::solve::FrameSolver> =
        Arc::new(crate::stacking::solve::CatalogSolver::new(ctx.clone()));
    let origins: FrameOrigins = Arc::new(
        plan_groups
            .iter()
            .flat_map(|g| g.frames.iter())
            .map(|f| {
                (
                    f.frame_id,
                    crate::stacking::origin::FrameOrigin::Catalog {
                        file_id: f.file_id,
                        attested: false,
                    },
                )
            })
            .collect(),
    );
    let summary = RunSummary {
        run_id,
        set_id: Some(set_id),
        project_id: None,
        set_name: set_name.to_string(),
        app_version: "test".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        status: "planning".to_string(),
        config: config.clone(),
        config_hash: hash.clone(),
        grid: None,
        reference: SummaryReference {
            frame_id: None,
            filename: None,
            mode: config.reference.mode,
            weight: None,
            switched_from: None,
        },
        measurement: SummaryMeasurement {
            seed_source: "matchedFilter".to_string(),
            scale_estimator: config.normalization.scale_estimator,
        },
        groups: Vec::new(),
        masters_built: Vec::new(),
        stages: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };

    RunContext {
        ctx,
        emitter,
        run_id,
        subject: StackSubject::FrameSet(set_id),
        set_name: set_name.to_string(),
        config,
        hash,
        plan_groups,
        masters_to_build: Vec::new(),
        excluded: Vec::new(),
        group_ids,
        layout,
        output_dir,
        cancel: Arc::new(AtomicBool::new(false)),
        app_version: "test".to_string(),
        warnings: Vec::new(),
        timings: Vec::new(),
        summary,
        last_emit: Instant::now(),
        rerun_from: None,
        memo: HashMemo::for_run(origins.clone()),
        origins,
        solver,
        wcs_rule: None,
        grid: None,
        hot_maps: HashMap::new(),
        cached_calibrated: HashMap::new(),
        runtime_exclusions: Vec::new(),
        measured: HashMap::new(),
        measured_groups: HashMap::new(),
        reference_frame_id: None,
        reference_calibrated: None,
        reference_width: 0,
        reference_height: 0,
        group_geometry: HashMap::new(),
        group_seeds_calibration: HashMap::new(),
        group_ln_reference_c: HashMap::new(),
        common_stars: HashMap::new(),
        planned_ln_scales: HashMap::new(),
        drizzle_attempted: 0,
        crop_items: Vec::new(),
        dry_pass_stars: HashMap::new(),
        fail_after_stage: None,
    }
}

/// [`test_context`] for a project subject (plan 6b-2): the project frame
/// source's own groups and origins, the project's handle namespace, a
/// summary carrying the project id. Project stages run only through this
/// until 6b-3 lets `start_stacking` start one.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_test_context(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    run_id: i64,
    project_id: &str,
    config: StackingConfig,
    plan_groups: Vec<IntegrationGroup>,
    origins: FrameOrigins,
    layout: WorkingLayout,
    output_dir: PathBuf,
    group_ids: HashMap<String, i64>,
) -> RunContext {
    let mut rc = test_context(
        ctx,
        emitter,
        run_id,
        0,
        project_id,
        config,
        plan_groups,
        layout,
        output_dir,
        group_ids,
    );
    rc.subject = StackSubject::Project(project_id.to_string());
    rc.memo = HashMemo::for_run(origins.clone());
    rc.origins = origins;
    rc.summary.set_id = None;
    rc.summary.project_id = Some(project_id.to_string());
    rc
}

#[cfg(test)]
mod tests;
