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
    upsert_frame_row, GroupUpdate, NewArtifact, NewFrameRow, NewGroup, NewMasterLight, NewRun,
    StackSubject,
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
    read_pass1, read_pass1_head, write_pass1, Pass1ArtifactError, Pass1Head, PASS1_ARTIFACT_KIND,
};
use crate::stacking::origin::{cfa_geometry, origin_of, stored_solve, FrameOrigin, FrameOrigins};
use crate::stacking::paths::{
    artifact_group_key, cleanup_work, cleanup_work_with, subject_layout, CleanupWhat, WorkingLayout,
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
pub(crate) use frames::*;
pub(crate) use fan_out::*;
use masters::*;
pub(crate) use calibrate::*;
use prepare::*;
use solve::*;
pub(crate) use measure::*;
use reference::*;

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
/// `stacking_artifacts` row's `size`/`modified_at` when deciding freshness.
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
        memo: HashMemo::new(origins.clone()),
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

/// The `.rej` file stems of one group's rejection-bitmap set, one per included frame, in `included` order: the
/// frame's [`frame_file_stem`], never the stem of its calibrated path. A calibrated row written before the
/// frame-level cache keeps its old `calibrated/<group_key>/c_<name>.fits` path, and a merged group can hold two such
/// frames of one file name from different old folders; the frame's own stem carries its id, so the two never share
/// a `.rej` (ruling R4 (c)). A member with no frame in the group (not expected) falls back to its position and id.
fn rejection_map_stems(group: &IntegrationGroup, included_ids: &[i64]) -> Vec<String> {
    let frames_by_id: HashMap<i64, &GroupFrame> =
        group.frames.iter().map(|f| (f.frame_id, f)).collect();
    included_ids
        .iter()
        .enumerate()
        .map(|(pos, frame_id)| match frames_by_id.get(frame_id) {
            Some(frame) => frame_file_stem(frame),
            None => format!("frame_{pos}_f{frame_id}"),
        })
        .collect()
}

// ── Stages 3-5 (Task 7): measure & select, reference, register ─────────────

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

/// Planes of one channel resident while `normalize_frame` runs: the
/// `RegisteredSource` (its raw read scratch plus f32 scratch) and the
/// `BandPlanes` buffer kept alive across channels, the detector's ADU-scaled
/// copy and rustafits' own internal maps during detection, and the
/// background model's tile tables and fit work — the same shape
/// `MEASURE_PLANES_RESIDENT` measured at 8 planes of one channel. (The old
/// `clean_plane` copy no longer exists: the tile-statistic background model
/// reads the plane in place.)
///
/// History: amended to 8 (perf tier 1 Task 3, 2026-09-19) after the Task 10
/// acceptance run found the analytic 4 undercounted it: on the real OSC set
/// (LDN 1272, 6248×4176, 16 GB / 10-core Mac) factor 4 admitted 10
/// Normalize workers for the OSC group and the run swapped (10.4 GB of
/// 11.3 GB swap used; per-frame `ln frame normalized` medians rose from
/// 4.6 s / 9.6 s to 14.5 s / 19.3 s; the stage took ≈ 19 min against
/// 12.5). 8 aligns with `MEASURE_PLANES_RESIDENT`, so a 16 GB machine admits
/// 4 workers instead of 10. Not directly measured with `/usr/bin/time -l`:
/// `ln_probe` builds the LN reference before it calls `normalize_frame`, and
/// that build dominates its peak RSS, so one frame's normalize working set
/// cannot be isolated there; nor re-measured since the background model
/// rewrite.
pub(crate) const LN_PLANES_RESIDENT: u64 = 8;

/// `LN_PLANES_RESIDENT` planes of one w×h channel, in bytes — Normalize's
/// fan-out working set (perf tier 1 Task 3).
pub(crate) fn ln_working_set_bytes(w: u64, h: u64) -> u64 {
    LN_PLANES_RESIDENT * w * h * 4
}

/// Plan 6b-3 D6: masked LN also holds, per worker, the overlap and eroded
/// masks (a byte per pixel each, together half an f32 plane). The reference
/// plane is read in place (`TileTable::restricted`); the masked copy and its
/// row buffer the original 2.5-plane count included no longer exist, so the
/// three planes counted here are half a plane of masks plus headroom.
pub(crate) const LN_MASKED_EXTRA_PLANES: u64 = 3;

/// The payload-only marker of a member that keeps global normalization
/// because it overlaps the LN reference on under `ln::LN_MIN_OVERLAP` of
/// its footprint (spec §5.6, plan 6b-3 D7): `config_hash` is the member's LN
/// hash, so the run and the plan count it as cached while nothing upstream
/// moves; payload `{"overlap": <fraction>}`.
pub(crate) const LN_GLOBAL_ARTIFACT_KIND: &str = "ln_global";

/// The group's LN field on a project's grid (plan 6b-3 D8, review I6), over
/// the frames the plan counts in scope — not manually excluded, not
/// unsupported (Prepare's `"unsupported file: …"` exclusion) — so the plan
/// builds the same field and the same hash. `None` for a frame set.
fn group_ln_field_for(
    rc: &RunContext,
    group: &IntegrationGroup,
) -> Result<Option<crate::stacking::origin::GroupLnField>, RunError> {
    let Some(grid) = &rc.grid else {
        return Ok(None);
    };
    let manual: HashSet<i64> = rc.excluded.iter().copied().collect();
    let unsupported: HashSet<i64> = rc
        .runtime_exclusions
        .iter()
        .filter(|(_, why)| why.starts_with("unsupported file: "))
        .map(|(id, _)| *id)
        .collect();
    let in_scope: Vec<i64> = group
        .frames
        .iter()
        .map(|f| f.frame_id)
        .filter(|id| !manual.contains(id) && !unsupported.contains(id))
        .collect();
    let conn = db(&rc.ctx)?.conn();
    crate::stacking::origin::group_ln_field(&conn, &rc.origins, &in_scope, &grid.grid)
        .map(Some)
        .map_err(|e| RunError::Other(format!("{e:#}")))
}

/// Each member's coverage for the anchor pick and the LN member list:
/// against the reference rectangle for a frame set (unchanged), against the
/// group's own field on a project's grid (plan 6b-3 D8).
fn member_coverages(
    field: Option<&crate::stacking::origin::GroupLnField>,
    members: &[GroupMember],
    ref_w: usize,
    ref_h: usize,
) -> Vec<f64> {
    match field {
        Some(field) => members
            .iter()
            .map(|m| field.coverage_of(m.frame_id))
            .collect(),
        None => members
            .iter()
            .map(|m| {
                reference_coverage(
                    &m.map,
                    m.measurement.width,
                    m.measurement.height,
                    ref_w,
                    ref_h,
                )
            })
            .collect(),
    }
}

pub(crate) fn ln_working_set_bytes_masked(w: u64, h: u64) -> u64 {
    ln_working_set_bytes(w, h) + LN_MASKED_EXTRA_PLANES * w * h * 4
}

/// Planes of ONE channel (ref geometry `w × h`) resident while a register
/// fan-out worker registers a MONO frame (perf tier A Task 6a fix round 1,
/// ruling R-TA-5): the pre-existing detect+align working set, unchanged.
///
/// Measured 2026-09-19 with `/usr/bin/time -l` on `register_probe --write`
/// (a real, committed flag — see its own doc), over 3 runs against two real
/// LDN1272-test calibrated frames (mono 6224×4168):
///
/// | | detect+align+write |
/// | ---- | ---- |
/// | mono, avg of 3 | 274.6 MB ≈ 2.65 planes |
///
/// Re-measured after fix round 2's plane-at-a-time write (ruling R-TA-6):
/// for a MONO (1-channel) subject the old all-at-once writer and the new
/// streaming one hold the exact same ONE plane's worth of output buffer
/// either way, so no drop was expected here — and none of consequence
/// showed up: 2.65 planes is close to the OLD design's own detect+align-
/// ONLY baseline (2.66, fix round 1's measurement), i.e. the write's
/// marginal cost over detect+align's own peak is now negligible for mono
/// too (detect+align's peak already exceeds the one-plane write cost by
/// itself). Left at 4 — comfortably above the fresh measurement, so mono
/// admission is UNCHANGED by this fix round.
pub(crate) const REGISTER_PLANES_RESIDENT: u64 = 4;

/// OSC (3-channel) register+write planes — see [`REGISTER_PLANES_RESIDENT`]'s
/// own doc for the measurement method; same date, same probe, against two
/// real LDN1272-test debayered OSC frames (6248×4176, 3 planes).
///
/// **Fix round 1** (all-at-once writer, `all = vec![NAN; plane_len *
/// channels]`, one buffer holding EVERY channel's output together for the
/// whole write), 2 runs: detect+align only 588.2 MB ≈ 5.64 planes,
/// detect+align+write 906.7 MB ≈ 8.69 planes — the write's own delta
/// (≈ 3.05 planes) was ONE plane PER CHANNEL, resident together, which is
/// what made the OSC register admission the fan-out's tightest budget.
///
/// **Fix round 2** (ruling R-TA-6, required item — plane-at-a-time write:
/// warp plane → write plane → drop its buffer → next plane, via
/// `fits_writer::write_fits_f32_streaming_with`), 3 runs: detect+align+write
/// 592.3 MB (range 588.2–598.1 MB) ≈ 5.68 planes (range 5.64–5.73) — the
/// write's own marginal cost is now negligible, same reasoning as the mono
/// re-measurement above: only ONE plane's buffer is ever resident during
/// the write, and detect+align's own peak (5.64 planes, fix round 1's own
/// baseline) already exceeds it, so the write no longer sets the peak at
/// all — close to the senior review's own rough guess of "≈5 planes
/// total" for this constant.
/// The measured total, rounded up with the same margin fix round 1's own 9
/// carried (≈0.3 planes of headroom above the average): **6**, down from 9.
///
/// If a real run's admission ever needs re-deriving from RAM instead of
/// this measurement, `total_ram_bytes() / 4 / (6 · w · h · 4)` is the
/// worker count an OSC group's register stage gets — on this dev Mac (16
/// GiB RAM, 10 cores) at LDN1272-test's 6248×4176 OSC geometry that is
/// `admission(626_199_552, 10) = 6` (up from `admission(939_299_328, 10) =
/// 4` at the old constant of 9 — both computed against this same machine's
/// real `total_ram_bytes()`, not the illustrative "16 GB machine" figure
/// fix round 1's own doc cited, which this fix round did not attempt to
/// reproduce exactly); see the fix round's own report for the full
/// before/after admission line.
pub(crate) const REGISTER_PLANES_RESIDENT_OSC: u64 = 6;

/// Register's fan-out working set (perf tier A Task 6a fix round 1) —
/// [`REGISTER_PLANES_RESIDENT`] or [`REGISTER_PLANES_RESIDENT_OSC`] planes
/// of one reference-geometry channel, in bytes, depending on the GROUP'S
/// OWN MEASURED PLANE COUNT (fix round 2, ruling R-TA-6 M3) — the caller's
/// `PendingRegistration`/`ArtifactRewriteItem::planes`, stage 3's own
/// `MeasuredFrame::planes`, not `group.color_mode`. Keying on color mode
/// charged 9 planes for an OSC group whose debayering is off and therefore
/// produces 1-plane frames (`calibration.debayer_osc = false`) — the
/// subject the writer actually opens never carries 3 channels in that
/// case, so the working set it needs never does either. `frame_planes < 3`
/// (i.e. 1) takes the mono constant, `>= 3` takes the OSC one; there is no
/// 2-channel case in this pipeline.
pub(crate) fn register_working_set_bytes(w: u64, h: u64, frame_planes: usize) -> u64 {
    let planes = if frame_planes >= 3 {
        REGISTER_PLANES_RESIDENT_OSC
    } else {
        REGISTER_PLANES_RESIDENT
    };
    planes * w * h * 4
}

/// A group's currently-included frame count — the viability gate (≥ 3) and
/// the progress total both read it, in three places since M4a Task 4.
fn included_count_of(rc: &RunContext, group_key: &str) -> usize {
    rc.measured
        .get(group_key)
        .map(|v| v.iter().filter(|e| e.included).count())
        .unwrap_or(0)
}

/// One frame's outcome from a single DRY [`register_group_pass`] over one
/// group: the entry index into that group's `RunContext::measured` vector,
/// the frame id, and either the alignment's `(rotation_deg, translation)`
/// or the failure text.
///
/// Only a `persist = false` pass produces these; a persisting pass returns
/// an EMPTY vector, because it records each frame's outcome where the rest
/// of the run reads it (`registration_results` and
/// `MeasuredFrame::registration`) and nothing wants a second copy. Pinned
/// by `a_dry_register_pass_writes_no_rows_and_no_outcomes`.
#[derive(Debug)]
struct PassResult {
    idx: usize,
    frame_id: i64,
    outcome: Result<(f64, (f64, f64)), String>,
}

/// One frame [`register_group_pass`] has decided to register afresh (no
/// fresh cached row), with everything resolved for it on the run thread
/// before the fan-out starts.
struct PendingRegistration {
    /// Index into the group's `RunContext::measured` vector.
    idx: usize,
    frame: GroupFrame,
    path: PathBuf,
    is_reference: bool,
    expected_hash: String,
    /// M4b: this frame's own scale window, optional plate-solve seed, and
    /// which of the two seeds leads (ruling R-T6-9).
    scale_gate: (f64, f64),
    hint: Option<Linear>,
    policy: SeedPolicy,
    /// Perf tier A Task 6a fix round 1 (ruling R-TA-5): this frame's own
    /// calibrated plane count, already known from stage 3
    /// (`MeasuredFrame::planes`) — threaded through so the fan-out worker
    /// that now writes the registered artifact never needs `rc` to learn
    /// whether the output filename gets the `_d` (debayered) marker.
    planes: usize,
    /// Tier C Task 3 (ruling C-6): this frame's own stage-1 (calibration)
    /// hash, already resolved above for `expected_hash` — kept so the
    /// `RegisterItem` build below can derive the `fits` artifact's own
    /// measurement hash (`measurement_hash_for`) without a second
    /// `calibration_hash_checked` call.
    calibrated_hash: String,
}

/// What one fan-out worker needs — the pixel-side half of a
/// [`PendingRegistration`]. The bookkeeping half stays on the run thread.
struct RegisterItem {
    frame_id: i64,
    /// Perf tier A Task 6a fix round 1: kept whole (not just `frame_id`) —
    /// the worker's own registered-artifact write needs it for
    /// `frame_file_stem`.
    frame: GroupFrame,
    path: PathBuf,
    is_reference: bool,
    scale_gate: (f64, f64),
    hint: Option<Linear>,
    policy: SeedPolicy,
    /// Perf tier 1 Task 9: this frame's detections, already known from the
    /// two-pass dry pass over the SAME group — `Some` skips
    /// [`detect_frame_stars`] entirely in the fan-out closure below. `None`
    /// for a genuine cache miss (every dry-pass item, and any persisting
    /// item the dry pass never saw — the reference's own identity row, or
    /// a frame the dry pass excluded).
    pre: Option<Arc<DetectedStars>>,
    /// Perf tier A Task 6a fix round 1: this item's `registration_results`
    /// config hash — the worker needs it to build the `RegistrationRecord`
    /// (`to_record`) itself now, so the write's provenance cards and the
    /// sequential loop's `upsert_registration` agree on ONE record instead
    /// of two independently-built ones.
    expected_hash: String,
    planes: usize,
    /// Tier C Task 3 (spec §7, ruling C-6): this frame's own fresh
    /// `fits.0` artifact, resolved on the run thread (`resolve_register_fits`)
    /// only when `pre.is_none()` (a `pre` hit skips `detect_frame_stars`
    /// entirely, so reading the artifact for it would be wasted I/O) —
    /// `None` for an OSC frame, a stale/missing artifact, or a `pre` hit.
    fits: Option<Vec<psf_signal::StarFit>>,
}

/// Perf tier A Task 6a fix round 1 (ruling R-TA-5): a register fan-out
/// worker's own registered-artifact outcome for a PERSISTING item whose
/// alignment succeeded — built and (attempted to be) written right here,
/// since the warp is the CPU cost the fan-out's admission exists to
/// parallelize (~3.3 s/plane), not something to redo one frame at a time
/// on the run thread afterward. `None` (the whole `Option`, at the call
/// site) covers the dry pass and an alignment failure — neither builds a
/// record or attempts a write here; the sequential loop's existing
/// handling for both is unchanged.
struct RegisteredWrite {
    /// Already-resolved (`to_record`) — the sequential loop's
    /// `upsert_registration` uses this SAME record, never rebuilding it.
    rec: RegistrationRecord,
    /// The hash the `registered` artifact is recorded under: the
    /// registration hash, or the composed one on a grid (plan 6b-3 D2).
    registered_hash: String,
    /// `Some(out_path)` iff the write succeeded; `None` on a write failure
    /// (logged here, non-fatal — the sequential loop then records no
    /// artifact row, and every downstream reader falls back to the
    /// on-the-fly warp for this frame).
    written: Option<PathBuf>,
    /// The write's error text when `written` is `None`; folded into the
    /// group's one "registered frames could not be written" run warning.
    write_error: Option<String>,
    /// The map the registered artifact was written with: the star map, or
    /// its composition onto the grid.
    map: PixelMap,
}

/// Failed registered-frame writes of one group's pass. The registered frame
/// is a required artifact, so a failed write must not leave the run
/// silently warping on the fly: each failure keeps its per-frame `warn!`,
/// and the pass pushes ONE run warning per group (the count and the first
/// error) from [`RegisteredWriteFailures::flush`].
#[derive(Default)]
struct RegisteredWriteFailures {
    count: usize,
    first_error: Option<String>,
}

impl RegisteredWriteFailures {
    fn note(&mut self, error: String) {
        self.count += 1;
        self.first_error.get_or_insert(error);
    }

    fn flush(self, rc: &mut RunContext, group: &IntegrationGroup) {
        let Some(first) = self.first_error else {
            return;
        };
        let count = self.count;
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count,
            error = %first,
            "registered frames could not be written"
        );
        rc.warnings.push(format!(
            "registered frames could not be written for {count} frames of {}: {first}; they are warped on the fly",
            group.key
        ));
    }
}

/// Perf tier A Task 6a fix round 1 (ruling R-TA-5): a CACHED registration
/// (`register_group_pass`'s reused branch) whose artifact turned out to be
/// missing or stale — everything the SEPARATE rewrite fan-out needs to redo
/// just the pixel work; no re-alignment, the map/record are already known.
struct ArtifactRewriteItem {
    frame_id: i64,
    calibrated: PathBuf,
    frame: GroupFrame,
    map: PixelMap,
    rec: RegistrationRecord,
    // The hash the rewritten artifact is recorded under (plan 6b-3 D2).
    registered_hash: String,
    planes: usize,
}

/// [`SeedPolicy`] as the `seed_policy` log field's value.
fn seed_policy_field(policy: SeedPolicy) -> &'static str {
    match policy {
        SeedPolicy::QuadFirst => "quad_first",
        SeedPolicy::WcsFirst => "wcs_first",
    }
}

/// Stage 5's stored-plate-solve memo (M4b Task 2): `frame_id` → its
/// `plate_solves` row, or `None` for a frame that has never been solved.
/// One lookup per frame for the whole stage, including the reference's,
/// which every other frame's seed needs again and again — and including
/// the negative answers, so an unsolved set does not re-query per frame.
type SolveCache = HashMap<i64, Option<PlateSolveRecord>>;

/// [`SolveCache`]'s lazy fill. A read failure is a warning, not a run
/// failure: the WCS seed is an accelerator, and losing it costs the quad
/// seed's work, nothing else.
fn solve_of(
    solves: &mut SolveCache,
    conn: &rusqlite::Connection,
    origins: &FrameOrigins,
    frame_id: i64,
) -> Option<PlateSolveRecord> {
    if !solves.contains_key(&frame_id) {
        // A frame with no origin was already logged at `error!` by
        // `origin_of`; it is treated like a read error below.
        let read = origin_of(origins, frame_id)
            .map_err(anyhow::Error::from)
            .and_then(|origin| stored_solve(conn, origin, frame_id));
        let row = match read {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(
                    frame_id,
                    error = %e,
                    "failed to read the stored plate solve; registering without a wcs seed"
                );
                None
            }
        };
        solves.insert(frame_id, row);
    }
    solves.get(&frame_id).cloned().flatten()
}

/// One frame's registration inputs (M4b Task 2, rulings R-M4b-2/3): the
/// scale window it is judged against, and the optional plate-solve seed.
///
/// The seed is built whenever BOTH this frame and the reference carry a
/// stored plate solve (ruling R-T6-9). What the frame's implied ratio
/// decides is only the ORDER: [`SeedPolicy::WcsFirst`] when the ratio says
/// a real scale step is expected (rulings R-T2-1 / R-T6-4, the
/// [`ratio_wants_seed`] tolerance), [`SeedPolicy::QuadFirst`] otherwise —
/// the M1 path, with the seed held in reserve for the frames it drops.
///
/// The tolerance decides the order rather than the window because the
/// ratio is a quotient of two MEASURED pixel scales:
/// `GroupFrame::pixel_scale_arcsec` prefers the stored plate solve, and two
/// solves of one rig disagree (typically in the fourth digit, out to 1.6 %
/// in the measured tail), so an exact comparison would put every frame of
/// an ordinary same-scale set on the WCS path — exactly the path M1–M4a's
/// pins were measured without. The gate itself stays centred on the exact
/// ratio either way: none of this changes how a frame is judged, only
/// which seed it is offered first.
///
/// A frame with no solve on either side gets no hint at all and takes the
/// M1 path with no fallback, exactly as it did before M4b.
///
/// Both passes of stage 5 go through here — the dry two-pass run and the
/// persisting one — so a frame is never measured against one gate and
/// registered against another.
fn registration_gate_and_hint(
    solves: &mut SolveCache,
    conn: &rusqlite::Connection,
    origins: &FrameOrigins,
    frame: &GroupFrame,
    reference_frame_id: i64,
    reference_scale: Option<f64>,
) -> ((f64, f64), Option<Linear>, SeedPolicy) {
    let scale_gate = scale_gate_for(frame.pixel_scale_arcsec, reference_scale);
    let ratio = scale_ratio_for(frame.pixel_scale_arcsec, reference_scale);
    let policy = if ratio_wants_seed(ratio) {
        SeedPolicy::WcsFirst
    } else {
        SeedPolicy::QuadFirst
    };
    let hint = match (
        solve_of(solves, conn, origins, frame.frame_id),
        solve_of(solves, conn, origins, reference_frame_id),
    ) {
        (Some(subject), Some(reference)) => seed_from_solves(
            &subject,
            &reference,
            (frame.width.max(0) as usize, frame.height.max(0) as usize),
        ),
        _ => None,
    };
    (scale_gate, hint, policy)
}

/// Tier C Task 3 (spec §7, ruling C-6): a mono frame's own fresh `fits.0`
/// artifact, for [`detect_frame_stars`]/[`reference_stars`] to reuse
/// instead of detecting on the frame's own pixels a second time — Measure
/// (stage 3) has already fitted this exact plane. `None` for a multi-plane
/// (OSC) frame — ruling C-6, "OSC frames keep the luminance detection" —
/// and every caller below checks `planes` before calling this at all, so
/// this only warns about a genuine mono fallback, never an OSC frame that
/// was never a candidate for reuse.
///
/// Best-effort: a DB error, a missing/stale row, an unreadable file, or a
/// cut+truncated star list below [`align::MIN_INLIERS`] (fix round 1,
/// ruling C-18, item 5) all return `None` with exactly one `warn!` naming
/// why (`run_id`/`group_key`/`frame_id`, fix round 1 item 7) — reuse is an
/// optimization, never a correctness requirement, so nothing here ever
/// fails the stage; the caller's existing detection path is always the
/// fallback.
fn resolve_register_fits(
    rc: &RunContext,
    group_key: &str,
    frame_id: i64,
    planes: usize,
    calibrated_hash: &str,
) -> Option<Vec<psf_signal::StarFit>> {
    if planes != 1 {
        return None;
    }
    // Ruling R3 (b): the invariant is structural. Registration folds Measure's output only for a group whose frames
    // read `fits.0` (`register_reads_measure_fits`), so a group that does not read it never reaches the artifact: a
    // frame that calibrates to one plane inside a three-plane group (an unparseable BAYERPAT) detects instead, the
    // safe direction.
    let prepare_path = crate::stacking::origin::subject_prepares(&rc.subject, &rc.origins);
    let reads_fits = rc
        .plan_groups
        .iter()
        .find(|g| g.key == group_key)
        .is_some_and(|g| {
            crate::stacking::plan::register_reads_measure_fits(&rc.config, g, prepare_path)
        });
    if !reads_fits {
        return None;
    }
    let expected_hash = group_measurement_hash(rc, &rc.config, group_key, calibrated_hash);
    let kind = fits_artifact::artifact_kind(0);

    // Ruling C-18, item 8: the SAME per-plane freshness rule the plan gate
    // applies (`plan::fits_artifact_fresh`/`fits_artifact_fresh_for_planes`)
    // — never a hand-rolled `find_artifact` + `is_fresh` pair that could
    // quietly drift from it. `planes` is already known to be 1 here, so
    // this checks exactly the one `fits.0` row.
    let fresh = (|| -> Result<bool, RunError> {
        let conn = db(&rc.ctx)?.conn();
        Ok(fits_artifact_fresh_for_planes(
            &conn,
            &rc.subject,
            frame_id,
            planes,
            &expected_hash,
        )?)
    })();
    let fresh = match fresh {
        Ok(fresh) => fresh,
        Err(e) => {
            // `RunError` carries no `Display`, only `Debug` — the same
            // convention every other `RunError`-typed warn in this file
            // already follows (`error = ?e`).
            tracing::warn!(
                run_id = rc.run_id,
                group_key,
                frame_id,
                error = ?e,
                "register: fits artifact lookup failed; detecting"
            );
            return None;
        }
    };
    if !fresh {
        tracing::warn!(
            run_id = rc.run_id,
            group_key,
            frame_id,
            "register: no fresh fits artifact; detecting"
        );
        return None;
    }

    // The freshness check above only answers yes/no — a second, separate
    // lookup gets the row's own `path` to actually read the file.
    let row = (|| -> Result<Option<crate::db::stacking::StackingArtifactRow>, RunError> {
        let conn = db(&rc.ctx)?.conn();
        Ok(crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            artifact_group_key(&kind, group_key),
            &kind,
            Some(frame_id),
        )?)
    })();
    let path = match row {
        Ok(row) => row.and_then(|r| r.path),
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key,
                frame_id,
                error = ?e,
                "register: fits artifact lookup failed; detecting"
            );
            return None;
        }
    };
    let Some(path) = path else {
        tracing::warn!(
            run_id = rc.run_id,
            group_key,
            frame_id,
            "register: fits artifact row has no path; detecting"
        );
        return None;
    };
    let fits = match fits_artifact::read_fits(Path::new(&path)) {
        Ok(fits) => fits,
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key,
                frame_id,
                path = %path,
                error = %e,
                "register: fits artifact unreadable; detecting"
            );
            return None;
        }
    };

    // Ruling C-18, item 5: a floor on the CUT+TRUNCATED list, not the raw
    // fits count — `stars_from_fits` may drop most of a frame's fits to
    // saturation/eccentricity/SNR cuts, and a star list too short to ever
    // reach `align::MIN_INLIERS` matched pairs is no better than no reuse
    // at all. This runs the SAME cut+truncate `detect_frame_stars`'s own
    // fits branch will (a cheap, allocation-only pass over an in-memory
    // `Vec`, no I/O) to judge it here rather than handing a doomed list
    // downstream and discovering the alignment failure only after RANSAC.
    let usable = stars_from_fits(
        &fits,
        &rc.config.registration.detection,
        rc.config.registration.max_stars,
    )
    .len();
    if usable < align::MIN_INLIERS {
        tracing::warn!(
            run_id = rc.run_id,
            group_key,
            frame_id,
            count = usable,
            "register: fits artifact has too few usable stars after cuts; detecting"
        );
        return None;
    }

    Some(fits)
}

/// [`resolve_register_fits`] for a frame identified only by `frame_id` —
/// the reference-detection call sites (`stage_register`,
/// `register_native_groups`) know the reference's `frame_id` before they
/// know which group it belongs to. A linear scan of `RunContext::measured`
/// (every group, every measured frame) is fine here: this runs at most
/// once per group per stage-5 pass, never per subject frame.
fn resolve_reference_fits(
    rc: &RunContext,
    frame_id: i64,
    calibrated_hash: &str,
) -> Option<Vec<psf_signal::StarFit>> {
    let (group_key, planes) = rc.measured.iter().find_map(|(key, frames)| {
        frames
            .iter()
            .find(|f| f.frame.frame_id == frame_id)
            .map(|f| (key.clone(), f.planes))
    })?;
    resolve_register_fits(rc, &group_key, frame_id, planes, calibrated_hash)
}

/// The rows the low-inlier diagnostic reads (robust quad seed RQ-6): `(frame_id, inliers, model)` of a group's
/// aligned non-reference frames, cached rows included. A failed frame, a frame with no outcome and the reference
/// are not part of the group's median.
fn low_inlier_rows(entries: &[MeasuredFrame]) -> Vec<(i64, usize, String)> {
    entries
        .iter()
        .filter_map(|e| match &e.registration {
            Some(RegisteredFrameOutcome::Aligned { record, .. }) if !record.is_reference => Some((
                e.frame.frame_id,
                record.matched_stars.max(0) as usize,
                record.model.clone().unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

/// Robust quad seed RQ-6: one `warn!` per frame whose registration kept under half its group's median inliers,
/// whatever seed carried it (`model` names it). The defect this counts stood through three catalog stages because
/// nothing counted it (R-T6-9's precedent: a distinct event makes the pattern countable). A log line only: no run
/// warning, no row, no UI.
fn warn_low_inlier_frames(run_id: i64, group_key: &str, rows: &[(i64, usize, String)]) {
    let counts: Vec<(i64, usize)> = rows.iter().map(|(id, n, _)| (*id, *n)).collect();
    let Some((median_inliers, flagged)) = low_inlier_frames(&counts) else {
        return;
    };
    for (frame_id, inliers) in flagged {
        let model = rows
            .iter()
            .find(|r| r.0 == frame_id)
            .map(|r| r.2.as_str())
            .unwrap_or_default();
        tracing::warn!(
            run_id,
            group_key = %group_key,
            frame_id,
            inliers,
            median_inliers,
            model = %model,
            "registration kept under half its group's median inliers"
        );
    }
}

/// Robust quad seed RQ-6, review r1 I1: the low-inlier diagnostic of a PERSISTING pass, read from the outcomes
/// already on the group's entries — every exit of [`register_group_pass`] that follows the cache walk calls it, so a
/// group whose every frame was a cache hit (nothing re-registered, an early return) is counted all the same.
fn note_low_inlier_frames(rc: &RunContext, group: &IntegrationGroup) {
    let rows = rc
        .measured
        .get(&group.key)
        .map(|entries| low_inlier_rows(entries))
        .unwrap_or_default();
    warn_low_inlier_frames(rc.run_id, &group.key, &rows);
}

/// The Register stage's progress `message` (frame-level cache spec §5.6): the two-pass dry pass.
const REGISTER_MESSAGE_CHECKING: &str = "checking the reference";
/// The Register stage's progress `message` for the persisting pass.
const REGISTER_MESSAGE_ALIGNING: &str = "aligning";

/// The reference's term of every member's registration hash (ruling R3 (a), [`reference_measurement_fold`]): its own
/// Measure output when it lives in a group other than `group`, `""` otherwise. `reference_hash` is the reference's
/// calibration hash. The one place the run, the gate and the two-pass key resolve it.
///
/// [`reference_measurement_fold`]: crate::stacking::plan::reference_measurement_fold
fn reference_measurement_term(
    rc: &RunContext,
    group: &IntegrationGroup,
    reference_frame_id: i64,
    reference_hash: &str,
    prepare_path: bool,
) -> Result<String, RunError> {
    let reference_group = rc
        .plan_groups
        .iter()
        .find(|g| g.frames.iter().any(|f| f.frame_id == reference_frame_id))
        .ok_or_else(|| {
            RunError::Other(format!(
                "registration reference (frame {reference_frame_id}) is in no group"
            ))
        })?;
    Ok(crate::stacking::plan::reference_measurement_fold(
        &rc.config,
        group,
        reference_group,
        prepare_path,
        reference_hash,
        rc.measured_groups
            .get(&reference_group.key)
            .and_then(|g| g.beta),
    ))
}

/// One member's calibration hash and the registration hash a row for it must carry against the reference (ruling
/// R3 (d)): exactly what [`register_group_pass`] compares a stored row to, and what the two-pass decision key folds.
/// `reference_measurement_hash` is [`reference_measurement_term`]'s value.
fn member_registration_hash(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    frame: &GroupFrame,
    reference_frame_id: i64,
    reference_hash: &str,
    reference_measurement_hash: &str,
    prepare_path: bool,
) -> Result<(String, String), RunError> {
    let cfg = rc.config.clone();
    let frame_hash = {
        let conn = db(&rc.ctx)?.conn();
        rc.memo.calibration_hash_checked(&conn, &cfg, frame)?
    };
    // Ruling C-18: the same measurement hash `plan.rs`'s `compute_register_stale` folds in - must match, or the gate
    // and the run disagree about whether a stored registration row is fresh.
    let frame_measurement_hash = crate::stacking::plan::registration_measurement_fold(
        &cfg,
        group,
        prepare_path,
        &frame_hash,
        rc.measured_groups.get(&group.key).and_then(|g| g.beta),
    );
    let expected = registration_hash_for(
        &cfg,
        reference_frame_id,
        reference_hash,
        &frame_hash,
        &frame_measurement_hash,
        reference_measurement_hash,
    );
    Ok((frame_hash, expected))
}

/// ONE registration pass over ONE group (M4a Task 4, ruling R-M4a-5 — the
/// body [`stage_register`]'s per-group loop used to inline).
///
/// `persist = true` is stage 5 as it has always been: an existing fresh
/// `registration_results` row is reused, everything else is registered and
/// then WRITTEN — the row ([`crate::stacking::plan::store_registration`]), the optional registered
/// artifact, the entry's own `registration` outcome, and the
/// exclusion/warning bookkeeping a failure implies.
///
/// `persist = false` is the two-pass pick's DRY pass. It never consults the
/// cache (pass 1 always registers afresh — a cached row belongs to the
/// FINAL reference, which is exactly what this pass exists to choose),
/// writes no row and no artifact, stores nothing on the entry, excludes
/// nobody and pushes no warning: a frame that cannot align against the
/// CURRENT reference may well align against the one this pass is about to
/// pick, so judging it here would judge it against a reference the run is
/// about to discard. Its verdicts come back in the returned [`PassResult`]s
/// instead — and `selection.exclude_on_registration_failure = false` does
/// NOT fail the run from a dry pass for the same reason.
///
/// `progress` is `(current, total)`, threaded through both passes so the
/// Register stage's progress bar counts them as one stage.
#[allow(clippy::too_many_arguments)]
fn register_group_pass(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    ref_stars: &ReferenceStars,
    reference_frame_id: i64,
    reference_hash: &str,
    by_frame: &HashMap<i64, RegistrationRecord>,
    force_fresh: bool,
    persist: bool,
    progress: &mut (usize, usize),
    solves: &mut SolveCache,
) -> Result<Vec<PassResult>, RunError> {
    let cfg = rc.config.clone();
    // Every event of the pass names it (frame-level cache spec §5.6).
    let message = if persist {
        REGISTER_MESSAGE_ALIGNING
    } else {
        REGISTER_MESSAGE_CHECKING
    };
    let mut pass: Vec<PassResult> = Vec::new();
    let mut write_failures = RegisteredWriteFailures::default();

    if included_count_of(rc, &group.key) < 3 {
        return Ok(pass);
    }

    // Plan 6b-2 C9: the Register boundary of the landed-file identity check.
    if exclude_changed_members(rc, group)? > 0 && included_count_of(rc, &group.key) < 3 {
        return Ok(pass);
    }

    let snapshot: Vec<(usize, GroupFrame, PathBuf, bool)> = rc
        .measured
        .get(&group.key)
        .map(|v| {
            v.iter()
                .enumerate()
                .filter(|(_, e)| e.included)
                .filter_map(|(i, e)| {
                    e.calibrated.clone().map(|p| {
                        (
                            i,
                            e.frame.clone(),
                            p,
                            e.frame.frame_id == reference_frame_id,
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // The reference's OWN pixel scale, re-read here rather than passed in:
    // the two-pass pick can move the reference between this function's two
    // callers, and looking it up per pass keeps the gate honest without a
    // second parameter to keep in step.
    let reference_scale = find_frame_in_groups(&rc.plan_groups, reference_frame_id)
        .and_then(|f| f.pixel_scale_arcsec);

    let origins = rc.origins.clone();
    // Spec §5.5, plan 6b-3 D1: on a project's grid every frame this pass
    // writes is composed onto the grid through its reference's WCS. The WCS
    // rule (stage 4) guarantees the reference has one; its absence here is a
    // pipeline fault, so it fails the run rather than writing
    // reference-geometry frames.
    let compose: Option<(Arc<GridRun>, Arc<ReferenceWcs>)> = match (&rc.grid, persist) {
        (Some(grid), true) => {
            let record = {
                let conn = db(&rc.ctx)?.conn();
                solve_of(solves, &conn, &origins, reference_frame_id)
            };
            let wcs = record
                .as_ref()
                .and_then(ReferenceWcs::from_record)
                .ok_or_else(|| {
                    RunError::Other(format!(
                        "the registration reference (frame {reference_frame_id}) has no usable WCS; the grid needs one"
                    ))
                })?;
            Some((Arc::clone(grid), Arc::new(wcs)))
        }
        _ => None,
    };
    let mut to_register: Vec<PendingRegistration> = Vec::new();
    // Perf tier A Task 6a fix round 1 (ruling R-TA-5): cache-hit
    // registrations whose artifact needs (re)writing — see the `reused`
    // branch below.
    let mut needs_artifact_rewrite: Vec<ArtifactRewriteItem> = Vec::new();
    // Frame-level cache spec §5.5: the measurement hash is folded only for a frame whose registration reads
    // `fits.0`; the gate derives the same flag (`plan::compute_register_stale`).
    let prepare_path = crate::stacking::origin::subject_prepares(&rc.subject, &rc.origins);
    // Ruling R3 (a): the reference's own Measure output, when its fits seed the reference stars and its group is not
    // this one (`plan::reference_measurement_fold`; the gate resolves the same value).
    let reference_measurement_hash =
        reference_measurement_term(rc, group, reference_frame_id, reference_hash, prepare_path)?;
    for (idx, frame, path, is_reference) in snapshot {
        let (frame_hash, expected_hash) = member_registration_hash(
            rc,
            group,
            &frame,
            reference_frame_id,
            reference_hash,
            &reference_measurement_hash,
            prepare_path,
        )?;

        let mut reused: Option<RegisteredFrameOutcome> = None;
        if persist && !force_fresh {
            if let Some(row) = by_frame.get(&frame.frame_id) {
                if registration_row_is_fresh(row, reference_frame_id, &expected_hash) {
                    match PixelMap::from_json(row.transform_json.as_deref().unwrap_or_default()) {
                        Ok(star) => {
                            let registration_hash = row.config_hash.clone().unwrap_or_default();
                            // D22: a cached star map is recomposed on every
                            // read, so a moved grid or reference WCS never
                            // needs star detection again.
                            let composed = match &compose {
                                None => Ok((star, registration_hash)),
                                Some((grid, reference)) => {
                                    subject_dims(frame.width, frame.height, &path)
                                        .and_then(|dims| {
                                            compose_to_grid(&star, dims, reference, grid)
                                        })
                                        .map(|map| {
                                            let hash = grid_registered_hash(
                                                &registration_hash,
                                                &reference.hash,
                                                &grid.hash,
                                            );
                                            (map, hash)
                                        })
                                }
                            };
                            match composed {
                                Ok((map, registered_hash)) => {
                                    reused = Some(RegisteredFrameOutcome::Aligned {
                                        map,
                                        record: row.clone(),
                                        cached: true,
                                        registered_hash,
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        run_id = rc.run_id,
                                        frame_id = frame.frame_id,
                                        error = %e,
                                        "grid composition of a cached star map failed; re-registering"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = frame.frame_id,
                                error = %e,
                                "stored registration transform failed to parse; re-registering"
                            );
                        }
                    }
                }
            }
        }

        if let Some(outcome) = reused {
            // Perf tier A Task 6a fix round 1 (ruling R-TA-5): a cached
            // `registration_results` row can still be missing its OWN
            // materialized artifact — never written before this became
            // required (an existing catalog's first run under this cycle),
            // or removed by a cleanup. The CHECK stays here (a cheap
            // `find_artifact` SELECT + a disk `stat`, no pixel I/O); the
            // WARP+WRITE, when one is needed, is queued into
            // `needs_artifact_rewrite` and runs in its own fan-out below —
            // never one frame at a time on the run thread, which is exactly
            // what the previous round's `ensure_registered_artifact` did
            // and what made a from-scratch backfill add minutes to this
            // stage.
            // I2 fix wave: a rewrite-needed item's pixel work (warp +
            // write) has NOT happened yet — it must not count as done
            // here, or the Register row reads 100 % for the whole rewrite
            // batch below. Only a truly fresh reused item (or a Failed
            // outcome, which has no pixel work left to do) is counted
            // immediately; a rewrite item is counted when the rewrite
            // fan-out's own ticker ticks it.
            let mut needs_rewrite = false;
            if let RegisteredFrameOutcome::Aligned {
                map,
                record,
                registered_hash,
                ..
            } = &outcome
            {
                let hash = registered_hash.as_str();
                let fresh = registered_artifact_is_fresh(rc, &group.key, frame.frame_id, hash)
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            run_id = rc.run_id,
                            frame_id = frame.frame_id,
                            error = ?e,
                            "failed to check the registered artifact's freshness; rewriting it"
                        );
                        false
                    });
                if !fresh {
                    needs_rewrite = true;
                    let planes = rc
                        .measured
                        .get(&group.key)
                        .and_then(|v| v.get(idx))
                        .map(|e| e.planes)
                        .unwrap_or(1);
                    needs_artifact_rewrite.push(ArtifactRewriteItem {
                        frame_id: frame.frame_id,
                        calibrated: path.clone(),
                        frame: frame.clone(),
                        map: map.clone(),
                        rec: record.clone(),
                        registered_hash: registered_hash.clone(),
                        planes,
                    });
                }
            }
            if let Some(entries) = rc.measured.get_mut(&group.key) {
                entries[idx].registration = Some(outcome);
            }
            if !needs_rewrite {
                progress.0 += 1;
                rc.progress(
                    Stage::Register,
                    Some(group.key.clone()),
                    progress.0,
                    progress.1,
                    0,
                    0,
                    Some(frame.frame_id),
                    Some(message.to_string()),
                );
            }
        } else {
            // M4b Task 2: resolved only for the frames this pass will
            // actually register, so a fully cached group pays nothing for
            // the seed and the log line never claims a gate for a frame
            // that is not being judged.
            let (scale_gate, hint, policy) = {
                let conn = db(&rc.ctx)?.conn();
                registration_gate_and_hint(
                    solves,
                    &conn,
                    &origins,
                    &frame,
                    reference_frame_id,
                    reference_scale,
                )
            };
            tracing::debug!(
                run_id = rc.run_id,
                frame_id = frame.frame_id,
                scale_gate_low = scale_gate.0,
                scale_gate_high = scale_gate.1,
                hint = hint.is_some(),
                seed_policy = seed_policy_field(policy),
                "registration gate"
            );
            // Perf tier A Task 6a fix round 1: this frame's own plane count
            // (stage 3's `MeasuredFrame::planes`), threaded through so the
            // fan-out worker that writes the registered artifact never
            // needs `rc` to name the `_d` (debayered) output file.
            let planes = rc
                .measured
                .get(&group.key)
                .and_then(|v| v.get(idx))
                .map(|e| e.planes)
                .unwrap_or(1);
            to_register.push(PendingRegistration {
                idx,
                frame,
                path,
                is_reference,
                expected_hash,
                scale_gate,
                hint,
                policy,
                planes,
                calibrated_hash: frame_hash,
            });
        }
    }

    if to_register.is_empty() && needs_artifact_rewrite.is_empty() {
        if persist {
            note_low_inlier_frames(rc, group);
        }
        return Ok(pass);
    }

    // Registration warps every frame onto its GROUP's reference geometry
    // (already resolved by the caller — the run-wide one in co-registered
    // mode, the group's own in native) — the OUTPUT buffer size, and a
    // more accurate admission bound than any per-frame native geometry
    // would be. On a project's grid the group geometry is already the
    // grid's (`set_group_geometry` stores the delivered dimensions).
    // `reference_name` is the registered artifact's own
    // `ATH_REGR` card value (perf tier A Task 6a fix round 1: hoisted here,
    // once per group, instead of once per frame inside the old
    // `write_registered_artifact`).
    let (ref_w, ref_h, reference_name) = {
        let geometry = rc.geometry_of(&group.key);
        (
            geometry.width,
            geometry.height,
            geometry
                .calibrated
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("reference")
                .to_string(),
        )
    };
    // Perf tier A Task 6a fix round 1 (ruling R-TA-5): the register
    // fan-out's worker now also warps and writes the registered artifact
    // (`REGISTER_PLANES_RESIDENT`'s own doc has the measurement) —
    // `register_working_set_bytes` folds that cost in, channel-aware.
    //
    // Fix round 2 (ruling R-TA-6 M3): keyed on this batch's own MEASURED
    // plane count, not `group.color_mode` — an OSC group whose debayering
    // is off produces 1-plane frames, and charging it for 9 resident
    // planes it never actually holds would under-admit it for no reason.
    // Both batches share one geometry and one group, so one MAX over
    // whichever items exist covers them; `unwrap_or(1)` is unreachable in
    // practice (the caller already returned above when both are empty).
    let batch_planes = to_register
        .iter()
        .map(|p| p.planes)
        .chain(needs_artifact_rewrite.iter().map(|p| p.planes))
        .max()
        .unwrap_or(1);
    let working_set_bytes = register_working_set_bytes(ref_w as u64, ref_h as u64, batch_planes);
    let budget = fan_budget(rc);
    let admission_n = admission(
        working_set_bytes,
        rc.ctx.image_pool.current_num_threads(),
        budget.bytes,
    );
    log_admission(
        rc,
        Stage::Register,
        &group.key,
        working_set_bytes,
        0,
        admission_n,
        budget,
        "register",
    );
    // Perf tier A Task 6a fix round 1: both fan-outs below write into the
    // SAME directory — created once here rather than once per frame inside
    // the old `write_registered_artifact`. Flat since the frame-level cache
    // (spec §5.1): a frame's registered file does not move with its group.
    let out_dir = rc.layout.registered_dir();
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| RunError::Other(format!("creating {}: {e}", out_dir.display())))?;
    let interp = cfg.registration.interpolation;
    let clamping = cfg.registration.clamping_threshold;
    let run_id = rc.run_id;
    let subject = rc.subject.clone();

    // ── Cache-hit rewrite batch (ruling R-TA-5): parallel, admitted the
    // SAME way as the main fan-out below — no per-frame `rc` access, so it
    // never fights the main batch over `&mut RunContext`. Registration
    // itself is already known (`map`/`rec`); only the pixel work runs
    // here.
    //
    // I2 fix wave: this batch used to report NOTHING while it ran — the
    // `reused` loop above counted every reused frame (rewrite-needed or
    // not) as done up front, so the Register row sat at 100 % for the
    // whole rewrite (0.43 s mono / 1.26 s OSC per frame). It no longer
    // does that (see the loop's own comment); this batch gets its own
    // ticker instead, continuing from the SAME `progress.0` the main
    // batch's ticker below picks up from, so the two compose into one
    // running count. ──
    if !needs_artifact_rewrite.is_empty() {
        let pool_ref = &rc.ctx.image_pool;
        let out_dir_ref = &out_dir;
        let reference_name_ref = &reference_name;
        let grid_hash_ref = compose.as_ref().map(|(g, _)| g.hash.as_str());
        let cancel_ref: &AtomicBool = &rc.cancel;
        let rewrite_ticker = FanOutTicker::new(
            rc,
            Stage::Register,
            Some(group.key.clone()),
            progress.0,
            progress.1,
            0,
            0,
        )
        .with_message(message);
        let rewrite_ticker_ref = &rewrite_ticker;
        let rewrite_results = fan_out(
            needs_artifact_rewrite,
            &AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n),
            FanOutLabel::new(rc, Stage::Register, Some(&group.key), "rewrite"),
            cancel_ref,
            move |item: ArtifactRewriteItem| -> Result<(i64, String, Result<PathBuf, String>), String> {
                let frame_id = item.frame_id;
                let stem = frame_file_stem(&item.frame);
                let out = out_dir_ref.join(registered_file_name(&stem, item.planes == 3));
                let written = write_one_registered_frame(
                    &item.calibrated,
                    &item.map,
                    ref_w,
                    ref_h,
                    interp,
                    clamping,
                    reference_name_ref,
                    &item.rec,
                    grid_hash_ref,
                    Some(pool_ref),
                    &out,
                );
                let result = match written {
                    Ok(()) => Ok((item.frame_id, item.registered_hash, Ok(out))),
                    Err(e) => {
                        tracing::warn!(
                            run_id,
                            frame_id = item.frame_id,
                            error = ?e,
                            "failed to write registered frame"
                        );
                        Ok((item.frame_id, item.registered_hash, Err(format!("{e:#}"))))
                    }
                };
                rewrite_ticker_ref.tick(Some(frame_id));
                result
            },
        );
        rc.check_cancel()?;
        progress.0 = rewrite_ticker.done();
        for res in rewrite_results {
            match res {
                None => return Err(RunError::Cancelled),
                Some(Ok((frame_id, registered_hash, Ok(out_path)))) => {
                    if let Err(e) = record_registered_artifact(
                        rc,
                        &group.key,
                        frame_id,
                        &registered_hash,
                        &out_path,
                    ) {
                        tracing::warn!(
                            run_id = rc.run_id,
                            frame_id,
                            error = ?e,
                            "failed to record registered artifact"
                        );
                    }
                }
                // Write failed (already warned above) or a hard error the
                // closure never actually constructs — either way, nothing
                // to record; the frame falls back to the on-the-fly warp.
                Some(Ok((_, _, Err(error)))) => write_failures.note(error),
                Some(Err(_)) => {}
            }
        }
    }

    if to_register.is_empty() {
        write_failures.flush(rc, group);
        if persist {
            note_low_inlier_frames(rc, group);
        }
        return Ok(pass);
    }

    let meta: Vec<(usize, GroupFrame, String, SeedPolicy)> = to_register
        .iter()
        .map(|p| (p.idx, p.frame.clone(), p.expected_hash.clone(), p.policy))
        .collect();
    // Perf tier 1 Task 9: a `persist` item consumes (and frees) its dry-pass
    // cache entry here, on the run thread, before the fan-out spawns — the
    // dry pass (`persist = false`) never consults the cache itself (a fresh
    // pass always registers afresh, per this function's own doc comment),
    // so `pre` is always `None` there.
    let items: Vec<RegisterItem> = to_register
        .into_iter()
        .map(|p| {
            let pre = if persist {
                rc.dry_pass_stars.remove(&p.frame.frame_id)
            } else {
                None
            };
            // Tier C Task 3 (ruling C-6): resolved only when this item will
            // actually reach `detect_frame_stars` below — a `pre` hit and
            // the reference's own identity row never call it, so reading
            // the artifact for either would be wasted I/O.
            let fits = if pre.is_none() && !p.is_reference {
                resolve_register_fits(
                    rc,
                    &group.key,
                    p.frame.frame_id,
                    p.planes,
                    &p.calibrated_hash,
                )
            } else {
                None
            };
            RegisterItem {
                frame_id: p.frame.frame_id,
                frame: p.frame,
                path: p.path,
                is_reference: p.is_reference,
                scale_gate: p.scale_gate,
                hint: p.hint,
                policy: p.policy,
                pre,
                expected_hash: p.expected_hash,
                planes: p.planes,
                fits,
            }
        })
        .collect();

    // v0.6.3: per-frame ticks from inside the fan-out. `progress.0` already
    // counts this group's reused frames (emitted one by one above), so the
    // ticker continues from it; the results loop below then adopts the
    // ticker's count instead of re-counting — re-emitting from the
    // pre-fan-out value would run the bar backwards.
    let ticker = FanOutTicker::new(
        rc,
        Stage::Register,
        Some(group.key.clone()),
        progress.0,
        progress.1,
        0,
        0,
    )
    .with_message(message);
    let ticker_ref = &ticker;
    let cancel_ref: &AtomicBool = &rc.cancel;
    let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    let reg_cfg = &cfg.registration;
    let ref_stars_ref = ref_stars;
    // Perf tier A Task 6a fix round 1 (ruling R-TA-5): the registered
    // artifact's write now happens INSIDE this closure, right after a
    // successful alignment — see `RegisteredWrite`'s own doc.
    let out_dir_ref = &out_dir;
    let reference_name_ref = &reference_name;
    let compose_ref = &compose;

    let gate = AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n);
    let lbl = FanOutLabel::new(rc, Stage::Register, Some(&group.key), "align");
    let results = fan_out(items, &gate, lbl, cancel_ref, move |item: RegisterItem| {
        let out = if item.is_reference {
            Ok((identity_registration(ref_stars_ref), None))
        } else {
            // Perf tier 1 Task 9: `item.pre` is this frame's dry-pass
            // detection, already known — skip `detect_frame_stars`
            // entirely on a cache hit, exactly as `register_frame` (its
            // composition, kept intact for every OTHER caller) would do
            // for a cache miss. `was_miss` (fix round 1, Minor 2) has to
            // be read before the `match` below moves `item.pre`.
            let was_miss = item.pre.is_none();
            let detected: Result<Arc<DetectedStars>, IntegrationError> = match item.pre {
                Some(d) => Ok(d),
                None => {
                    detect_frame_stars(&item.path, reg_cfg, Some(pool_ref), item.fits.as_deref())
                        .map(Arc::new)
                }
            };
            match detected {
                Err(e) => Err(format!("registration failed: {e}")),
                Ok(d) => {
                    // Fix round 1, Minor 3: `register_detected` itself has
                    // no cancel awareness (it is pure CPU, by design) —
                    // `register_frame`'s own two checks (before the read,
                    // after the detect) covered this before the split.
                    // Checked between the detect and the align so a cancel
                    // raised mid-fan-out short-circuits before the
                    // (possibly expensive) RANSAC/distortion work starts;
                    // `rc.check_cancel()?` right after `fan_out` returns
                    // below is what actually stops the run either way, so
                    // this string is never shown to a user.
                    if cancel_ref.load(Ordering::Relaxed) {
                        Err("registration cancelled".to_string())
                    } else {
                        let mut reg = register_detected(
                            ref_stars_ref,
                            &d,
                            reg_cfg,
                            item.hint.as_ref(),
                            item.policy,
                            item.scale_gate,
                        );
                        // Fix round 1, Minor 2: `register_detected`'s own
                        // `duration_ms` is align-only (Task 9) — restore
                        // the miss arm's read+detect time onto what gets
                        // PERSISTED as `registration_results.compute_
                        // time_ms`, exactly as `register_frame` (every
                        // OTHER caller) still reports for a fresh
                        // detection. A cache hit (`!was_miss`) stays
                        // align-only (ruling 3) — the persisting pass paid
                        // no read/detect for it at all.
                        if was_miss {
                            reg.duration_ms += d.read_ms + d.detect_ms;
                        }
                        // Returned only for a dry pass (`!persist`) — the
                        // persisting pass consumed its cache entry
                        // building this very item (or never had one), so
                        // it has nothing new to cache.
                        Ok((reg, (!persist).then_some(d)))
                    }
                }
            }
        };
        // Perf tier A Task 6a fix round 1 (ruling R-TA-5): for a
        // PERSISTING item whose alignment just succeeded, build the
        // registration record AND write the registered artifact right
        // here — the full-frame warp this used to defer to the sequential
        // results loop is exactly the CPU cost `admission_n` exists to
        // parallelize. A dry pass, or an alignment failure, builds nothing
        // (`None`) — the sequential loop's existing handling for both is
        // unchanged.
        let out = out.and_then(|(reg, dry_stars)| -> Result<_, String> {
            let persisted = if persist {
                match &reg.outcome {
                    Ok(alignment) => {
                        let now =
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let is_reference_row = item.frame_id == reference_frame_id;
                        // The set id here is a placeholder: `store_registration`
                        // overwrites it with the real one (a project's record
                        // is its payload-only star-map artifact, plan 6b-3 D2).
                        let rec = to_record(
                            subject.frame_set_id().unwrap_or(0),
                            item.frame_id,
                            reference_frame_id,
                            is_reference_row,
                            &reg,
                            &item.expected_hash,
                            &now,
                        );
                        // Plan 6b-3 D1: the composed map is what the
                        // registered artifact is written with; `rec` keeps
                        // the STAR map's JSON (a composed map never reaches
                        // `to_record` or `to_json`).
                        let (map, registered_hash) = match compose_ref {
                            None => (alignment.map.clone(), item.expected_hash.clone()),
                            Some((grid, reference)) => {
                                let dims =
                                    subject_dims(item.frame.width, item.frame.height, &item.path)
                                        .map_err(|e| format!("grid composition failed: {e}"))?;
                                let map = compose_to_grid(&alignment.map, dims, reference, grid)
                                    .map_err(|e| format!("grid composition failed: {e}"))?;
                                let hash = grid_registered_hash(
                                    &item.expected_hash,
                                    &reference.hash,
                                    &grid.hash,
                                );
                                (map, hash)
                            }
                        };
                        let stem = frame_file_stem(&item.frame);
                        let out_path =
                            out_dir_ref.join(registered_file_name(&stem, item.planes == 3));
                        let (written, write_error) = match write_one_registered_frame(
                            &item.path,
                            &map,
                            ref_w,
                            ref_h,
                            interp,
                            clamping,
                            reference_name_ref,
                            &rec,
                            compose_ref.as_ref().map(|(g, _)| g.hash.as_str()),
                            Some(pool_ref),
                            &out_path,
                        ) {
                            Ok(()) => (Some(out_path), None),
                            Err(e) => {
                                tracing::warn!(
                                    run_id,
                                    frame_id = item.frame_id,
                                    error = ?e,
                                    "failed to write registered frame"
                                );
                                (None, Some(format!("{e:#}")))
                            }
                        };
                        Some(RegisteredWrite {
                            rec,
                            registered_hash,
                            written,
                            write_error,
                            map,
                        })
                    }
                    Err(_) => None,
                }
            } else {
                None
            };
            Ok((reg, dry_stars, persisted))
        });
        ticker_ref.tick(Some(item.frame_id));
        out
    });

    rc.check_cancel()?;
    progress.0 = ticker.done();

    for (pos, res) in results.into_iter().enumerate() {
        let (idx, frame, hash, policy) = &meta[pos];
        let (idx, policy) = (*idx, *policy);
        match res {
            None => return Err(RunError::Cancelled),
            Some(Err(msg)) => {
                if !persist {
                    pass.push(PassResult {
                        idx,
                        frame_id: frame.frame_id,
                        outcome: Err(msg),
                    });
                } else if cfg.selection.exclude_on_registration_failure {
                    rc.runtime_exclusions.push((frame.frame_id, msg.clone()));
                    if let Some(entries) = rc.measured.get_mut(&group.key) {
                        entries[idx].included = false;
                        entries[idx].reason = Some(msg.clone());
                        entries[idx].registration =
                            Some(RegisteredFrameOutcome::Failed(msg.clone()));
                    }
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = frame.frame_id,
                        reason = %msg,
                        "frame excluded"
                    );
                } else {
                    return Err(RunError::Other(msg.clone()));
                }
            }
            Some(Ok((reg, dry_stars, persisted))) => {
                // Perf tier 1 Task 9: a dry pass's own successful
                // detections are cached for the persisting pass right
                // after it — regardless of whether THIS pass's alignment
                // succeeded (a fresh detection is exactly what the
                // persisting pass needs even when the dry pass's own
                // probe against the OLD reference failed to align).
                if let Some(d) = dry_stars {
                    rc.dry_pass_stars.insert(frame.frame_id, d);
                }
                match &reg.outcome {
                    Ok(alignment) => {
                        if !persist {
                            pass.push(PassResult {
                                idx,
                                frame_id: frame.frame_id,
                                outcome: Ok((alignment.rotation_deg, alignment.translation)),
                            });
                        } else {
                            tracing::debug!(
                                run_id = rc.run_id,
                                frame_id = frame.frame_id,
                                inliers = alignment.inliers,
                                rms_px = alignment.rms_px,
                                "frame registered"
                            );
                            // Ruling R-T6-9: the quad matcher dropped this
                            // frame and the plate-solve seed picked it up. Its
                            // own note is in `warnings` below, but a distinct
                            // event is what makes the pattern countable — on
                            // real data it clusters by filter, not by frame.
                            if alignment.seed == SeedKind::Wcs && policy == SeedPolicy::QuadFirst {
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    "quad seed failed; the plate-solve seed carried this frame"
                                );
                            }
                            for note in &alignment.warnings {
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    note = %note,
                                    "registration warning"
                                );
                                rc.warnings
                                    .push(format!("frame {}: {note}", frame.frame_id));
                            }
                            // Perf tier A Task 6a fix round 1 (ruling
                            // R-TA-5): the record AND the registered
                            // artifact's pixel work were both already done
                            // inside the fan-out worker above — `persisted`
                            // is `Some` for every successfully-aligned,
                            // persisting item (the worker builds it
                            // unconditionally in that case; see the
                            // closure). Only the DB writes — this record's
                            // `registration_results` row and, when the
                            // write succeeded, the `registered` artifact
                            // row — stay here: `rusqlite` connections are
                            // not `Send` across `fan_out`'s plain threads.
                            let RegisteredWrite {
                                rec,
                                registered_hash,
                                written,
                                write_error,
                                map,
                            } = persisted.expect(
                                "a persisting, successfully-aligned item always carries a RegisteredWrite",
                            );
                            {
                                let conn = db(&rc.ctx)?.conn();
                                store_registration(&conn, &rc.subject, |set_id| {
                                    RegistrationRecord {
                                        frames_set_id: set_id,
                                        ..rec.clone()
                                    }
                                })?;
                            }
                            if let Some(error) = write_error {
                                write_failures.note(error);
                            }
                            if let Some(out_path) = &written {
                                if let Err(e) = record_registered_artifact(
                                    rc,
                                    &group.key,
                                    frame.frame_id,
                                    &registered_hash,
                                    out_path,
                                ) {
                                    tracing::warn!(
                                        run_id = rc.run_id,
                                        frame_id = frame.frame_id,
                                        error = ?e,
                                        "failed to record registered artifact"
                                    );
                                }
                            }
                            if let Some(entries) = rc.measured.get_mut(&group.key) {
                                entries[idx].registration = Some(RegisteredFrameOutcome::Aligned {
                                    map,
                                    record: rec,
                                    cached: false,
                                    registered_hash,
                                });
                            }
                        }
                    }
                    Err(align_err) => {
                        let reason = format!("registration failed: {align_err}");
                        if !persist {
                            pass.push(PassResult {
                                idx,
                                frame_id: frame.frame_id,
                                outcome: Err(reason),
                            });
                        } else {
                            let now = chrono::Utc::now()
                                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                            {
                                let conn = db(&rc.ctx)?.conn();
                                store_registration(&conn, &rc.subject, |set_id| {
                                    to_record(
                                        set_id,
                                        frame.frame_id,
                                        reference_frame_id,
                                        false,
                                        &reg,
                                        hash,
                                        &now,
                                    )
                                })?;
                            }
                            if cfg.selection.exclude_on_registration_failure {
                                rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
                                if let Some(entries) = rc.measured.get_mut(&group.key) {
                                    entries[idx].included = false;
                                    entries[idx].reason = Some(reason.clone());
                                    entries[idx].registration =
                                        Some(RegisteredFrameOutcome::Failed(reason.clone()));
                                }
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    reason = %reason,
                                    "frame excluded"
                                );
                            } else {
                                return Err(RunError::Other(reason));
                            }
                        }
                    }
                }
            }
        }
    }
    // Robust quad seed RQ-6: with every outcome of the persisting pass in
    // (cached rows included), name the frames that kept under half the
    // group's median inliers. The two early exits above call it too.
    if persist {
        note_low_inlier_frames(rc, group);
    }
    // One settled tick after the bookkeeping, so the last throttled
    // in-flight tick is never the stage's final word.
    rc.progress(
        Stage::Register,
        Some(group.key.clone()),
        progress.0,
        progress.1,
        0,
        0,
        None,
        Some(message.to_string()),
    );

    write_failures.flush(rc, group);
    Ok(pass)
}

/// The frame ONE group's two-pass dry pass chose, with everything the
/// caller needs to adopt it (M4a ruling R-M4a-5; per group since M4b's
/// native mode, ruling R-M4b-4). `None` means "keep the current
/// reference" — no candidate beat it by [`TWO_PASS_MIN_GAIN_PX`], the dry
/// pass never saw the reference itself, or the winner turned out to be a
/// frame this run cannot adopt (every such case is warned about inside
/// [`two_pass_refine`] and is degrading, never fatal).
struct TwoPassSwitch {
    /// The reference this replaces.
    from: i64,
    frame_id: i64,
    filename: String,
    calibrated: PathBuf,
    weight: Option<f64>,
    stars: ReferenceStars,
    hash: String,
}

/// The two-pass pick's candidates from one dry pass: every frame that aligned, with its Measure-time weight, except a
/// frame under the Measure-time floor that the PSF modes defer (PD-18): it shifts neither the median nor the top-N. The
/// current reference stays whatever its flag, because the pick reads every deviation against it.
fn two_pass_candidates(
    entries: Option<&[MeasuredFrame]>,
    pass: &[PassResult],
    reference_frame_id: i64,
) -> Vec<TwoPassCandidate> {
    pass.iter()
        .filter(|r| {
            r.frame_id == reference_frame_id
                || !entries
                    .and_then(|v| v.get(r.idx))
                    .is_some_and(|e| e.measure_floored)
        })
        .filter_map(|r| {
            let (rotation_deg, translation) = r.outcome.as_ref().ok()?;
            // A frame whose weight never computed ranks last rather
            // than dropping out — it is still a legitimate geometry
            // candidate, just never a top-N one.
            let weight = entries
                .and_then(|v| v.get(r.idx))
                .and_then(|e| e.weight.as_ref())
                .map(|w| w.normalized_mean)
                .unwrap_or(0.0);
            Some(TwoPassCandidate {
                idx: r.idx,
                weight,
                rotation_deg: *rotation_deg,
                translation: *translation,
            })
        })
        .collect()
}

/// The group-level payload row of a two-pass dry pass's decision (frame-level cache spec §5.6): `config_hash` is
/// [`two_pass_key`], payload `{"switchTo": <frame id> | null}`. Group-level ([`artifact_group_key`]), outside
/// `paths::INTERMEDIATE_ARTIFACT_KINDS` (survives "Delete intermediates"), dropped by "All".
pub(crate) const TWO_PASS_ARTIFACT_KIND: &str = "two_pass";

/// The decision a `two_pass` row holds.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TwoPassPayload {
    switch_to: Option<i64>,
}

/// What the pick draws its finalists from (ruling R14 (a)): the whole weight ranking, not only the top
/// `TWO_PASS_CANDIDATES` of it, because a member entering or leaving the top ranks through a weight edit or a
/// floor edit changes the pick's input.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TwoPassRanking {
    /// Every included member the pick can draw from with a finite weight, ids in rank order (weight descending, the
    /// lower index first on a tie).
    ranked: Vec<i64>,
    /// The ids among them whose weight is not finite (the pick drops them), ascending.
    non_finite: Vec<i64>,
    /// The included members under the Measure-time floor that the PSF modes defer, ascending; the candidate reference
    /// is left out (it stays a candidate whatever its flag).
    floored: Vec<i64>,
}

/// The key of a two-pass decision: everything the dry pass and the pick read. The registration subtree (the dry
/// pass registers with it), [`TWO_PASS_PICK_VERSION`], the candidate reference and its calibration hash, the
/// [`TwoPassRanking`] (the full ranking, the members whose weight is not finite, the members under the Measure-time
/// floor), a project's WCS-allowed ids, and every included member's registration hash against the candidate (which
/// carries the member's calibration and, where registration reads Measure's fits, its measurement). Registration is
/// deterministic (RQ-1), so equal inputs give the equal decision.
pub(crate) fn two_pass_key(
    cfg: &StackingConfig,
    candidate_id: i64,
    candidate_calib_hash: &str,
    ranking: &TwoPassRanking,
    allowed_ids: Option<&[i64]>,
    member_hashes: &[(i64, String)],
) -> String {
    two_pass_key_with_version(
        cfg,
        TWO_PASS_PICK_VERSION,
        candidate_id,
        candidate_calib_hash,
        ranking,
        allowed_ids,
        member_hashes,
    )
}

/// [`two_pass_key`] with the pick version supplied, so a test can prove the key follows it.
fn two_pass_key_with_version(
    cfg: &StackingConfig,
    pick_version: u32,
    candidate_id: i64,
    candidate_calib_hash: &str,
    ranking: &TwoPassRanking,
    allowed_ids: Option<&[i64]>,
    member_hashes: &[(i64, String)],
) -> String {
    let join = |ids: &[i64]| {
        ids.iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut upstream: Vec<String> = vec![
        format!("pick:{pick_version}"),
        format!("top:{}", crate::stacking::weights::TWO_PASS_CANDIDATES),
        format!("gain:{:?}", crate::stacking::weights::TWO_PASS_MIN_GAIN_PX),
        format!("ref:{candidate_id}"),
        candidate_calib_hash.to_string(),
        format!("ranked:{}", join(&ranking.ranked)),
        format!("unweighed:{}", join(&ranking.non_finite)),
        format!("floored:{}", join(&ranking.floored)),
    ];
    if let Some(allowed) = allowed_ids {
        upstream.push(format!("allowed:{}", join(allowed)));
    }
    for (id, hash) in member_hashes {
        upstream.push(format!("member:{id}:{hash}"));
    }
    let refs: Vec<&str> = upstream.iter().map(String::as_str).collect();
    crate::stacking::config::stage_hash(
        &crate::stacking::config::registration_subtree(cfg),
        &refs,
        &[],
    )
}

/// One group's two-pass cache lookup: the key, and the stored decision when the row is fresh.
struct TwoPassLookup {
    key: String,
    /// `Some(switch_to)` on a hit.
    cached: Option<Option<i64>>,
}

/// The [`TwoPassRanking`] of a group's entries: the included frames with a calibrated file, the current reference
/// whatever its flag and every other one only when it is not under the Measure-time floor (as in
/// [`two_pass_candidates`]), restricted to `allowed` on a project. The finite-weight ones are ordered by weight with
/// the lower index first on a tie, as `weights::two_pass_pick_among` orders them, and NOT cut to the top N.
fn two_pass_ranking(
    entries: &[MeasuredFrame],
    reference_frame_id: i64,
    allowed: Option<&HashSet<i64>>,
) -> TwoPassRanking {
    let mut floored: Vec<i64> = entries
        .iter()
        .filter(|e| e.included && e.calibrated.is_some())
        .filter(|e| e.measure_floored && e.frame.frame_id != reference_frame_id)
        .map(|e| e.frame.frame_id)
        .collect();
    floored.sort_unstable();
    let mut ranked: Vec<(usize, f64, i64)> = Vec::new();
    let mut non_finite: Vec<i64> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if !(e.included && e.calibrated.is_some()) {
            continue;
        }
        let id = e.frame.frame_id;
        if id != reference_frame_id && (e.measure_floored || allowed.is_some_and(|a| !a.contains(&id))) {
            continue;
        }
        let weight = e.weight.as_ref().map_or(0.0, |w| w.normalized_mean);
        if weight.is_finite() {
            ranked.push((i, weight, id));
        } else {
            non_finite.push(id);
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    non_finite.sort_unstable();
    TwoPassRanking {
        ranked: ranked.into_iter().map(|(_, _, id)| id).collect(),
        non_finite,
        floored,
    }
}

/// The two-pass key of `group` against the candidate reference, and the stored decision when its `two_pass` row is
/// fresh. `force_fresh` (a rerun from Register or earlier) never reads the row. A row that is stale, unparseable, or
/// names a frame this group cannot adopt reads as a miss (the last two are logged). Member hashes go through
/// [`member_registration_hash`], the very call [`register_group_pass`] compares its stored rows with (ruling R3 (d)).
fn two_pass_lookup(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    reference_frame_id: i64,
    reference_hash: &str,
    force_fresh: bool,
) -> Result<TwoPassLookup, RunError> {
    let prepare_path = crate::stacking::origin::subject_prepares(&rc.subject, &rc.origins);
    let reference_measurement_hash =
        reference_measurement_term(rc, group, reference_frame_id, reference_hash, prepare_path)?;
    let members: Vec<GroupFrame> = rc
        .measured
        .get(&group.key)
        .map(|v| {
            v.iter()
                .filter(|e| e.included && e.calibrated.is_some())
                .map(|e| e.frame.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut member_hashes: Vec<(i64, String)> = Vec::with_capacity(members.len());
    for frame in &members {
        let (_, hash) = member_registration_hash(
            rc,
            group,
            frame,
            reference_frame_id,
            reference_hash,
            &reference_measurement_hash,
            prepare_path,
        )?;
        member_hashes.push((frame.frame_id, hash));
    }
    member_hashes.sort_by_key(|(id, _)| *id);

    let allowed: Option<HashSet<i64>> = rc.wcs_rule.as_ref().map(|rule| {
        members
            .iter()
            .map(|f| f.frame_id)
            .filter(|id| rule.candidates.contains(id))
            .collect()
    });
    let allowed_ids: Option<Vec<i64>> = allowed.as_ref().map(|set| {
        let mut ids: Vec<i64> = set.iter().copied().collect();
        ids.sort_unstable();
        ids
    });
    let ranking = rc
        .measured
        .get(&group.key)
        .map(|v| two_pass_ranking(v, reference_frame_id, allowed.as_ref()))
        .unwrap_or_default();
    let key = two_pass_key(
        &rc.config,
        reference_frame_id,
        reference_hash,
        &ranking,
        allowed_ids.as_deref(),
        &member_hashes,
    );
    if force_fresh {
        return Ok(TwoPassLookup { key, cached: None });
    }

    let row = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            artifact_group_key(TWO_PASS_ARTIFACT_KIND, &group.key),
            TWO_PASS_ARTIFACT_KIND,
            None,
        )?
    };
    let cached = row.filter(|r| r.config_hash == key).and_then(|r| {
        let payload: TwoPassPayload = match r
            .payload_json
            .as_deref()
            .map(serde_json::from_str::<TwoPassPayload>)
        {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %e,
                    "two-pass decision unreadable; checking the reference again"
                );
                return None;
            }
            None => return None,
        };
        if let Some(id) = payload.switch_to {
            let adoptable = id != reference_frame_id
                && members.iter().any(|f| f.frame_id == id)
                && rc
                    .measured
                    .get(&group.key)
                    .is_some_and(|v| v.iter().any(|e| e.frame.frame_id == id && e.in_reference_pick()));
            if !adoptable {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    frame_id = id,
                    "two-pass decision names a frame this group cannot adopt; checking the reference again"
                );
                return None;
            }
        }
        Some(payload.switch_to)
    });
    Ok(TwoPassLookup { key, cached })
}

/// [`two_pass_lookup`] after the Register boundary of the landed-file identity check (ruling R14 (b)): a member whose
/// landed file changed is excluded FIRST, so a hit and a miss see the same members and a cached decision cannot name a
/// member this run is about to drop. `exclude_changed_members` is idempotent, so the persisting pass repeats it for
/// nothing. `None` when fewer than 3 included members remain: no decision to look up, and the pass is skipped.
fn two_pass_lookup_after_identity_check(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    reference_frame_id: i64,
    reference_hash: &str,
    force_fresh: bool,
) -> Result<Option<TwoPassLookup>, RunError> {
    exclude_changed_members(rc, group)?;
    if included_count_of(rc, &group.key) < 3 {
        return Ok(None);
    }
    two_pass_lookup(rc, group, reference_frame_id, reference_hash, force_fresh).map(Some)
}

/// Record the dry pass's decision under the key [`two_pass_lookup`] computed before it ran. A failed write is logged
/// and costs only the next run its cache hit.
fn store_two_pass_decision(
    rc: &RunContext,
    group: &IntegrationGroup,
    lookup: &TwoPassLookup,
    switch_to: Option<i64>,
) {
    let payload = match serde_json::to_string(&TwoPassPayload { switch_to }) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(run_id = rc.run_id, group_key = %group.key, error = %e, "two-pass decision not recorded");
            return;
        }
    };
    let written = db(&rc.ctx).map_err(|e| e.to_string()).and_then(|d| {
        let conn = d.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: None,
                group_key: artifact_group_key(TWO_PASS_ARTIFACT_KIND, &group.key),
                kind: TWO_PASS_ARTIFACT_KIND,
                path: None,
                config_hash: &lookup.key,
                size: None,
                modified_at: None,
                payload_json: Some(&payload),
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    });
    if let Err(error) = written {
        tracing::warn!(run_id = rc.run_id, group_key = %group.key, error = %error, "two-pass decision not recorded");
    }
}

/// ONE group's two-pass dry pass and re-pick (M4a ruling R-M4a-5, factored
/// out of `stage_register` by M4b Task 3 so both geometry modes run the
/// SAME pick: co-registered runs it once, over the run reference's own
/// group; native runs it per group, over that group's own members).
///
/// Registers the group once with `persist = false` — no rows, no
/// artifacts, no exclusions — measures each frame's rotation/translation
/// against the CURRENT reference, and asks [`two_pass_pick`] whether one of
/// the top-weighted candidates sits closer to the group's median framing.
/// A winner is only returned once its star detection and stage-1 hash have
/// both been resolved: a switch is an improvement, never a requirement, so
/// a frame whose calibrated file went away between the dry pass and here
/// costs the run its refinement, not its master.
///
/// The `info!` and the run warning that record a switch are emitted here —
/// identical in both modes — while everything a switch IMPLIES for the
/// run-level reference (the `stacking_runs` row, `RunSummary.reference`)
/// stays with the caller, which alone knows whether this group's reference
/// is also the run's.
#[allow(clippy::too_many_arguments)]
fn two_pass_refine(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    ref_stars: &ReferenceStars,
    reference_frame_id: i64,
    reference_hash: &str,
    by_frame: &HashMap<i64, RegistrationRecord>,
    force_fresh: bool,
    progress: &mut (usize, usize),
    solves: &mut SolveCache,
) -> Result<Option<TwoPassSwitch>, RunError> {
    // Frame-level cache spec §5.6: the decision of an earlier run on the same inputs. A hit skips the dry pass; the
    // switch is built through `adopt_two_pass_pick`'s fresh-detection fallback (`rc.dry_pass_stars` is empty).
    let Some(lookup) =
        two_pass_lookup_after_identity_check(rc, group, reference_frame_id, reference_hash, force_fresh)?
    else {
        return Ok(None);
    };
    if let Some(cached) = lookup.cached {
        tracing::info!(
            run_id = rc.run_id,
            group_key = %group.key,
            frame_id = reference_frame_id,
            "two-pass decision reused"
        );
        let Some(switch_to) = cached else {
            return Ok(None);
        };
        // `two_pass_lookup` only reports a hit for a frame of this group.
        let Some(new_idx) = rc
            .measured
            .get(&group.key)
            .and_then(|v| v.iter().position(|e| e.frame.frame_id == switch_to))
        else {
            return Ok(None);
        };
        return adopt_two_pass_pick(rc, group, new_idx, reference_frame_id, None);
    }
    // The identity check may have excluded the only frame the re-pick could land on (ruling R15): no dry pass then,
    // and the progress totals do not count one.
    if !group_can_switch(rc, &group.key, reference_frame_id) {
        return Ok(None);
    }

    let pass = register_group_pass(
        rc,
        group,
        ref_stars,
        reference_frame_id,
        reference_hash,
        by_frame,
        force_fresh,
        false,
        progress,
        solves,
    )?;

    let candidates = two_pass_candidates(
        rc.measured.get(&group.key).map(Vec::as_slice),
        &pass,
        reference_frame_id,
    );

    let reference_idx = pass
        .iter()
        .find(|r| r.frame_id == reference_frame_id)
        .map(|r| r.idx);
    // The CURRENT reference's own geometry — `ref_stars` is the very read
    // `rc.reference_width`/`height` (co-registered) and this group's
    // `GroupGeometry` (native) were both filled from.
    let diagonal_px = (ref_stars.width as f64).hypot(ref_stars.height as f64);

    let Some(reference_idx) = reference_idx else {
        // The reference's own identity row is produced by the same
        // fan-out as everything else, so this only happens if the
        // dry pass never saw the reference at all (it lost its
        // calibrated file between stages) — say so, keep it.
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            frame_id = reference_frame_id,
            "the two-pass dry pass produced no alignment for the reference itself; keeping it"
        );
        return Ok(None);
    };

    // Plan 6b-2 C15: on a project run only a WCS candidate may win the
    // re-pick; a frame set calls today's pick, unchanged.
    let allowed: Option<HashSet<usize>> = rc.wcs_rule.as_ref().map(|rule| {
        rc.measured
            .get(&group.key)
            .map(|v| {
                v.iter()
                    .enumerate()
                    .filter(|(_, e)| rule.candidates.contains(&e.frame.frame_id))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default()
    });
    let pick = match &allowed {
        None => two_pass_pick(
            &candidates,
            reference_idx,
            diagonal_px,
            TWO_PASS_MIN_GAIN_PX,
        ),
        Some(allowed) => two_pass_pick_among(
            &candidates,
            reference_idx,
            diagonal_px,
            TWO_PASS_MIN_GAIN_PX,
            &|idx| allowed.contains(&idx),
        ),
    };
    let Some(new_idx) = pick else {
        // A real decision: no candidate beats the current reference.
        store_two_pass_decision(rc, group, &lookup, None);
        return Ok(None);
    };

    let corner_px_before =
        two_pass_deviation_px(&candidates, reference_idx, diagonal_px).unwrap_or(f64::NAN);
    let corner_px_after =
        two_pass_deviation_px(&candidates, new_idx, diagonal_px).unwrap_or(f64::NAN);
    let switch = adopt_two_pass_pick(
        rc,
        group,
        new_idx,
        reference_frame_id,
        Some((corner_px_before, corner_px_after)),
    )?;
    if let Some(switch) = &switch {
        store_two_pass_decision(rc, group, &lookup, Some(switch.frame_id));
    }
    Ok(switch)
}

/// Resolve the frame at `new_idx` of the group into a [`TwoPassSwitch`]: the end of [`two_pass_refine`], shared by a
/// dry-pass pick and a cached decision. `corner_px` is the pick's `(before, after)` corner displacement, `None` when
/// the decision comes from the cache (the dry pass did not run, so `rc.dry_pass_stars` is empty and the stars come
/// from the fresh-detection fallback below). `Ok(None)` keeps the current reference, with a warn.
fn adopt_two_pass_pick(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    new_idx: usize,
    reference_frame_id: i64,
    corner_px: Option<(f64, f64)>,
) -> Result<Option<TwoPassSwitch>, RunError> {
    let cfg = rc.config.clone();
    let picked = rc
        .measured
        .get(&group.key)
        .and_then(|v| v.get(new_idx))
        .map(|e| {
            (
                e.frame.frame_id,
                e.frame.filename.clone(),
                e.calibrated.clone(),
                e.weight.as_ref().map(|w| w.normalized_mean),
            )
        });
    let Some((new_frame_id, new_filename, Some(new_calibrated), new_weight)) = picked else {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            "the two-pass pick chose a frame with no calibrated file; keeping the current reference"
        );
        return Ok(None);
    };

    // Everything the switch needs from the chosen frame, resolved before
    // anything is mutated — and DEGRADING, not fatal (fix round 1, item
    // 4). A switch is an improvement, never a requirement: the current
    // reference is already resolved, already star-detected and perfectly
    // usable, so a frame whose calibrated file went away or turned
    // unreadable between the dry pass and here must cost the run its
    // refinement, not its master. Every failure below therefore falls
    // through to the same "keeping the current reference" warn as the two
    // sibling no-op paths above.
    //
    // No test forces this branch: the dry pass read THIS SAME calibrated
    // file through the same `detect_frame_stars` read+detect path moments
    // earlier, so the only window is a genuine race (the working folder
    // swept, a volume unmounted, an SMB hiccup) that no in-process fixture
    // can open
    // without a fault-injection hook. It is guarded, logged and non-fatal
    // by construction instead.
    let prepared: Result<(ReferenceStars, String), String> =
        match find_frame_in_groups(&rc.plan_groups, new_frame_id) {
            None => Err("it is not in any plan group".to_string()),
            Some(new_group_frame) => {
                // Tier C Task 3 (ruling C-6): the hash now comes FIRST (it
                // used to follow `stars`) so a fresh mono `fits` artifact
                // can be resolved before the fallback detection below needs
                // it.
                let new_hash: Result<String, String> = {
                    let conn = db(&rc.ctx)?.conn();
                    rc.memo
                        .calibration_hash_checked(&conn, &cfg, &new_group_frame)
                        .map_err(|e| format!("its calibration hash is unresolvable: {e}"))
                };
                match new_hash {
                    Err(e) => Err(e),
                    Ok(new_hash) => {
                        // Perf tier 1 Task 9: the dry pass already detected
                        // this frame's stars moments ago, as a regular
                        // candidate, the SAME `cfg.detection`/`cfg.max_stars`
                        // a fresh `reference_stars` read below would use —
                        // reuse rather than pay for a second read+detect of
                        // the frame the pick just chose. Read here, not
                        // removed: the persisting registration pass that
                        // follows builds a `RegisterItem` for EVERY frame it
                        // registers (`rc.dry_pass_stars.remove`, this
                        // function's caller a few frames up the stack) — the
                        // reference's own item included, whether or not its
                        // dry-pass pick changed here — so the entry is still
                        // removed, just not by this lookup;
                        // `identity_registration` then ignores the
                        // `RegisterItem.pre` it carries (it takes no
                        // detections), so the removal costs nothing, it just
                        // isn't this line's job.
                        let stars = match rc.dry_pass_stars.get(&new_frame_id) {
                            Some(cached) => Ok(ReferenceStars::from(cached.as_ref())),
                            None => {
                                let pool_ref = &rc.ctx.image_pool;
                                // Tier C Task 3 (ruling C-6): this branch has
                                // no test forcing it (see the doc comment
                                // above) — a race between the dry pass and
                                // here, not a real pipeline state — so the
                                // fits reuse is wired the same way every
                                // other reference read is rather than left
                                // stubbed, but it costs nothing when the
                                // branch never runs.
                                // Fix round 1, item 6: `0`, not `1` — an
                                // unresolvable frame must never default to
                                // "mono" (which would wrongly make it
                                // eligible for fits reuse); `0 != 1` fails
                                // `resolve_register_fits`'s own planes check
                                // and falls back to detection instead.
                                let planes = rc
                                    .measured
                                    .get(&group.key)
                                    .and_then(|v| v.get(new_idx))
                                    .map(|e| e.planes)
                                    .unwrap_or(0);
                                let fits = resolve_register_fits(
                                    rc,
                                    &group.key,
                                    new_frame_id,
                                    planes,
                                    &new_hash,
                                );
                                reference_stars(
                                    &new_calibrated,
                                    &cfg.registration,
                                    Some(pool_ref),
                                    fits.as_deref(),
                                )
                                .map_err(|e| format!("its star detection failed: {e}"))
                            }
                        };
                        stars.map(|s| (s, new_hash))
                    }
                }
            }
        };

    let (new_ref_stars, new_hash) = match prepared {
        Err(reason) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                frame_id = new_frame_id,
                reason = %reason,
                "the two-pass pick chose a frame the run cannot adopt; keeping the current reference"
            );
            return Ok(None);
        }
        Ok(prepared) => prepared,
    };

    match corner_px {
        Some((corner_px_before, corner_px_after)) => {
            tracing::info!(
                run_id = rc.run_id,
                group_key = %group.key,
                from = reference_frame_id,
                to = new_frame_id,
                corner_px_before,
                corner_px_after,
                "reference switched by the two-pass pick"
            );
            rc.warnings.push(format!(
                "reference switched by the two-pass pick: {reference_frame_id} → \
                 {new_frame_id} (corner displacement \
                 {corner_px_before:.1} → {corner_px_after:.1} px)"
            ));
        }
        None => {
            tracing::info!(
                run_id = rc.run_id,
                group_key = %group.key,
                from = reference_frame_id,
                to = new_frame_id,
                "reference switched by the cached two-pass decision"
            );
            rc.warnings.push(format!(
                "reference switched by the two-pass pick: {reference_frame_id} → \
                 {new_frame_id} (cached decision)"
            ));
        }
    }

    Ok(Some(TwoPassSwitch {
        from: reference_frame_id,
        frame_id: new_frame_id,
        filename: new_filename,
        calibrated: new_calibrated,
        weight: new_weight,
        stars: new_ref_stars,
        hash: new_hash,
    }))
}

/// Point EVERY group's [`GroupGeometry`] at the run-wide reference and its
/// just-detected geometry (M4b ruling R-M4b-5, co-registered mode only):
/// the run has ONE reference, so a group's entry can never differ from it
/// — not when stage 5 first reads the reference's stars, and not after the
/// two-pass pick moves the reference mid-stage.
///
/// `rc.reference_frame_id`/`reference_width`/`reference_height` are the
/// source of truth here; the calibrated path and stage-1 hash come from
/// the caller, which has just resolved both. On a grid the delivered
/// dimensions are the grid's and deliberately differ from the reference's.
fn adopt_co_registered_geometry(rc: &mut RunContext, calibrated: &Path, hash: &str) {
    let Some(reference_frame_id) = rc.reference_frame_id else {
        return;
    };
    let (width, height) = delivered_dims(rc, (rc.reference_width, rc.reference_height));
    for entry in rc.group_geometry.values_mut() {
        entry.reference_frame_id = reference_frame_id;
        entry.width = width;
        entry.height = height;
        entry.calibrated = calibrated.to_path_buf();
        entry.hash = hash.to_string();
    }
}

/// Whether one group's own reference is still open to the two-pass
/// re-pick: the toggle is on and this group's reference was AUTO-chosen. A
/// `Manual` reference never moves (M4a ruling R-M4a-5) — but it pins its
/// OWN group only (M4b ruling R-M4b-4), so in native mode every other
/// group's auto pick is refined exactly as it is in `Auto` mode.
fn group_two_pass_wanted(
    cfg: &StackingConfig,
    group_reference_frame_id: i64,
    manual_reference_frame_id: Option<i64>,
) -> bool {
    cfg.reference.two_pass && manual_reference_frame_id != Some(group_reference_frame_id)
}

/// Whether a group's two-pass re-pick can move anything (plan 6b-2 C15).
/// The re-pick can only land on a frame the pick may see
/// ([`MeasuredFrame::in_reference_pick`], PD-18) other than the current
/// reference, and on a project run only a WCS candidate may win. A group
/// holding no such frame has nothing to switch to, and a dry pass would only
/// cost time: for a group that borrows a foreign reference (a project group
/// with no candidate, a frame-set group whose every included frame is under
/// the Measure-time floor) it would also log a false "no alignment for the
/// reference itself" warning. The group's viability (>= 3 included) is the
/// caller's, on `included`.
fn group_can_switch(rc: &RunContext, key: &str, reference_frame_id: i64) -> bool {
    rc.measured.get(key).is_some_and(|v| {
        v.iter().any(|e| {
            e.in_reference_pick()
                && e.frame.frame_id != reference_frame_id
                && rc
                    .wcs_rule
                    .as_ref()
                    .is_none_or(|rule| rule.candidates.contains(&e.frame.frame_id))
        })
    })
}

/// The Register progress total of a native run: every viable group's included members once, plus once more for
/// each group that will run a dry pass (the same way the co-registered path counts them). The dry pass is counted
/// only when, AFTER the identity check and the cache lookup (ruling R14 (b)), the decision is not cached and the
/// group can still switch (ruling R15): a member the check excluded may have been the only frame the re-pick could
/// land on, and `two_pass_refine` then skips the pass.
fn native_register_total(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    manual_reference_frame_id: Option<i64>,
    force_fresh: bool,
) -> Result<usize, RunError> {
    let groups = rc.plan_groups.clone();
    let mut total = 0usize;
    for g in &groups {
        if included_count_of(rc, &g.key) < 3 {
            continue;
        }
        let (group_reference, group_reference_hash) = {
            let geometry = rc.geometry_of(&g.key);
            (geometry.reference_frame_id, geometry.hash.clone())
        };
        if group_two_pass_wanted(cfg, group_reference, manual_reference_frame_id)
            && group_can_switch(rc, &g.key, group_reference)
        {
            let Some(lookup) = two_pass_lookup_after_identity_check(
                rc,
                g,
                group_reference,
                &group_reference_hash,
                force_fresh,
            )?
            else {
                continue;
            };
            let included = included_count_of(rc, &g.key);
            total += included;
            if lookup.cached.is_none() && group_can_switch(rc, &g.key, group_reference) {
                total += included;
            }
        } else {
            total += included_count_of(rc, &g.key);
        }
    }
    Ok(total)
}

/// Stage 5 in `native` geometry mode (M4b rulings R-M4b-4/5): every group
/// registers onto ITS OWN reference, in its own geometry, with no
/// cross-group registration at all.
///
/// Per viable group, in plan order: read that group's reference's stars
/// (which fixes the geometry its master, its `.athln` sidecars, its
/// rejection bitmaps and its drizzled output are all delivered in),
/// optionally run the two-pass re-pick over the group's OWN members, then
/// register the group persistently against whatever came out. A switch in
/// the group that also holds the RUN's reference moves the run-level
/// record with it (`stacking_runs.reference_frame_id` stays the largest
/// group's reference, ruling R-M4b-4).
///
/// The co-registered path's one up-front `reference_stars` read has no
/// counterpart here on purpose: each group reads its own, and the run-wide
/// reference is always some group's reference too, so nothing is detected
/// twice.
fn register_native_groups(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let run_reference_frame_id = rc
        .reference_frame_id
        .ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;
    let manual_reference_frame_id =
        (cfg.reference.mode == ReferenceMode::Manual).then_some(run_reference_frame_id);

    let by_frame: HashMap<i64, RegistrationRecord> = {
        let conn = db(&rc.ctx)?.conn();
        stored_registrations(&conn, &rc.subject)?
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect()
    };

    let groups = rc.plan_groups.clone();
    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Register);
    let origins = rc.origins.clone();
    let mut solves: SolveCache = SolveCache::new();

    let total = native_register_total(rc, &cfg, manual_reference_frame_id, force_fresh)?;
    let mut progress = (0usize, total);
    rc.progress(
        Stage::Register,
        None,
        progress.0,
        progress.1,
        0,
        0,
        None,
        None,
    );

    for group in &groups {
        rc.check_cancel()?;
        if included_count_of(rc, &group.key) < 3 {
            continue;
        }

        let geometry = rc.geometry_of(&group.key).clone();
        let mut reference_frame_id = geometry.reference_frame_id;
        let mut reference_hash = geometry.hash.clone();
        // Tier C Task 3 (ruling C-6): `geometry.hash` IS this group's
        // reference's own stage-1 hash already — no extra lookup needed to
        // resolve its `fits` artifact, unlike the co-registered path above.
        let reference_fits = resolve_reference_fits(rc, reference_frame_id, &reference_hash);
        fail_if_reference_changed(rc, reference_frame_id)?;
        let mut ref_stars = {
            let pool_ref = &rc.ctx.image_pool;
            reference_stars(
                &geometry.calibrated,
                &cfg.registration,
                Some(pool_ref),
                reference_fits.as_deref(),
            )
            .map_err(|e| RunError::Other(format!("reference star detection failed: {e}")))?
        };
        set_group_geometry(
            rc,
            &group.key,
            reference_frame_id,
            &ref_stars,
            &geometry.calibrated,
            &reference_hash,
        );

        // Every cross-scale frame of this group needs its reference's own
        // stored solve; warming the cache once here keeps the per-frame
        // lookups to one query for the whole group (including the negative
        // answer).
        {
            let conn = db(&rc.ctx)?.conn();
            solve_of(&mut solves, &conn, &origins, reference_frame_id);
        }

        if group_two_pass_wanted(&cfg, reference_frame_id, manual_reference_frame_id)
            && group_can_switch(rc, &group.key, reference_frame_id)
        {
            if let Some(switch) = two_pass_refine(
                rc,
                group,
                &ref_stars,
                reference_frame_id,
                &reference_hash,
                &by_frame,
                force_fresh,
                &mut progress,
                &mut solves,
            )? {
                let previous = reference_frame_id;
                reference_frame_id = switch.frame_id;
                ref_stars = switch.stars;
                reference_hash = switch.hash;
                set_group_geometry(
                    rc,
                    &group.key,
                    reference_frame_id,
                    &ref_stars,
                    &switch.calibrated,
                    &reference_hash,
                );
                // The run-level reference is the largest group's (ruling
                // R-M4b-4) — when THAT group's own reference moves, the run
                // row and the summary header move with it; every other
                // group's switch is a per-group fact only.
                if previous == run_reference_frame_id {
                    {
                        let conn = db(&rc.ctx)?.conn();
                        set_run_reference(
                            &conn,
                            rc.run_id,
                            reference_frame_id,
                            reference_mode_wire(cfg.reference.mode),
                        )?;
                    }
                    rc.reference_frame_id = Some(reference_frame_id);
                    rc.reference_calibrated = Some(switch.calibrated.clone());
                    rc.reference_width = ref_stars.width;
                    rc.reference_height = ref_stars.height;
                    rc.summary.reference.frame_id = Some(reference_frame_id);
                    rc.summary.reference.filename = Some(switch.filename);
                    rc.summary.reference.weight = switch.weight;
                    rc.summary.reference.switched_from = Some(switch.from);
                }
            }
        }

        register_group_pass(
            rc,
            group,
            &ref_stars,
            reference_frame_id,
            &reference_hash,
            &by_frame,
            force_fresh,
            true,
            &mut progress,
            &mut solves,
        )?;
    }

    Ok(())
}

/// Rewrite ONE group's [`GroupGeometry`] from its resolved reference (M4b
/// native mode): the frame, the geometry its own `reference_stars` read
/// reports, its calibrated file and its stage-1 hash. Also refreshes the
/// run-wide `reference_width`/`height` when this group's reference is the
/// run's, so the two never disagree about the same frame. On a grid the
/// group's delivered dimensions are the grid's, not the reference's.
fn set_group_geometry(
    rc: &mut RunContext,
    key: &str,
    reference_frame_id: i64,
    ref_stars: &ReferenceStars,
    calibrated: &Path,
    hash: &str,
) {
    let (width, height) = delivered_dims(rc, (ref_stars.width, ref_stars.height));
    if let Some(entry) = rc.group_geometry.get_mut(key) {
        entry.reference_frame_id = reference_frame_id;
        entry.width = width;
        entry.height = height;
        entry.calibrated = calibrated.to_path_buf();
        entry.hash = hash.to_string();
    }
    if rc.reference_frame_id == Some(reference_frame_id) {
        rc.reference_width = ref_stars.width;
        rc.reference_height = ref_stars.height;
    }
}

/// The geometry a group is delivered in (plan 6b-3): the grid's on a
/// project run, else the group reference's own `native` dimensions.
fn delivered_dims(rc: &RunContext, native: (usize, usize)) -> (usize, usize) {
    rc.grid.as_ref().map(|g| g.dims()).unwrap_or(native)
}

/// Stage 5 (register, spec §3, ruling 10): register every included frame of
/// every VIABLE group (≥ 3 included, per stage 3) onto the ONE global
/// reference (its stars computed once), reusing an existing
/// `registration_results` row when it is still fresh
/// ([`registration_row_is_fresh`], the SAME predicate the plan's own
/// `stale_stages` uses — ruling 10), fanning the rest out. Writes every
/// group's every frame's `stacking_run_frames` row at the end
/// ([`write_frame_rows`]), included or not.
///
/// M4a Task 4 (ruling R-M4a-5): in `Auto` mode with `reference.two_pass`
/// on, the reference's OWN group is registered once first through
/// [`register_group_pass`] with `persist = false` — a dry pass whose only
/// product is each frame's rotation/translation against the stage-4 pick.
/// [`two_pass_pick`] then re-chooses the reference among the top-weighted
/// of those frames by corner displacement from that group's median
/// transform, and the real, persisting loop below runs with whatever came
/// out (re-detecting the new reference's stars and recomputing its stage-1
/// hash first, so every row the second pass writes names the FINAL
/// reference). Both passes live inside ONE `Stage::Register` timing and
/// share ONE progress total. A `Manual` reference never moves.
fn stage_register(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
    let meter = crate::stacking::telemetry::StageMeter::start(rc, Stage::Register, None);
    let cfg = rc.config.clone();

    if cfg.registration.geometry == RegistrationGeometry::Native {
        register_native_groups(rc)?;
        reweigh_groups(rc)?;
        write_frame_rows(rc)?;
        meter.finish();
        rc.timings.push(crate::stacking::provenance::StageTiming {
            stage: Stage::Register,
            duration_ms: stage_start.elapsed().as_millis() as u64,
        });
        // Perf tier 1 Task 9, belt and braces: `register_group_pass`
        // already drains its own dry-pass entries as the persisting pass
        // consumes them (per group, since native mode runs its dry pass
        // through the SAME function once per group), but a group that
        // never reaches a persisting pass — cancelled mid-stage, or with
        // fewer than 3 included frames after the dry pass excluded one —
        // would otherwise leave its entries stranded until the run ends.
        rc.dry_pass_stars.clear();
        return Ok(());
    }

    let mut reference_frame_id = rc
        .reference_frame_id
        .ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;
    let reference_calibrated = rc
        .reference_calibrated
        .clone()
        .ok_or_else(|| RunError::Other("reference frame has no calibrated file".to_string()))?;

    // Tier C Task 3 (ruling C-6): the reference's own calibration hash is
    // resolved BEFORE the star read now (it used to follow it) so a fresh
    // mono `fits` artifact can be looked up and handed to `reference_stars`
    // instead of a fresh detection — `calibration_hash_checked` is memoized,
    // so hoisting this costs nothing new.
    let reference_group_frame = find_frame_in_groups(&rc.plan_groups, reference_frame_id)
        .ok_or_else(|| RunError::Other("reference frame not found in any group".to_string()))?;
    let mut reference_hash = {
        let conn = db(&rc.ctx)?.conn();
        rc.memo
            .calibration_hash_checked(&conn, &cfg, &reference_group_frame)?
    };
    let reference_fits = resolve_reference_fits(rc, reference_frame_id, &reference_hash);

    // The identity check comes before the reference's star read, so that
    // read never sees a changed landed file.
    fail_if_reference_changed(rc, reference_frame_id)?;
    let mut ref_stars = {
        let pool_ref = &rc.ctx.image_pool;
        reference_stars(
            &reference_calibrated,
            &cfg.registration,
            Some(pool_ref),
            reference_fits.as_deref(),
        )
        .map_err(|e| RunError::Other(format!("reference star detection failed: {e}")))?
    };
    rc.reference_width = ref_stars.width;
    rc.reference_height = ref_stars.height;

    // M4b ruling R-M4b-5: co-registered means every group's geometry IS the
    // run-wide reference's — the measured dimensions stage 4 recorded are
    // replaced here by the authoritative `reference_stars` read.
    adopt_co_registered_geometry(rc, &reference_calibrated, &reference_hash);

    let by_frame: HashMap<i64, RegistrationRecord> = {
        let conn = db(&rc.ctx)?.conn();
        stored_registrations(&conn, &rc.subject)?
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect()
    };

    let groups = rc.plan_groups.clone();
    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Register);

    // M4b Task 2: one stored-solve lookup per frame for the WHOLE stage,
    // shared by the dry pass and the persisting one. The reference's own
    // row goes in first — every cross-scale frame's seed needs it.
    let origins = rc.origins.clone();
    let mut solves: SolveCache = SolveCache::new();
    {
        let conn = db(&rc.ctx)?.conn();
        solve_of(&mut solves, &conn, &origins, reference_frame_id);
    }

    // The dry pass's own group (ruling R-M4a-5): Auto mode, two-pass on,
    // and the reference's group viable — a group the persisting loop would
    // skip anyway gives nothing to take a median over.
    let dry_group: Option<IntegrationGroup> =
        if cfg.reference.mode == ReferenceMode::Auto && cfg.reference.two_pass {
            groups
                .iter()
                .find(|g| {
                    g.frames.iter().any(|f| f.frame_id == reference_frame_id)
                        && included_count_of(rc, &g.key) >= 3
                        && group_can_switch(rc, &g.key, reference_frame_id)
                })
                .cloned()
        } else {
            None
        };
    // A cached two-pass decision removes the dry pass from the total (frame-level cache spec §5.6).
    // The lookup runs after the identity check, as `two_pass_refine` does (ruling R14 (b)), so the totals count the
    // members that will register.
    let dry_total = match dry_group.as_ref() {
        Some(g) => {
            match two_pass_lookup_after_identity_check(rc, g, reference_frame_id, &reference_hash, force_fresh)? {
                Some(lookup) if lookup.cached.is_none() && group_can_switch(rc, &g.key, reference_frame_id) => {
                    included_count_of(rc, &g.key)
                }
                _ => 0,
            }
        }
        None => 0,
    };
    let mut viable_total = 0usize;
    for g in &groups {
        let included = included_count_of(rc, &g.key);
        if included >= 3 {
            viable_total += included;
        }
    }

    let mut progress = (0usize, viable_total + dry_total);
    rc.progress(
        Stage::Register,
        None,
        progress.0,
        progress.1,
        0,
        0,
        None,
        None,
    );

    if let Some(group) = &dry_group {
        rc.check_cancel()?;
        if let Some(switch) = two_pass_refine(
            rc,
            group,
            &ref_stars,
            reference_frame_id,
            &reference_hash,
            &by_frame,
            force_fresh,
            &mut progress,
            &mut solves,
        )? {
            {
                let conn = db(&rc.ctx)?.conn();
                set_run_reference(
                    &conn,
                    rc.run_id,
                    switch.frame_id,
                    reference_mode_wire(cfg.reference.mode),
                )?;
            }

            reference_frame_id = switch.frame_id;
            ref_stars = switch.stars;
            reference_hash = switch.hash;
            rc.reference_width = ref_stars.width;
            rc.reference_height = ref_stars.height;
            rc.reference_frame_id = Some(switch.frame_id);
            rc.reference_calibrated = Some(switch.calibrated.clone());
            rc.summary.reference.frame_id = Some(switch.frame_id);
            rc.summary.reference.filename = Some(switch.filename);
            rc.summary.reference.weight = switch.weight;
            rc.summary.reference.switched_from = Some(switch.from);
            // Co-registered: every group registers onto the run's ONE
            // reference, so the switch moves every group's geometry with it
            // (ruling R-M4b-5).
            adopt_co_registered_geometry(rc, &switch.calibrated, &reference_hash);
        }
    }

    for group in &groups {
        rc.check_cancel()?;
        register_group_pass(
            rc,
            group,
            &ref_stars,
            reference_frame_id,
            &reference_hash,
            &by_frame,
            force_fresh,
            true,
            &mut progress,
            &mut solves,
        )?;
    }

    reweigh_groups(rc)?;
    write_frame_rows(rc)?;

    meter.finish();
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Register,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // Perf tier 1 Task 9, belt and braces (see the native-mode return
    // above for why this can't rely solely on the per-item drain in
    // `register_group_pass`).
    rc.dry_pass_stars.clear();

    Ok(())
}

/// Perf tier A Task 6a fix round 1 (ruling R-TA-5): the registered
/// artifact's PIXEL work ONLY — source cards, the warp, the tmp+atomic-
/// rename write — with no `RunContext`/DB access at all, so BOTH register
/// fan-out closures (the main align-and-write one, and the cache-hit
/// rewrite-only one) can call it directly from a worker thread. `rec`'s
/// `model`/`transform_json`/`rms_residual_px` (already resolved by
/// [`to_record`]) feed the `ATH_REG*` provenance cards — see
/// [`RegisteredCards`]'s own doc for what each one means. The registered
/// artifact is a REQUIRED per-frame output since this task — Normalize and
/// Integrate both read it in place of re-warping the calibrated frame on
/// every band (`RegisteredSource::open_materialized`).
///
/// The config field `registration.write_registered_frames` (Settings →
/// Stacking, `RegisterPanel`'s "Write registered frames" toggle) no longer
/// gates this write — kept only because removing it would be a
/// command-surface change out of this task's scope; it is otherwise inert.
#[allow(clippy::too_many_arguments)]
fn write_one_registered_frame(
    calibrated: &Path,
    map: &PixelMap,
    ref_w: usize,
    ref_h: usize,
    interp: Interpolation,
    clamping: f32,
    reference_name: &str,
    rec: &RegistrationRecord,
    grid_hash: Option<&str>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    out: &Path,
) -> anyhow::Result<()> {
    let source = source_cards_from_file(calibrated)?;
    let cards = build_registered_cards(
        &source,
        &RegisteredCards {
            reference_name,
            model: rec.model.as_deref().unwrap_or_default(),
            transform_json: rec.transform_json.as_deref().unwrap_or_default(),
            interpolation: interp,
            clamping,
            rms_px: rec.rms_residual_px,
            // D3: a grid frame states the grid's own row order, never the
            // reference's.
            reference_roworder: grid_hash.map(|_| "BOTTOM-UP"),
            grid_hash,
        },
    )?;
    write_registered_frame(
        calibrated, map, ref_w, ref_h, interp, clamping, &cards, pool, out,
    )?;
    Ok(())
}

/// Perf tier A Task 6a fix round 1: the registered artifact's DB
/// bookkeeping ONLY — `file_identity` + the `registered` artifact row,
/// keyed on `registered_hash` (plan 6b-3 D2): the registration hash, already
/// folded with the frame's own calibration hash by `registration_hash_for`
/// — a re-registration or a re-calibration invalidates it exactly like the
/// LN `.athln` sidecars — or, on a grid, the composed hash that also folds
/// the reference's WCS and the grid. Split from the pixel work
/// ([`write_one_registered_frame`]) because `rusqlite` connections are not
/// `Send` across `fan_out`'s plain `thread::scope` workers — this runs on
/// the run thread, in the sequential results loop, right after the write
/// a worker already finished.
fn record_registered_artifact(
    rc: &mut RunContext,
    group_key: &str,
    frame_id: i64,
    registered_hash: &str,
    out: &Path,
) -> anyhow::Result<()> {
    let (size, modified_at) = file_identity(out)?;
    let conn = db(&rc.ctx)?.conn();
    let group_key = artifact_group_key("registered", group_key);
    // The row this write replaces, read BEFORE the upsert (see `replace_frame_level_file`).
    let old = crate::db::stacking::find_artifact(&conn, &rc.subject, group_key, "registered", Some(frame_id))?;
    upsert_artifact(
        &conn,
        &NewArtifact {
            subject: &rc.subject,
            frame_id: Some(frame_id),
            group_key,
            kind: "registered",
            path: out.to_str(),
            config_hash: registered_hash,
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: None,
        },
    )?;
    replace_frame_level_file(&conn, &rc.layout.registered_root(), old.as_ref(), out);
    Ok(())
}

/// Perf tier A Task 6a fix round 1: the CHEAP half of what used to be
/// `ensure_registered_artifact` — a `find_artifact` SELECT + [`is_fresh`]
/// (a disk `stat`, no pixel I/O) — called from `register_group_pass`'s
/// cache-hit branch to decide whether a frame needs queuing into
/// `needs_artifact_rewrite`. The expensive half (warp + write) now runs in
/// that batch's own fan-out, not here.
fn registered_artifact_is_fresh(
    rc: &RunContext,
    group_key: &str,
    frame_id: i64,
    hash: &str,
) -> anyhow::Result<bool> {
    let conn = db(&rc.ctx)?.conn();
    Ok(crate::db::stacking::find_artifact(
        &conn,
        &rc.subject,
        artifact_group_key("registered", group_key),
        "registered",
        Some(frame_id),
    )?
    .is_some_and(|row| is_fresh(&row, hash)))
}

/// `r_<stem>[_d].fits` for the SAME frame-only `stem`
/// [`frame_file_stem`] gave the calibrated file (fix round 1: the two
/// naming schemes must agree by construction). `debayered` is the caller's
/// own known fact (`MeasuredFrame::planes == 3`) — NOT inferred by
/// checking whether the calibrated file's own on-disk name ends in `_d`
/// (fix round 1's addendum: a SOURCE light whose own filename already ends
/// in `_d`, e.g. a mono `vega_d.fits`, made that heuristic misfire —
/// `c_vega_d.fits` "looks" debayered by name alone even though it is not).
/// Matches `register_probe.rs`'s own convention of keeping the marker on
/// the registered output too.
fn registered_file_name(stem: &str, debayered: bool) -> String {
    if debayered {
        format!("r_{stem}_d.fits")
    } else {
        format!("r_{stem}.fits")
    }
}

// ── Plan D D1w: the reweigh (end of stage 5) ────────────────────────────────

/// `reason` of a fallback names "under 20 common stars"; the count it names is the module's.
const _: () = assert!(common_set::COMMON_MIN_STARS == 20);

/// A registration map's linear scale at the frame centre: the square root of the Jacobian determinant of
/// `forward_exact`, by central differences over one pixel. `NaN` for a map that is not finite there.
fn map_scale(map: &PixelMap, width: usize, height: usize) -> f64 {
    let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
    let (xp, yp) = map.forward_exact(cx + 1.0, cy);
    let (xm, ym) = map.forward_exact(cx - 1.0, cy);
    let (xq, yq) = map.forward_exact(cx, cy + 1.0);
    let (xn, yn) = map.forward_exact(cx, cy - 1.0);
    let (a, c) = ((xp - xm) / 2.0, (yp - ym) / 2.0);
    let (b, d) = ((xq - xn) / 2.0, (yq - yn) / 2.0);
    (a * d - b * c).abs().sqrt()
}

/// Why one (group, plane) keeps its Measure-time weight inputs (ruling PD-6). Nothing here is per frame.
#[derive(Debug, Clone, PartialEq)]
enum Fallback {
    /// Fewer than `COMMON_MIN_STARS` stars are common to the members.
    TooFewStars(usize),
    /// This member has no fresh readable `.athf` for the plane (or no such plane).
    MissingFits(i64),
    /// No member at or above the median star count has a usable FWHM, so there is no anchor.
    NoAnchor,
    /// The set is big enough, but the anchor's aperture sums over it add to zero or less (PD-30), so the members' ratios
    /// have nothing to scale.
    AnchorFluxNotPositive(usize),
    /// The match table did not fit the candidates: a wiring bug of this module, not a property of the sky. It is
    /// reported as an `error!` where it is found and falls back like the others, never as "under 20 common stars".
    MalformedTable,
}

impl Fallback {
    /// The dictionary's `reason` of the fallback warn.
    fn reason(&self) -> &'static str {
        match self {
            Fallback::TooFewStars(_) => "under 20 common stars",
            Fallback::MissingFits(_) => "member without a fresh .athf",
            Fallback::NoAnchor => "no anchor",
            Fallback::AnchorFluxNotPositive(_) => "anchor flux not positive",
            Fallback::MalformedTable => "match table malformed",
        }
    }

    /// The dictionary's `common_stars` of the fallback warn: the set size when one was counted, else 0.
    fn common_stars(&self) -> usize {
        match self {
            Fallback::TooFewStars(n) | Fallback::AnchorFluxNotPositive(n) => *n,
            _ => 0,
        }
    }
}

/// One (group, plane) after the match: the set and every member's flux over it, or why there is none.
enum PlaneOutcome {
    Measured {
        positions: Vec<(f64, f64)>,
        per_member: Vec<common_set::CommonFlux>,
        /// The anchor's candidate stars the set was drawn from (`common_set::candidates`): the denominator of the
        /// anchor-map warn (PD-39).
        candidates: usize,
    },
    Fallback(Fallback),
}

/// One member of a group being reweighed: its position in `rc.measured[group]`, its frame id and the measurement hash
/// its `.athf` rows are keyed on.
struct ReweighMember {
    idx: usize,
    frame_id: i64,
    hash: String,
}

/// One member's fresh readable `.athf` for `plane`, the LN lookup (`find_artifact` + `is_fresh` on the measurement
/// hash). `Ok(None)` when it is missing, stale or unreadable - an unreadable file is logged here; the caller falls
/// back.
fn read_member_fits(
    rc: &RunContext,
    member: &ReweighMember,
    plane: usize,
) -> Result<Option<Vec<psf_signal::StarFit>>, RunError> {
    let kind = fits_artifact::artifact_kind(plane);
    let row = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            artifact_group_key(&kind, ""),
            &kind,
            Some(member.frame_id),
        )?
    };
    let Some(path) = row
        .filter(|r| is_fresh(r, &member.hash))
        .and_then(|r| r.path)
    else {
        return Ok(None);
    };
    match fits_artifact::read_fits(Path::new(&path)) {
        Ok(fits) => Ok(Some(fits)),
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                frame_id = member.frame_id,
                plane,
                path = %path,
                error = %e,
                "weights: fits artifact unreadable"
            );
            Ok(None)
        }
    }
}

/// The common set of one (group, plane) and every member's flux over it: the anchor's candidates, then each member's
/// match through its own registration map, one member's fits resident at a time (`common_set`, PD-3).
fn plane_outcome(
    rc: &RunContext,
    group_key: &str,
    entries: &[MeasuredFrame],
    members: &[ReweighMember],
    plane: usize,
    planes: usize,
    anchor: usize,
) -> Result<PlaneOutcome, RunError> {
    // The member's plane and map, `None` for a member the plane does not exist on.
    let plane_of = |k: usize| {
        let e = &entries[members[k].idx];
        let ch = e.measurement.as_ref()?.channels.get(plane)?;
        match &e.registration {
            Some(RegisteredFrameOutcome::Aligned { map, .. }) => Some((ch, map)),
            _ => None,
        }
    };
    let missing = |k: usize| PlaneOutcome::Fallback(Fallback::MissingFits(members[k].frame_id));

    let cands = {
        let Some((ch, map)) = plane_of(anchor) else {
            return Ok(missing(anchor));
        };
        let Some(fits) = read_member_fits(rc, &members[anchor], plane)? else {
            return Ok(missing(anchor));
        };
        common_set::candidates(&common_set::MemberPlane {
            fits: &fits,
            map,
            fwhm_px: ch.fwhm_px,
            noise: ch.noise,
            c_stand_in: ch.aperture_correction_group.unwrap_or(1.0),
            planes,
        })
    };
    let mut usable = Vec::with_capacity(members.len());
    for k in 0..members.len() {
        // Between members, as `stage_prepare`'s per-frame loop and `solve_project_frames`' per-job loop do: a group's
        // planes x members `.athf` reads are the longest stretch of the reweigh.
        rc.check_cancel()?;
        let Some((ch, map)) = plane_of(k) else {
            return Ok(missing(k));
        };
        let Some(fits) = read_member_fits(rc, &members[k], plane)? else {
            return Ok(missing(k));
        };
        usable.push(common_set::match_member(
            &cands,
            &common_set::MemberPlane {
                fits: &fits,
                map,
                fwhm_px: ch.fwhm_px,
                noise: ch.noise,
                c_stand_in: ch.aperture_correction_group.unwrap_or(1.0),
                planes,
            },
        ));
    }
    Ok(match common_set::common_flux(&cands, &usable, anchor, planes) {
        common_set::CommonOutcome::Measured {
            positions,
            per_member,
        } => PlaneOutcome::Measured {
            positions,
            per_member,
            candidates: cands.len(),
        },
        common_set::CommonOutcome::TooFewStars { stars } => {
            PlaneOutcome::Fallback(Fallback::TooFewStars(stars))
        }
        common_set::CommonOutcome::NoAnchorFlux { stars } => {
            PlaneOutcome::Fallback(Fallback::AnchorFluxNotPositive(stars))
        }
        common_set::CommonOutcome::MalformedTable => {
            tracing::error!(
                run_id = rc.run_id,
                group_key,
                plane,
                candidates = cands.len(),
                members = usable.len(),
                "weights: common-set match table malformed"
            );
            PlaneOutcome::Fallback(Fallback::MalformedTable)
        }
    })
}

/// Plan D D1w (rulings PD-1, PD-2, PD-5, PD-6): at the end of stage 5, under the PSF-family weight modes, every group's
/// PSF weights are recomputed on its frame-common star set, and the 5 % weight floor that stage 3 deferred is applied to
/// the new weight.
///
/// Per group, over the included members that registered ([`RegisteredFrameOutcome::Aligned`]): the anchor
/// ([`common_set::pick_anchor`]), then per plane each member's `.athf` fits (the LN lookup) matched through its own
/// stored map ([`plane_outcome`]; no pixel is read). On a set, the plane's set is recorded in `rc.common_stars` (the
/// SNR gain's stars); under the PSF family the plane's `common_*` fields too, and `psf_signal_weight` / `psf_snr` are
/// recomputed from them (the `take_group_c` precedent: in memory, the `metrics` artifact is untouched); on a fallback
/// the plane keeps its Measure-time inputs, with a `warn!` and a run warning.
///
/// Plan D DF-c (PD-19 b): that first phase runs in EVERY weight mode - the set is a property of the group's frames and
/// the SNR gain is read on it, as v0.7.0 showed it in every mode. Outside the PSF family ([`defers_weight_gate`] false)
/// nothing else changes: the channel writes and the weights below are the PSF family's alone, so the frames table's
/// `psfSnr`, `psf_signal_weight` and weights stay stage 3's bit for bit (PD-5).
///
/// Then the group's weights: with at least one plane on a set they are recomputed over the members (planes at their new
/// or their Measure-time inputs), and a frame of the group that was not weighed (out at Measure, or at Register) loses
/// its weight - its Measure-time value is normalized to another maximum and the frames table has one scale. A group
/// whose every plane fell back keeps stage 3's weights as they are. Either way the floor ([`weight_gate`]) runs on the
/// members' weights.
///
/// Never re-admits an excluded frame. The registration reference stays the Measure-time pick (PD-11), and an Auto
/// reference under the floor is excluded like any frame (PD-17): the grid's origin is geometry, not pixels, and no later
/// stage needs it in the stack. A MANUAL pin stays, with a warn and a run warning: stage 4 forces a pin in despite the
/// selection.
fn reweigh_groups(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let groups = rc.plan_groups.clone();
    for group in &groups {
        rc.check_cancel()?;
        reweigh_group(rc, group, &cfg)?;
    }
    Ok(())
}

fn reweigh_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    cfg: &StackingConfig,
) -> Result<(), RunError> {
    // The members: included, registered, measured, weighed.
    let member_idx: Vec<usize> = match rc.measured.get(&group.key) {
        Some(entries) => entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.included
                    && e.measurement.is_some()
                    && e.weight.is_some()
                    && matches!(e.registration, Some(RegisteredFrameOutcome::Aligned { .. }))
            })
            .map(|(i, _)| i)
            .collect(),
        None => return Ok(()),
    };
    if member_idx.is_empty() {
        return Ok(());
    }
    let mut members = Vec::with_capacity(member_idx.len());
    for idx in member_idx {
        let frame = rc.measured[&group.key][idx].frame.clone();
        let calib_hash = {
            let conn = db(&rc.ctx)?.conn();
            rc.memo.calibration_hash_checked(&conn, cfg, &frame)?
        };
        members.push(ReweighMember {
            idx,
            frame_id: frame.frame_id,
            hash: group_measurement_hash(rc, cfg, &group.key, &calib_hash),
        });
    }

    // PD-19 b: the set is built in every mode; the channel writes and the weights (phases 2 and 3 below) are the PSF
    // family's.
    let psf_family = defers_weight_gate(cfg.measurement.weight_mode);

    // Phase 1, on shared borrows: the anchor and each plane's outcome.
    let (anchor, outcomes) = {
        let entries = rc.measured[&group.key].as_slice();
        let meas = |k: usize| {
            entries[members[k].idx]
                .measurement
                .as_ref()
                .expect("a member is measured")
        };
        let n = members.len();
        let nch = (0..n).map(|k| meas(k).channels.len()).max().unwrap_or(0);
        let mean_fwhm: Vec<f64> = (0..n).map(|k| meas(k).mean_fwhm_px()).collect();
        let unsaturated: Vec<usize> = (0..n)
            .map(|k| meas(k).channels.iter().map(|c| c.stars_unsaturated).sum())
            .collect();
        let frame_ids: Vec<i64> = members.iter().map(|m| m.frame_id).collect();
        let anchor = common_set::pick_anchor(&mean_fwhm, &unsaturated, &frame_ids);

        // The match radius is built from frame-pixel FWHMs and applied in group pixels (D1 review M4): a map whose
        // scale is not 1 is not corrected for, only logged.
        for (k, m) in members.iter().enumerate() {
            if let Some(RegisteredFrameOutcome::Aligned { map, .. }) = &entries[m.idx].registration {
                let scale = map_scale(map, meas(k).width, meas(k).height);
                if scale.is_finite() && (scale - 1.0).abs() > 0.05 {
                    tracing::debug!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        frame_id = m.frame_id,
                        map_scale = scale,
                        "weights: registration map scale differs from 1"
                    );
                }
            }
        }

        let mut outcomes = Vec::with_capacity(nch);
        for plane in 0..nch {
            outcomes.push(match anchor {
                None => PlaneOutcome::Fallback(Fallback::NoAnchor),
                Some(a) => plane_outcome(rc, &group.key, entries, &members, plane, nch, a)?,
            });
        }
        (anchor, outcomes)
    };
    let anchor_frame_id = anchor.map(|a| members[a].frame_id);

    // Phase 2: record each plane's outcome.
    let any_measured = outcomes
        .iter()
        .any(|o| matches!(o, PlaneOutcome::Measured { .. }));
    let nch = outcomes.len();
    let mut paired_by_plane: Vec<Option<Vec<usize>>> = vec![None; nch];
    let mut sets: Vec<Option<Vec<(f64, f64)>>> = Vec::with_capacity(nch);
    for (plane, outcome) in outcomes.into_iter().enumerate() {
        match outcome {
            PlaneOutcome::Measured {
                positions,
                per_member,
                candidates,
            } => {
                let stars = positions.len();
                if psf_family {
                    let entries = rc
                        .measured
                        .get_mut(&group.key)
                        .expect("the group's entries");
                    for (m, cf) in members.iter().zip(&per_member) {
                        let ch = &mut entries[m.idx]
                            .measurement
                            .as_mut()
                            .expect("a member is measured")
                            .channels[plane];
                        ch.common_tflux = Some(cf.tflux);
                        ch.common_tmean_flux = Some(cf.tmean_flux);
                        ch.common_stars = Some(stars);
                        // All native units: the scale-invariant estimators read the channel's own noise and M*.
                        ch.psf_signal_weight =
                            psf_signal::psf_signal_weight(cf.tflux, cf.tmean_flux, ch.noise, ch.m_star);
                        ch.psf_snr = psf_signal::psf_snr(cf.tflux, ch.noise);
                    }
                }
                paired_by_plane[plane] = Some(per_member.iter().map(|cf| cf.paired).collect());
                tracing::info!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    plane,
                    anchor_frame_id = anchor_frame_id.unwrap_or_default(),
                    common_stars = stars,
                    candidates,
                    count = members.len(),
                    "weights: frame-common star set"
                );
                // PD-39: candidates enter group coordinates through the anchor's one registration map, so an error of
                // that map larger than the match radius takes a star out of every member at once. Nothing moves; the
                // line is where a wrong map shows.
                if common_set::set_is_small_against_candidates(stars, candidates) {
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        plane,
                        anchor_frame_id = anchor_frame_id.unwrap_or_default(),
                        common_stars = stars,
                        candidates,
                        "weights: common set far under the anchor's candidates; the anchor's registration map may be wrong"
                    );
                }
                sets.push(Some(positions));
            }
            PlaneOutcome::Fallback(f) => {
                match &f {
                    Fallback::MissingFits(frame_id) => tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        plane,
                        frame_id = *frame_id,
                        common_stars = f.common_stars(),
                        fallback = "measure_time",
                        reason = f.reason(),
                        "weights: no frame-common star set"
                    ),
                    _ => tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        plane,
                        common_stars = f.common_stars(),
                        fallback = "measure_time",
                        reason = f.reason(),
                        "weights: no frame-common star set"
                    ),
                }
                // Outside the PSF family the plane has no weights to keep (they are stage 3's either way): what it
                // loses is the SNR gain.
                let consequence = if psf_family {
                    "its weights keep the Measure-time values"
                } else {
                    "its SNR gain is not measured"
                };
                rc.warnings.push(format!(
                    "group {}: no frame-common star set on plane {plane} ({}); {consequence}",
                    group.key,
                    f.reason()
                ));
                sets.push(None);
            }
        }
    }
    // The record stays whenever this phase ran (D1b re-review m1): a group whose every plane fell back reaches integrate
    // as `Some([None, ...])`, a fallback that warns, so `None` there means "never reweighed" and nothing else.
    rc.common_stars.insert(group.key.clone(), sets);
    if !psf_family {
        // PD-19 b: stage 3's weights and its floor stand as they are (PD-5).
        return Ok(());
    }

    // Phase 3: the weights over the members, and the floor that stage 3 deferred. A group whose every plane fell back
    // keeps the weights stage 3 gave it, on stage 3's one scale (PD-6); the floor runs on them. Otherwise the members'
    // weights are recomputed, planes at their new or their Measure-time inputs, and normalized over the members.
    let recomputed: Option<Vec<FrameWeight>> = any_measured.then(|| {
        let entries = &rc.measured[&group.key];
        let inputs: Vec<WeightInput> = members
            .iter()
            .map(|m| WeightInput {
                measurement: entries[m.idx]
                    .measurement
                    .as_ref()
                    .expect("a member is measured"),
                exposure_s: entries[m.idx].frame.exposure_s,
                keyword_value: None,
            })
            .collect();
        let excluded = vec![false; inputs.len()];
        compute_weights(
            &inputs,
            cfg.measurement.weight_mode,
            &cfg.measurement.formula,
            &excluded,
        )
    });
    let weights: Vec<FrameWeight> = match recomputed {
        Some(w) => w,
        None => members
            .iter()
            .map(|m| {
                rc.measured[&group.key][m.idx]
                    .weight
                    .clone()
                    .expect("a member is weighed")
            })
            .collect(),
    };
    // PD-17: only a MANUAL pin stays under the floor (stage 4 forces a pin in despite the selection); an Auto reference is
    // the grid's origin and nothing more, and is excluded like any frame.
    let manual_pin: Option<i64> = (cfg.reference.mode == ReferenceMode::Manual)
        .then_some(rc.reference_frame_id)
        .flatten();
    for (k, w) in weights.into_iter().enumerate() {
        let (idx, frame_id) = (members[k].idx, members[k].frame_id);
        for (plane, paired) in paired_by_plane.iter().enumerate() {
            if let (Some(paired), Some(weight)) = (paired, w.normalized.get(plane)) {
                tracing::debug!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    frame_id,
                    plane,
                    paired_stars = paired[k],
                    weight = *weight,
                    "frame reweighed"
                );
            }
        }
        let normalized_mean = w.normalized_mean;
        let gate = weight_gate(&w, &cfg.selection);
        let entry = &mut rc
            .measured
            .get_mut(&group.key)
            .expect("the group's entries")[idx];
        entry.weight = Some(w);
        if let Some(reason) = gate {
            if Some(frame_id) == manual_pin {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    frame_id,
                    reason = %reason,
                    "weights: manual reference kept despite the weight floor"
                );
                rc.warnings.push(format!(
                    "manual reference frame {frame_id} kept despite its weight: {reason}"
                ));
            } else {
                entry.included = false;
                entry.reason = Some(reason.clone());
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id,
                    reason = %reason,
                    "frame excluded"
                );
            }
        }
        if rc.summary.reference.frame_id == Some(frame_id) {
            rc.summary.reference.weight = Some(normalized_mean);
        }
    }
    // One scale per frames table (PD-6): a frame the reweigh did not weigh carries the Measure-time scale.
    if any_measured {
        let member_set: HashSet<usize> = members.iter().map(|m| m.idx).collect();
        for (i, e) in rc
            .measured
            .get_mut(&group.key)
            .expect("the group's entries")
            .iter_mut()
            .enumerate()
        {
            if !member_set.contains(&i) {
                e.weight = None;
            }
        }
    }
    Ok(())
}

// ── Stages 6/7/9 (Task 8): normalize, integrate, output ────────────────────

/// One process, one writer: `master_file_name`/`write_master_light`
/// (`resolve_collision`) are check-then-write, so two groups' writes — even
/// across two concurrent runs of the same process — must never race on the
/// same free name (carry-forward).
static OUTPUT_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// One included member of a group, snapshotted out of `rc.measured` before
/// the (possibly slow) integration call so nothing below needs a borrow of
/// `rc` to survive it — the fields [`crate::stacking::integrate::StackFrame`]
/// needs, plus `frame_id` (`StackFrame` also carries `frame_id` now, purely
/// for the Task 6a fallback warning — this struct's own copy stays, since a
/// lot of code here reads `member.frame_id` before any `StackFrame` exists).
struct GroupMember {
    frame_id: i64,
    /// The catalog `files.filename` (fix round 1, item 3) — `ATH_STKF`
    /// names the SOURCE frame, not the calibrated artifact, and this needs
    /// no `c_`/`_d` stripping at all (unlike `calibrated`'s own name).
    filename: String,
    calibrated: PathBuf,
    map: PixelMap,
    measurement: FrameMeasurement,
    weight: FrameWeight,
    exposure_s: Option<f64>,
    date_obs: Option<String>,
    /// This frame's stage-5 `registration_results.config_hash` (spec §9.3) —
    /// `String::new()` for a member somehow reaching here without an
    /// `Aligned` outcome (never happens: the loop below only pushes members
    /// whose `registration` matched `Aligned`, see [`process_group_output`]).
    /// Stage 6's own [`normalization_hash_for`] keys a frame's `ln` artifact
    /// on this, so a re-registered frame's sidecar is recomputed too.
    registration_hash: String,
    /// Perf tier A Task 6a: this frame's materialized registered artifact —
    /// `Some` when a `stacking_artifacts` row of `kind = "registered"`
    /// exists for it with `config_hash == registration_hash` (a HASH-only
    /// check, deliberately not the disk-`stat`ing [`plan::is_fresh`] — see
    /// the field's own build site); `None` when no such row exists at all,
    /// which falls back SILENTLY (nothing was ever expected here). Every
    /// reader ([`crate::stacking::integrate::integrate_planes`],
    /// `ln::normalize_frame`) then tries `Some` paths for real and falls
    /// back LOUDLY (one `warn!`) only if the file turns out to be missing,
    /// unreadable, or the wrong size at that point — never a run failure
    /// either way.
    registered_path: Option<PathBuf>,
}

/// What became of one group at Output time.
enum GroupOutcome {
    /// Fewer than 3 included frames — per stage 3, possibly narrowed further
    /// by a stage-5 registration exclusion — never attempted.
    Skipped,
    /// `integrate_group`, the master-card build or the write itself failed
    /// (anything other than a cancel). The group row is already `failed`
    /// with the message, a warning already logged/pushed, and a
    /// `SummaryGroup` with no master already pushed — the caller only needs
    /// to know this group did not produce one.
    Failed,
    /// A master was written; the group row and `SummaryGroup` are already
    /// updated/pushed.
    Written,
}

/// The serde name (camelCase, per every stacking config enum's own
/// attribute) of an enum value — `MasterCardInputs::weight_mode`/
/// `::normalization` want the SAME strings the config itself round-trips as
/// (`"psfSignalWeight"`, `"scaleZeroOffset"`, …), not a hand-written mirror
/// that could drift from the enum's own `#[serde(rename_all = "camelCase")]`.
/// Fix round 1, item 10: propagates instead of silently falling back to an
/// empty string — a mis-serialized enum belongs in the run's own failure
/// text, never a blank `ATH_STKW`/`ATH_STKO` card value.
fn enum_serde_name<T: Serialize>(v: &T) -> Result<String, RunError> {
    let value = serde_json::to_value(v)
        .map_err(|e| RunError::Other(format!("failed to serialize enum value: {e}")))?;
    value.as_str().map(str::to_string).ok_or_else(|| {
        RunError::Other(format!("enum value did not serialize to a string: {value}"))
    })
}

/// Build one [`SummaryFrame`] from a frame's whole stage 3-5 outcome plus,
/// once integration has run, its `rejected_fraction` (`None` for anything
/// integration never actually combined — excluded before it ever ran, or
/// dropped by `integrate_group`'s OWN min-weight floor, a check independent
/// of `cfg.selection`'s stage-3 floor; `GroupOutput` reports only a COUNT of
/// such drops, not which frames, so `included`/`SummaryFrame.included` here
/// still reflect stage 3-5's decision, not that further drop — a known,
/// narrow gap, not something any of this task's required tests exercises).
fn summary_frame_for(entry: &MeasuredFrame, rejected_fraction: Option<f64>) -> SummaryFrame {
    let m = entry.measurement.as_ref();
    let mean_channel =
        |f: fn(&crate::stacking::measure::ChannelMeasurement) -> f64| -> Option<f64> {
            m.map(|meas| {
                if meas.channels.is_empty() {
                    0.0
                } else {
                    meas.channels.iter().map(|c| f(c)).sum::<f64>() / meas.channels.len() as f64
                }
            })
        };
    let (reg_status, reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio, reg_flipped) =
        match &entry.registration {
            Some(RegisteredFrameOutcome::Aligned { record, .. }) => (
                Some(record.status.clone()),
                record.model.clone(),
                Some(record.rms_residual_px),
                Some(record.matched_stars as usize),
                record.inlier_ratio,
                Some(record.flipped),
            ),
            Some(RegisteredFrameOutcome::Failed(_)) => {
                (Some("failed".to_string()), None, None, None, None, None)
            }
            None => (Some("skipped".to_string()), None, None, None, None, None),
        };
    SummaryFrame {
        frame_id: entry.frame.frame_id,
        filename: entry.frame.filename.clone(),
        included: entry.included,
        exclusion_reason: entry.reason.clone(),
        weight: entry.weight.as_ref().map(|w| w.normalized_mean),
        weight_channels: entry
            .weight
            .as_ref()
            .map(|w| w.normalized.clone())
            .unwrap_or_default(),
        fwhm_px: m.map(FrameMeasurement::mean_fwhm_px),
        eccentricity: m.map(FrameMeasurement::mean_eccentricity),
        stars: m.map(FrameMeasurement::min_stars),
        psf_signal_weight: m.map(FrameMeasurement::mean_psf_signal_weight),
        psf_snr: mean_channel(|c| c.psf_snr),
        noise: mean_channel(|c| c.noise),
        reg_status,
        reg_model,
        reg_rms_px,
        reg_inliers,
        reg_inlier_ratio,
        reg_flipped,
        rejected_fraction,
        calibrated_path: entry.calibrated.as_ref().map(|p| p.display().to_string()),
        cached_calibrated: entry.cached_calibrated,
        cached_metrics: entry.cached_metrics,
        cached_registration: matches!(
            entry.registration,
            Some(RegisteredFrameOutcome::Aligned { cached: true, .. })
        ),
        ln_scale: entry.ln_scale,
        cached_ln: entry.cached_ln,
        ln_growth_factor: entry.ln_growth_factor,
        ln_growth_pairs: entry.ln_growth_pairs,
        ln_aperture_correction: entry.ln_aperture_correction,
        // Task 11b: the frame's own β, where the two-pass Measure read one (mean across channels, as `fwhm_px`).
        beta_own: m.and_then(|meas| {
            let own: Vec<f64> = meas.channels.iter().filter_map(|c| c.beta_own).collect();
            (!own.is_empty() && own.len() == meas.channels.len())
                .then(|| own.iter().sum::<f64>() / own.len() as f64)
        }),
        beta_own_at_cap: m.is_some_and(|meas| meas.channels.iter().any(|c| c.beta_own_at_cap)),
        field_width_at_floor: m
            .is_some_and(|meas| meas.channels.iter().any(|c| c.field_width_at_floor)),
    }
}

/// Test hook (plan 6b-3 fix round 1): hand a grid drizzle no covered
/// rectangle, to reach its defensive arm — otherwise unreachable once the
/// master passed its own floor. Only project runs read the rectangle, and
/// those tests hold the grid-counters lock.
#[cfg(test)]
static FORCE_NO_DRIZZLE_STATS_RECT: AtomicBool = AtomicBool::new(false);

/// Test hook (plan 6b-3 D10 fix round 1): the next `write_crop_item` call
/// fails once, to reach the crop pass's uncropped fallback. One-shot and read
/// only by project runs: those tests hold the grid-counters lock, while
/// concurrent frame-set runs never touch it.
#[cfg(test)]
static FORCE_CROP_FAILURE: AtomicBool = AtomicBool::new(false);

/// Test hook (plan 6b-3 D10 fix round 2): the next `record_crop_item` call
/// fails once, as a database error would. One-shot, project runs only (see
/// [`FORCE_CROP_FAILURE`]).
#[cfg(test)]
static FORCE_RECORD_FAILURE: AtomicBool = AtomicBool::new(false);

/// Test hook (final review): the next `record_crop_item` call fails once AFTER
/// `update_group` succeeded, inside its transaction. One-shot, project runs
/// only (see [`FORCE_CROP_FAILURE`]).
#[cfg(test)]
static FORCE_RECORD_FAILURE_AFTER_GROUP: AtomicBool = AtomicBool::new(false);

/// Test hook (plan 6b-3 D10 fix round 1): raise the run's cancel flag after
/// the next group has written, to cancel between two groups. One-shot, project
/// runs only (see [`FORCE_CROP_FAILURE`]).
#[cfg(test)]
static CANCEL_AFTER_NEXT_GROUP: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
fn drizzle_stats_rect(
    rect: Option<crate::stacking::crop::CropRect>,
) -> Option<crate::stacking::crop::CropRect> {
    if FORCE_NO_DRIZZLE_STATS_RECT.load(Ordering::SeqCst) {
        None
    } else {
        rect
    }
}

#[cfg(not(test))]
fn drizzle_stats_rect(
    rect: Option<crate::stacking::crop::CropRect>,
) -> Option<crate::stacking::crop::CropRect> {
    rect
}

/// Push one [`SummaryGroup`] for `group` onto `rc.summary.groups` — called
/// exactly once per plan group, whatever its outcome (skipped/failed/
/// written), so the run's provenance document always accounts for every
/// group. `rejected_by_frame` is empty for a skipped/failed group (nothing
/// was ever combined).
#[allow(clippy::too_many_arguments)]
fn push_summary_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    included_count: usize,
    master_path: Option<String>,
    rejection_low_path: Option<String>,
    rejection_high_path: Option<String>,
    stats: Option<GroupStats>,
    normalization_reference_frame_id: Option<i64>,
    rejected_by_frame: &HashMap<i64, f64>,
    ln_reference_path: Option<String>,
    // M3 Task 5: `None` from every call site except the group-Written path
    // in `process_group_output`, which passes whatever its own drizzle
    // attempt (if any) produced.
    drizzle_path: Option<String>,
    weight_map_path: Option<String>,
    drizzle: Option<DrizzleStats>,
    coverage_path: Option<String>,
    drizzle_coverage_path: Option<String>,
    ln_scale_px: Option<u32>,
) {
    let frames = rc
        .measured
        .get(&group.key)
        .map(|entries| {
            entries
                .iter()
                .map(|e| {
                    let rf = rejected_by_frame.get(&e.frame.frame_id).copied();
                    summary_frame_for(e, rf)
                })
                .collect()
        })
        .unwrap_or_default();

    // M4b ruling R-M4b-4: whichever frame this group actually registered
    // onto — its `GroupGeometry` is the one place that knows, in either
    // mode.
    //
    // Ruling R-T3-3 (fix round 2): reported ONLY for a group whose master
    // was WRITTEN. Stage 4 resolves a `GroupGeometry` for every plan group,
    // and a group can still end up with no master at several later points
    // — fewer than 3 included frames at stage 5, fewer than 3 ALIGNED
    // members at integration, a local-normalization narrowing, an
    // integration or write failure. Every one of those reaches here with
    // `master_path: None`, and reporting a reference for any of them would
    // put a `reference #N` on a Results card with nothing behind it. The
    // written-master flag is the one signal that covers them all; fix
    // round 1's "did stage 5 touch this group" test did not (a `Failed`
    // registration outcome is still an outcome).
    let reference_frame_id = master_path.as_ref().and_then(|_| {
        rc.group_geometry
            .get(&group.key)
            .map(|g| g.reference_frame_id)
    });

    // Perf tier C Task 1: whatever stage 3 resolved for this group, if
    // anything — `None` for a group `push_summary_group` is called for
    // before stage 3 ever ran for it (there is no such call site today,
    // but this reads honestly either way).
    let beta = rc.measured_groups.get(&group.key).and_then(|mg| mg.beta);

    // Perf tier C Task 2 fix round 4 (ruling C-14 item 2): the group's own
    // per-channel seeds calibration, `None` for a group whose LN never ran
    // or whose calibration could not be measured.
    let seeds_calibration = rc
        .group_seeds_calibration
        .get(&group.key)
        .map(|c| c.k.clone());

    // Plan B B0 (PB-2): the LN reference's `c` per plane, `None` for a group whose LN never reached its reference.
    let ln_reference_aperture_correction = rc.group_ln_reference_c.get(&group.key).cloned();

    // Spec 2026-10-04 §8: the estimator rides the summary whenever LN ran
    // for the group; the aperture only under the common aperture.
    let ln_local = rc.config.normalization.local;
    let ln_flux_estimator = ln_scale_px.map(|_| ln_local.flux_estimator);
    let ln_aperture = (ln_scale_px.is_some()
        && ln_local.flux_estimator == crate::stacking::integrate::LnFluxEstimator::CommonAperture)
        .then_some(ln_local.aperture);

    rc.summary.groups.push(SummaryGroup {
        key: group.key.clone(),
        frame_count: group.frames.len(),
        included_count,
        master_path,
        rejection_low_path,
        rejection_high_path,
        stats,
        normalization_reference_frame_id,
        reference_frame_id,
        ln_reference_path,
        drizzle_path,
        weight_map_path,
        coverage_path,
        drizzle_coverage_path,
        drizzle,
        beta,
        seeds_calibration,
        ln_scale_px,
        ln_flux_estimator,
        ln_aperture,
        ln_reference_aperture_correction,
        frames,
    });
}

/// Mark a group `skipped` (DB row + `SummaryGroup` with no master) and
/// return [`GroupOutcome::Skipped`] — the shared tail for "fewer than 3
/// included frames reached Output", whether stage 3 already said so or a
/// stage-5 registration exclusion narrowed it further since.
fn skip_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    included_count: usize,
) -> Result<GroupOutcome, RunError> {
    let group_id = *rc.group_ids.get(&group.key).ok_or_else(|| {
        RunError::Other(format!(
            "no stacking_run_groups row for group {}",
            group.key
        ))
    })?;
    {
        let conn = db(&rc.ctx)?.conn();
        update_group(
            &conn,
            group_id,
            &GroupUpdate {
                included_count: Some(included_count as i64),
                status: Some("skipped"),
                ..Default::default()
            },
        )?;
    }
    push_summary_group(
        rc,
        group,
        included_count,
        None,
        None,
        None,
        None,
        None,
        &HashMap::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    Ok(GroupOutcome::Skipped)
}

/// Mark a group `failed` (DB row + `warn!` + `SummaryGroup` with no master)
/// and return [`GroupOutcome::Failed`] — the shared tail of every non-cancel
/// error path in [`process_group_output`] (brief: "any other error → group
/// `failed`, `warn!`, continue with the next group").
fn fail_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    included_count: usize,
    msg: &str,
) -> Result<GroupOutcome, RunError> {
    tracing::warn!(
        run_id = rc.run_id,
        group_key = %group.key,
        error = %msg,
        "group integration failed"
    );
    rc.warnings.push(format!("group {}: {msg}", group.key));
    let group_id = *rc.group_ids.get(&group.key).ok_or_else(|| {
        RunError::Other(format!(
            "no stacking_run_groups row for group {}",
            group.key
        ))
    })?;
    {
        let conn = db(&rc.ctx)?.conn();
        update_group(
            &conn,
            group_id,
            &GroupUpdate {
                included_count: Some(included_count as i64),
                status: Some("failed"),
                error: Some(msg),
                ..Default::default()
            },
        )?;
    }
    push_summary_group(
        rc,
        group,
        included_count,
        None,
        None,
        None,
        None,
        None,
        &HashMap::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    Ok(GroupOutcome::Failed)
}

/// Shared, lock-protected state one group's `Integrate`-stage progress
/// ticks are latched against (fix round 2). A run emits one group's
/// `Integrate` progress at a time (`stage_output`'s group loop is
/// sequential), so a single instance scoped to one [`process_group_output`]
/// call — not a `HashMap<(Stage, Option<String>), _>` spanning the whole
/// run — is enough to key the latch by `(stage, group_key)`: this state
/// itself never sees a second `(stage, group_key)` pair.
struct IntegrateTickState {
    last_emit: Instant,
    /// The highest `percent` emitted so far — monotonic across the WHOLE
    /// group (by construction: `plane` only advances, `frac` is always in
    /// `[0, 1]`, so a stale/out-of-order tick's percent can never exceed
    /// the current watermark).
    last_percent: f64,
    /// Which plane `last_bytes_done` belongs to.
    last_plane: usize,
    /// The highest `bytes_done` emitted so far FOR `last_plane` — scoped to
    /// one plane, not the whole group, because `bytes_done`/`bytes_total`
    /// are the engine's own PER-PLANE pair (`EngineProgress::on_band`'s own
    /// doc: they reset every plane) — latching it across a plane boundary
    /// would silently drop every tick of a new plane, since a fresh plane
    /// always starts back at `bytes_done: 0`.
    last_bytes_done: u64,
    /// The plane's combine progress in ROWS — the one measure that moves
    /// through the whole plane (reading a band is ≈ 10 % of its time,
    /// combining it the rest), so it is what `frac` is built from on both
    /// the read and the combine tick. Before this tracker the combine tick
    /// passed `frac = 1.0` and a one-plane group sat at 100 % for the whole
    /// combine while only the bytes text moved (owner report 2026-09-20).
    rows: CombineRowsTracker,
}

impl IntegrateTickState {
    fn new() -> Self {
        IntegrateTickState {
            last_emit: Instant::now() - Duration::from_millis(PROGRESS_THROTTLE_MS),
            last_percent: f64::NEG_INFINITY,
            last_plane: 0,
            last_bytes_done: 0,
            rows: CombineRowsTracker::default(),
        }
    }
}

/// Rows combined so far in the CURRENT plane, folded from the engine's two
/// callbacks: `on_band(k, …)` fires when band `k` (1-based) has finished
/// READING and is about to combine; `on_combine(done, h)` reports rows
/// combined WITHIN that band (`h` = the band's height, `done` reaches `h`
/// on its last tick and may repeat). A band's rows are committed exactly
/// once — on the first tick that reports `done == h` for a band not yet
/// committed — so a repeated final tick, or a band whose ticks never reach
/// `h`, cannot double-count; `frac` for the plane is `rows() / plane_rows`.
/// Monotone by construction, which is what the max-latch in
/// [`emit_integrate_tick`] needs from both callbacks: the read tick of band
/// `k+1` reports the same `rows()` as band `k`'s last combine tick, never
/// less, so it is never dropped and its bytes text still updates.
#[derive(Default)]
struct CombineRowsTracker {
    /// Rows of every band already committed.
    committed: u64,
    /// The band the last committed rows belong to (0 = none yet).
    committed_band: usize,
    /// The band currently combining, from the last `on_band` tick.
    cur_band: usize,
    /// `done` of the current band's last tick, 0 after a commit.
    cur_done: u64,
}

impl CombineRowsTracker {
    /// A new plane starts: nothing combined yet.
    fn on_plane(&mut self) {
        *self = CombineRowsTracker::default();
    }

    /// Band `band` (1-based) finished reading; a `0` is the engine's forecast
    /// tick before pass 2's first band and names no band.
    fn on_band(&mut self, band: usize) {
        if band > 0 && band != self.cur_band {
            self.cur_band = band;
            self.cur_done = 0;
        }
    }

    /// `done` of `h` rows of the current band are combined.
    fn on_combine(&mut self, done: u64, h: u64) {
        if self.cur_band == 0 || self.committed_band == self.cur_band {
            // No band announced yet, or this band is already committed (a
            // repeated final tick): nothing to add.
            return;
        }
        if done >= h && h > 0 {
            self.committed += h;
            self.committed_band = self.cur_band;
            self.cur_done = 0;
        } else {
            self.cur_done = done.min(h);
        }
    }

    /// Rows combined so far in this plane.
    fn rows(&self) -> u64 {
        self.committed + self.cur_done
    }

    /// `rows() / plane_rows`, clamped to `[0, 1]`; `0` for a plane with no
    /// rows (never true of a real plane).
    fn frac(&self, plane_rows: u64) -> f64 {
        if plane_rows == 0 {
            0.0
        } else {
            (self.rows() as f64 / plane_rows as f64).min(1.0)
        }
    }
}

/// One `Integrate`-stage progress tick, throttled AND max-latched against
/// `state`. Percent = `100 * (plane + frac) / channels`. A free function
/// (not a closure borrowing `RunContext`) so `on_plane`/`on_band`/`on_combine`
/// — `Sync` closures the engine may call from any of its own worker threads
/// — can call it with only cloned-out plain values, and so it is directly
/// unit-testable on its own (see `emit_integrate_ticks_never_race_percent_backwards`
/// below).
///
/// Fix round 1, item 1: the throttle guard (`state.lock()`) stays held
/// THROUGH the emit, not dropped before it — this makes ONE caller's own
/// check/update/compute/emit sequence indivisible, so two ticks can never
/// interleave or land torn. It does NOT, on its own, stop a worker that
/// read a LOWER `(plane, frac)` and was then descheduled from taking the
/// lock AFTER a higher tick already went out and emitting the lower value —
/// atomicity of one critical section says nothing about the relative order
/// of two SEPARATE critical sections whose inputs were computed outside
/// the lock. Fix round 2 closes that: a max-latch, held under the SAME
/// lock as the throttle state, drops (never clamps — clamping would emit
/// a duplicate percent under a fresh, misleadingly-newer `bytes_done`) any
/// tick whose `percent` (whole-group scope) or `bytes_done` (current-plane
/// scope) regresses relative to what has already been emitted for this
/// `(stage, group_key)`. `force` bypasses the THROTTLE only — the latch
/// always applies, so the stage's first (`force: true`, plane 0/frac 0) and
/// last (`force: true`, `percent: 100.0`) events are still guaranteed to be
/// the true minimum/maximum rather than merely unthrottled. Emitting under
/// the lock is cheap (a channel send / IPC call); a slow emitter only slows
/// the engine's own callbacks, the same tradeoff `OUTPUT_WRITE_LOCK` makes
/// for the master write.
#[allow(clippy::too_many_arguments)]
fn emit_integrate_tick(
    state: &Mutex<IntegrateTickState>,
    emitter: &dyn ProgressEmitter,
    run_id: i64,
    subject: &StackSubject,
    group_key: &str,
    channels: usize,
    plane: usize,
    frac: f64,
    bytes_done: u64,
    bytes_total: u64,
    force: bool,
    message: Option<String>,
) {
    let now = Instant::now();
    let percent = 100.0 * (plane as f64 + frac) / channels.max(1) as f64;
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());

    // Max-latch (fix round 2), checked BEFORE the throttle so a dropped
    // tick never resets `last_emit` either. `percent` regressing drops the
    // tick outright, whatever plane it claims to be from. Otherwise: moving
    // to a strictly LATER plane re-arms the bytes_done watermark at 0 (a
    // fresh plane's own counter); staying on the SAME plane still latches
    // `bytes_done`; a tick claiming an EARLIER plane than what is already
    // recorded has, by construction, a `percent` that cannot exceed the
    // current watermark (plane only advances, frac in [0, 1]) — it is
    // already caught by the percent check above and never reaches here.
    if percent < s.last_percent {
        return;
    }
    if plane > s.last_plane {
        s.last_plane = plane;
        s.last_bytes_done = 0;
    } else if bytes_done < s.last_bytes_done {
        return;
    }

    if !force && now.duration_since(s.last_emit) < Duration::from_millis(PROGRESS_THROTTLE_MS) {
        return;
    }

    s.last_emit = now;
    s.last_percent = percent;
    s.last_bytes_done = bytes_done;
    // After the throttle: a dropped tick never builds the subject pair.
    let (set_id, project_id) = wire_subject(subject);
    // `s` (the `MutexGuard`) stays alive through the emit below — fix round
    // 1's own point still holds: the watermark update above only decides
    // WHICH tick is allowed to claim the next slot, not what order two
    // already-claimed emits reach the recorder in. Dropping the guard here
    // would let a second thread claim the NEXT slot and race this thread's
    // own `emit_event` call, reopening exactly the interleaving fix round 1
    // closed (a higher, later-claimed tick landing before this one's).

    emit_event(
        emitter,
        STACKING_PROGRESS_EVENT,
        &StackingProgressEvent {
            run_id,
            set_id,
            project_id,
            stage: Stage::Integrate,
            group_key: Some(group_key.to_string()),
            current: plane,
            total: channels,
            percent,
            bytes_done,
            bytes_total,
            frame_id: None,
            message,
        },
    );
}

/// The Integrate row's own words for a tick (v0.6.3): which plane of how
/// many, a `pass 2` marker when large-scale rejection integrates the group
/// twice (`plane >= channels`), and the band or the combine step — the
/// row used to show a bare percentage for a stage that takes twenty
/// minutes on a real set.
fn integrate_tick_message(
    plane: usize,
    planes_total: usize,
    channels: usize,
    step: &str,
) -> String {
    let channels = channels.max(1);
    let pass = if planes_total > channels && plane >= channels {
        " · pass 2"
    } else {
        ""
    };
    format!("plane {}/{channels}{pass} · {step}", plane % channels + 1)
}

/// Throttle state for the Drizzle stage's per-frame progress ticks (M3 Task
/// 5, fix round 1, Important I1). Unlike [`IntegrateTickState`], no
/// max-latch is needed: `drizzle_group` calls `DrizzleProgress::on_frame`
/// strictly sequentially, from one control-flow thread — only the per-frame
/// pixel DEPOSIT fans out over `pool`, never `on_frame` itself (see
/// `drizzle::mod::drizzle_group`'s own per-frame loop) — so `done_units`
/// only ever increases, unlike Integrate's per-band ticks racing across
/// worker threads. `Mutex` only because `DrizzleProgress::on_frame` must be
/// `Sync` to satisfy the trait bound.
/// Which per-frame rejection bitmaps the Drizzle stage reads (M4c Task 3,
/// fix round 1, ruling R-T3-1): the bits the MASTER was built with, always.
///
/// With the large-scale second pass on, that is the SECOND pass's own set
/// (`rej/run-<id>/<group>/pass2/…`, [`RejBitmapSet::second_pass_path`]) —
/// the forced structures AND whatever pass 2's own per-pixel tests rejected,
/// i.e. exactly the samples the returned master left out. Without it, the
/// first pass's set is that record, unchanged from M3. The processed
/// `.rejl` files are neither: they are the intermediate the forced set was
/// derived FROM, and carry no algorithmic rejection at all.
///
/// `second_pass_rej_ok` false with the second pass having run means those
/// files are missing or incomplete — the caller treats that as a bitmap
/// failure and skips drizzle for the group rather than handing it pass 1's
/// bits, which describe a different integration.
fn drizzle_rejection_paths(
    set: Option<&RejBitmapSet>,
    included: usize,
    second_pass_rej_ok: bool,
) -> Vec<Option<PathBuf>> {
    (0..included)
        .map(|k| {
            set.map(|s| {
                if second_pass_rej_ok {
                    s.second_pass_path(k)
                } else {
                    s.path(k).to_path_buf()
                }
            })
        })
        .collect()
}

struct DrizzleTickState {
    last_emit: Instant,
}

impl DrizzleTickState {
    fn new() -> Self {
        DrizzleTickState {
            last_emit: Instant::now() - Duration::from_millis(PROGRESS_THROTTLE_MS),
        }
    }
}

/// One `Drizzle`-stage progress tick, throttled like every other stage's
/// (`PROGRESS_THROTTLE_MS`; `force` bypasses it). A free function, not
/// `RunContext::progress`, because `DrizzleProgress::on_frame` is a `Sync`
/// closure `drizzle_group` calls with no `&mut RunContext` in reach (the
/// drizzle block re-borrows `rc.cancel`/`rc.ctx.image_pool` immutably for
/// the call, so nothing here may need `&mut rc` either) — mirrors
/// [`emit_integrate_tick`]'s shape, minus the max-latch (see
/// [`DrizzleTickState`]'s own doc for why one isn't needed here).
#[allow(clippy::too_many_arguments)]
fn emit_drizzle_tick(
    state: &Mutex<DrizzleTickState>,
    emitter: &dyn ProgressEmitter,
    run_id: i64,
    subject: &StackSubject,
    group_key: &str,
    current: usize,
    total: usize,
    frame_id: Option<i64>,
    force: bool,
) {
    let now = Instant::now();
    let percent = if total == 0 {
        100.0
    } else {
        100.0 * current as f64 / total as f64
    };
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    if !force && now.duration_since(s.last_emit) < Duration::from_millis(PROGRESS_THROTTLE_MS) {
        return;
    }
    s.last_emit = now;
    let (set_id, project_id) = wire_subject(subject);
    emit_event(
        emitter,
        STACKING_PROGRESS_EVENT,
        &StackingProgressEvent {
            run_id,
            set_id,
            project_id,
            stage: Stage::Drizzle,
            group_key: Some(group_key.to_string()),
            current,
            total,
            percent,
            bytes_done: 0,
            bytes_total: 0,
            frame_id,
            message: None,
        },
    );
}

/// The drizzle stage's own failure classification (M3 Task 5), folding
/// every fallible step past `drizzle_group` itself (scaling the WCS,
/// building the cards, writing the file) into the SAME two-way split
/// ruling R-M3-7 draws for `drizzle_group`'s own [`DrizzleError`]:
/// `Cancelled` propagates as the run's cancel, everything else is a
/// per-group warning — the master is already written and stays untouched
/// either way.
enum DrizzleFailure {
    Cancelled,
    Other(String),
}

impl From<DrizzleError> for DrizzleFailure {
    fn from(e: DrizzleError) -> Self {
        match e {
            DrizzleError::Cancelled => DrizzleFailure::Cancelled,
            other => DrizzleFailure::Other(other.to_string()),
        }
    }
}

/// Plan 6b-3 D10: one group's outputs waiting in the scratch folder for the
/// run's crop rectangle, with everything [`finish_crop`] needs to finalize
/// them without re-reading a header.
pub(crate) struct CropItem {
    group_key: String,
    group_id: i64,
    /// The group's own `coverage >= K` box; `None` when no pixel reached `K`.
    rect: Option<CropRect>,
    width: usize,
    height: usize,
    channels: usize,
    frames: usize,
    total_exposure_s: Option<f64>,
    /// The master's cards as written: the uncropped WCS.
    cards: Vec<Card>,
    /// The record those cards' WCS block was written from.
    wcs: Option<PlateSolveRecord>,
    /// The scratch files.
    master: WrittenMaster,
    drizzle: Option<CropDrizzle>,
}

pub(crate) struct CropDrizzle {
    written: WrittenDrizzle,
    cards: Vec<Card>,
    scale: u32,
}

/// One group's final files after its crop pass.
struct CropOutputs {
    rect: CropRect,
    master: WrittenMaster,
    /// The drizzle family and its (scaled) rectangle.
    drizzle: Option<(WrittenDrizzle, CropRect)>,
    /// Why the drizzle family was not written (the master family still is).
    drizzle_error: Option<String>,
}

/// The result of one `pick_reference_idx` call (ruling R-M3-17 v2 + its
/// coverage addendum, fix round 1 Important 3): `idx` is always a valid
/// index into the `members` slice the closure was called with — a group
/// with `>= 3` members always gets an anchor, restoring the pre-this-
/// ruling guarantee `best_by_weight` used to provide unconditionally.
/// `coverage_dropped`/`sky_fallback` are `Some(count)` when the closure
/// had to relax ITS OWN filter to reach that guarantee (fewer than 3
/// members covered the reference geometry; or every covering member's
/// background was non-finite/non-positive) — the caller (which alone holds
/// `&mut rc`) turns either into a `warn!` + a run warning.
struct AnchorPick {
    idx: usize,
    coverage_dropped: Option<usize>,
    sky_fallback: Option<usize>,
}

/// Normalize (stage 6, M2 Task 5 — [`run_group_normalization`]), integrate
/// and write the master for one group. LN DISABLED keeps the exact pre-M2
/// shape: a static 1/1 "Normalize" progress tick and no other effect (spec
/// ruling 14). LN enabled resolves/caches the group's LN reference and
/// per-frame `.athln` sidecars (real per-frame progress); a frame LN could
/// not measure is excluded here when LN drives OUTPUT normalization,
/// narrowing `members` before integration ever sees it (see
/// [`run_group_normalization`]'s own doc). Returns the outcome plus
/// (normalize, integrate, output) wall time for [`stage_output`]'s own
/// per-stage [`crate::stacking::provenance::StageTiming`] totals — ruling R5
/// (reverses fix round 1, item 6): the `normalize` component is
/// `run_group_normalization`'s own wall time (reference build + fan-out),
/// timed here because the acceptance run found it the single most expensive
/// stage and therefore not one `RunSummary.stages` can leave invisible;
/// `stage_output` only pushes a `Normalize` `StageTiming` when the group's
/// own config had LN active (disabled stays a bare progress tick, nothing
/// worth reporting).
/// `Err(RunError::Cancelled)` — from `IntegrationError::Cancelled` or a
/// cancel noticed before this group started — is the ONLY error that
/// propagates; everything else becomes `Ok((GroupOutcome::Failed, ..))` via
/// [`fail_group`], per ruling 12 (a cancelled run keeps its artifacts and
/// writes no master) vs. the brief's per-group failure policy.
fn process_group_output(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    measure_opts: &MeasureOptions,
    wcs: Option<&PlateSolveRecord>,
) -> Result<(GroupOutcome, Duration, Duration, Duration, Duration), RunError> {
    let zero = || {
        (
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        )
    };

    // Plan 6b-2 C9: the Output boundary — the member snapshot below reads
    // the files Normalize, Integrate and Drizzle work from.
    exclude_changed_members(rc, group)?;

    let included_count = rc
        .measured
        .get(&group.key)
        .map(|v| v.iter().filter(|e| e.included).count())
        .unwrap_or(0);
    if included_count < 3 {
        let (n, i, d, o) = zero();
        return Ok((skip_group(rc, group, included_count)?, n, i, d, o));
    }

    // Snapshot the included members' data — nothing below needs `rc` again
    // until the write completes, so no borrow of it needs to survive the
    // (potentially slow) integration call.
    let mut members: Vec<GroupMember> = Vec::with_capacity(included_count);
    if let Some(entries) = rc.measured.get(&group.key) {
        for e in entries {
            if !e.included {
                continue;
            }
            let Some(calibrated) = e.calibrated.clone() else {
                continue;
            };
            let Some(measurement) = e.measurement.clone() else {
                continue;
            };
            let (map, registration_hash) = match &e.registration {
                Some(RegisteredFrameOutcome::Aligned {
                    map,
                    registered_hash,
                    ..
                }) => (map.clone(), registered_hash.clone()),
                _ => continue,
            };
            let weight = e.weight.clone().unwrap_or(FrameWeight {
                channels: Vec::new(),
                normalized: Vec::new(),
                mean: 0.0,
                normalized_mean: 0.0,
                missing: None,
            });
            // Perf tier A Task 6a (fix round 1: both register fan-out
            // closures — the align-and-write one and the cache-hit
            // rewrite-only one, see `register_group_pass`): this snapshot is
            // taken AFTER stage 5 has already written (or refreshed) every
            // included, aligned member's registered artifact, so a fresh
            // row here is the ordinary case; `None` (the write failed, or a
            // config predating this task never ran stage 5 again) just
            // means every reader below falls back to the on-the-fly warp
            // for this one frame, silently (nothing was ever expected on
            // disk).
            //
            // Deliberately HASH-ONLY, not the full [`is_fresh`] disk check
            // stage 5's own [`registered_artifact_is_fresh`] uses to decide
            // whether a REWRITE is needed: the actual "is the file usable
            // right now" question belongs to
            // `RegisteredSource::open_materialized`'s own `PlaneReader::open`
            // attempt at read time, which is what turns a file that
            // vanished AFTER this snapshot (deleted mid-run, a cleanup, a
            // foreign edit) into the loud, warned fallback rather than a
            // silent one — re-`stat`-ing here would only race that same
            // check and answer it twice.
            let registered_path = {
                let conn = db(&rc.ctx)?.conn();
                crate::db::stacking::find_artifact(
                    &conn,
                    &rc.subject,
                    artifact_group_key("registered", &group.key),
                    "registered",
                    Some(e.frame.frame_id),
                )?
                .filter(|row| row.config_hash == registration_hash)
                .and_then(|row| row.path.map(PathBuf::from))
            };
            members.push(GroupMember {
                frame_id: e.frame.frame_id,
                filename: e.frame.filename.clone(),
                calibrated,
                map,
                measurement,
                weight,
                exposure_s: e.frame.exposure_s,
                date_obs: e.frame.date_obs.clone(),
                registered_path,
                registration_hash,
            });
        }
    }

    if members.len() < 3 {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = members.len(),
            "stacking group skipped: fewer than 3 registered members reached integration"
        );
        let (n, i, d, o) = zero();
        return Ok((skip_group(rc, group, members.len())?, n, i, d, o));
    }

    // Ruling R-M3-17 v2 (supersedes Ruling 7): the set's reference (manual
    // pin or auto best-by-weight, `rc.reference_frame_id`) is the
    // REGISTRATION reference only — it no longer anchors normalization just
    // because it happens to be a member of this group. Every group's
    // NORMALIZATION anchor is chosen sky-penalized instead
    // (`sky_penalized_order`, spec §4.4): among the group's admissible
    // members, the one with the highest `weight / sqrt(background)` wins —
    // faint-signal SNR scales as `1/sqrt(background)`, so a bright-sky
    // top-weight member no longer outranks a slightly-lower-weight
    // dark-sky one (LDN 1272, 2026-09-10: the OSC group's normalization
    // reference was the top-PSF-weight member of a 3x-brighter-sky night;
    // the master inherited that night's sky level).
    //
    // Addendum: a member whose registered footprint does not cover the
    // reference rectangle (`reference_coverage` < `MIN_REFERENCE_COVERAGE`
    // — a rotated frame, a mismatched camera angle, a badly offset one) is
    // not admissible either — normalizing to it (or modelling the LN
    // reference's background on it) would carry its uncovered corners/
    // edges into every other frame. The coverage filter itself is dropped
    // for a group where fewer than 3 members would pass it (a `warn!` +
    // run warning), rather than ranking among too few candidates to mean
    // anything.
    //
    // Factored into a closure (not just a one-shot call) because stage 6's
    // LN pass, below, can narrow `members` further (a `TooFewMatches`
    // exclusion when LN drives OUTPUT normalization) — the anchor is
    // re-derived from whichever members survive that. `run_id`/`ref_width`/
    // `ref_height`/`group` are captured by copy/shared reference (never
    // `rc` itself), so the closure holds no live borrow of `rc` across the
    // mutable uses of it between the two call sites below — the coverage
    // filter's "dropped" case is instead returned to the caller, which
    // pushes the run warning itself (the closure can only `tracing::warn!`,
    // never touch `rc.warnings`).
    let run_id = rc.run_id;
    let (ref_width, ref_height) = {
        let geometry = rc.geometry_of(&group.key);
        (geometry.width, geometry.height)
    };
    let ln_field = group_ln_field_for(rc, group)?;
    let pick_reference_idx = |members: &[GroupMember]| -> Option<AnchorPick> {
        let weights: Vec<FrameWeight> = members.iter().map(|m| m.weight.clone()).collect();
        let star_counts: Vec<usize> = members.iter().map(|m| m.measurement.min_stars()).collect();
        let sky: Vec<f64> = members
            .iter()
            .map(|m| m.measurement.mean_median())
            .collect();

        let coverage = member_coverages(ln_field.as_ref(), members, ref_width, ref_height);
        let coverage_mask: Vec<bool> = coverage
            .iter()
            .map(|&c| c >= MIN_REFERENCE_COVERAGE)
            .collect();
        let admissible_count = coverage_mask.iter().filter(|&&ok| ok).count();
        let (included_mask, coverage_dropped) = if admissible_count >= 3 {
            (coverage_mask, None)
        } else {
            (vec![true; members.len()], Some(admissible_count))
        };
        if let Some(count) = coverage_dropped {
            tracing::warn!(
                run_id,
                group_key = %group.key,
                count,
                min_coverage = MIN_REFERENCE_COVERAGE,
                "reference-coverage filter dropped for this group: fewer than 3 members pass"
            );
        }

        // Fix round 1, Important 3: `sky_penalized_order` returning empty
        // (every admissible member's background non-finite/non-positive —
        // e.g. a narrowband set whose dark slightly over-subtracts every
        // frame) used to fail the WHOLE GROUP here. A group with `>= 3`
        // members always got an anchor before this ruling existed
        // (`best_by_weight` never refuses a non-empty candidate set); this
        // restores that guarantee by falling back to it over the SAME
        // `included_mask` (still coverage-filtered) the sky-penalized rank
        // used, rather than failing the group over a background
        // measurement problem the anchor pick itself can't fix.
        let ordered = sky_penalized_order(&weights, &included_mask, &star_counts, &sky);
        let (idx, sky_fallback) = match ordered.into_iter().next() {
            Some(idx) => (idx, None),
            None => {
                let count = included_mask.iter().filter(|&&ok| ok).count();
                tracing::warn!(
                    run_id,
                    group_key = %group.key,
                    count,
                    "no admissible member has a usable measured background; \
                     anchoring by weight alone"
                );
                let idx = best_by_weight(&weights, &included_mask, &star_counts)?;
                (idx, Some(count))
            }
        };
        tracing::info!(
            run_id,
            group_key = %group.key,
            frame_id = members[idx].frame_id,
            weight = weights[idx].normalized_mean,
            background = sky[idx],
            "normalization anchor chosen"
        );
        Some(AnchorPick {
            idx,
            coverage_dropped,
            sky_fallback,
        })
    };
    // Same shape as `StackFrame`'s own construction from a `GroupMember`
    // list — a closure, not a one-shot `.map()`, because the LN pass can
    // force this to run a second time over a narrowed `members`.
    let build_stack_frames = |members: &[GroupMember]| -> Vec<StackFrame> {
        members
            .iter()
            .map(|m| StackFrame {
                path: m.calibrated.clone(),
                map: m.map.clone(),
                measurement: m.measurement.clone(),
                weight: m.weight.clone(),
                exposure_s: m.exposure_s.unwrap_or(0.0),
                date_obs: m.date_obs.clone(),
                frame_id: m.frame_id,
                registered_path: m.registered_path.clone(),
            })
            .collect()
    };

    let mut reference_idx = match pick_reference_idx(&members) {
        Some(pick) => {
            if let Some(count) = pick.coverage_dropped {
                rc.warnings.push(format!(
                    "group {}: reference-coverage filter dropped for normalization anchoring — \
                     only {count} member(s) cover the reference geometry (need >= 3)",
                    group.key
                ));
            }
            if let Some(count) = pick.sky_fallback {
                rc.warnings.push(format!(
                    "group {}: no admissible member has a usable measured background — \
                     anchored by weight alone among {count} candidate(s)",
                    group.key
                ));
            }
            pick.idx
        }
        None => {
            let (n, i, d, o) = zero();
            return Ok((
                fail_group(
                    rc,
                    group,
                    members.len(),
                    "no included frame to anchor normalization",
                )?,
                n,
                i,
                d,
                o,
            ));
        }
    };
    let mut normalization_reference_frame_id = members[reference_idx].frame_id;

    // Ruling 6, generalized by M4b ruling R-M4b-5: every group's master
    // adopts ITS OWN reference's geometry — the run-wide reference's in
    // co-registered mode (where every group's entry is that same one), the
    // group's own in native mode. The group's OWN plane count (mono vs.
    // debayered OSC) is whatever this group's own measurements carry.
    let mut channels = members[reference_idx].measurement.channels.len();
    let (width, height) = (ref_width, ref_height);

    let mut stack_frames: Vec<StackFrame> = build_stack_frames(&members);

    // Maps memory check (carry-forward): turn maps off for THIS group with a
    // warning rather than fail, when they would not fit the budget.
    let mut group_integration = rc.config.integration.clone();
    if group_integration.write_rejection_maps {
        let maps_bytes = 3u64 * channels as u64 * width as u64 * height as u64 * 4;
        // Fix round 1, item 5: unknown RAM is treated as NOT fitting
        // (conservative — the same reading `admission`'s own "unknown → 1"
        // fallback takes), not as fitting.
        let fits = total_ram_bytes()
            .map(|total| maps_bytes <= total / 4)
            .unwrap_or(false);
        if !fits {
            group_integration.write_rejection_maps = false;
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                "rejection maps disabled for this group: over the memory budget"
            );
            rc.warnings.push(format!(
                "group {}: rejection maps disabled — would exceed the memory budget",
                group.key
            ));
        }
    }
    let normalization_cfg = rc.config.normalization.clone();

    let input = GroupInput {
        frames: &stack_frames,
        reference: reference_idx,
        width,
        height,
        channels,
        interpolation: rc.config.registration.interpolation,
        clamping: rc.config.registration.clamping_threshold,
        integration: &group_integration,
        normalization: &normalization_cfg,
        // Stage 6 has not run yet at this point — nothing to hand
        // `integrate_group` until the LN pass below (if any) resolves real
        // sidecars; the second `GroupInput` further down carries them.
        ln: None,
        // M3 Task 2: this `input` only ever reaches `run_group_normalization`
        // (the LN pass), never `integrate_group` — the real `GroupInput`
        // further down is the one a `RejBitmapSet` (Task 5) will attach to.
        rej: None,
        second_pass_bitmaps: false,
        coverage_map: false,
        stats_on_covered: false,
        // Plan D D1b: this `input` only reaches the LN pass, which reads no statistics - the real one below carries the set.
        common_stars: None,
    };

    let paths: Vec<PathBuf> = members.iter().map(|m| m.calibrated.clone()).collect();
    let io: IoPolicy = {
        let conn = db(&rc.ctx)?.conn();
        crate::integration::io_policy::resolve(
            &conn,
            &rc.ctx.settings,
            &paths,
            rc.ctx.image_pool.current_num_threads(),
        )?
        // Task 13c L3: registered artifacts are positional file reads, so
        // Integrate and the LN reference build prefetch on every class.
        .with_local_prefetch()
    };

    // Stage 6 (local normalization, M2 Task 5): resolves/caches the group's
    // LN reference, models its backgrounds once, then fans `normalize_frame`
    // out over the included members. When LN drives OUTPUT normalization
    // (`normalization.local.enabled`) a frame whose relative scale could not
    // be measured is excluded here — that narrows `members` below what
    // `reference_idx`/`stack_frames`/`input` above were built from, so all
    // three are rebuilt from the surviving members before integration ever
    // sees them. LN DISABLED keeps the exact pre-M2 behaviour: a static
    // 1/1 "Normalize" progress tick and nothing else (spec ruling 14).
    // Ruling R5 (reverses fix round 1, item 6): the acceptance run measured
    // LN as the run's single most expensive stage (≈28 of 49 min) — a stage
    // that costs more than every other one combined cannot stay invisible
    // in `RunSummary.stages`, so its wall time IS timed here
    // (`normalize_start`, wrapping the whole match below — reference build +
    // fan-out, i.e. `run_group_normalization`'s own wall time, never folded
    // into `integrate_dur`) and pushed by `stage_output` as a `Normalize`
    // `StageTiming`, but only when `ln_active` (disabled stays invisible —
    // the pre-M2 shape — since it is genuinely a no-op tick).
    let ln_active = normalization_cfg.local.enabled
        || normalization_cfg.rejection == RejectionNormalization::Local;
    let normalize_start = Instant::now();
    let mut normalize_meter = ln_active.then(|| {
        crate::stacking::telemetry::StageMeter::start(rc, Stage::Normalize, Some(&group.key))
    });
    let mut ln_reference_path: Option<String> = None;
    let mut ln_scale_px: Option<u32> = None;
    // M2 Task 7 (fix round 1, item 3): every member `run_group_normalization`
    // itself confirmed a READABLE `ln` artifact for THIS run — verified at
    // the source (the cache-hit branch re-reads the artifact row's own file
    // before trusting it; the fan-out branch re-reads what it just wrote),
    // so nothing past this point ever opens a file or handles a read
    // failure: an unreadable sidecar never reaches `process_group_output`
    // at all — it is either re-normalized in the same run or already
    // folded into `excluded_frame_ids` below. `None` (never populated)
    // whenever LN never actually ran for the group at all
    // (`LnGroupOutcome.reference_path` is `None`: disabled, or a
    // ruling-R3-shaped fallback to global normalization — carry-over (a),
    // Task 5's re-review: `Some` means "LN ran", full stop).
    let mut ln_sidecar_grids: Option<HashMap<i64, LnFrameGrids>> = None;
    if ln_active {
        match run_group_normalization(
            rc,
            group,
            &members,
            &input,
            measure_opts,
            io,
            ln_field.as_ref(),
        ) {
            Ok(outcome) => {
                ln_reference_path = outcome.reference_path.clone();
                ln_scale_px = outcome.scale_px;
                if outcome.reference_path.is_some() {
                    ln_sidecar_grids = Some(outcome.sidecar_grids);
                }
                if !outcome.excluded_frame_ids.is_empty() {
                    let excluded: HashSet<i64> = outcome.excluded_frame_ids.into_iter().collect();
                    members.retain(|m| !excluded.contains(&m.frame_id));
                    if members.len() < 3 {
                        tracing::warn!(
                            run_id = rc.run_id,
                            group_key = %group.key,
                            count = members.len(),
                            "stacking group skipped: local normalization left fewer than 3 members"
                        );
                        finish_meter(&mut normalize_meter);
                        let n = normalize_start.elapsed();
                        let (_, i, d, o) = zero();
                        return Ok((skip_group(rc, group, members.len())?, n, i, d, o));
                    }
                    reference_idx = match pick_reference_idx(&members) {
                        Some(pick) => {
                            if let Some(count) = pick.coverage_dropped {
                                rc.warnings.push(format!(
                                    "group {}: reference-coverage filter dropped for \
                                     normalization anchoring — only {count} member(s) cover \
                                     the reference geometry (need >= 3)",
                                    group.key
                                ));
                            }
                            if let Some(count) = pick.sky_fallback {
                                rc.warnings.push(format!(
                                    "group {}: no admissible member has a usable measured \
                                     background — anchored by weight alone among {count} \
                                     candidate(s)",
                                    group.key
                                ));
                            }
                            pick.idx
                        }
                        None => {
                            finish_meter(&mut normalize_meter);
                            let n = normalize_start.elapsed();
                            let (_, i, d, o) = zero();
                            return Ok((
                                fail_group(
                                    rc,
                                    group,
                                    members.len(),
                                    "no included frame to anchor normalization",
                                )?,
                                n,
                                i,
                                d,
                                o,
                            ));
                        }
                    };
                    normalization_reference_frame_id = members[reference_idx].frame_id;
                    channels = members[reference_idx].measurement.channels.len();
                    stack_frames = build_stack_frames(&members);
                }
            }
            Err(RunError::Cancelled) => return Err(RunError::Cancelled),
            // Carry-over (b), Task 5's re-review: this can only reach
            // `process_group_output` after `exclude_frame_and_persist`
            // already flipped a frame's in-memory state — the group cannot
            // safely continue as if that exclusion never happened, so it
            // fails outright (same shape every other group-fatal condition
            // in this function uses) rather than falling into the generic
            // "warn and continue with global normalization" arm below.
            Err(RunError::ExclusionPersistFailed(msg)) => {
                finish_meter(&mut normalize_meter);
                let n = normalize_start.elapsed();
                let (_, i, d, o) = zero();
                return Ok((
                    fail_group(
                        rc,
                        group,
                        members.len(),
                        &format!("local normalization: persisting a frame exclusion failed: {msg}"),
                    )?,
                    n,
                    i,
                    d,
                    o,
                ));
            }
            Err(RunError::Other(msg)) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %msg,
                    "local normalization failed for this group; continuing with global normalization"
                );
                rc.warnings.push(ln_fallback_warning(
                    &group.key,
                    !normalization_cfg.local.enabled,
                    &LnFallback::GroupFailed { msg: &msg },
                ));
            }
        }
    } else {
        rc.progress(
            Stage::Normalize,
            Some(group.key.clone()),
            1,
            1,
            0,
            0,
            None,
            None,
        );
    }
    finish_meter(&mut normalize_meter);
    let normalize_dur = normalize_start.elapsed();

    // M2 Task 7 (fix round 1, item 3): `GroupInput.ln`, aligned with
    // `members`' (possibly LN-narrowed) order, built directly from the
    // already-verified grids `run_group_normalization` handed back — no
    // file I/O and no exclusion handling here, both moved to the source
    // (see `ln_sidecar_grids`'s own doc above). A member absent from the
    // map (kept with a warning while LN drives rejection only, after its
    // OWN `normalize_frame` call failed this run, or a plan 6b-3 D7
    // low-overlap member that keeps global normalization) gets `None`. `None`
    // (the whole field) whenever LN never ran for the group at all,
    // matching every M1 caller byte-for-byte.
    let ln_grids: Option<Vec<Option<LnFrameGrids>>> = ln_sidecar_grids
        .map(|mut grids| members.iter().map(|m| grids.remove(&m.frame_id)).collect());

    // M3 Task 5 (spec Section 6.2, ruling R-M3-8): when drizzle wants the
    // per-frame rejection survivor mask, the RUN — not `integrate_group` —
    // creates the bitmap set, sized and ordered to the SAME included set
    // `integrate_group` will compute internally via the extracted
    // `included_after_min_weight` rule (called here with the exact same
    // `stack_frames`/`reference_idx`/`min_weight` `integrate_group` itself
    // will use, so the two can never disagree about which frames survive
    // the floor). A `create` failure skips this group's drizzle entirely,
    // with a warning below (ruling text: "the master still integrates,
    // rej: None") — `rej_set_failure` carries the reason for that later
    // warning.
    //
    // M4c Task 3 (ruling R-M4c-4) widens the condition: large-scale
    // rejection derives its structures from the SAME bitmaps, so it needs
    // the set even with drizzle off. `group_integration` (not
    // `rc.config.integration`) is read for the flag on purpose — it is the
    // config `integrate_group` itself will see, so its own
    // `large_scale_passes` answer can never disagree with this one.
    let bitmaps_for_drizzle = rc.config.drizzle.enabled && rc.config.drizzle.use_rejection;
    let bitmaps_for_large_scale = group_integration.large_scale.enabled;
    // Which consumer(s) a bitmap failure would cost, for an honest warning.
    let bitmap_consumers = match (bitmaps_for_drizzle, bitmaps_for_large_scale) {
        (true, true) => "drizzle and large-scale rejection",
        (true, false) => "drizzle",
        _ => "large-scale rejection",
    };
    let mut rej_set: Option<RejBitmapSet> = None;
    let mut rej_set_failure: Option<String> = None;
    if bitmaps_for_drizzle || bitmaps_for_large_scale {
        let included_for_rej =
            included_after_min_weight(&stack_frames, reference_idx, group_integration.min_weight);
        let included_ids: Vec<i64> =
            included_for_rej.iter().map(|&i| stack_frames[i].frame_id).collect();
        let stems = rejection_map_stems(group, &included_ids);
        match RejBitmapSet::create(
            &rc.layout.rej_dir(rc.run_id, &group.key),
            &stems,
            width,
            height,
            channels,
        ) {
            Ok(set) => rej_set = Some(set),
            Err(e) => {
                let msg = format!("failed to create the rejection-bitmap set: {e:#}");
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %msg,
                    consumers = bitmap_consumers,
                    "rejection bitmaps unavailable for this group"
                );
                rc.warnings.push(format!(
                    "group {}: {bitmap_consumers} skipped — {msg}",
                    group.key
                ));
                rej_set_failure = Some(msg);
            }
        }
    }

    // Plan D D1b: the set is cloned out of `rc` (a few KB) so `input` holds no borrow of it.
    let common_stars = rc.common_stars.get(&group.key).cloned();
    // `input` borrowed the STACK_FRAMES built above; when the LN pass
    // narrowed `members` (and rebuilt `stack_frames`), that borrow must be
    // dropped and rebuilt before anything below reads it — always rebuilding
    // here (cheap: a handful of references/copies) is simpler than tracking
    // whether anything actually changed.
    let input = GroupInput {
        frames: &stack_frames,
        reference: reference_idx,
        width,
        height,
        channels,
        interpolation: rc.config.registration.interpolation,
        clamping: rc.config.registration.clamping_threshold,
        integration: &group_integration,
        normalization: &normalization_cfg,
        ln: ln_grids.as_deref(),
        // M3 Task 2/5: the bitmap set created just above (`None` when
        // drizzle doesn't want rejection bitmaps this run, or `create`
        // itself failed — `integrate_group` runs fine either way, it just
        // never gets bitmaps to write).
        rej: rej_set.as_ref(),
        // Final fix wave: the large-scale SECOND pass writes a bitmap set
        // of its own only when the Drizzle stage — its one reader — is
        // going to open it. `bitmaps_for_drizzle` is the same condition
        // that decides whether drizzle reads bitmaps at all, so a
        // standalone large-scale run writes two sets per frame instead of
        // three and nothing downstream misses the third.
        second_pass_bitmaps: bitmaps_for_drizzle,
        coverage_map: rc.config.output.coverage_map || rc.config.output.crop_to_coverage > 0,
        stats_on_covered: rc.grid.is_some(),
        // Plan D D1b: the master PSF SNR and the SNR gain read the group's frame-common star set (`reweigh_groups`).
        common_stars: common_stars.as_deref(),
    };

    // Progress plumbing: `on_plane`/`on_band`/`on_combine` are `Sync`
    // closures the engine may call from any of its own worker threads — they
    // take no `&mut RunContext`, only cloned-out plain values plus an
    // `AtomicUsize`/`Mutex<Instant>` for the shared "which plane, when did we
    // last emit" state (same pattern as `api::masters::run_build`).
    // Percent = `100 * (plane + band_fraction) / planes`: `on_plane` stamps
    // the current plane (fired at plane start, `band_fraction = 0`);
    // `on_band`/`on_combine` read it back and derive `band_fraction` from
    // the engine's own `bytes_done/bytes_total` pair for that plane.
    //
    // M4c Task 3: the plane space is `channels × passes` wide — large-scale
    // rejection integrates the group TWICE, and `integrate_group` reports
    // pass 2's planes offset by `channels` (see `large_scale_passes`, the
    // one place that rule lives; called here with the same inputs
    // `integrate_group` will use, so the denominator below and the plane
    // indices it receives can never disagree).
    let integrate_planes_total =
        channels * large_scale_passes(&group_integration, rej_set.is_some());
    let current_plane = std::sync::atomic::AtomicUsize::new(0);
    let tick_state: Mutex<IntegrateTickState> = Mutex::new(IntegrateTickState::new());
    // The plane's row count for the combine-progress fraction: the group's
    // own (reference) geometry, the same rows the engine combines.
    let plane_rows = rc.geometry_of(&group.key).height as u64;
    let emitter = rc.emitter.clone();
    let run_id = rc.run_id;
    let tick_subject = rc.subject.clone();
    let group_key_for_progress = group.key.clone();

    let on_plane = |p: usize, _total: usize| {
        current_plane.store(p, Ordering::Relaxed);
        tick_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rows
            .on_plane();
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            &tick_subject,
            &group_key_for_progress,
            integrate_planes_total,
            p,
            0.0,
            0,
            0,
            true,
            Some(integrate_tick_message(
                p,
                integrate_planes_total,
                channels,
                "reading",
            )),
        );
    };
    let on_band = |band: usize, bands: usize, bytes_done: u64, bytes_total: u64| {
        let plane = current_plane.load(Ordering::Relaxed);
        // Percent follows ROWS COMBINED, not bytes read: a band's read is
        // ≈ 10 % of its time and the bytes pair is what the text shows
        // anyway. Reporting the bytes fraction here used to put the bar
        // AHEAD of the combine tick, whose rows fraction the max-latch then
        // dropped for the rest of the band.
        let frac = {
            let mut st = tick_state.lock().unwrap_or_else(|e| e.into_inner());
            st.rows.on_band(band);
            st.rows.frac(plane_rows)
        };
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            &tick_subject,
            &group_key_for_progress,
            integrate_planes_total,
            plane,
            frac,
            bytes_done,
            bytes_total,
            false,
            Some(integrate_tick_message(
                plane,
                integrate_planes_total,
                channels,
                &format!("band {band}/{bands}"),
            )),
        );
    };
    let on_combine = |rows: usize, rows_total: usize, bytes_done: u64, bytes_total: u64| {
        let plane = current_plane.load(Ordering::Relaxed);
        let frac = {
            let mut st = tick_state.lock().unwrap_or_else(|e| e.into_inner());
            st.rows.on_combine(rows as u64, rows_total as u64);
            st.rows.frac(plane_rows)
        };
        emit_integrate_tick(
            &tick_state,
            emitter.as_ref(),
            run_id,
            &tick_subject,
            &group_key_for_progress,
            integrate_planes_total,
            plane,
            frac,
            bytes_done,
            bytes_total,
            false,
            Some(integrate_tick_message(
                plane,
                integrate_planes_total,
                channels,
                "combining",
            )),
        );
    };
    let progress = GroupProgress {
        on_plane: &on_plane,
        engine: EngineProgress {
            on_band: &on_band,
            on_combine: &on_combine,
        },
    };

    let integrate_start = Instant::now();
    let mut integrate_meter = Some(crate::stacking::telemetry::StageMeter::start(
        rc,
        Stage::Integrate,
        Some(&group.key),
    ));
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "integrate",
        &group.key,
        "integrate band plan",
    );
    let cancel: &AtomicBool = &rc.cancel;
    let pool: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    // The span names the group on the integration's own events (the master
    // statistics rectangle among them); logging only.
    let integrated = {
        let _span = tracing::debug_span!("integrate_group", group_key = %group.key).entered();
        integrate_group(&input, measure_opts, pool, cancel, &progress, io)
    };
    let mut output = match integrated {
        Ok(o) => o,
        Err(IntegrationError::Cancelled) => return Err(RunError::Cancelled),
        Err(e) => {
            finish_meter(&mut integrate_meter);
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("group integration failed: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_start.elapsed(),
                Duration::ZERO,
                Duration::ZERO,
            ));
        }
    };
    emit_integrate_tick(
        &tick_state,
        emitter.as_ref(),
        run_id,
        &tick_subject,
        &group_key_for_progress,
        integrate_planes_total,
        integrate_planes_total,
        0.0,
        0,
        0,
        true,
        None,
    );
    finish_meter(&mut integrate_meter);
    let integrate_dur = integrate_start.elapsed();

    // B3 (M3 final fix wave, I3): integration itself completed correctly —
    // `rej_set`'s own `failure()` is only ever set by a WRITE fault the
    // engine's band loop hit mid-integration (`RejPlaneSink::record_band`
    // latches it and returns `Ok`, per ruling: never fail a run because
    // drizzle-support machinery failed). Folded into `rej_set_failure` here
    // so the SAME check below (which already handles a `create` failure)
    // treats both the same way: drizzle skipped for this group, the master
    // stays untouched.
    if let Some(reason) = rej_set.as_ref().and_then(|s| s.failure()) {
        rc.warnings.push(format!(
            "{bitmap_consumers} skipped for {}: rejection bitmaps could not be written: {reason}",
            group.key
        ));
        rej_set_failure = Some(reason);
    }

    // M4c Task 3: large-scale rejection was asked for but did not run for
    // this group — `integrate_group` logged WHY (an unusable first-pass
    // bitmap set, or a processing failure) and kept the first pass's
    // master; the user is told here, since the warning list is the run's
    // own user-visible channel. The bitmap-write fault above already has
    // its own line, so this one covers the remaining causes only.
    if bitmaps_for_large_scale
        && output.stats.large_scale_rejected_fraction.is_none()
        && rej_set_failure.is_none()
    {
        rc.warnings.push(format!(
            "group {}: large-scale rejection did not run — the master is the per-pixel result \
             (see the log for the reason)",
            group.key
        ));
    }

    // Fix round 1 (ruling R-T3-1): the second pass ran but left no usable
    // bitmap set of its own, so nothing on disk describes THIS master's
    // rejected samples — drizzle is skipped for the group rather than
    // pointed at pass 1's bits, which describe the integration the second
    // pass replaced. The master itself is untouched and already written.
    //
    // Final fix wave: gated on `bitmaps_for_drizzle`, because the whole
    // degradation this branch describes is drizzle's. With drizzle off the
    // second pass deliberately writes no set of its own (`GroupInput::
    // second_pass_bitmaps`), so `second_pass_rej_ok` is false by design and
    // warning here told the user about a drizzle that was never going to
    // run. `rej_set_failure` — the only thing this branch sets — is itself
    // read only under `rc.config.drizzle.enabled`, so nothing else loses
    // information.
    if bitmaps_for_drizzle
        && output.stats.large_scale_rejected_fraction.is_some()
        && !output.second_pass_rej_ok
        && rej_set_failure.is_none()
    {
        let msg = "the second pass's rejection bitmaps are unavailable".to_string();
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            error = %msg,
            consumers = "drizzle",
            "rejection bitmaps unavailable for this group"
        );
        rc.warnings
            .push(format!("drizzle skipped for {}: {msg}", group.key));
        rej_set_failure = Some(msg);
    }

    // Plan 6b-3 D10 (spec §5.9): with `cropToCoverage = K > 0` this group's
    // outputs are written as FITS into the run's scratch folder and wait for
    // the run's rectangle (`finish_crop`). The group's own `coverage >= K` box
    // is taken here, from the count the engine kept for it; when the
    // coverage map itself is off, that count is dropped before the write.
    let crop_k = rc.config.output.crop_to_coverage;
    let crop_mode = crop_k > 0;
    let group_rect = if crop_mode {
        output.coverage.as_deref().and_then(|c| {
            crate::stacking::crop::coverage_rect(c, output.width, output.height, crop_k)
        })
    } else {
        None
    };
    if crop_mode && !rc.config.output.coverage_map {
        output.coverage = None;
    }
    let (out_dir, out_format) = if crop_mode {
        (
            rc.layout.uncropped_dir(rc.run_id),
            crate::stacking::config::OutputFormat::Fits,
        )
    } else {
        (rc.output_dir.clone(), rc.config.output.format)
    };

    let output_start = Instant::now();

    // Spec §5.9, plan 6b-3 D3, D11: what a grid master states — cloned out
    // so the drizzle block below can read it while `rc` is borrowed.
    let grid_stamp: Option<(Arc<crate::stacking::grid_map::GridRun>, String)> = rc
        .grid
        .clone()
        .zip(rc.subject.project_id().map(str::to_string));

    let group_reference_calibrated = members[reference_idx].calibrated.clone();
    let reference_cards = match source_cards_from_file(&group_reference_calibrated) {
        Ok(c) => c,
        Err(e) => {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to read the group reference's header: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    };

    let date_obs_first = output
        .included
        .iter()
        .filter_map(|&i| members[i].date_obs.as_deref())
        .min();
    let date_obs_last = output
        .included
        .iter()
        .filter_map(|&i| members[i].date_obs.as_deref())
        .max();

    // Fix round 1, item 3: `ATH_STKF` names the SOURCE frame, not the
    // calibrated artifact — Checkpoint B's own master reads
    // `ATH_STKF = '2025-09-14_02-19-02__-9.90_180.00s_0019'`, the raw
    // `files.filename` stem, never `c_`/`_d`-stripped (the calibrated
    // file's own stem needed stripping in the probe only because the probe
    // had no `GroupFrame.filename` to read directly).
    let reference_id = Path::new(&members[reference_idx].filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("reference")
        .to_string();
    let weight_mode_str = enum_serde_name(&rc.config.measurement.weight_mode)?;
    let normalization_str = format!(
        "{}/{}",
        enum_serde_name(&rc.config.normalization.output)?,
        enum_serde_name(&rc.config.normalization.rejection)?
    );
    let run_id_str = rc.run_id.to_string();

    let mut cards = match build_master_light_cards(&MasterCardInputs {
        reference_cards: &reference_cards,
        wcs,
        frames: output.stats.included,
        weighted_exposure_s: output.stats.weighted_exposure_s,
        date_obs_first,
        date_obs_last,
        recipe: &output.stats.recipe,
        weight_mode: &weight_mode_str,
        normalization: &normalization_str,
        reference_id: &reference_id,
        group_key: &group.key,
        cameras: &group.cameras,
        run_id: &run_id_str,
        app_version: &rc.app_version,
        geometry: rc.config.registration.geometry,
    }) {
        Ok(c) => c,
        Err(e) => {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to build the master header: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    };
    if grid_stamp.is_some()
        && group
            .filter
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        // Review M4: a grid master never carries the reference's raw FILTER.
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            "the group's canonical filter is empty; the grid master carries no FILTER"
        );
        rc.warnings.push(format!(
            "group {}: its canonical filter is empty, so the master carries no FILTER card",
            group.key
        ));
    }
    if let Some((grid, project_id)) = &grid_stamp {
        if let Err(e) = stamp_grid_cards(
            &mut cards,
            &GridStamp {
                project_id,
                grid_hash: &grid.hash,
                filter: group.filter.as_deref(),
            },
        ) {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to build the master header: {e}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    }

    // Plan 6c D9: the exposures of the frames the master integrated
    // (`output.included` indexes `members`), not the group's label alone.
    let exposure = exposure_label(
        rc.config.grouping.exposure_mode,
        group.exposure_s,
        output.included.iter().map(|&i| members[i].exposure_s),
    );
    let name = master_file_name(
        &rc.set_name,
        group.filter.as_deref(),
        group.color_mode,
        group.binning,
        exposure,
        output.stats.included,
    );

    let written = {
        // Fix round 1, item 2: `unwrap_or_else(|e| e.into_inner())`, not
        // `.unwrap()` — this is a process-wide `static`, so one panic
        // inside a PRIOR `write_master_light` call (any group, any run)
        // would otherwise poison it for the rest of the process's life,
        // failing every later master write with no write ever attempted.
        // Safe to recover: the guarded section only claims a name
        // (`resolve_collision`) and writes through `write_fits_f32` or, for
        // `output.format = xisf` (M4d Task 2), `write_xisf_f32` — both of
        // which write to a sibling temp file and atomically rename it into
        // place (`fits_writer::writer::write_fits_f32`,
        // `fits_writer::xisf_writer::write_xisf_f32`) — so a panic mid-write
        // leaves an orphaned `*.fits.tmp.<pid>.<seq>` (or
        // `*.xisf.tmp.<pid>.<seq>`) file, never a partial file AT the
        // resolved name itself; a later `resolve_collision` call still sees
        // that name correctly free and reuses it, and there is no partial
        // state under this lock for a later writer to misread.
        let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        write_master_light(&out_dir, &name, &output, &cards, out_format)
    };
    let written = match written {
        Ok(w) => w,
        Err(e) => {
            let outcome = fail_group(
                rc,
                group,
                members.len(),
                &format!("failed to write the master: {e:#}"),
            )?;
            return Ok((
                outcome,
                normalize_dur,
                integrate_dur,
                Duration::ZERO,
                output_start.elapsed(),
            ));
        }
    };

    let stats_json = serde_json::to_string(&output.stats)
        .map_err(|e| RunError::Other(format!("failed to serialize group stats: {e}")))?;
    let group_id = *rc.group_ids.get(&group.key).ok_or_else(|| {
        RunError::Other(format!(
            "no stacking_run_groups row for group {}",
            group.key
        ))
    })?;
    let master_path_str = written.master.display().to_string();
    let rejection_low_str = written
        .rejection_low
        .as_ref()
        .map(|p| p.display().to_string());
    let rejection_high_str = written
        .rejection_high
        .as_ref()
        .map(|p| p.display().to_string());
    let coverage_path_str = written.coverage.as_ref().map(|p| p.display().to_string());

    // M4d Task 3 (ruling R-M4d-4): the `master_lights` row's own facts.
    // `frames` is the group's included count — the SAME number the group row
    // and the master's `ATH_STKN` card carry, so the three can never
    // disagree. `total_exposure_s` is the PLAIN sum of those frames'
    // exposures (not `weighted_exposure_s`, which is the integration's own
    // weighted figure): `Option`'s own `Sum` collapses to `None` the moment
    // one member carries no `EXPTIME`, which is exactly the nullable column
    // the schema asks for — a partial total would be a lie.
    let output_format_str = enum_serde_name(&out_format)?;
    let total_exposure_s: Option<f64> = output
        .included
        .iter()
        .map(|&i| members[i].exposure_s)
        .sum::<Option<f64>>();

    let mut rejected_by_frame: HashMap<i64, f64> = HashMap::with_capacity(output.included.len());
    {
        let conn = db(&rc.ctx)?.conn();
        update_group(
            &conn,
            group_id,
            &GroupUpdate {
                included_count: Some(output.stats.included as i64),
                master_path: Some(&master_path_str),
                rejection_low_path: rejection_low_str.as_deref(),
                rejection_high_path: rejection_high_str.as_deref(),
                stats_json: Some(&stats_json),
                status: Some("done"),
                ..Default::default()
            },
        )?;

        // M4d Task 3: one row per WRITTEN output, alongside the group-row
        // update that records the same master — the master light is
        // cataloged here and nowhere else (it never becomes a `frames`
        // row). `width`/`height`/`channels` are what the writer actually
        // wrote: this group's own reference geometry and plane count.
        insert_master_light(
            &conn,
            &NewMasterLight {
                subject: &rc.subject,
                run_id: rc.run_id,
                group_key: &group.key,
                kind: MasterLightKind::Master.as_db_str(),
                path: &master_path_str,
                format: &output_format_str,
                width: width as i64,
                height: height as i64,
                channels: channels as i64,
                frames: output.stats.included as i64,
                total_exposure_s,
            },
        )?;
        // Plan 6b-3 D9: the coverage map's own row, in the same DB
        // block as the master's.
        if let Some(coverage) = coverage_path_str.as_deref() {
            insert_master_light(
                &conn,
                &NewMasterLight {
                    subject: &rc.subject,
                    run_id: rc.run_id,
                    group_key: &group.key,
                    kind: MasterLightKind::Coverage.as_db_str(),
                    path: coverage,
                    format: "fits",
                    width: width as i64,
                    height: height as i64,
                    channels: 1,
                    frames: output.stats.included as i64,
                    total_exposure_s,
                },
            )?;
        }

        for (k, &idx) in output.included.iter().enumerate() {
            if let Some(frac) = output.stats.rejected_fraction_per_frame.get(k).copied() {
                let frame_id = members[idx].frame_id;
                set_frame_rejected_fraction(&conn, &rc.subject, rc.run_id, frame_id, frac)?;
                rejected_by_frame.insert(frame_id, frac);
            }
        }
    }

    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        path = %master_path_str,
        "master written"
    );

    // M4d Task 2 fix round 1 (ruling R-T2-1): XISF has no row-order concept
    // — row 0 IS the top row for every XISF reader — while this master's
    // array is simply its source frames' order, carried through every stage
    // unflipped. For a bottom-up set (or one whose frames carry no
    // `ROWORDER` at all, the astronomical default) the XISF master therefore
    // displays mirrored relative to the FITS master of the same run. The
    // pixels are deliberately not flipped (that would have to transform the
    // WCS and its SIP terms — a follow-up); the XISF header states the
    // effective order explicitly and this says so once per group. The
    // group's drizzled master and weight map share this array order, so
    // they are covered by this one warning rather than repeating it.
    if rc.config.output.format == crate::stacking::config::OutputFormat::Xisf
        && cards_row_order_is_bottom_up(&cards)
    {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            path = %master_path_str,
            "xisf output keeps the frames' bottom-up row order"
        );
        rc.warnings.push(format!(
            "group {}: XISF output keeps the frames' bottom-up row order; XISF viewers \
             will show it flipped relative to the FITS master",
            group.key
        ));
    }

    // M3 Task 5 (spec Stage::Drizzle, ruling R-M3-11): runs AFTER the master
    // is written and its DB row updated, in this SAME call, so a drizzle
    // failure of any kind can never turn a successful master build into a
    // reported group failure (ruling R-M3-7) — the master above is already
    // good either way. `drizzle_path`/`weight_map_path`/`drizzle_stats` feed
    // `push_summary_group` below; every early-return path above this point
    // passes `None` for all three via the existing three-`None` call sites.
    let mut drizzle_dur = Duration::ZERO;
    let mut drizzle_path: Option<String> = None;
    let mut weight_map_path: Option<String> = None;
    let mut drizzle_coverage_path: Option<String> = None;
    let mut drizzle_stats: Option<DrizzleStats> = None;
    let mut crop_drizzle: Option<CropDrizzle> = None;
    if rc.config.drizzle.enabled {
        if let Some(reason) = &rej_set_failure {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                error = %reason,
                "drizzle skipped for this group; the master is unaffected"
            );
            // B7 (M3 final fix wave, M4): drizzle was never attempted for
            // this group — no `0/total` starting tick was ever emitted
            // either, so without a terminal tick here the Drizzle row's
            // last observed event is whatever the PREVIOUS group (or stage)
            // left it at, forever. `0/0` (not `total/total` — there is no
            // `total` to report; nothing was attempted) with the skip
            // reason as `message` gives the row an honest, final state.
            rc.progress(
                Stage::Drizzle,
                Some(group.key.clone()),
                0,
                0,
                0,
                0,
                None,
                Some(format!("rejection bitmaps could not be written: {reason}")),
            );
        } else {
            // Fix round 1, Minor M6: counts as "attempted" from here —
            // bitmap creation (if wanted) already succeeded, so the only
            // remaining outcomes are a shape refusal, `drizzle_group`
            // itself, or the post-`drizzle_group` write steps; every one
            // of those is a real attempt for `stage_output`'s own
            // `Stage::Drizzle` `StageTiming` gate to count (never
            // `any_master`, which is only a proxy — see that gate's own
            // doc).
            rc.drizzle_attempted += 1;

            // Fix round 1, Minor M2: the run-wide `dropShrink` clamp
            // (ruling R-M3-10) now happens ONCE, in `stage_output` before
            // the group loop — `rc.config.drizzle.drop_shrink` is already
            // in `[0.5, 1.0]` by the time any group reads it here, so
            // reading it straight off `rc.config` (not clamping again) can
            // never repeat the run warning once per group.
            let drizzle_cfg = rc.config.drizzle.clone();
            let scale = drizzle_cfg.scale;
            let drop_shrink = drizzle_cfg.drop_shrink;

            // M4d Task 1 (ruling R-M4d-2): Bayer drizzle's per-frame source.
            // Resolved HERE, before the immutable borrows below, because it
            // needs a DB connection: the frame's own `calibrated_mosaic`
            // artifact (stage 1 wrote it in the same generation as the
            // debayered frame) plus the mosaic's phase-corrected pattern,
            // read through the SAME two functions the generator used, so the
            // deposit and the debayer can never disagree about which pixel is
            // red. A frame missing either falls back to the debayered deposit
            // with a named warning — an interpolated frame in the stack is
            // honest and useful; refusing the whole group over one frame is
            // not.
            // A 3-channel group is the only one a mosaic can be routed into
            // (`drizzle_group` refuses the combination outright), and
            // `channels` is this GROUP's own plane count — an OSC group whose
            // frames somehow measured one plane is not one.
            let bayer_wanted = wants_cfa_mosaic(&rc.config, group) && channels == 3;
            let mut bayer_sources: Vec<Option<(PathBuf, astroimage::BayerPattern)>> =
                vec![None; output.included.len()];
            if bayer_wanted {
                let conn = db(&rc.ctx)?.conn();
                for (k, &idx) in output.included.iter().enumerate() {
                    let frame_id = members[idx].frame_id;
                    let path = crate::db::stacking::find_artifact(
                        &conn,
                        &rc.subject,
                        artifact_group_key("calibrated_mosaic", &group.key),
                        "calibrated_mosaic",
                        Some(frame_id),
                    )?
                    .and_then(|a| a.path)
                    .map(PathBuf::from);
                    let pattern = match origin_of(&rc.origins, frame_id)
                        .map_err(anyhow::Error::from)
                        .and_then(|origin| cfa_geometry(&conn, origin, frame_id))
                    {
                        Ok(geom) => geom.map(crate::export::calibrated_generator::bayer_for),
                        Err(error) => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id,
                                error = %format!("{error:#}"),
                                "bayer drizzle: reading the frame's mosaic phase failed"
                            );
                            None
                        }
                    };
                    bayer_sources[k] = match (path, pattern) {
                        (Some(path), Some(pattern)) => Some((path, pattern)),
                        _ => None,
                    };
                }
            }
            let bayer_missing = bayer_sources.iter().filter(|s| s.is_none()).count();
            if bayer_wanted && bayer_missing > 0 {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    count = bayer_missing,
                    "bayer drizzle: frames without a cfa mosaic fall back to the debayered deposit"
                );
                rc.warnings.push(format!(
                    "group {}: Bayer drizzle deposited {bayer_missing} frame(s) from their \
                     debayered planes — no calibrated CFA mosaic was available for them",
                    group.key
                ));
            }

            let drizzle_progress_total = output.included.len() * channels;
            rc.progress(
                Stage::Drizzle,
                Some(group.key.clone()),
                0,
                drizzle_progress_total,
                0,
                0,
                None,
                None,
            );
            let drizzle_start = Instant::now();
            let mut drizzle_meter = Some(crate::stacking::telemetry::StageMeter::start(
                rc,
                Stage::Drizzle,
                Some(&group.key),
            ));

            // Fix round 1, Minor M4: `output.output_pairs[p][k]` below is a
            // bare double index — a shape mismatch would otherwise panic
            // inside `process_group_output`, get caught by `run_thread`'s
            // `catch_unwind`, and fail the WHOLE RUN, exactly what R-M3-7
            // forbids for a drizzle-only problem. Guaranteed safe today
            // (`integrate_planes` pushes one `output_pairs` entry per
            // plane, one inner entry per included frame — `integrate.rs`),
            // checked anyway as defense-in-depth: a mismatch degrades to
            // `DrizzleFailure::Other`, same as any other post-`drizzle_group`
            // step's error.
            let output_pairs_shape_ok = output.output_pairs.len() == channels
                && output
                    .output_pairs
                    .iter()
                    .all(|p| p.len() == output.included.len());

            let drizzle_outcome: Result<(WrittenDrizzle, DrizzleStats, Vec<Card>), DrizzleFailure> =
                if !output_pairs_shape_ok {
                    Err(DrizzleFailure::Other(format!(
                        "output_pairs shape does not match {channels} channel(s) x {} included frame(s)",
                        output.included.len()
                    )))
                } else {
                    // Per included frame (`output.included`, engine order —
                    // the SAME order `rej_set` (when created) was
                    // sized/stemmed to), transpose `output.output_pairs`
                    // (outer: plane, inner: included frame) into a
                    // per-frame, per-plane slice — the shape
                    // `DrizzleFrame::output_pair` wants.
                    let output_pairs_by_frame: Vec<Vec<NormalizationPair>> =
                        (0..output.included.len())
                            .map(|k| (0..channels).map(|p| output.output_pairs[p][k]).collect())
                            .collect();
                    // M4c Task 3, fix round 1 (ruling R-T3-1): drizzle reads
                    // exactly the bits the master was built with — see
                    // `drizzle_rejection_paths`.
                    let rej_paths = drizzle_rejection_paths(
                        rej_set.as_ref(),
                        output.included.len(),
                        output.second_pass_rej_ok,
                    );
                    let drizzle_frames: Vec<DrizzleFrame> = output
                        .included
                        .iter()
                        .enumerate()
                        .map(|(k, &idx)| DrizzleFrame {
                            path: stack_frames[idx].path.as_path(),
                            map: &stack_frames[idx].map,
                            weight: &stack_frames[idx].weight.normalized,
                            output_pair: &output_pairs_by_frame[k],
                            ln: ln_grids
                                .as_ref()
                                .and_then(|g| g.get(idx))
                                .and_then(|o| o.as_ref()),
                            rej: rej_paths[k].as_deref(),
                            // M4d Task 1: indexed by `k` like `rej_paths` —
                            // the included-frame order, not `idx`.
                            cfa: bayer_sources[k].as_ref().map(|(path, pattern)| {
                                crate::stacking::drizzle::CfaSource {
                                    path: path.as_path(),
                                    pattern: *pattern,
                                }
                            }),
                        })
                        .collect();

                    let drizzle_input = DrizzleInput {
                        frames: &drizzle_frames,
                        width,
                        height,
                        channels,
                        scale,
                        drop_shrink,
                        kernel: drizzle_cfg.kernel,
                        use_weights: drizzle_cfg.use_weights,
                        use_rejection: drizzle_cfg.use_rejection && rej_set.is_some(),
                        // The drizzle follows the plain master's OUTPUT
                        // normalization: LN grids computed for rejection
                        // alone (`rejection = Local`, `local.enabled =
                        // false` - the shipped default) never normalize
                        // the drizzled master, or it would be locally
                        // normalized while the master it sits beside is not.
                        use_local_normalization: drizzle_cfg.use_local_normalization
                            && rc.config.normalization.local.enabled,
                        write_weight_map: drizzle_cfg.write_weight_map,
                        write_coverage: rc.config.output.coverage_map,
                        // Plan 6b-3 ruling (b): a grid drizzle's statistics ride
                        // the master's own covered rectangle; `None` there is
                        // the defensive arm (the drizzle fails, the master stays).
                        stats_on_covered: rc.grid.is_some(),
                        stats_rect: drizzle_stats_rect(output.measure_rect),
                        measure: measure_opts,
                        ram_total_bytes: None,
                        // Tier C item C2: the run always drizzles through
                        // whichever overlap arm ruling C-3 resolves to; only
                        // the `drizzle_probe` forces the exact clip.
                        force_exact_overlap_for_measurement: false,
                    };

                    // Fix round 1, Important I1: real per-frame progress —
                    // `drizzle_group` calls `on_frame` once per (frame,
                    // plane) unit, strictly sequentially (see
                    // `DrizzleTickState`'s own doc), so a throttled
                    // `emit_drizzle_tick` reaches the SAME `stacking-progress`
                    // channel `rc.progress` uses, via cloned-out plain
                    // values (`emitter`/`run_id`/`subject`/`group_key`) since
                    // the closure cannot hold `&mut rc`. `frame_id` is the
                    // k-th drizzled frame's own id — `done` is 1-based
                    // (incremented before the call, `drizzle/mod.rs`), so
                    // `(done - 1) % n` recovers `k` regardless of which
                    // plane `done` is currently in (channels > 1 wraps
                    // `done` past `n` once per plane).
                    let drizzle_frame_ids: Vec<i64> = output
                        .included
                        .iter()
                        .map(|&idx| members[idx].frame_id)
                        .collect();
                    let drizzle_tick_state: Mutex<DrizzleTickState> =
                        Mutex::new(DrizzleTickState::new());
                    let drizzle_tick_emitter = rc.emitter.clone();
                    let drizzle_tick_run_id = rc.run_id;
                    let drizzle_tick_subject = rc.subject.clone();
                    let drizzle_tick_group_key = group.key.clone();
                    let drizzle_on_frame = |done: usize, total: usize| {
                        let n = drizzle_frame_ids.len();
                        let frame_id = if n == 0 {
                            None
                        } else {
                            drizzle_frame_ids.get(done.saturating_sub(1) % n).copied()
                        };
                        emit_drizzle_tick(
                            &drizzle_tick_state,
                            drizzle_tick_emitter.as_ref(),
                            drizzle_tick_run_id,
                            &drizzle_tick_subject,
                            &drizzle_tick_group_key,
                            done,
                            total,
                            frame_id,
                            false,
                        );
                    };
                    let drizzle_progress = DrizzleProgress {
                        on_frame: &drizzle_on_frame,
                    };

                    // Fresh, tightly-scoped borrows of
                    // `rc.cancel`/`rc.ctx.image_pool` — the outer
                    // `cancel`/`pool` (defined once, before
                    // `integrate_group`'s own call) are NOT reused here:
                    // this block runs well after several `&mut rc` accesses
                    // (`rc.progress`, `rc.drizzle_attempted += 1`) the outer
                    // borrow would otherwise have to span, which the borrow
                    // checker refuses (an immutable borrow of `rc` cannot be
                    // held live across a `&mut rc` use). Re-taken here,
                    // after the last `&mut rc` access before this point (the
                    // `Stage::Drizzle` progress tick above), the borrow only
                    // needs to live to the `drizzle_group` call a few lines
                    // down — no `&mut rc` happens in between.
                    let cancel: &AtomicBool = &rc.cancel;
                    let pool: &rayon::ThreadPool = rc.ctx.image_pool.as_ref();
                    // A labeled block, not a closure: every fallible step
                    // below needs `pool`/`cancel` (the fresh borrows just
                    // above) and `rc.output_dir`/`cards`/`written` (plain
                    // reads) — no `&mut rc` access happens until after the
                    // block ends, so this stays a plain immutable borrow the
                    // same way `integrate_group`'s own call above does.
                    'attempt: {
                        let drz =
                            match drizzle_group(&drizzle_input, pool, cancel, &drizzle_progress) {
                                Ok(o) => o,
                                Err(e) => break 'attempt Err(e.into()),
                            };
                        let scaled = match wcs.map(|w| scale_plate_solve(w, scale)).transpose() {
                            Ok(s) => s,
                            Err(e) => break 'attempt Err(DrizzleFailure::Other(e.to_string())),
                        };
                        let mut drz_cards = match build_drizzle_cards(
                            &cards,
                            scaled.as_ref(),
                            scale,
                            drop_shrink,
                            drizzle_cfg.kernel,
                            rc.config.registration.geometry,
                        ) {
                            Ok(c) => c,
                            Err(e) => break 'attempt Err(DrizzleFailure::Other(e.to_string())),
                        };
                        if let Some((grid, project_id)) = &grid_stamp {
                            if let Err(e) = stamp_grid_cards(
                                &mut drz_cards,
                                &GridStamp {
                                    project_id,
                                    grid_hash: &grid.hash,
                                    filter: group.filter.as_deref(),
                                },
                            ) {
                                break 'attempt Err(DrizzleFailure::Other(e.to_string()));
                            }
                        }
                        let master_stem = written
                            .master
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("master")
                            .to_string();
                        // Same "one process, one writer" concern
                        // `OUTPUT_WRITE_LOCK` guards the master write
                        // against: `write_drizzled_master` is
                        // check-then-write (`resolve_collision`) into the
                        // SAME output dir.
                        let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                        match write_drizzled_master(
                            &out_dir,
                            &master_stem,
                            &drz,
                            &drz_cards,
                            // A plain immutable read of `rc`, like
                            // `out_dir` above: the drizzled master
                            // and its weight map land in the SAME
                            // container as the master (ruling R-M4d-3).
                            out_format,
                        ) {
                            Ok(wd) => Ok((wd, drz.stats, drz_cards)),
                            Err(e) => Err(DrizzleFailure::Other(format!("{e:#}"))),
                        }
                    }
                };
            finish_meter(&mut drizzle_meter);
            drizzle_dur = drizzle_start.elapsed();
            crate::stacking::telemetry::memory_checkpoint(
                rc.run_id,
                "drizzle",
                &group.key,
                "drizzle buffers dropped",
            );

            match drizzle_outcome {
                Ok((wd, stats, drz_cards)) => {
                    let drizzle_path_str = wd.drizzle.display().to_string();
                    let weight_map_path_str =
                        wd.weight_map.as_ref().map(|p| p.display().to_string());
                    let drizzle_coverage_path_str =
                        wd.coverage.as_ref().map(|p| p.display().to_string());
                    {
                        let conn = db(&rc.ctx)?.conn();
                        update_group(
                            &conn,
                            group_id,
                            &GroupUpdate {
                                drizzle_path: Some(&drizzle_path_str),
                                ..Default::default()
                            },
                        )?;
                        // M4d Task 3 (ruling R-M4d-4): the drizzled master
                        // and — when one was written — its weight map are
                        // outputs of this run just as the master is, and
                        // get their own rows in the same DB touch as the
                        // group-row update that records the drizzle path.
                        // Their geometry is the drizzle's OWN output grid
                        // (`scale x` the master's), which the stats report;
                        // the weight map shares it exactly. `frames` and
                        // `total_exposure_s` are the same group figures the
                        // master row carries — a drizzle deposits the same
                        // included frames.
                        insert_master_light(
                            &conn,
                            &NewMasterLight {
                                subject: &rc.subject,
                                run_id: rc.run_id,
                                group_key: &group.key,
                                kind: MasterLightKind::Drizzle.as_db_str(),
                                path: &drizzle_path_str,
                                format: &output_format_str,
                                width: stats.out_width as i64,
                                height: stats.out_height as i64,
                                channels: channels as i64,
                                frames: output.stats.included as i64,
                                total_exposure_s,
                            },
                        )?;
                        if let Some(weight_map) = weight_map_path_str.as_deref() {
                            insert_master_light(
                                &conn,
                                &NewMasterLight {
                                    subject: &rc.subject,
                                    run_id: rc.run_id,
                                    group_key: &group.key,
                                    kind: MasterLightKind::WeightMap.as_db_str(),
                                    path: weight_map,
                                    format: &output_format_str,
                                    width: stats.out_width as i64,
                                    height: stats.out_height as i64,
                                    channels: channels as i64,
                                    frames: output.stats.included as i64,
                                    total_exposure_s,
                                },
                            )?;
                        }
                        // Plan 6b-3 D9 (spec §5.8): drizzle's own coverage —
                        // always FITS, the drizzle's own grid.
                        if let Some(coverage) = drizzle_coverage_path_str.as_deref() {
                            insert_master_light(
                                &conn,
                                &NewMasterLight {
                                    subject: &rc.subject,
                                    run_id: rc.run_id,
                                    group_key: &group.key,
                                    kind: MasterLightKind::DrizzleCoverage.as_db_str(),
                                    path: coverage,
                                    format: "fits",
                                    width: stats.out_width as i64,
                                    height: stats.out_height as i64,
                                    channels: channels as i64,
                                    frames: output.stats.included as i64,
                                    total_exposure_s,
                                },
                            )?;
                        }
                    }
                    // Fix round 1, Important I1: the final, forced
                    // `current == total` tick — `rc.progress` is available
                    // again here (the labeled block above has ended, the
                    // `pool`/`cancel` borrows are dropped), and `current ==
                    // total` is one of `RunContext::progress`'s own two
                    // unconditional-force cases, so this always reaches the
                    // recorder even under the 300 ms throttle.
                    rc.progress(
                        Stage::Drizzle,
                        Some(group.key.clone()),
                        drizzle_progress_total,
                        drizzle_progress_total,
                        0,
                        0,
                        None,
                        None,
                    );
                    tracing::info!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        path = %drizzle_path_str,
                        drizzle_scale = scale,
                        out_width = stats.out_width,
                        out_height = stats.out_height,
                        duration_ms = drizzle_dur.as_millis() as u64,
                        "drizzled master written"
                    );
                    drizzle_path = Some(drizzle_path_str);
                    weight_map_path = weight_map_path_str;
                    drizzle_coverage_path = drizzle_coverage_path_str;
                    if crop_mode {
                        crop_drizzle = Some(CropDrizzle {
                            written: wd.clone(),
                            cards: drz_cards,
                            scale,
                        });
                    }
                    drizzle_stats = Some(stats);
                }
                Err(DrizzleFailure::Cancelled) => return Err(RunError::Cancelled),
                Err(DrizzleFailure::Other(msg)) => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        error = %msg,
                        "drizzle failed for this group; the master is unaffected"
                    );
                    rc.warnings
                        .push(format!("group {}: drizzle failed: {msg}", group.key));
                    // B7 (M3 final fix wave, M4): a forced `total/total`
                    // terminal tick (the starting `0/total` tick already
                    // went out above) — without it the row's last observed
                    // event stays at whatever partial percent the failure
                    // landed on, forever. Carries `msg` so a live listener
                    // can show why the row completed without a
                    // `drizzlePath`.
                    rc.progress(
                        Stage::Drizzle,
                        Some(group.key.clone()),
                        drizzle_progress_total,
                        drizzle_progress_total,
                        0,
                        0,
                        None,
                        Some(msg),
                    );
                }
            }
        }
    }

    if crop_mode {
        rc.crop_items.push(CropItem {
            group_key: group.key.clone(),
            group_id,
            rect: group_rect,
            width: output.width,
            height: output.height,
            channels,
            frames: output.stats.included,
            total_exposure_s,
            cards: cards.clone(),
            wcs: wcs.cloned(),
            master: written.clone(),
            drizzle: crop_drizzle,
        });
    }

    push_summary_group(
        rc,
        group,
        output.stats.included,
        Some(master_path_str),
        rejection_low_str,
        rejection_high_str,
        Some(output.stats.clone()),
        Some(normalization_reference_frame_id),
        &rejected_by_frame,
        ln_reference_path,
        drizzle_path,
        weight_map_path,
        drizzle_stats,
        coverage_path_str,
        drizzle_coverage_path,
        ln_scale_px,
    );

    // B2 (M3 final fix wave, I2): `output_start` was taken BEFORE the
    // drizzle block above, which runs entirely inside this same timer's
    // span — without subtracting it, `StageTiming{Output}` double-counted
    // every second of `StageTiming{Drizzle}` (on the acceptance set, ~14
    // minutes of drizzle reported as the master-write stage's own
    // duration). Drizzle and Output are meant to be disjoint spans (ruling
    // R-M3-11 places Drizzle strictly between Integrate and Output); this
    // is the correction.
    let output_dur = output_start.elapsed().saturating_sub(drizzle_dur);
    drop(members);
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "output",
        &group.key,
        "group members dropped",
    );
    rc.progress(
        Stage::Output,
        Some(group.key.clone()),
        1,
        1,
        0,
        0,
        None,
        None,
    );

    Ok((
        GroupOutcome::Written,
        normalize_dur,
        integrate_dur,
        drizzle_dur,
        output_dur,
    ))
}

/// One group's stage-6 result: the reference's own artifact path (feeds
/// `SummaryGroup.ln_reference_path` — `None` means "LN did not run for this
/// group", full stop: disabled by the caller, OR EITHER shape of the
/// group-level ruling-R3-style fallback below — carry-over (a), Task 5's
/// re-review) and which frame ids the pass excluded (only ever non-empty
/// when LN drives OUTPUT normalization; ruling R2). `sidecar_grids` (M2 Task
/// 7, fix round 1 item 3) carries the ALREADY-PARSED, ALREADY-VERIFIED
/// `LnFrameGrids` for every member this call itself confirmed has a
/// READABLE `ln` artifact THIS run — a fresh cache hit whose file was
/// re-opened and parsed (not just trusted by hash/size), or a fan-out item
/// that normalized, wrote, and had its own new file read straight back —
/// `process_group_output` hands these to `GroupInput.ln` directly, no
/// second read. A member absent from this map (kept with a warning while
/// LN drives rejection only, after its OWN `normalize_frame` call failed)
/// has no verified grid and must fall back to `None` in `GroupInput.ln`.
/// Always empty when `reference_path` is `None` (LN never ran, nothing to
/// verify).
struct LnGroupOutcome {
    reference_path: Option<String>,
    excluded_frame_ids: Vec<i64>,
    sidecar_grids: HashMap<i64, LnFrameGrids>,
    // The LN scale the group ran at (spec §8); `None` on every return before
    // LN actually ran for the group.
    scale_px: Option<u32>,
}

/// The scalar diagnostics [`crate::stacking::ln::LnFrameOutcome`] carries, persisted as the `ln`
/// artifact's `payload_json` (same convention as the `metrics` artifact
/// storing a serialized [`FrameMeasurement`]) so a cache hit can fill
/// `SummaryFrame.ln_scale` without re-reading the `.athln` sidecar's actual
/// grids — nothing in the summary needs those, only these three numbers.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct LnArtifactPayload {
    scale: f64,
    matches: usize,
    // The background model's counters (spec 2026-10-04 §10), each summed over
    // channels; a payload written before them reads as 0.
    #[serde(default)]
    samples: usize,
    #[serde(default)]
    samples_rejected: usize,
    #[serde(default)]
    samples_shared: usize,
    #[serde(default)]
    cells_unsupported: usize,
    /// Perf tier C Task 2 fix round 4 (ruling C-14 items 2/3): `"seeds"` or
    /// `"detected"` — this frame's OWN scale source, so a run pin (and a
    /// reader of the cached row) can tell which path produced the sidecar
    /// without a global counter that any concurrent frame could bump.
    /// `#[serde(default)]`: a payload written before this field existed
    /// reads as `""`, which is "not recorded", not a claim either way.
    #[serde(default)]
    ln_scale_source: String,
    /// The group's seeds calibration this frame was normalized under
    /// (ruling C-14 item 2) — recorded so a stored sidecar says what it was
    /// written with, beside being folded into the row's own `config_hash`.
    #[serde(default)]
    seeds_calibration: Option<ln::SeedsCalibration>,
}

/// Sets `ln_scale`/`cached_ln` on `group_key`'s `MeasuredFrame` entry for
/// `frame_id` (a no-op if either lookup misses, which never happens for a
/// frame [`run_group_normalization`] itself just fanned `normalize_frame`
/// out over or found a fresh `ln` artifact for).
///
/// Plan B B0 (PB-2): also keeps F1's `growth_factor` and `growth_pairs` and the target side's `target_c` (all from this
/// run's own [`ln::LnFrameOutcome`]; `None` for a frame LN did not normalize and on an LN cache hit, which carries only
/// the scale).
#[allow(clippy::too_many_arguments)]
fn set_ln_summary(
    rc: &mut RunContext,
    group_key: &str,
    frame_id: i64,
    ln_scale: Option<f64>,
    cached: bool,
    growth_factor: Option<f64>,
    growth_pairs: Option<usize>,
    target_c: Option<f64>,
) {
    if let Some(entries) = rc.measured.get_mut(group_key) {
        if let Some(e) = entries.iter_mut().find(|e| e.frame.frame_id == frame_id) {
            apply_ln_summary(
                e,
                ln_scale,
                cached,
                growth_factor,
                growth_pairs,
                target_c,
            );
        }
    }
}

/// [`set_ln_summary`]'s write onto one frame's entry, apart from the run so a test reaches it.
fn apply_ln_summary(
    entry: &mut MeasuredFrame,
    ln_scale: Option<f64>,
    cached: bool,
    growth_factor: Option<f64>,
    growth_pairs: Option<usize>,
    target_c: Option<f64>,
) {
    entry.ln_scale = ln_scale;
    entry.cached_ln = cached;
    entry.ln_growth_factor = growth_factor;
    entry.ln_growth_pairs = growth_pairs;
    entry.ln_aperture_correction = target_c;
}

/// The LN fallback paths of a run, as the user reads them in the run
/// warnings (the `tracing` events of the same paths keep their stable
/// phrases).
enum LnFallback<'a> {
    /// The whole group's LN pass failed (`Err(RunError::Other)`).
    GroupFailed { msg: &'a str },
    /// One frame's own normalization failed and the frame stays on the
    /// global normalization.
    FrameFailed { frame_id: i64, msg: &'a str },
    /// `referenceFrames` under 3: no LN reference for the group.
    ReferenceFramesBelowThree { reference_frames: u32 },
    /// The LN reference's background model was refused.
    BackgroundRefused { reason: &'a str },
    /// Frames with too little overlap with the LN reference stay global.
    LittleOverlap { count: usize, percent: f64 },
}

/// The run-warning sentence of one LN fallback path.
///
/// `rejection_only` is true when LN runs for REJECTION alone: the run's
/// output normalization is global (`normalization.local.enabled == false`)
/// and `rejection == Local`, which is the shipped default. The user never
/// turned local normalization on, so a warning that says "local normalization
/// failed" names something they did not ask for (owner ruling 2026-11-01,
/// OD-1a); it names the rejection normalization they got instead and what
/// the fallback costs.
fn ln_fallback_warning(group_key: &str, rejection_only: bool, what: &LnFallback<'_>) -> String {
    const TAIL: &str =
        "rejection used the global normalization, which can clip star cores where seeing varies";
    match (rejection_only, what) {
        (false, LnFallback::GroupFailed { msg }) => {
            format!("group {group_key}: local normalization failed: {msg}")
        }
        (true, LnFallback::GroupFailed { msg }) => format!(
            "group {group_key}: local rejection normalization could not be computed ({msg}); {TAIL}"
        ),
        (false, LnFallback::FrameFailed { frame_id, msg }) => {
            format!("frame {frame_id}: local normalization failed: {msg}")
        }
        (true, LnFallback::FrameFailed { frame_id, msg }) => format!(
            "frame {frame_id}: local rejection normalization could not be computed ({msg}); this frame's rejection used the global normalization"
        ),
        (false, LnFallback::ReferenceFramesBelowThree { reference_frames }) => format!(
            "group {group_key}: local normalization off — referenceFrames ({reference_frames}) < 3; using global normalization"
        ),
        (true, LnFallback::ReferenceFramesBelowThree { reference_frames }) => format!(
            "group {group_key}: local rejection normalization could not be computed — referenceFrames ({reference_frames}) < 3; {TAIL}"
        ),
        (false, LnFallback::BackgroundRefused { reason }) => format!(
            "group {group_key}: local normalization off — the LN reference's background model is refused ({reason}); using global normalization"
        ),
        (true, LnFallback::BackgroundRefused { reason }) => format!(
            "group {group_key}: local rejection normalization could not be computed — the reference frame's background model is refused ({reason}); {TAIL}"
        ),
        (false, LnFallback::LittleOverlap { count, percent }) => format!(
            "group {group_key}: local normalization kept {count} frame(s) on global normalization — under {percent:.0} % overlap with the LN reference"
        ),
        (true, LnFallback::LittleOverlap { count, percent }) => format!(
            "group {group_key}: {count} frame(s) used the global normalization for rejection — under {percent:.0} % overlap with the reference frame"
        ),
    }
}
/// Stage 6 proper (spec §5.2, M2 Task 5): resolves/caches `group`'s LN
/// reference (best `referenceFrames` of `members` by weight, linear-fit
/// rejection, global normalization — [`build_ln_reference`]), models its
/// backgrounds once per channel, then fans [`normalize_frame`] out over
/// EVERY member (the measure stage's own [`admission`] sizing, working set
/// `channels × W × H × 4 × 2` bytes — one warped target plane plus its
/// background grid, twice over for the read-then-decode round trip),
/// skipping a member whose `ln` artifact is already fresh (path + size +
/// hash — same [`is_fresh`] rule every other per-frame artifact uses).
///
/// Ruling R3: fewer than 3 of `members` (after `referenceFrames` is applied)
/// falls back to global normalization for this WHOLE group with a warning —
/// `Ok(LnGroupOutcome { reference_path: None, excluded_frame_ids: vec![] })`,
/// never a hard failure. A per-member [`LnError`] (most commonly
/// [`LnError::TooFewMatches`]) excludes that member (`rc.runtime_exclusions`,
/// its `MeasuredFrame::included`/`reason`) when `cfg.normalization.local.enabled`
/// — LN is the output normalization the run actually wants — and is only
/// `warn!`ed otherwise (LN drives rejection normalization alone; the frame
/// keeps global normalization). `Err(RunError::Cancelled)` propagates from a
/// cancel noticed while resolving/building the reference; a fan-out item's
/// own cancellation surfaces as an ordinary per-item error and is caught by
/// the `rc.check_cancel()?` immediately after the fan-out returns, the same
/// pattern [`stage_measure`] uses.
fn run_group_normalization(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    members: &[GroupMember],
    input: &GroupInput<'_>,
    measure_opts: &MeasureOptions,
    io: IoPolicy,
    ln_field: Option<&crate::stacking::origin::GroupLnField>,
) -> Result<LnGroupOutcome, RunError> {
    let cfg = rc.config.clone();
    let ln_cfg = cfg.normalization.local;
    let ln_drives_output = ln_cfg.enabled;
    let grid_mode = rc.grid.is_some();

    // Sky-penalized members first (spec §4.4/§5.2, ruling R-M3-17 v2 —
    // supersedes plain best-weighted-first): the SAME `sky_penalized_order`
    // ranking `process_group_output`'s `pick_reference_idx` uses for the
    // normalization anchor (coverage filter included — the addendum
    // filters BOTH candidate lists the same way), computed here too
    // because the normalize-stage hash needs the member LIST before
    // knowing whether the reference will be rebuilt or reused (a cache
    // hit's `LnReference::frames_used` always comes back empty, see its
    // own doc). [`build_ln_reference`] (`ln::reference::build_reference`)
    // itself is UNCHANGED — it still ranks by weight internally whatever
    // `included` list it is handed — but `build_and_write_ln_reference`
    // below now hands it the sky-penalized top-`n` slice computed here,
    // not the whole group, so its internal re-sort only reorders those `n`
    // (already the right SET), never changes WHICH `n` are used.
    let weights: Vec<FrameWeight> = members.iter().map(|m| m.weight.clone()).collect();
    let star_counts: Vec<usize> = members.iter().map(|m| m.measurement.min_stars()).collect();
    let sky: Vec<f64> = members
        .iter()
        .map(|m| m.measurement.mean_median())
        .collect();
    let (ref_width, ref_height) = {
        let geometry = rc.geometry_of(&group.key);
        (geometry.width, geometry.height)
    };
    // Spec §2.3 (L-10): the one scale rule, on this group's LN geometry.
    let scale_px = ln::resolve_scale(&ln_cfg, ref_width, ref_height);
    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_scale_px = scale_px,
        ln_scale_mode = ln::effective_scale_mode(&ln_cfg).as_str(),
        ln_flux_estimator = ln_cfg.flux_estimator.as_str(),
        width = ref_width,
        height = ref_height,
        "ln scale resolved"
    );
    if let Some(&planned) = rc.planned_ln_scales.get(&group.key) {
        if planned != scale_px {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                ln_scale_px = scale_px,
                expected = planned,
                "ln scale differs from the plan's estimate"
            );
            rc.warnings.push(format!(
                "group {}: local normalization scale resolved to {scale_px} px; the plan estimated {planned} px",
                group.key
            ));
        }
    }
    let coverage = member_coverages(ln_field, members, ref_width, ref_height);
    let coverage_mask: Vec<bool> = coverage
        .iter()
        .map(|&c| c >= MIN_REFERENCE_COVERAGE)
        .collect();
    let coverage_admissible_count = coverage_mask.iter().filter(|&&ok| ok).count();
    let included_mask = if coverage_admissible_count >= 3 {
        coverage_mask
    } else {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = coverage_admissible_count,
            min_coverage = MIN_REFERENCE_COVERAGE,
            "reference-coverage filter dropped for this group: fewer than 3 members pass"
        );
        rc.warnings.push(format!(
            "group {}: reference-coverage filter dropped for the LN reference's member list — \
             only {coverage_admissible_count} member(s) cover the reference geometry (need >= 3)",
            group.key
        ));
        vec![true; members.len()]
    };
    let ranked = sky_penalized_order(&weights, &included_mask, &star_counts, &sky);
    let n = (ln_cfg.reference_frames as usize).min(ranked.len());
    if n < 3 {
        // Fix round 1, item 10: this branch fires ONLY when the config's
        // own `referenceFrames < 3` (a group under 3 MEMBERS never reaches
        // this function — `process_group_output` skips it upstream), so the
        // message must name that, not repeat "fewer than 3 included frames"
        // (a different, already-handled condition).
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            reference_frames = ln_cfg.reference_frames,
            "local normalization off for this group: referenceFrames < 3"
        );
        rc.warnings.push(ln_fallback_warning(
            &group.key,
            !ln_drives_output,
            &LnFallback::ReferenceFramesBelowThree {
                reference_frames: ln_cfg.reference_frames,
            },
        ));
        return Ok(LnGroupOutcome {
            reference_path: None,
            excluded_frame_ids: Vec::new(),
            sidecar_grids: HashMap::new(),
            scale_px: None,
        });
    }
    let reference_members: Vec<usize> = ranked[..n].to_vec();
    let reference_member_ids: Vec<i64> = reference_members
        .iter()
        .map(|&i| members[i].frame_id)
        .collect();

    // Plan 6b-3 D6: what every LN hash folds as a member's registration.
    let member_ln_upstreams: Vec<String> = members
        .iter()
        .map(|m| match ln_field {
            Some(field) => {
                crate::stacking::plan::ln_upstream_on_field(&m.registration_hash, &field.hash)
            }
            None => {
                crate::stacking::plan::ln_registration_upstream(&m.registration_hash, grid_mode)
            }
        })
        .collect();
    let mut member_reg_hashes: Vec<&str> = reference_members
        .iter()
        .map(|&i| member_ln_upstreams[i].as_str())
        .collect();
    member_reg_hashes.sort_unstable();
    let combined_registration_hash = member_reg_hashes.join(",");

    // C-17: every LN hash below folds in the frame's own MEASUREMENT hash
    // (`measurement_hash_for`, the value its `fits` artifact is keyed on),
    // because stage 6 reads that artifact — see `normalization_hash_for`'s
    // own doc for what went wrong without it. Resolved ONCE per member
    // here, above the reference hash that needs the combined form, and
    // read again by the `fits` path resolution below instead of being
    // recomputed there. `String::new()` for a member with no `GroupFrame`
    // (never happens in practice) hashes as "not recorded", the same way
    // the plan gate's own `None` does.
    let group_frames_by_id: HashMap<i64, &GroupFrame> =
        group.frames.iter().map(|f| (f.frame_id, f)).collect();
    let mut member_measurement_hashes: Vec<String> = Vec::with_capacity(members.len());
    for m in members.iter() {
        let hash = match group_frames_by_id.get(&m.frame_id).copied() {
            Some(gf) => {
                let calib_hash = {
                    let conn = db(&rc.ctx)?.conn();
                    rc.memo.calibration_hash_checked(&conn, &cfg, gf)?
                };
                group_measurement_hash(rc, &cfg, &group.key, &calib_hash)
            }
            None => String::new(),
        };
        member_measurement_hashes.push(hash);
    }
    let mut reference_measure_hashes: Vec<&str> = reference_members
        .iter()
        .map(|&i| member_measurement_hashes[i].as_str())
        .collect();
    reference_measure_hashes.sort_unstable();
    let combined_measurement_hash = reference_measure_hashes.join(",");

    // M1 (final fix wave, ruling C-30): the group β stage 3 resolved from
    // its members' own `Auto` picks — the value `stacking_run_groups.beta`
    // records and every LN hash below folds in, because stage 6 FITS at it
    // (see `normalization_hash_for`'s own doc). Resolved here, above the
    // first hash that needs it, and read again by `group_beta` below for
    // the fit itself; `None` (a group stage 3 never measured a frame for)
    // folds nothing, which is exactly what the plan gate does with a NULL
    // `stacking_run_groups.beta`.
    let group_beta_recorded: Option<f64> =
        rc.measured_groups.get(&group.key).and_then(|mg| mg.beta);

    // Fix round 1, item 2: the reference's OWN hash has no further
    // reference to fold in — `""` (see `normalization_hash_for`'s own doc).
    let reference_hash = normalization_hash_for(
        &cfg,
        &combined_registration_hash,
        &combined_measurement_hash,
        &reference_member_ids,
        "",
        None,
        group_beta_recorded,
        None,
    );

    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Normalize);
    let reference_path_buf = rc.layout.ln_reference_path(&group.key);
    std::fs::create_dir_all(rc.layout.ln_dir(&group.key)).map_err(|e| {
        RunError::Other(format!(
            "creating {}: {e}",
            rc.layout.ln_dir(&group.key).display()
        ))
    })?;

    let existing_reference_artifact = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(&conn, &rc.subject, &group.key, "ln_reference", None)?
    };
    let cached_reference_path: Option<PathBuf> = if force_fresh {
        None
    } else {
        existing_reference_artifact
            .as_ref()
            .filter(|row| is_fresh(row, &reference_hash))
            .and_then(|row| row.path.clone())
            .map(PathBuf::from)
    };

    // M6 (final fix wave): the path reported in the summary must be the
    // RESOLVED one — the artifact row's own stored path on a cache hit
    // (which can differ from `reference_path_buf` if the working folder
    // moved since that row was written, same reasoning Task 7's fix round
    // already applied to a per-frame sidecar's own path), the freshly
    // written `reference_path_buf` otherwise (`build_and_write_ln_reference`
    // always writes there). Defaults to `reference_path_buf` and is
    // overridden only by the cache-hit-success arm below.
    let mut resolved_reference_path: PathBuf = reference_path_buf.clone();
    // L-23: the reference-frames saturation masks, thresholded with this
    // run's `starSelection.saturation` (the float map stays on disk, so a
    // `saturation` change never rebuilds it). A cached reference without a
    // usable map beside it (missing, unreadable, another shape) is rebuilt
    // exactly like one whose own file cannot be read.
    let saturation = ln_cfg.star_selection.saturation;
    let cached: Option<(PathBuf, LnReference, Vec<SaturationMask>)> = match cached_reference_path {
        Some(path) => match read_reference(&path) {
            Ok(r) => {
                let map_path = path.with_file_name(LN_REFERENCE_SATURATION_FILE);
                match read_reference_saturation_masks(
                    &map_path,
                    r.width,
                    r.height,
                    r.planes.len(),
                    saturation,
                ) {
                    Ok(masks) => Some((path, r, masks)),
                    Err(e) => {
                        tracing::warn!(
                            run_id = rc.run_id,
                            group_key = %group.key,
                            path = %map_path.display(),
                            error = %format!("{e:#}"),
                            "the cached LN reference has no usable saturation map; rebuilding"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %e,
                    "failed to read the cached LN reference; rebuilding"
                );
                None
            }
        },
        None => None,
    };
    let (ln_reference, ref_frames_saturation): (LnReference, Vec<SaturationMask>) = match cached {
        Some((path, r, masks)) => {
            resolved_reference_path = path;
            (r, masks)
        }
        None => build_and_write_ln_reference(
            rc,
            group,
            input,
            &reference_members,
            n,
            io,
            &reference_hash,
            &reference_member_ids,
            scale_px,
            saturation,
        )?,
    };

    // Perf tier A Task 12: the group's own pool, so the tile measurement
    // fans out across workers instead of running one tile at a time on this
    // thread — this reference model is built once per group (a handful of
    // channels), not once per frame.
    let bg_pool = Some(&rc.ctx.image_pool);
    // A reference plane the model refuses makes no frame's sidecar
    // trustworthy, so this is a group-level ruling-R3-shaped fallback (warn
    // + global normalization), not a per-frame failure. Carry-over (a), Task
    // 5's re-review: `reference_path` is `None` here, same as the `n < 3`
    // fallback above — `Some` means "LN ran"; the reference FILE was still
    // written to disk at `reference_path_buf` (harmless, just not reported
    // as a usable one), but nothing below normalized against it, so there is
    // nothing to read back.
    let mut ref_models: Vec<BackgroundModel> = Vec::with_capacity(ln_reference.planes.len());
    for (p, plane) in ln_reference.planes.iter().enumerate() {
        let table = TileTable::measure(
            plane,
            ln_reference.width,
            ln_reference.height,
            &CLEANING,
            bg_pool,
        );
        match BackgroundModel::fit(table, scale_px) {
            Ok(m) => {
                tracing::debug!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    channel = p,
                    ln_samples = m.samples,
                    ln_samples_rejected = m.samples_rejected,
                    ln_cells_unsupported = m.cells_unsupported,
                    "ln reference background modelled"
                );
                ref_models.push(m);
            }
            Err(r) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    channel = p,
                    error = %r,
                    "local normalization off for this group: the LN reference's background model is refused"
                );
                rc.warnings.push(ln_fallback_warning(
                    &group.key,
                    !ln_drives_output,
                    &LnFallback::BackgroundRefused {
                        reason: &r.to_string(),
                    },
                ));
                return Ok(LnGroupOutcome {
                    reference_path: None,
                    excluded_frame_ids: Vec::new(),
                    sidecar_grids: HashMap::new(),
                    scale_px: None,
                });
            }
        }
    }

    // Perf tier C Task 2 (ruling C-1): whatever stage 3 resolved for this
    // group's members' own `Auto` picks — `None` for a group stage 3 never
    // measured a frame for (falls back to `Auto`'s own default, same as
    // `psf_signal::group_beta` on an empty slice). M1 (final fix wave)
    // resolved the `Option` further up, where the first LN hash needs it;
    // this is the same value, with that fallback applied.
    let group_beta = group_beta_recorded.unwrap_or(4.0);

    // Fix round 1, item 6: computed ONCE per group, not once per frame.
    // I2 (final fix wave): also hoists the reference-side star detection +
    // PSF fit + match tree (`prepared`), so `normalize_frame`'s
    // `relative_scale_against` call never re-detects/re-fits the
    // reference plane on every frame either.
    //
    // Perf tier C Task 2 (ruling C-1): `normalization.local.psfModel =
    // Auto` resolves to the GROUP's own β instead of a fresh per-reference
    // `Auto` search — Measure's persisted fits (which `normalize_frame`
    // now prefers over re-detecting) were fitted at the group β too, so
    // fitting the reference at anything else would reintroduce exactly the
    // β mismatch `psf_signal::fit_stars_with_beta`'s own doc warns against.
    // `Moffat4` (or any future explicit choice) is untouched. There is one
    // prepared beta and no other (fix round 2, ruling C-12 retired fix
    // round 1's per-member-beta preparation — the diagnostics round traced
    // the real bias to the fit itself, not a beta mismatch, and
    // `relative_scale_from_seeds` always fits at this one beta regardless
    // of what beta a seed source's own fits carry; fix round 4 removed the
    // unused machinery outright, the review's M2).
    let reference_psf = match ln_cfg.psf_model {
        psf_signal::PsfModel::Auto => psf_signal::PsfModel::Fixed(group_beta),
        other => other,
    };

    // Plan B B0 (ruling R-PB0-13): the reference's own detection and fit run under `ln_reference{group_key}` (and, per
    // plane, `ln_reference_plane{plane}`), so their `star_measure` events name the group and plane instead of
    // landing spanless between the Measure stage's lines and the first `ln_plane`.
    let reference_for_detection = {
        let _reference_span =
            tracing::debug_span!("ln_reference", group_key = %group.key).entered();
        if grid_mode {
            LnReferenceForDetection::build_masked(
                &ln_reference,
                reference_psf,
                measure_opts.max_stars,
                Some(&rc.ctx.image_pool),
                Some(ref_frames_saturation),
            )
        } else {
            LnReferenceForDetection::build(
                &ln_reference,
                reference_psf,
                measure_opts.max_stars,
                Some(&rc.ctx.image_pool),
                Some(ref_frames_saturation),
            )
        }
    };

    // Perf tier C Task 2 (spec §2.2.3): resolve each member's per-plane
    // `fits` artifact PATHS up front — a metadata-only DB lookup, the same
    // shape as the `ln` artifact check below — keyed on the SAME
    // measurement hash `stage_measure` wrote the artifact under. The FILE
    // CONTENTS are read later, inside the fan-out closure, so a member this
    // run reuses from a fresh `ln` cache hit (never reaches
    // `needs_normalize`) never pays for it. `is_fresh` (a hash match plus a
    // disk `stat`) is what makes a `.athf` deleted between Measure and
    // Normalize read back as `None` here — the same file-gone-missing
    // signal every other cached stage artifact uses — so a caller handed
    // `None` for a plane falls back to detection, never a hard error.
    //
    // M2 (final fix wave, ruling C-30): the `find_artifact` + `is_fresh`
    // pair below is DELIBERATELY per PLANE rather than a call to
    // `plan::fits_artifact_fresh_for_planes`. That helper answers one
    // all-or-nothing question about a whole frame, which is what the plan
    // gate and stage 5 need (`resolve_register_fits` reads plane 0 only,
    // and Measure's reuse decision is per frame). Normalize's fallback is
    // per PLANE — one unreadable channel falls back to detection on that
    // channel alone while its siblings keep their seeds — so collapsing
    // this to the frame-level helper would throw away the other planes'
    // usable artifacts. The PREDICATE is still the shared one: `is_fresh`
    // is exactly what `fits_artifact_fresh_for_planes` applies to each
    // plane in its own loop, so the two can never drift about what "fresh"
    // means, only about how many planes have to be.
    let mut fits_paths: Vec<Vec<Option<PathBuf>>> = Vec::with_capacity(members.len());
    for (i, m) in members.iter().enumerate() {
        let channels = m.measurement.channels.len();
        // C-17: the SAME value the LN hashes above folded in, resolved once.
        let expected_hash: Option<&str> =
            Some(member_measurement_hashes[i].as_str()).filter(|h| !h.is_empty());
        let mut paths: Vec<Option<PathBuf>> = Vec::with_capacity(channels);
        for plane in 0..channels {
            let resolved = match &expected_hash {
                Some(hash) => {
                    let kind = fits_artifact::artifact_kind(plane);
                    let row = {
                        let conn = db(&rc.ctx)?.conn();
                        crate::db::stacking::find_artifact(
                            &conn,
                            &rc.subject,
                            artifact_group_key(&kind, &group.key),
                            &kind,
                            Some(m.frame_id),
                        )?
                    };
                    row.filter(|r| is_fresh(r, hash))
                        .and_then(|r| r.path.clone())
                        .map(PathBuf::from)
                }
                None => None,
            };
            paths.push(resolved);
        }
        fits_paths.push(paths);
    }

    let mut sidecar_paths: Vec<PathBuf> = Vec::with_capacity(members.len());
    // Perf tier C Task 2 fix round 4: each member's existing `ln` row,
    // fetched ONCE — the calibration resolution below has to ask "is
    // anything stale?" before the freshness loop runs, and neither pass
    // should hit the DB a second time for the same row.
    let mut existing_ln_rows: Vec<Option<crate::db::stacking::StackingArtifactRow>> =
        Vec::with_capacity(members.len());
    let mut existing_global_rows: Vec<Option<crate::db::stacking::StackingArtifactRow>> =
        Vec::with_capacity(members.len());
    for m in members.iter() {
        let stem = match group_frames_by_id.get(&m.frame_id).copied() {
            Some(gf) => frame_file_stem(gf),
            // Same `<stem>_f<id>` rule as `frame_file_stem` (a member outside `group_frames_by_id` is not expected).
            None => format!(
                "{}_f{}",
                Path::new(&m.filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&m.filename),
                m.frame_id
            ),
        };
        sidecar_paths.push(rc.layout.ln_sidecar_path(&group.key, &stem));
        let existing = {
            let conn = db(&rc.ctx)?.conn();
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group.key,
                "ln",
                Some(m.frame_id),
            )?
        };
        existing_ln_rows.push(existing);
        // Plan 6b-3 D7: the member's global-normalization marker, fetched
        // beside its sidecar row.
        let marker = {
            let conn = db(&rc.ctx)?.conn();
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group.key,
                LN_GLOBAL_ARTIFACT_KIND,
                Some(m.frame_id),
            )?
        };
        existing_global_rows.push(marker);
    }

    // The group's seeds calibration (ruling C-14). It has to be resolved
    // BEFORE the per-member hashes, because `k` is part of every member's
    // own LN hash — a changed `k` must re-normalize the whole group rather
    // than leave it mixing frames written at two different factors (the
    // review's I3). The measurement itself is ≈ 45 s per group, so it runs
    // only when (a) nothing fresh is stored for exactly these inputs AND
    // (b) at least one member would be re-normalized anyway: a fully-cached
    // group pays nothing.
    let calibration_member_weights: Vec<(i64, f64)> = members
        .iter()
        .map(|m| (m.frame_id, m.weight.normalized_mean))
        .collect();
    let calibration_indices = ln::select_calibration_frames(
        rc.geometry_of(&group.key).reference_frame_id,
        &calibration_member_weights,
        ln::SEEDS_CALIBRATION_FRAMES,
    );
    let calibration_frames: Vec<(i64, &str, &str)> = calibration_indices
        .iter()
        .map(|&i| {
            (
                members[i].frame_id,
                member_ln_upstreams[i].as_str(),
                member_measurement_hashes[i].as_str(),
            )
        })
        .collect();
    let calibration_input_hash = seeds_calibration_hash_for(
        &cfg,
        &reference_hash,
        &calibration_frames,
        group_beta_recorded,
        scale_px,
    );
    let ln_channels = ln_reference.planes.len();
    let stored_calibration: Option<ln::SeedsCalibration> = if force_fresh {
        None
    } else {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(&conn, &rc.subject, &group.key, "ln_calibration", None)?
            .filter(|row| row.config_hash == calibration_input_hash)
            .and_then(|row| row.payload_json)
            .and_then(|s| serde_json::from_str::<ln::SeedsCalibration>(&s).ok())
            .filter(|c| c.k.len() == ln_channels)
    };

    let member_hashes = |calibration: Option<&ln::SeedsCalibration>| -> Vec<String> {
        members
            .iter()
            .enumerate()
            .map(|(i, _m)| {
                normalization_hash_for(
                    &cfg,
                    &member_ln_upstreams[i],
                    &member_measurement_hashes[i],
                    &reference_member_ids,
                    &reference_hash,
                    calibration,
                    group_beta_recorded,
                    Some(scale_px),
                )
            })
            .collect()
    };
    let mut seeds_calibration = stored_calibration.clone();
    let mut per_member_hash: Vec<String> = member_hashes(seeds_calibration.as_ref());
    let anything_stale = force_fresh
        || (0..members.len()).any(|i| {
            !(existing_ln_rows[i]
                .as_ref()
                .is_some_and(|row| is_fresh(row, &per_member_hash[i]))
                || existing_global_rows[i]
                    .as_ref()
                    .is_some_and(|row| row.config_hash == per_member_hash[i]))
        });
    // The Normalize row is ONE count: when the measurement runs, its frames
    // are the first `calibration_units` units of the stage total and the
    // per-frame fan-out below continues from there, so the bar never goes
    // back (pcores review r1, I3).
    let mut calibration_units = 0usize;
    if anything_stale && stored_calibration.is_none() {
        calibration_units = calibration_indices.len();
        seeds_calibration = measure_ln_seeds_calibration(
            rc,
            group,
            members.len(),
            &calibration_indices,
            &fits_paths,
            input.frames,
            &ln_reference,
            &reference_for_detection,
            &ref_models,
            &ln_cfg,
            scale_px,
            measure_opts,
            input.interpolation,
            input.clamping,
            if grid_mode {
                ln_working_set_bytes_masked(input.width as u64, input.height as u64)
            } else {
                ln_working_set_bytes(input.width as u64, input.height as u64)
            },
            &rc.ctx.image_pool,
            &rc.cancel,
        );
        // A cancelled measurement returns `None` silently (the review's
        // M3); the run's own cancel check is what stops the group.
        rc.check_cancel()?;
        if let Some(c) = &seeds_calibration {
            let payload = serde_json::to_string(c).map_err(|e| {
                RunError::Other(format!("failed to serialize ln_calibration payload: {e}"))
            })?;
            let conn = db(&rc.ctx)?.conn();
            upsert_artifact(
                &conn,
                &NewArtifact {
                    subject: &rc.subject,
                    frame_id: None,
                    group_key: &group.key,
                    kind: "ln_calibration",
                    path: None,
                    config_hash: &calibration_input_hash,
                    size: None,
                    modified_at: None,
                    payload_json: Some(&payload),
                },
            )?;
            per_member_hash = member_hashes(seeds_calibration.as_ref());
        }
    }
    if let Some(c) = &seeds_calibration {
        rc.group_seeds_calibration
            .insert(group.key.clone(), c.clone());
    }
    // Plan B B0 (PB-2, R-PB0-7): the reference's own `c` per plane, read off the prepared channels; a plane at floor (i)
    // stays `None` (`null` in the summary), not the 1.0 that was applied.
    rc.group_ln_reference_c.insert(
        group.key.clone(),
        reference_for_detection
            .prepared
            .iter()
            .map(|p| p.aperture_correction())
            .collect(),
    );

    let mut needs_normalize: Vec<usize> = Vec::new();
    // M2 Task 7 (fix round 1, item 3): every member this function itself
    // VERIFIES has a READABLE `ln` artifact THIS run (a fresh cache hit
    // whose file was re-opened and parsed, or a fan-out item that
    // normalized, wrote, and had its own new file read straight back) —
    // becomes the returned `LnGroupOutcome.sidecar_grids`, already parsed,
    // so `process_group_output` never has to read a file itself. A member
    // that fails or is never attempted is simply absent.
    let mut sidecar_grids: HashMap<i64, LnFrameGrids> = HashMap::with_capacity(members.len());
    // Plan 6b-3 D7: members that keep global normalization (cached or new).
    let mut low_overlap = 0usize;
    // Final review I-1: members excluded because the common aperture could
    // not measure enough stars (one group-level run warning below).
    let mut aperture_shortfalls = 0usize;

    for (i, m) in members.iter().enumerate() {
        // Fix round 1, item 2: the hash folds in the group's OWN reference
        // hash — re-registering or recalibrating ONE reference member
        // rebuilds the reference (a new `B_ref`, a new scale anchor), which
        // must invalidate every OTHER frame's sidecar too, not just that
        // one member's (the id LIST alone is unchanged by a member's own
        // re-registration, so it can't catch this on its own) — and, since
        // fix round 4, the group's own seeds calibration (ruling C-14 item
        // 2). Both were resolved above; this loop only reads them.
        let frame_hash = &per_member_hash[i];
        let existing = &existing_ln_rows[i];
        let fresh = !force_fresh
            && existing
                .as_ref()
                .is_some_and(|row| is_fresh(row, frame_hash));
        if fresh {
            let cached_payload: Option<LnArtifactPayload> = existing
                .as_ref()
                .and_then(|row| row.payload_json.as_deref())
                .and_then(|s| serde_json::from_str(s).ok());
            match cached_payload {
                Some(payload) => {
                    // Fix round 1, items 2 + 3: verify against the
                    // ARTIFACT ROW's OWN path — `is_fresh` already
                    // confirmed THAT path's size matches, not the
                    // recomputed `sidecar` above (the working folder may
                    // have moved since this artifact was written, leaving
                    // `sidecar` pointing at a location nothing has ever
                    // written to — reading it would wrongly find nothing
                    // even though the real, fresh file is still sitting at
                    // the row's own path). A read failure here means
                    // RE-NORMALIZE in this same run, never an "unreadable"
                    // exclusion for what a fresh write in this very run can
                    // fix.
                    let row_path = existing.as_ref().and_then(|row| row.path.as_deref());
                    match row_path.map(|p| LnFrameGrids::read(Path::new(p))) {
                        Some(Ok(grids)) => {
                            set_ln_summary(
                                rc,
                                &group.key,
                                m.frame_id,
                                Some(payload.scale),
                                true,
                                None,
                                None,
                                None,
                            );
                            sidecar_grids.insert(m.frame_id, grids);
                        }
                        Some(Err(e)) => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = m.frame_id,
                                error = %e,
                                "cached ln sidecar unreadable; re-normalizing"
                            );
                            needs_normalize.push(i);
                        }
                        None => {
                            // `is_fresh`'s own `.and_then` chain requires
                            // `row.path` to be `Some` before it can ever
                            // return `true` — unreachable in practice,
                            // handled the same as a read failure for
                            // safety.
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = m.frame_id,
                                "fresh ln artifact row has no stored path; re-normalizing"
                            );
                            needs_normalize.push(i);
                        }
                    }
                }
                None => {
                    // Fix round 1, item 9: a fresh-by-hash row whose
                    // payload doesn't parse is NOT a usable cache hit — it
                    // used to yield `cached_ln = true` with `ln_scale =
                    // None`, a summary that claims "reused" while reporting
                    // nothing reused. Re-normalize instead.
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = m.frame_id,
                        "ln artifact payload unreadable; treating as stale"
                    );
                    needs_normalize.push(i);
                }
            }
        } else if !force_fresh
            && existing_global_rows[i]
                .as_ref()
                .is_some_and(|row| row.config_hash == *frame_hash)
        {
            set_ln_summary(rc, &group.key, m.frame_id, None, true, None, None, None);
            low_overlap += 1;
        } else {
            needs_normalize.push(i);
        }
    }

    let total = members.len();
    // Members plus the seeds-calibration frames counted above: the units the
    // Normalize row climbs through, in order, without a reset.
    let stage_total = total + calibration_units;
    let stage_done = calibration_units + total - needs_normalize.len();
    rc.progress(
        Stage::Normalize,
        Some(group.key.clone()),
        stage_done,
        stage_total,
        0,
        0,
        None,
        None,
    );

    let working_set_bytes = if grid_mode {
        ln_working_set_bytes_masked(input.width as u64, input.height as u64)
    } else {
        ln_working_set_bytes(input.width as u64, input.height as u64)
    };
    let budget = fan_budget(rc);
    let admission_n = admission(
        working_set_bytes,
        rc.ctx.image_pool.current_num_threads(),
        budget.bytes,
    );
    log_admission(
        rc,
        Stage::Normalize,
        &group.key,
        working_set_bytes,
        0,
        admission_n,
        budget,
        "normalize",
    );
    let interpolation = input.interpolation;
    let clamping = input.clamping;
    let cancel_ref: &AtomicBool = &rc.cancel;
    let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    let stack_frames_ref: &[StackFrame] = input.frames;
    let ref_models_ref: &[BackgroundModel] = &ref_models;
    let ln_reference_ref: &LnReference = &ln_reference;
    let reference_for_detection_ref: &LnReferenceForDetection = &reference_for_detection;
    let sidecar_paths_ref: &[PathBuf] = &sidecar_paths;
    let fits_paths_ref: &[Vec<Option<PathBuf>>] = &fits_paths;

    // Ruling C-14 item 1: the group's per-CHANNEL factors, resolved (from
    // the cache or by measurement) above the freshness loop, since they are
    // part of every member's own LN hash. C-17: handed over whole, so the
    // apply site reads them through `SeedsCalibration::k_for`.
    let seeds_calibration_ref: Option<&ln::SeedsCalibration> = seeds_calibration.as_ref();

    // v0.6.3: per-frame ticks from inside the fan-out; the cached members
    // (`total - needs_normalize.len()`) count as done from the start.
    let ticker = FanOutTicker::new(
        rc,
        Stage::Normalize,
        Some(group.key.clone()),
        stage_done,
        stage_total,
        0,
        0,
    );
    let ticker_ref = &ticker;
    let member_ids: Vec<i64> = members.iter().map(|m| m.frame_id).collect();
    let member_ids_ref: &[i64] = &member_ids;
    let results = fan_out(
        needs_normalize.clone(),
        &AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n),
        FanOutLabel::new(rc, Stage::Normalize, Some(&group.key), "normalize"),
        cancel_ref,
        move |i: usize| {
            // Perf tier C Task 2: the fits FILE CONTENTS are read here,
            // inside the fan-out, not in the sequential loop above that
            // only resolved paths — a plane whose path is `None` (no fresh
            // artifact) or whose file fails to read (deleted, truncated,
            // wrong version — the same signal an artifact row that outlived
            // its file always produces) is handed to `normalize_frame` as
            // an empty list, which is its own "fall back to detection for
            // this plane" case, never a hard failure here. Fix round 1:
            // the read failure itself is logged at `debug!`, not `warn!` —
            // `normalize_frame`'s own ONE fallback `warn!` per frame is the
            // user-facing signal; a second WARN here for the same event
            // would make a frame warn twice.
            let planes: &[Option<PathBuf>] =
                fits_paths_ref.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
            let mut fits_by_plane: Vec<Vec<psf_signal::StarFit>> = Vec::with_capacity(planes.len());
            for path in planes {
                let plane_fits = match path {
                    Some(p) => fits_artifact::read_fits(p).unwrap_or_else(|e| {
                        tracing::debug!(
                            frame_id = member_ids_ref.get(i).copied().unwrap_or_default(),
                            path = %p.display(),
                            error = %e,
                            "ln: fits artifact unreadable; falling back to detection for this plane"
                        );
                        Vec::new()
                    }),
                    None => Vec::new(),
                };
                fits_by_plane.push(plane_fits);
            }
            let out = normalize_frame(
                ln_reference_ref,
                reference_for_detection_ref,
                ref_models_ref,
                &stack_frames_ref[i],
                LnScaleSeeds::Measured(&fits_by_plane),
                &ln_cfg,
                scale_px,
                measure_opts,
                interpolation,
                clamping,
                &sidecar_paths_ref[i],
                seeds_calibration_ref,
                Some(pool_ref),
                cancel_ref,
            );
            ticker_ref.tick(member_ids_ref.get(i).copied());
            out
        },
    );
    rc.check_cancel()?;

    let mut excluded_frame_ids: Vec<i64> = Vec::new();
    // Shared handling for a frame whose local normalization has genuinely
    // failed this run — `normalize_frame` itself failed, OR (fix round 1,
    // item 3) writing succeeded but reading the file straight back failed.
    // Excludes the frame (persisted, `rc.runtime_exclusions`) when LN
    // drives OUTPUT normalization; otherwise warns and leaves it on global
    // normalization (ruling R2). `rc`/`excluded_frame_ids` are explicit
    // parameters, not captures, so the closure can be called from either
    // match arm below without fighting the borrow checker over `rc`.
    let fail_ln_frame = |rc: &mut RunContext,
                         excluded_frame_ids: &mut Vec<i64>,
                         frame_id: i64,
                         msg: String|
     -> Result<(), RunError> {
        if ln_drives_output {
            rc.runtime_exclusions.push((frame_id, msg.clone()));
            excluded_frame_ids.push(frame_id);
            // Fix round 1, item 1: flip AND persist — `write_frame_rows`
            // already ran at the end of stage 5, so without this the
            // frame's `stacking_run_frames` row still says `included = 1`
            // and the frames table would render a frame the master does
            // not contain as stacked.
            //
            // Carry-over (b), Task 5's re-review: a DB error HERE happens
            // AFTER `exclude_frame_and_persist` already flipped
            // `rc.measured`'s in-memory state — the generic "Other -> warn,
            // continue with global normalization" treatment the caller
            // gives every other internal error would silently leave this
            // frame excluded in memory while the group proceeds as if the
            // exclusion never happened. `RunError::ExclusionPersistFailed`
            // forces `process_group_output` to fail the group outright
            // instead.
            if let Err(e) = exclude_frame_and_persist(rc, group, frame_id, msg.clone()) {
                let text = match e {
                    RunError::Cancelled => return Err(RunError::Cancelled),
                    RunError::Other(inner) => inner,
                    RunError::ExclusionPersistFailed(inner) => inner,
                };
                return Err(RunError::ExclusionPersistFailed(text));
            }
            tracing::warn!(
                run_id = rc.run_id,
                frame_id,
                reason = %msg,
                "frame excluded: local normalization"
            );
        } else {
            tracing::warn!(
                run_id = rc.run_id,
                frame_id,
                reason = %msg,
                "local normalization failed; frame keeps global normalization"
            );
            // Only reached when LN does not drive output: rejection-only.
            rc.warnings.push(ln_fallback_warning(
                &group.key,
                true,
                &LnFallback::FrameFailed {
                    frame_id,
                    msg: &msg,
                },
            ));
        }
        Ok(())
    };

    // Perf tier C Task 2: how many of this group's freshly-normalized
    // frames used Measure's persisted fits as seed positions rather than
    // falling back to detection — logged once per group below.
    let mut seeds_sourced_count = 0usize;
    for (pos, res) in results.into_iter().enumerate() {
        let member_idx = needs_normalize[pos];
        let frame_id = members[member_idx].frame_id;
        match res {
            None => return Err(RunError::Cancelled),
            Some(Ok(outcome)) => {
                let (size, modified_at) = file_identity(&outcome.sidecar)
                    .map_err(|e| RunError::Other(format!("{e:#}")))?;
                let payload = serde_json::to_string(&LnArtifactPayload {
                    scale: outcome.scale,
                    matches: outcome.matches,
                    samples: outcome.samples,
                    samples_rejected: outcome.samples_rejected,
                    samples_shared: outcome.samples_shared,
                    cells_unsupported: outcome.cells_unsupported,
                    ln_scale_source: outcome.ln_scale_source.to_string(),
                    seeds_calibration: seeds_calibration.clone(),
                })
                .map_err(|e| RunError::Other(format!("failed to serialize ln payload: {e}")))?;
                {
                    let conn = db(&rc.ctx)?.conn();
                    upsert_artifact(
                        &conn,
                        &NewArtifact {
                            subject: &rc.subject,
                            frame_id: Some(frame_id),
                            group_key: &group.key,
                            kind: "ln",
                            path: Some(&outcome.sidecar.to_string_lossy()),
                            config_hash: &per_member_hash[member_idx],
                            size: Some(size),
                            modified_at: Some(&modified_at),
                            payload_json: Some(&payload),
                        },
                    )?;
                }
                // Fix round 1, item 3: read the just-written sidecar back
                // HERE — a hand-off, not a second read: the parsed grids
                // become this frame's `LnGroupOutcome.sidecar_grids` entry
                // directly, so `process_group_output` never re-opens the
                // file. On the rare chance this immediate read-back fails
                // (the write itself already succeeded), the frame is
                // treated exactly like any other normalization failure —
                // excluded (LN drives output) or warned (rejection only) —
                // via the SAME `fail_ln_frame` path, its error text folded
                // into the reason (item 5: the reason must say why).
                match LnFrameGrids::read(&outcome.sidecar) {
                    Ok(grids) => {
                        tracing::debug!(
                            run_id = rc.run_id,
                            frame_id,
                            ln_scale = outcome.scale,
                            ln_matches = outcome.matches,
                            ln_samples = outcome.samples,
                            ln_samples_rejected = outcome.samples_rejected,
                            ln_samples_shared = outcome.samples_shared,
                            ln_cells_unsupported = outcome.cells_unsupported,
                            warp_ms = outcome.warp_ms,
                            background_ms = outcome.background_ms,
                            scale_ms = outcome.scale_ms,
                            write_ms = outcome.write_ms,
                            ln_detect_ms = outcome.detect_ms,
                            ln_fit_ms = outcome.fit_ms,
                            ln_match_ms = outcome.match_ms,
                            ln_scale_source = outcome.ln_scale_source,
                            "ln frame normalized"
                        );
                        if outcome.ln_scale_source == "seeds" {
                            seeds_sourced_count += 1;
                        }
                        set_ln_summary(
                            rc,
                            &group.key,
                            frame_id,
                            Some(outcome.scale),
                            false,
                            outcome.growth_factor,
                            outcome.growth_pairs,
                            outcome.target_aperture_correction,
                        );
                        sidecar_grids.insert(frame_id, grids);
                    }
                    Err(e) => {
                        fail_ln_frame(
                            rc,
                            &mut excluded_frame_ids,
                            frame_id,
                            format!(
                                "local normalization: writing .athln sidecar: reading it back failed: {e:#}"
                            ),
                        )?;
                    }
                }
            }
            Some(Err(LnError::LowOverlap { fraction })) => {
                let payload = serde_json::json!({ "overlap": fraction }).to_string();
                {
                    let conn = db(&rc.ctx)?.conn();
                    upsert_artifact(
                        &conn,
                        &NewArtifact {
                            subject: &rc.subject,
                            frame_id: Some(frame_id),
                            group_key: &group.key,
                            kind: LN_GLOBAL_ARTIFACT_KIND,
                            path: None,
                            config_hash: &per_member_hash[member_idx],
                            size: None,
                            modified_at: None,
                            payload_json: Some(&payload),
                        },
                    )?;
                }
                tracing::debug!(
                    run_id = rc.run_id,
                    frame_id,
                    overlap = fraction,
                    "ln: frame keeps global normalization (little overlap with the LN reference)"
                );
                set_ln_summary(rc, &group.key, frame_id, None, false, None, None, None);
                low_overlap += 1;
            }
            Some(Err(e)) => {
                if ln_drives_output && matches!(e, LnError::CommonApertureShortfall { .. }) {
                    aperture_shortfalls += 1;
                }
                fail_ln_frame(rc, &mut excluded_frame_ids, frame_id, e.to_string())?;
            }
        }
    }

    if aperture_shortfalls > 0 {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = aperture_shortfalls,
            "local normalization excluded frames the common aperture could not measure"
        );
        rc.warnings.push(format!(
            "group {}: {aperture_shortfalls} frame(s) were left out of local normalization because the common aperture could not measure enough stars on them; Star flux: PSF signal may keep them",
            group.key
        ));
    }

    if low_overlap > 0 {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = low_overlap,
            "local normalization kept frames with little overlap on global normalization"
        );
        rc.warnings.push(ln_fallback_warning(
            &group.key,
            !ln_drives_output,
            &LnFallback::LittleOverlap {
                count: low_overlap,
                percent: ln::LN_MIN_OVERLAP * 100.0,
            },
        ));
    }

    if !needs_normalize.is_empty() {
        tracing::info!(
            run_id = rc.run_id,
            group_key = %group.key,
            stage = "normalize",
            count = seeds_sourced_count,
            "ln: frames normalized from measured seeds"
        );
    }

    rc.progress(
        Stage::Normalize,
        Some(group.key.clone()),
        stage_total,
        stage_total,
        0,
        0,
        None,
        None,
    );

    Ok(LnGroupOutcome {
        reference_path: Some(resolved_reference_path.display().to_string()),
        excluded_frame_ids,
        sidecar_grids,
        scale_px: Some(scale_px),
    })
}

/// Perf tier C Task 2 fix round 4 (ruling C-14): the group's per-CHANNEL
/// seeds calibration `k`, measured once per group. `ln::calibration` owns
/// the measurement itself (the frame selection, both `normalize_frame`
/// arms, the per-channel median and its band guard); this wrapper is the
/// run-level half — reading each calibration frame's persisted fits,
/// emitting a stage event while the measurement runs (M1: it is ≈ 45 s of
/// work that used to show as a stalled Normalize row), and turning the
/// per-channel verdicts into the group's log lines.
///
/// `indices` points into `frames` (index-aligned with the group's own
/// member list) and comes from `ln::select_calibration_frames`, so the
/// caller can hash the sample BEFORE deciding whether the measurement is
/// needed at all.
/// The stage events it emits are the FIRST `indices.len()` units of the
/// Normalize row's count, whose total is `members + indices.len()`
/// (`members_total` is the group's member count): the per-frame fan-out
/// continues from where this stops, so the row never goes backwards.
/// `None` means no calibration was measured — a cancel (silent, the
/// review's M3) or a scratch directory that could not be created; the
/// caller then records nothing and the group runs uncalibrated.
#[allow(clippy::too_many_arguments)]
fn measure_ln_seeds_calibration(
    rc: &RunContext,
    group: &IntegrationGroup,
    members_total: usize,
    indices: &[usize],
    fits_paths: &[Vec<Option<PathBuf>>],
    stack_frames: &[StackFrame],
    ln_reference: &LnReference,
    reference_for_detection: &LnReferenceForDetection<'_>,
    ref_models: &[BackgroundModel],
    ln_cfg: &crate::stacking::integrate::LocalNormalizationConfig,
    scale_px: u32,
    measure_opts: &MeasureOptions,
    interpolation: Interpolation,
    clamping: f32,
    working_set_bytes: u64,
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
) -> Option<ln::SeedsCalibration> {
    if indices.is_empty() {
        return None;
    }

    let load_fits = |i: usize| -> Vec<Vec<psf_signal::StarFit>> {
        let planes: &[Option<PathBuf>] = fits_paths.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        planes
            .iter()
            .map(|path| match path {
                Some(p) => fits_artifact::read_fits(p).unwrap_or_else(|e| {
                    tracing::warn!(
                        path = %p.display(),
                        error = %e,
                        "ln calibration: fits artifact unreadable; this plane contributes no stars"
                    );
                    Vec::new()
                }),
                None => Vec::new(),
            })
            .collect()
    };

    // M1: the measurement is minutes of work on a real group, so it emits
    // its own ticks — the same direct `emit_event` shape
    // `build_and_write_ln_reference` uses, since `rc` is borrowed
    // immutably here and `RunContext::progress` needs `&mut self`.
    let emitter = rc.emitter.clone();
    let (run_id, group_key) = (rc.run_id, group.key.clone());
    let (set_id, project_id) = wire_subject(&rc.subject);
    // C-17: the row counts the CALIBRATION's own frames while it runs, so a
    // minute of real work never reads as a stalled Normalize row. They are
    // the first `indices.len()` units of the stage's one count (members plus
    // these), and the per-frame fan-out continues from there: the percent of
    // the row only ever climbs (pcores review r1, I3).
    let stage_total = members_total + indices.len();
    // Workers finish in any order: the guard is held across the emit (the
    // `FanOutTicker` lesson) so a late worker cannot deliver a smaller count
    // after a larger one.
    let last_current = Mutex::new(0usize);
    let on_progress = |done: usize, _total: usize, frame_id: i64| {
        let current = done + 1;
        let mut last = last_current.lock().unwrap_or_else(|e| e.into_inner());
        if current <= *last {
            return;
        }
        *last = current;
        let percent = if stage_total == 0 {
            100.0
        } else {
            100.0 * current as f64 / stage_total as f64
        };
        emit_event(
            emitter.as_ref(),
            STACKING_PROGRESS_EVENT,
            &StackingProgressEvent {
                run_id,
                set_id,
                project_id: project_id.clone(),
                stage: Stage::Normalize,
                group_key: Some(group_key.clone()),
                current,
                total: stage_total,
                percent,
                bytes_done: 0,
                bytes_total: 0,
                frame_id: Some(frame_id),
                message: Some(format!(
                    "calibrating the seeds path · frame {current}/{}",
                    indices.len()
                )),
            },
        );
    };

    // The 2 x N arms admit like the main Normalize fan-out (same working set,
    // same budget); that fan-out's own `fan-out admitted` line is the one
    // lab parsers read, so none is emitted here. Results land by job index.
    let admission_n = admission(
        working_set_bytes,
        pool.current_num_threads(),
        fan_budget(rc).bytes,
    );
    let run_jobs = |n: usize,
                    job: &(dyn Fn(usize) -> Option<ln::LnFrameOutcome> + Sync)|
     -> Vec<Option<ln::LnFrameOutcome>> {
        let gate = AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n);
        fan_out(
            (0..n).collect::<Vec<usize>>(),
            &gate,
            FanOutLabel::new(rc, Stage::Normalize, Some(&group.key), "seeds"),
            cancel,
            |j| Ok::<_, std::convert::Infallible>(job(j)),
        )
        .into_iter()
        .map(|r| r.and_then(|r| r.ok()).flatten())
        .collect()
    };

    let measurement = ln::measure_seeds_calibration(
        ln_reference,
        reference_for_detection,
        ref_models,
        stack_frames,
        indices,
        &load_fits,
        ln_cfg,
        scale_px,
        measure_opts,
        interpolation,
        clamping,
        &rc.layout.ln_calibration_scratch_dir(&group.key),
        Some(pool),
        cancel,
        &on_progress,
        &run_jobs,
    )?;

    // One warn for the whole group when any channel ran short of ratios
    // (ruling C-14 item 1), one per channel for a refused median, and one
    // info per channel that was actually calibrated.
    let short = measurement
        .channels
        .iter()
        .filter(|c| c.outcome == ln::ChannelOutcome::TooFewRatios)
        .map(|c| c.samples)
        .min();
    if let Some(count) = short {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count,
            "ln seeds calibration: too few usable calibration frames; the seeds path runs uncalibrated for this group"
        );
    }
    for (channel, c) in measurement.channels.iter().enumerate() {
        match c.outcome {
            ln::ChannelOutcome::Calibrated => tracing::info!(
                run_id = rc.run_id,
                group_key = %group.key,
                stage = "normalize",
                channel,
                count = c.samples,
                ln_scale = c.k,
                "ln seeds calibration"
            ),
            ln::ChannelOutcome::OutOfBand { measured } => tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                stage = "normalize",
                channel,
                count = c.samples,
                ln_scale = measured,
                "ln seeds calibration: the measured factor left its band; this channel runs uncalibrated"
            ),
            ln::ChannelOutcome::TooFewRatios => {}
        }
    }
    // One checkpoint per group, after every channel's line (final review A, M2).
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "normalize",
        &group.key,
        "ln seeds calibration",
    );

    Some(measurement.calibration())
}

/// The cache-miss half of [`run_group_normalization`]'s reference
/// resolution: builds the group's LN reference from `input` (best `n`
/// included members by weight, [`build_ln_reference`]), writes it to
/// `ln/<group>/reference.fits` and upserts the `ln_reference` artifact row.
/// `reference_hash` is the caller's already-computed config hash — this
/// function only persists it, never recomputes it.
///
/// L-23: right after the reference it builds the reference-frames
/// saturation map over the SAME members (`frames_used`, read through the
/// same source `integrate_planes` used), writes it beside the reference
/// (`reference-saturation.fits`, part of the `ln_reference` artifact),
/// thresholds it with `saturation` and drops the float map: the returned
/// masks are what the LN pass keeps. Built here rather than inside
/// `build_reference`, which stays the plain integration its tests pin.
#[allow(clippy::too_many_arguments)]
fn build_and_write_ln_reference(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    input: &GroupInput<'_>,
    // Ruling R-M3-17 v2: the sky-penalized top-`n` slice
    // `run_group_normalization` already computed — `build_ln_reference`
    // (`ln::reference::build_reference`) below is UNCHANGED and still
    // ranks whatever it is handed BY WEIGHT internally, but since this is
    // already exactly `n` candidates, its `take = n.min(len)` uses all of
    // them: the SET is the caller's sky-penalized choice, only the internal
    // `frames_used` order (irrelevant to equal-weighted integration) is
    // re-derived by weight.
    included: &[usize],
    n: usize,
    io: IoPolicy,
    reference_hash: &str,
    reference_member_ids: &[i64],
    scale_px: u32,
    saturation: f64,
) -> Result<(LnReference, Vec<SaturationMask>), RunError> {
    let build_start = Instant::now();
    let empty_as_nan = rc.grid.is_some();
    // v0.6.3: the reference build is an integration of the group's best
    // `n` members — a minute or more on a real set that used to show as a
    // Normalize row sitting at 0 of N. Plane-level ticks with a message
    // are what `build_reference` offers; enough to say what is happening.
    let build_emitter = rc.emitter.clone();
    let (build_run_id, build_group_key) = (rc.run_id, group.key.clone());
    let (build_set_id, build_project_id) = wire_subject(&rc.subject);
    // The stage's own total (every included member of the group), not the
    // top-`n` slice. When the seeds calibration runs in this call, the ticks
    // that follow climb to members + K instead (one total for the calibration
    // and the fan-out); these build ticks stay at 0 %, so the row's percentage
    // never drops, only its denominator grows once.
    let build_total = input.frames.len();
    let build_n = n.min(included.len());
    let on_build_progress = |plane: usize, planes: usize| {
        emit_event(
            build_emitter.as_ref(),
            STACKING_PROGRESS_EVENT,
            &StackingProgressEvent {
                run_id: build_run_id,
                set_id: build_set_id,
                project_id: build_project_id.clone(),
                stage: Stage::Normalize,
                group_key: Some(build_group_key.clone()),
                current: 0,
                total: build_total,
                percent: 0.0,
                bytes_done: 0,
                bytes_total: 0,
                frame_id: None,
                message: Some(format!(
                    "building the LN reference from {build_n} frames · plane {}/{planes}",
                    plane + 1
                )),
            },
        );
    };
    let cancel: &AtomicBool = &rc.cancel;
    let pool: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;

    let built = build_ln_reference(
        input,
        included,
        n,
        pool,
        cancel,
        io,
        &on_build_progress,
        empty_as_nan,
    )
    .map_err(|e| {
        if matches!(e, IntegrationError::Cancelled) {
            RunError::Cancelled
        } else {
            RunError::Other(format!("LN reference build failed: {e}"))
        }
    })?;

    let reference_path = rc.layout.ln_reference_path(&group.key);
    let cards = vec![
        Card::new("ATH_STKI", CardValue::Str(rc.run_id.to_string()))
            .map_err(|e| RunError::Other(format!("building LN reference cards: {e}")))?,
        Card::new("ATH_STKG", CardValue::Str(group.key.clone()))
            .map_err(|e| RunError::Other(format!("building LN reference cards: {e}")))?,
    ];
    write_reference(&built, &reference_path, &cards)
        .map_err(|e| RunError::Other(format!("writing LN reference: {e:#}")))?;

    // L-23: the reference-frames saturation map, one frame and one channel
    // at a time, written beside the reference before the artifact row
    // claims the pair. One tick first, so the Normalize row says what it is
    // doing instead of keeping the reference's last plane message.
    emit_event(
        build_emitter.as_ref(),
        STACKING_PROGRESS_EVENT,
        &StackingProgressEvent {
            run_id: build_run_id,
            set_id: build_set_id,
            project_id: build_project_id.clone(),
            stage: Stage::Normalize,
            group_key: Some(build_group_key.clone()),
            current: 0,
            total: build_total,
            percent: 0.0,
            bytes_done: 0,
            bytes_total: 0,
            frame_id: None,
            message: Some(format!(
                "building the LN reference saturation map from {} frames",
                built.frames_used.len()
            )),
        },
    );
    let map_start = Instant::now();
    let mut maps =
        build_reference_saturation(input, &built.frames_used, pool, cancel).map_err(|e| {
            if matches!(e, IntegrationError::Cancelled) {
                RunError::Cancelled
            } else {
                tracing::error!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %e,
                    "ln reference saturation map failed"
                );
                RunError::Other(format!("LN reference saturation map failed: {e}"))
            }
        })?;
    let map_path = rc.layout.ln_reference_saturation_path(&group.key);
    let masks = write_reference_saturation(
        &mut maps,
        built.width,
        built.height,
        &map_path,
        &cards,
        saturation,
    )
    .map_err(|e| {
        tracing::error!(
            run_id = rc.run_id,
            group_key = %group.key,
            path = %map_path.display(),
            error = %format!("{e:#}"),
            "writing the ln reference saturation map failed"
        );
        RunError::Other(format!("writing the LN reference saturation map: {e:#}"))
    })?;
    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_reference_frames = built.frames_used.len(),
        duration_ms = map_start.elapsed().as_millis() as u64,
        "ln reference saturation map built"
    );

    let (size, modified_at) =
        file_identity(&reference_path).map_err(|e| RunError::Other(format!("{e:#}")))?;
    // Fix round 1, item 5: `build_plan` verifies every `ln` row's freshness
    // WITHOUT the stage-3 weight ranking that picked this member list — it
    // can only do that by trusting a STORED copy of exactly the two inputs
    // `normalization_hash_for` needs beyond `cfg` and each frame's own
    // registration hash. `reference_hash` here duplicates the artifact
    // row's own `config_hash` column (kept in the payload too so `build_plan`
    // needs only this one parse, never a second read of that column).
    let reference_payload = serde_json::to_string(&LnReferencePayload {
        reference_member_ids: reference_member_ids.to_vec(),
        reference_hash: reference_hash.to_string(),
        width: built.width,
        height: built.height,
        scale_px,
    })
    .map_err(|e| RunError::Other(format!("failed to serialize ln_reference payload: {e}")))?;
    {
        let conn = db(&rc.ctx)?.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: None,
                group_key: &group.key,
                kind: "ln_reference",
                path: Some(&reference_path.to_string_lossy()),
                config_hash: reference_hash,
                size: Some(size),
                modified_at: Some(&modified_at),
                payload_json: Some(&reference_payload),
            },
        )?;
    }

    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_reference_frames = built.frames_used.len(),
        ln_scale_px = scale_px,
        duration_ms = build_start.elapsed().as_millis() as u64,
        "ln reference built"
    );
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "normalize",
        &group.key,
        "ln reference built",
    );

    Ok((built, masks))
}

/// Stages 6 (normalize), 7 (integrate) and 9 (output). Per group, in
/// `rc.plan_groups` (plan) order: [`process_group_output`] recomputes
/// viability from `rc.measured`'s FINAL state (stage 5 can have narrowed a
/// group below 3 included since stage 3's own check), integrates through
/// Plan 4's `integrate_group`, and writes the master + optional rejection
/// maps. A cancel (from `IntegrationError::Cancelled` or noticed between
/// groups) aborts the whole run — ruling 12: a cancelled run keeps its
/// artifacts and writes no master. Any other integration/write failure fails
/// only that group; a run in which NO group ever wrote a master ends the
/// whole run `failed` too — there is nothing to hand back either way. Once
/// every group has been attempted and at least one wrote a master, the
/// configured cleanup policy runs exactly once.
///
/// Plan 6b-3 D10 (fix round 1): when the stage ends in an error or a cancel
/// with groups still waiting in a crop run's scratch folder, they are
/// finalized uncropped ([`finalize_unfinished_crop`]) so no master is left
/// only in scratch.
fn stage_output(rc: &mut RunContext) -> Result<(), RunError> {
    let result = stage_output_groups(rc);
    if result.is_err() {
        finalize_unfinished_crop(rc);
    }
    result
}

fn stage_output_groups(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let measure_opts = cfg
        .measurement
        .measure_options(cfg.normalization.scale_estimator);

    // M4b ruling R-M4b-8: a master carries ITS OWN reference's WCS — the
    // run-wide reference's in co-registered mode (where every group names
    // the same frame, so this resolves once and the missing-solve warning
    // is emitted once, exactly as before), the group's own in native mode.
    // Keyed by frame id so the lookup AND the warning happen once per
    // distinct reference, never once per group.
    let mut solves: HashMap<i64, Option<PlateSolveRecord>> = HashMap::new();
    let mut warned_missing_wcs: HashSet<i64> = HashSet::new();

    // Fix round 1, Minor M2: the `dropShrink` clamp (ruling R-M3-10) runs
    // ONCE here, for the whole run, instead of once per group — `drizzle.
    // dropShrink` is run-wide config, so a multi-group run under an
    // out-of-range value used to put the identical warning into
    // `summary.warnings` once per group. Mutating `rc.config.drizzle.
    // drop_shrink` in place means every group's later `rc.config.drizzle.
    // clone()` (inside `process_group_output`) already sees the clamped
    // value — nothing there needs to clamp (or warn) again.
    if cfg.drizzle.enabled {
        let configured = rc.config.drizzle.drop_shrink;
        if !(0.5..=1.0).contains(&configured) {
            let clamped = configured.clamp(0.5, 1.0);
            tracing::warn!(
                run_id = rc.run_id,
                drop_shrink = configured,
                clamped,
                "drizzle drop shrink out of [0.5, 1.0]; clamped"
            );
            rc.warnings.push(format!(
                "drizzle drop shrink {configured} out of [0.5, 1.0]; clamped to {clamped}"
            ));
            rc.config.drizzle.drop_shrink = clamped;
            // B10 (M3 final fix wave, M7): `rc.summary.config` was captured
            // in `start_stacking`, BEFORE this clamp ever runs — without
            // this, the persisted `RunSummary.config` (`summary_json` /
            // `runs/run-<id>.json`) would keep naming the PRE-clamp value
            // forever, disagreeing with the value the run actually used
            // (and with the drizzled master's own `ATH_DRZP` card, which is
            // built from `rc.config.drizzle.drop_shrink` after this point).
            rc.summary.config.drizzle.drop_shrink = clamped;
        }
    }

    let groups = rc.plan_groups.clone();
    let mut any_master = false;
    let mut normalize_total = Duration::ZERO;
    let mut integrate_total = Duration::ZERO;
    let mut drizzle_total = Duration::ZERO;
    let mut output_total = Duration::ZERO;
    let solved_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // Same expression `process_group_output` uses per group to decide
    // whether its own LN pass ran at all — one run has one `normalization`
    // config, so this is the same answer for every group.
    let ln_active = cfg.normalization.local.enabled
        || cfg.normalization.rejection == RejectionNormalization::Local;

    for group in &groups {
        rc.check_cancel()?;
        let reference_frame_id = rc.geometry_of(&group.key).reference_frame_id;
        // Fix round 1 (m2): a group `process_group_output` is about to skip
        // never writes a master, so neither the solve lookup nor the
        // missing-WCS warning belongs to it. `included_count_of` is the
        // same count that function's `included_count < 3` early return
        // reads, but it is read BEFORE the Output boundary's landed-file
        // check (`exclude_changed_members`) can lower it, so a project group
        // may still skip after passing this gate (the solve lookup is then
        // merely wasted, never wrong).
        let wcs = if included_count_of(rc, &group.key) >= 3 {
            match &rc.grid {
                // Spec §5.9, plan 6b-3 D11: every master of a project run
                // carries the grid's TAN.
                Some(grid) => Some(crate::stacking::grid_map::grid_plate_solve(
                    &grid.grid, &solved_at,
                )),
                None => {
                    if let std::collections::hash_map::Entry::Vacant(slot) =
                        solves.entry(reference_frame_id)
                    {
                        let conn = db(&rc.ctx)?.conn();
                        let origin = origin_of(&rc.origins, reference_frame_id)?;
                        slot.insert(stored_solve(&conn, origin, reference_frame_id)?);
                    }
                    solves
                        .get(&reference_frame_id)
                        .and_then(|s| s.as_ref())
                        .cloned()
                }
            }
        } else {
            None
        };
        let (outcome, n, i, d, o) = process_group_output(rc, group, &measure_opts, wcs.as_ref())?;
        normalize_total += n;
        integrate_total += i;
        drizzle_total += d;
        output_total += o;
        #[cfg(test)]
        if rc.subject.project_id().is_some()
            && CANCEL_AFTER_NEXT_GROUP.swap(false, Ordering::SeqCst)
        {
            rc.cancel.store(true, Ordering::SeqCst);
        }
        if matches!(outcome, GroupOutcome::Written) {
            any_master = true;
            // … and the warning belongs to a master that was ACTUALLY
            // written without a WCS — once per distinct reference frame, so
            // co-registered mode (where every group names the same one)
            // still warns exactly once, as it always has.
            if wcs.is_none() && warned_missing_wcs.insert(reference_frame_id) {
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id = reference_frame_id,
                    "no plate solve on the reference frame; the master has no WCS"
                );
                rc.warnings.push(
                    "no plate solve on the reference frame; the master has no WCS".to_string(),
                );
            }
        }
    }

    // Plan 6b-3 D10: the crop pass, once every group has written.
    if cfg.output.crop_to_coverage > 0 {
        let crop_start = Instant::now();
        finish_crop(rc)?;
        output_total += crop_start.elapsed();
    }

    // Ruling R5 (reverses fix round 1, item 6): pushed BEFORE `Integrate`
    // (stage order) and only when this run's own config had LN active — LN
    // disabled keeps the pre-M2 shape (no entry at all, not a zero one).
    if ln_active {
        rc.timings.push(crate::stacking::provenance::StageTiming {
            stage: Stage::Normalize,
            duration_ms: normalize_total.as_millis() as u64,
        });
    }
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Integrate,
        duration_ms: integrate_total.as_millis() as u64,
    });
    // M3 Task 5 (ruling R-M3-11, fix round 1 Minor M6): pushed BEFORE
    // `Output`, only when this run's own config had drizzle enabled AND at
    // least one group actually ATTEMPTED it (`rc.drizzle_attempted`, an
    // exact per-group counter incremented inside `process_group_output`
    // itself — not the `any_master` proxy this used to read, which would
    // still push a `0 ms` timing if every group's own drizzle attempt was
    // refused before `drizzle_group` ever ran, e.g. every `RejBitmapSet::
    // create` failing). Drizzle off, or a run where no group ever attempted
    // it, keeps the pre-M3 shape: no entry at all, not a zero one.
    if cfg.drizzle.enabled && rc.drizzle_attempted > 0 {
        rc.timings.push(crate::stacking::provenance::StageTiming {
            stage: Stage::Drizzle,
            duration_ms: drizzle_total.as_millis() as u64,
        });
    }
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Output,
        duration_ms: output_total.as_millis() as u64,
    });

    if !any_master {
        return Err(RunError::Other("no group produced a master".to_string()));
    }

    match cfg.output.cleanup {
        CleanupPolicy::KeepAll => {}
        CleanupPolicy::DeleteRegistered => {
            let freed = {
                let conn = db(&rc.ctx)?.conn();
                cleanup_work(&conn, &rc.subject, &rc.layout, CleanupWhat::Registered)?
            };
            tracing::info!(run_id = rc.run_id, freed_bytes = freed, "cleanup applied");
        }
        CleanupPolicy::DeleteIntermediates => {
            let freed = {
                let conn = db(&rc.ctx)?.conn();
                let sweep = !other_stack_active(&rc.ctx, &rc.subject);
                cleanup_work_with(
                    &conn,
                    &rc.subject,
                    &rc.layout,
                    CleanupWhat::Intermediates,
                    sweep,
                )?
            };
            tracing::info!(run_id = rc.run_id, freed_bytes = freed, "cleanup applied");
        }
    }

    Ok(())
}

/// Plan 6b-3 D10 (spec §5.9): the crop pass, after the last group of a run
/// with `cropToCoverage = K > 0`. Co-registered: ONE rectangle, the union of
/// the groups' `coverage >= K` boxes (every group shares one pixel grid: the
/// run reference's, or the project's canonical grid), so the run's masters
/// stay pixel-aligned. Native: each group's own box. Each group's outputs
/// are re-read from the scratch folder, cropped and written in the
/// configured format into the output folder, and its rows, group paths and
/// summary paths move onto them. A group with no rectangle, or whose crop
/// fails, is finalized uncropped with a warning (said once the uncropped
/// write has succeeded); a group that cannot be finalized at all keeps its
/// scratch files, which its rows already name — a master is never lost. A
/// drizzle family that cannot be cropped never costs the master (ruling
/// R-M3-7): the master family is finalized and the drizzle family stays in
/// the scratch folder, named in a warning. The scratch folder goes when every
/// output is final; otherwise it stays, and no cleanup policy removes it
/// (`paths::cleanup_work`): it is the user's master data, and only a manual
/// action does. The pass deliberately ignores a cancel — stopping between
/// groups would strand the rest in the scratch folder — so a large drizzle
/// family adds cancel latency; one Output-stage tick per group reports the
/// pass (final review B2) and a cancel that arrived during it is logged once.
/// Database errors fail the run, as everywhere in
/// Output, but only after every group has been tried: a group whose rows
/// could not be updated counts as kept, so the scratch folder (which its rows
/// may still name) survives, and a warning names it.
fn finish_crop(rc: &mut RunContext) -> Result<(), RunError> {
    let items = std::mem::take(&mut rc.crop_items);
    if items.is_empty() {
        return Ok(());
    }
    let k = rc.config.output.crop_to_coverage;
    let scratch = rc.layout.uncropped_dir(rc.run_id);
    let run_rect = match rc.config.registration.geometry {
        RegistrationGeometry::CoRegistered => {
            Some(union_rects(items.iter().filter_map(|i| i.rect)))
        }
        RegistrationGeometry::Native => None,
    };
    let mut kept = 0usize;
    let mut record_error: Option<RunError> = None;
    let crop_total = items.len();
    for (crop_done, item) in items.into_iter().enumerate() {
        rc.crop_progress(&item.group_key, crop_done, crop_total);
        let full = CropRect::full(item.width, item.height);
        let wanted = run_rect.unwrap_or(item.rect);
        let (target, note) = match wanted {
            Some(r) if r.fits_within(item.width, item.height) => (r, None),
            Some(r) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    crop_x0 = r.x0,
                    crop_y0 = r.y0,
                    crop_width = r.width,
                    crop_height = r.height,
                    "crop rectangle outside the group's master; outputs written uncropped"
                );
                let note = format!(
                    "group {}: the run's crop rectangle does not fit its master; \
                     its outputs are written uncropped",
                    item.group_key
                );
                (full, Some(note))
            }
            None => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    count = k,
                    "no pixel reached the crop coverage; outputs written uncropped"
                );
                let note = format!(
                    "group {}: no pixel is covered by {k} frames; \
                     its outputs are written uncropped",
                    item.group_key
                );
                (full, Some(note))
            }
        };
        let result = match write_crop_item(rc, &item, target) {
            Ok(o) => Ok((o, note)),
            Err(e) if target != full => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    error = %format!("{e:#}"),
                    "crop failed; writing the outputs uncropped"
                );
                match write_crop_item(rc, &item, full) {
                    Ok(o) => {
                        let note = format!(
                            "group {}: the crop failed ({e:#}); its outputs are written uncropped",
                            item.group_key
                        );
                        Ok((o, Some(note)))
                    }
                    Err(e2) => Err(anyhow::anyhow!(
                        "the crop failed ({e:#}); the uncropped write failed too ({e2:#})"
                    )),
                }
            }
            Err(e) => Err(e),
        };
        match result {
            Ok((o, note)) => {
                if let Err(e) = record_crop_item(rc, &item, &o) {
                    // Its rows may still name the scratch folder, so the
                    // folder stays, whatever the other groups do, and the
                    // pass goes on with them. The final files just written
                    // are removed again: nothing names them, and a leftover
                    // would only be an orphan beside a later successful run.
                    kept += 1;
                    remove_partial_outputs(&crop_output_paths(&o));
                    tracing::error!(
                        run_id = rc.run_id,
                        group_key = %item.group_key,
                        path = %scratch.display(),
                        error = ?e,
                        "crop outputs written but their rows could not be updated; \
                         final files removed, scratch kept"
                    );
                    rc.warnings.push(format!(
                        "group {}: its rows could not be updated after the crop; \
                         its just-written final files were removed and the scratch folder {} is kept",
                        item.group_key,
                        scratch.display()
                    ));
                    record_error.get_or_insert(e);
                    continue;
                }
                if let Some(note) = note {
                    rc.warnings.push(note);
                }
                if let Some(msg) = &o.drizzle_error {
                    kept += 1;
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %item.group_key,
                        path = %scratch.display(),
                        error = %msg,
                        "drizzle outputs could not be cropped; kept in the scratch folder"
                    );
                    rc.warnings.push(format!(
                        "group {}: its drizzle outputs could not be cropped ({msg}); \
                         they stay in {}",
                        item.group_key,
                        scratch.display()
                    ));
                }
            }
            Err(e) => {
                kept += 1;
                tracing::error!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    path = %scratch.display(),
                    error = %format!("{e:#}"),
                    "crop outputs could not be finalized; kept in the scratch folder"
                );
                rc.warnings.push(format!(
                    "group {}: its outputs could not be finalized ({e:#}); they stay in {}",
                    item.group_key,
                    scratch.display()
                ));
            }
        }
    }
    rc.crop_progress("", crop_total, crop_total);
    if rc.cancel.load(Ordering::SeqCst) {
        tracing::info!(
            run_id = rc.run_id,
            "crop pass finished after a cancel; the outputs are final"
        );
    }
    remove_scratch_unless_kept(rc, &scratch, kept);
    match record_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Every final file one crop item wrote: the master family and, when it
/// was written, the drizzle family.
fn crop_output_paths(o: &CropOutputs) -> Vec<PathBuf> {
    let mut paths = vec![o.master.master.clone()];
    paths.extend(o.master.rejection_low.clone());
    paths.extend(o.master.rejection_high.clone());
    paths.extend(o.master.coverage.clone());
    if let Some((d, _)) = &o.drizzle {
        paths.push(d.drizzle.clone());
        paths.extend(d.weight_map.clone());
        paths.extend(d.coverage.clone());
    }
    paths
}

fn remove_scratch_unless_kept(rc: &RunContext, scratch: &Path, kept: usize) {
    if kept > 0 {
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(scratch) {
        tracing::warn!(
            run_id = rc.run_id,
            path = %scratch.display(),
            error = %error,
            "removing the crop scratch folder failed"
        );
    }
}

/// Plan 6b-3 D10 (fix round 1): the exit path of a crop run that did not
/// reach [`finish_crop`] — cancelled between groups, or failed. Every group
/// already written to the scratch folder is finalized UNCROPPED (the full
/// rectangle through the crop writer), so its master lands in the output
/// folder as a K = 0 run's would. One run warning says so. A group that
/// cannot be finalized keeps its scratch, named in a warning. Nothing here
/// fails: the run is already ending.
fn finalize_unfinished_crop(rc: &mut RunContext) {
    let items = std::mem::take(&mut rc.crop_items);
    if items.is_empty() {
        return;
    }
    let scratch = rc.layout.uncropped_dir(rc.run_id);
    let (mut kept, mut finalized) = (0usize, 0usize);
    let crop_total = items.len();
    for (crop_done, item) in items.iter().enumerate() {
        rc.crop_progress(&item.group_key, crop_done, crop_total);
        let full = CropRect::full(item.width, item.height);
        let result = write_crop_item(rc, item, full)
            .map_err(|e| format!("{e:#}"))
            .and_then(|o| {
                record_crop_item(rc, item, &o).map_err(|e| format!("{e:?}"))?;
                Ok(o)
            });
        match result {
            Ok(o) => {
                finalized += 1;
                if let Some(msg) = o.drizzle_error {
                    kept += 1;
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %item.group_key,
                        path = %scratch.display(),
                        error = %msg,
                        "drizzle outputs could not be finalized; kept in the scratch folder"
                    );
                    rc.warnings.push(format!(
                        "group {}: its drizzle outputs could not be finalized ({msg}); \
                         they stay in {}",
                        item.group_key,
                        scratch.display()
                    ));
                }
            }
            Err(msg) => {
                kept += 1;
                tracing::error!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    path = %scratch.display(),
                    error = %msg,
                    "crop outputs could not be finalized; kept in the scratch folder"
                );
                rc.warnings.push(format!(
                    "group {}: its outputs could not be finalized ({msg}); they stay in {}",
                    item.group_key,
                    scratch.display()
                ));
            }
        }
    }
    rc.crop_progress("", crop_total, crop_total);
    if finalized > 0 {
        tracing::warn!(
            run_id = rc.run_id,
            count = finalized,
            "run did not finish; written outputs finalized uncropped"
        );
        rc.warnings.push(format!(
            "{finalized} group(s): outputs were written uncropped because the run did not finish"
        ));
    }
    remove_scratch_unless_kept(rc, &scratch, kept);
}

/// The file half of one group's crop: the master family, then the drizzle
/// family, each into the output folder. `rect` may be the full master: that
/// finalization writes exactly what the K = 0 path writes, the same writer
/// on the same pixels and cards (review I7). A drizzle family that cannot be
/// written never costs the master (ruling R-M3-7): the master family stays
/// final and the failure is returned in `drizzle_error`. The drizzle WCS is
/// `scale_plate_solve` of the master's record again (review M13): the first
/// write already scaled the same record, so it does not fail here. Names
/// come from the scratch files, so a name already taken in the output folder
/// resolves as `_2`, `_2_2` where the K = 0 path would say `_2`, `_3`.
fn write_crop_item(
    rc: &RunContext,
    item: &CropItem,
    rect: CropRect,
) -> anyhow::Result<CropOutputs> {
    #[cfg(test)]
    if rc.subject.project_id().is_some() && FORCE_CROP_FAILURE.swap(false, Ordering::SeqCst) {
        anyhow::bail!("forced crop failure");
    }
    let format = rc.config.output.format;
    let mut cards = item.cards.clone();
    if let Some(wcs) = &item.wcs {
        restate_crpix(&mut cards, &crop_plate_solve(wcs, rect.x0, rect.y0));
    }
    let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let master = write_cropped_master(&rc.output_dir, &item.master, rect, &cards, format)?;
    let mut drizzle = None;
    let mut drizzle_error = None;
    if let Some(d) = &item.drizzle {
        let drect = rect.scaled(d.scale);
        let attempt = (|| -> anyhow::Result<WrittenDrizzle> {
            let mut dcards = d.cards.clone();
            if let Some(wcs) = &item.wcs {
                let scaled = scale_plate_solve(wcs, d.scale)?;
                restate_crpix(&mut dcards, &crop_plate_solve(&scaled, drect.x0, drect.y0));
            }
            let stem = master
                .master
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("master");
            write_cropped_drizzle(
                &rc.output_dir,
                stem,
                d.scale,
                &d.written,
                drect,
                &dcards,
                format,
            )
        })();
        match attempt {
            Ok(w) => drizzle = Some((w, drect)),
            Err(e) => drizzle_error = Some(format!("{e:#}")),
        }
    }
    Ok(CropOutputs {
        rect,
        master,
        drizzle,
        drizzle_error,
    })
}

/// The record half: the group row's paths, one `master_lights` upsert per
/// final file (the key is `(run, group, kind)`, so each scratch row is
/// replaced), and the summary's paths.
fn record_crop_item(
    rc: &mut RunContext,
    item: &CropItem,
    out: &CropOutputs,
) -> Result<(), RunError> {
    #[cfg(test)]
    if rc.subject.project_id().is_some() && FORCE_RECORD_FAILURE.swap(false, Ordering::SeqCst) {
        return Err(RunError::Other("forced record failure".to_string()));
    }
    let show = |p: &Path| p.display().to_string();
    let format_str = enum_serde_name(&rc.config.output.format)?;
    let master_path = show(&out.master.master);
    let rejection_low = out.master.rejection_low.as_deref().map(show);
    let rejection_high = out.master.rejection_high.as_deref().map(show);
    let coverage = out.master.coverage.as_deref().map(show);
    let drizzle = out.drizzle.as_ref().map(|(d, r)| (show(&d.drizzle), *r));
    let weight_map = out
        .drizzle
        .as_ref()
        .and_then(|(d, _)| d.weight_map.as_deref().map(show));
    let drizzle_coverage = out
        .drizzle
        .as_ref()
        .and_then(|(d, _)| d.coverage.as_deref().map(show));
    {
        let conn = db(&rc.ctx)?.conn();
        // One IMMEDIATE transaction: the group row and every `master_lights`
        // row land together or not at all, so a failure never leaves the group
        // naming a final file the caller then removes. Dropping `tx` on an
        // early return rolls it back.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(anyhow::Error::from)?;
        update_group(
            &tx,
            item.group_id,
            &GroupUpdate {
                master_path: Some(&master_path),
                rejection_low_path: rejection_low.as_deref(),
                rejection_high_path: rejection_high.as_deref(),
                drizzle_path: drizzle.as_ref().map(|(p, _)| p.as_str()),
                ..Default::default()
            },
        )?;
        #[cfg(test)]
        if rc.subject.project_id().is_some()
            && FORCE_RECORD_FAILURE_AFTER_GROUP.swap(false, Ordering::SeqCst)
        {
            return Err(RunError::Other(
                "forced record failure after the group row".to_string(),
            ));
        }
        let row =
            |kind: MasterLightKind, path: &str, format: &str, rect: CropRect, channels: usize| {
                insert_master_light(
                    &tx,
                    &NewMasterLight {
                        subject: &rc.subject,
                        run_id: rc.run_id,
                        group_key: &item.group_key,
                        kind: kind.as_db_str(),
                        path,
                        format,
                        width: rect.width as i64,
                        height: rect.height as i64,
                        channels: channels as i64,
                        frames: item.frames as i64,
                        total_exposure_s: item.total_exposure_s,
                    },
                )
            };
        row(
            MasterLightKind::Master,
            &master_path,
            &format_str,
            out.rect,
            item.channels,
        )?;
        if let Some(path) = coverage.as_deref() {
            row(MasterLightKind::Coverage, path, "fits", out.rect, 1)?;
        }
        if let Some((path, drect)) = drizzle.as_ref() {
            row(
                MasterLightKind::Drizzle,
                path,
                &format_str,
                *drect,
                item.channels,
            )?;
            if let Some(path) = weight_map.as_deref() {
                row(
                    MasterLightKind::WeightMap,
                    path,
                    &format_str,
                    *drect,
                    item.channels,
                )?;
            }
            if let Some(path) = drizzle_coverage.as_deref() {
                row(
                    MasterLightKind::DrizzleCoverage,
                    path,
                    "fits",
                    *drect,
                    item.channels,
                )?;
            }
        }
        tx.commit().map_err(anyhow::Error::from)?;
    }
    if let Some(g) = rc
        .summary
        .groups
        .iter_mut()
        .find(|g| g.key == item.group_key)
    {
        g.master_path = Some(master_path.clone());
        g.rejection_low_path = rejection_low;
        g.rejection_high_path = rejection_high;
        g.coverage_path = coverage;
        if let Some((path, _)) = drizzle {
            g.drizzle_path = Some(path);
            g.weight_map_path = weight_map;
            g.drizzle_coverage_path = drizzle_coverage;
        }
    }
    let message = if out.rect.is_full(item.width, item.height) {
        "outputs finalized uncropped"
    } else {
        "outputs cropped"
    };
    tracing::info!(
        run_id = rc.run_id,
        group_key = %item.group_key,
        crop_x0 = out.rect.x0,
        crop_y0 = out.rect.y0,
        crop_width = out.rect.width,
        crop_height = out.rect.height,
        path = %master_path,
        "{}",
        message
    );
    Ok(())
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
        memo: HashMemo::new(origins.clone()),
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
    rc.memo = HashMemo::new(origins.clone());
    rc.origins = origins;
    rc.summary.set_id = None;
    rc.summary.project_id = Some(project_id.to_string());
    rc
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reweigh_tests;
