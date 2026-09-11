//! The stacking run's provenance document (spec §9.1's `summary_json` /
//! `runs/run-<id>.json` — the SAME document: one copy is written to the
//! `stacking_runs.summary_json` column, the other to disk under the working
//! layout's `runs/` subtree; see `run.rs::run_thread`, the single place that
//! writes both). Every field a completed (or failed/cancelled) run is
//! willing to answer about itself: what it was asked to do
//! (`config`/`config_hash`), what it resolved (`reference`/`measurement`),
//! what each group produced (`groups`/[`SummaryGroup`]/[`SummaryFrame`]), how
//! long each stage took (`stages`), and anything worth a human's attention
//! (`warnings`/`error`).
//!
//! Plan 5a Task 6 only runs stage 1 (calibrate): `groups` stays empty and
//! `reference`/`measurement` carry only what the plan gate already knew
//! (`Auto` mode's actual frame, every per-frame metric) until Tasks 7-8 land
//! stages 3-9 and fill the rest in on the SAME [`RunSummary`] `run.rs`'s
//! `RunContext` builds up over the pipeline's lifetime.

use serde::{Deserialize, Serialize};

use crate::integration::stats::ScaleEstimator;
use crate::stacking::config::{ReferenceMode, StackingConfig};
use crate::stacking::drizzle::DrizzleStats;
use crate::stacking::integrate::GroupStats;
use crate::stacking::plan::{MasterWork, Stage};

/// One stage's wall-clock cost, in the order it ran
/// (`run.rs`'s `RunContext::timings`).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StageTiming {
    pub stage: Stage,
    pub duration_ms: u64,
}

/// One master stage 0.5 built or rebuilt (spec §2 row 0.5, owner requirement
/// 2026-09-09). `set_id` echoes the [`crate::stacking::plan::PlanMaster`]
/// that drove this item — the raw set id for `Build`, the master set id for
/// `Rebuild` (see that struct's own doc comment); `master_set_id` is always
/// the resulting MASTER's `calibration_set` id (identical to `set_id` for a
/// `Rebuild` item, different for `Build`). `path` is the master's on-disk
/// path after the build.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterBuilt {
    pub set_id: i64,
    pub kind: MasterWork,
    pub master_set_id: i64,
    pub path: String,
    pub duration_ms: u64,
}

/// The run's resolved reference frame. A `Manual` run knows this before the
/// pipeline even starts (the plan gate already resolved and validated it);
/// `Auto` mode fills `frame_id`/`filename`/`weight` in once stage 4 (Task 7)
/// picks the best-weighted frame of the largest group. `weight` stays `None`
/// until stage 3 has measured it, in either mode.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SummaryReference {
    pub frame_id: Option<i64>,
    pub filename: Option<String>,
    pub mode: ReferenceMode,
    pub weight: Option<f64>,
    /// The frame stage 4 originally picked, when stage 5's two-pass pick
    /// then moved the reference somewhere else (spec §4.4, ruling
    /// R-M4a-5) — `None` on every run that kept its first choice, which is
    /// most of them. `#[serde(default)]` so a `runs/run-<id>.json` written
    /// before M4a still deserializes.
    #[serde(default)]
    pub switched_from: Option<i64>,
}

/// What stage 3 (measurement, Task 7) used, recorded once at the top of the
/// summary — a run-wide choice, not a per-frame one. `seed_source` is always
/// `"fast"` in M1 (the fast detector path is the pipeline's only seed
/// source; ruling 13's carry-forward — the probe never recorded it, this
/// summary does). `scale_estimator` mirrors `cfg.normalization.scale_estimator`,
/// which [`crate::stacking::config::MeasurementConfig::measure_options`]
/// keeps in lockstep with the measurement stage's own estimator, so this
/// field is knowable from the config alone, before stage 3 ever runs.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SummaryMeasurement {
    pub seed_source: String,
    pub scale_estimator: ScaleEstimator,
}

/// One frame's whole story across the pipeline. Task 6's calibrate stage
/// fills `frame_id`/`filename`/`included`/`exclusion_reason`/
/// `calibrated_path`/`cached_calibrated`; Tasks 7-8 fill the rest (weights,
/// PSF/registration metrics, `rejected_fraction`) as their stages run. Every
/// `SummaryGroup.frames` entry is built once, at Output time (Task 8) —
/// there is no partial per-stage version of this struct on disk or in the DB
/// before then (see the module doc).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SummaryFrame {
    pub frame_id: i64,
    pub filename: String,
    pub included: bool,
    pub exclusion_reason: Option<String>,
    pub weight: Option<f64>,
    pub weight_channels: Vec<f64>,
    pub fwhm_px: Option<f64>,
    pub eccentricity: Option<f64>,
    pub stars: Option<usize>,
    pub psf_signal_weight: Option<f64>,
    pub psf_snr: Option<f64>,
    pub noise: Option<f64>,
    pub reg_status: Option<String>,
    pub reg_model: Option<String>,
    pub reg_rms_px: Option<f64>,
    pub reg_inliers: Option<usize>,
    pub reg_inlier_ratio: Option<f64>,
    pub reg_flipped: Option<bool>,
    pub rejected_fraction: Option<f64>,
    pub calibrated_path: Option<String>,
    pub cached_calibrated: bool,
    pub cached_metrics: bool,
    pub cached_registration: bool,
    /// Stage 6 (local normalization, M2): the frame's own relative scale
    /// (mean across channels — [`crate::stacking::ln::LnFrameOutcome::scale`]),
    /// `None` when local normalization never ran for this group (disabled,
    /// or a ruling-R3 fallback to global normalization) or this frame was
    /// excluded before reaching it. `#[serde(default)]` so a `runs/run-<id>.json`
    /// written before M2 still deserializes.
    #[serde(default)]
    pub ln_scale: Option<f64>,
    /// Whether stage 6 REUSED an existing `ln` artifact for this frame
    /// rather than normalizing it fresh — same convention as
    /// `cached_calibrated`/`cached_metrics`. `#[serde(default)]`, see
    /// `ln_scale`'s own doc.
    #[serde(default)]
    pub cached_ln: bool,
}

/// One integration group's whole result. `stats`/`master_path`/rejection
/// paths/`frames` stay `None`/empty until Task 8's Output stage writes the
/// group's master — see the module doc.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SummaryGroup {
    pub key: String,
    pub frame_count: usize,
    pub included_count: usize,
    pub master_path: Option<String>,
    pub rejection_low_path: Option<String>,
    pub rejection_high_path: Option<String>,
    pub stats: Option<GroupStats>,
    pub normalization_reference_frame_id: Option<i64>,
    /// M4b (ruling R-M4b-4): the frame every member of THIS group was
    /// registered onto — the run-wide reference in `coRegistered` mode
    /// (the same id `RunSummary::reference` carries), the group's own in
    /// `native` mode. `None` on a summary written before M4b, and for a
    /// group that never reached stage 5 (fewer than 3 included frames):
    /// such a group has a resolved reference in the run's own bookkeeping,
    /// but it never warped anything onto it and never wrote a master, so
    /// reporting one here would put a `reference #N` on a Results card
    /// with no master behind it. `#[serde(default)]`, see
    /// [`SummaryFrame::ln_scale`]'s own doc.
    #[serde(default)]
    pub reference_frame_id: Option<i64>,
    /// Stage 6 (local normalization, M2): the group's LN reference file
    /// (`ln/<group>/reference.fits`, spec §9.5), when local normalization
    /// ran for this group at all — `None` when it is disabled, the group
    /// never reached Output (skipped/failed), or a ruling-R3 fallback to
    /// global normalization applied. `#[serde(default)]`, see
    /// `SummaryFrame::ln_scale`'s own doc.
    #[serde(default)]
    pub ln_reference_path: Option<String>,
    /// M3 Task 5 (spec §7, rulings R-M3-7/R-M3-9): the group's drizzled
    /// master, when the run's drizzle stage wrote one for it — `None` when
    /// drizzle is off, this group's drizzle failed (`warnings` carries the
    /// reason; the master above is unaffected either way), or the group
    /// never reached Output. `#[serde(default)]`, see `SummaryFrame::ln_scale`'s
    /// own doc for why every M3 field here follows that convention.
    #[serde(default)]
    pub drizzle_path: Option<String>,
    /// The drizzle weight map alongside `drizzle_path`, when
    /// `DrizzleConfig::write_weight_map` was on for this run — `None`
    /// whenever `drizzle_path` is `None` too, or the toggle was off.
    #[serde(default)]
    pub weight_map_path: Option<String>,
    /// The drizzle stage's own per-group stats, `Some` exactly when
    /// `drizzle_path` is.
    #[serde(default)]
    pub drizzle: Option<DrizzleStats>,
    pub frames: Vec<SummaryFrame>,
}

/// The whole run, as `stacking_runs.summary_json` AND `runs/run-<id>.json` —
/// deliberately the same Rust type for both, so there is only ever one shape
/// to keep in sync. Built up progressively on `run.rs`'s
/// `RunContext::summary` over the pipeline's lifetime; `status`/
/// `finished_at`/`warnings`/`stages`/`error` are finalized by
/// `run_thread` right before this is serialized to both destinations.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub run_id: i64,
    pub set_id: i64,
    pub set_name: String,
    pub app_version: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub config: StackingConfig,
    pub config_hash: String,
    pub reference: SummaryReference,
    pub measurement: SummaryMeasurement,
    pub groups: Vec<SummaryGroup>,
    /// Stage 0.5's own result list (spec §2 row 0.5, owner requirement
    /// 2026-09-09) — every master the run built or rebuilt before
    /// calibrating. Empty when `masters_to_build` was empty (nothing to do)
    /// or the run never reached stage 0.5 (a blocker or an earlier failure).
    #[serde(default)]
    pub masters_built: Vec<MasterBuilt>,
    pub stages: Vec<StageTiming>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}
