//! Group integration driver (spec §9.2, §6.3): the `integration:`/
//! `normalization:` configuration, the Auto rejection rule resolved per
//! group size, and `integrate_group` — which turns Plan 2's per-frame
//! measurements and weights into the rejection/output normalization pairs
//! the weighted engine (`integration::engine::integrate_stack`) needs,
//! opens each plane through a `RegisteredSource` (spec §6.1's hybrid
//! materialization — transforms persist, pixels do not), and folds the
//! per-plane `StackOutput`s into one `GroupOutput`.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::geometry::PixelMap;
use crate::integration::combine::{Combination, IntegrationRecipe, Rejection};
use crate::integration::engine::{
    integrate_stack, EngineProgress, LocalNormRow, LocalNormRowFactory, StackOutput, StackParams,
};
use crate::integration::io_policy::IoPolicy;
use crate::integration::registered_source::{MaterializedFrame, RegisteredFrame, RegisteredSource};
use crate::integration::source::{RejectionBitSink, RejectionBitSource};
use crate::integration::stats::{
    output_pair, rejection_pair, NormalizationPair, OutputNormalization, RejectionNormalization,
    ScaleEstimator,
};
use crate::integration::IntegrationError;
use crate::resample::Interpolation;
use crate::stacking::ln::grid::LnScratch;
use crate::stacking::ln::{LnFrameGrids, LnGrid};
use crate::stacking::measure::{
    measure_planes_diagnosed, FrameMeasurement, MeasureOptions, SeedOverride,
};
use crate::stacking::rej::{process_large_scale, RejBitmap, RejBitmapSet, RejForcedSource};
use crate::stacking::weights::{best_by_weight, FrameWeight};

/// spec §9.2 `integration:`; every field defaulted, camelCase on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct IntegrationConfig {
    /// The master builder's `Combination` — one enum, one spelling
    /// (`"average"`/`"median"`, snake_case per `combine.rs`'s own attribute).
    pub combination: Combination,
    pub rejection: RejectionChoice,
    /// The weight floor (spec §6.2): a frame whose lowest per-channel
    /// normalized weight falls below this is dropped from the group.
    pub min_weight: f64,
    /// Range rejection on the raw pixel value: reject `raw <= range_low`.
    pub range_low: Option<f64>,
    /// Reject `raw >= range_high`; `None` until the user turns it on.
    pub range_high: Option<f64>,
    pub write_rejection_maps: bool,
    /// Large-scale (structure-aware) pixel rejection, M4c (spec §6.2,
    /// ruling R-M4c-4).
    #[serde(default)]
    pub large_scale: LargeScaleRejection,
}

impl Default for IntegrationConfig {
    fn default() -> Self {
        IntegrationConfig {
            combination: Combination::Average,
            rejection: RejectionChoice::Auto,
            min_weight: 0.005,
            range_low: Some(0.0),
            range_high: None,
            write_rejection_maps: false,
            large_scale: LargeScaleRejection::default(),
        }
    }
}

/// spec §9.2 `integration.largeScale:` (M4c, ruling R-M4c-4). When
/// `enabled`, integration runs TWICE: the first pass writes every included
/// frame's per-pixel rejection bitmap, those bitmaps are filtered to the
/// structures big enough to be real (a satellite trail, an aircraft) and
/// grown by `growth` px, and the second pass forces exactly those samples
/// out before any per-pixel algorithm runs — so a trail is removed as the
/// one object it is, not as the speckle the per-pixel tests leave of it.
///
/// **One bit, no side split.** The spec's own `low`/`high` pair collapses
/// into this single `enabled` (ruling R-M4c-4): the per-frame bitmap format
/// is one bit per pixel — rejected or not — so it does not carry which SIDE
/// a rejection fell on, and processing the two sides separately is not a
/// distinction this data can express. A format bump to carry it would buy a
/// difference no acceptance test can see.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct LargeScaleRejection {
    pub enabled: bool,
    /// The scale selector: structures thinner than roughly `2^layers / 2`
    /// pixels are erased (see [`process_large_scale`]'s own doc for the
    /// exact thresholds — 3 px at `2`, 5 px at `3`, 9 px at `4`). 1–6.
    pub protected_layers: u8,
    /// Radius in pixels of the disc every surviving structure is grown by,
    /// so the structure's own faint edges — which the per-pixel test never
    /// reached — are covered too. 0–4.
    pub growth: u8,
}

impl Default for LargeScaleRejection {
    fn default() -> Self {
        LargeScaleRejection {
            enabled: false,
            protected_layers: 2,
            growth: 2,
        }
    }
}

/// spec §9.2 `normalization:` — `rejection == `[`RejectionNormalization::Local`]`
/// (M2) means `integrate_group` applies the per-frame LN grids
/// (`GroupInput.ln`) as the rejection-normalization pair instead of a
/// global one; a frame with no grid falls back to its own global pair (see
/// [`integrate_planes`]'s own doc). Note the asymmetry between the two
/// normalization axes: local *output* normalization is `local.enabled`, a
/// boolean sibling of `output` — when it doesn't apply, `output` is still
/// there to fall back to. Local *rejection* normalization is instead a
/// **variant of** `RejectionNormalization` itself (`Local`), so there is no
/// separate field left to fall back to — `stats::rejection_pair` resolves
/// `Local` to the same pair `ScaleZeroOffset` would give it, by construction
/// rather than by a fallback field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct NormalizationConfig {
    pub output: OutputNormalization,
    pub rejection: RejectionNormalization,
    /// Informational at this layer: the estimator that produced the
    /// stage-3 location/scale is `MeasureOptions::scale_estimator` — the
    /// orchestrator (Plan 5) keeps the two equal.
    pub scale_estimator: ScaleEstimator,
    /// Local (small-scale) normalization settings (spec §5.2, M2).
    #[serde(default)]
    pub local: LocalNormalizationConfig,
}

/// How the LN scale is chosen (spec 2026-10-04 §2): derived from the LN
/// geometry (`auto`, the default) or the stored `scale` (`fixed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LnScaleMode {
    Auto,
    Fixed,
}

impl LnScaleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LnScaleMode::Auto => "auto",
            LnScaleMode::Fixed => "fixed",
        }
    }
}

/// The star selection for the LN scale (spec 2026-10-04 §3, L-11).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct LnStarSelection {
    // Both sides' (peak − background) / σ_local must reach this (5–200).
    pub min_peak_snr: f64,
    // At most this many matched PAIRS, brightest first (20–5000).
    pub max_stars: u32,
    // A pixel within max(2, FWHM) of either centroid above this fraction of
    // that plane's maximum leaves the pair out (0.5–1.0).
    pub saturation: f64,
}

impl Default for LnStarSelection {
    fn default() -> Self {
        LnStarSelection {
            min_peak_snr: 40.0,
            max_stars: 500,
            saturation: 0.75,
        }
    }
}

impl LnStarSelection {
    /// No selection at all: every matched pair, in rank order. Tests and the
    /// probe's legacy arm only (P-13); never stored — it is not valid JSON.
    pub const OFF: LnStarSelection = LnStarSelection {
        min_peak_snr: f64::NEG_INFINITY,
        max_stars: u32::MAX,
        saturation: f64::INFINITY,
    };
}

/// The LN scale's flux definition (spec 2026-10-04 §4): the PSF fit's own
/// signal, or a common circular aperture (F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LnFluxEstimator {
    PsfSignal,
    // 2026-10-06: F1 growth curve is the default (L-24).
    #[default]
    CommonAperture,
}

impl LnFluxEstimator {
    pub fn as_str(self) -> &'static str {
        match self {
            LnFluxEstimator::PsfSignal => "psfSignal",
            LnFluxEstimator::CommonAperture => "commonAperture",
        }
    }
}

/// F1's circle (spec 2026-10-04 §4.1): 2 or 3 x FWHM, or 2 x FWHM with the
/// growth-curve correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LnAperture {
    Fwhm2,
    Fwhm3,
    // 2026-10-06: F1 growth curve is the default (L-24).
    #[default]
    GrowthCurve,
}

impl LnAperture {
    pub fn as_str(self) -> &'static str {
        match self {
            LnAperture::Fwhm2 => "fwhm2",
            LnAperture::Fwhm3 => "fwhm3",
            LnAperture::GrowthCurve => "growthCurve",
        }
    }
}

/// spec §9.2 `normalization.local:` (M2). `enabled` turns LN into the
/// OUTPUT normalization for a group; `local_scale` (a scale SPLINE over
/// the stride grid, not just the frame-global `A`) went live in M4c
/// (ruling R-M4c-8, `ln::scale::fit_local_scale` + `ln::a_grid`) and stays
/// `false` by default — it only means anything while `enabled` is on;
/// every other field is carried so the block round-trips through a stored
/// config unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalNormalizationConfig {
    pub enabled: bool,
    // The LN scale in pixels when `scaleMode` is `fixed`; snapped to the
    // supported list by `resolve_config`. Read only by `ln::resolve_scale`.
    pub scale: u32,
    // `None` only on a document stored before 2026-10 (field-level default,
    // so the struct's own default cannot fill it): `resolve_config` turns it
    // into `Some` by the legacy rule L-9.
    #[serde(default)]
    pub scale_mode: Option<LnScaleMode>,
    // Auto scale = the short side of the LN geometry / gridSize, snapped (2-8).
    pub grid_size: u32,
    pub reference_frames: u32,
    pub psf_model: crate::stacking::psf_signal::PsfModel,
    pub local_scale: bool,
    // Which matched stars set the scale (spec §3).
    pub star_selection: LnStarSelection,
    // Which flux the scale ratios use (spec 2026-10-04 §4.1).
    pub flux_estimator: LnFluxEstimator,
    // The common aperture's circle; read only under `commonAperture` (§4.1).
    pub aperture: LnAperture,
}

impl Default for LocalNormalizationConfig {
    fn default() -> Self {
        LocalNormalizationConfig {
            enabled: false,
            scale: 1024,
            scale_mode: Some(LnScaleMode::Auto),
            grid_size: 4,
            reference_frames: 20,
            psf_model: crate::stacking::psf_signal::PsfModel::Auto,
            local_scale: false,
            star_selection: LnStarSelection::default(),
            // 2026-10-06: F1 growth curve is the default (L-24).
            flux_estimator: LnFluxEstimator::CommonAperture,
            aperture: LnAperture::GrowthCurve,
        }
    }
}

/// spec §6.3: the user's rejection choice, resolved to a concrete
/// [`Rejection`] once the group size is known.
/// camelCase on the wire; the resolved `Rejection` persisted in master recipes
/// stays snake_case — the two JSON shapes are not interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "method"
)]
pub enum RejectionChoice {
    #[default]
    Auto,
    None,
    PercentileClip {
        low: f64,
        high: f64,
    },
    SigmaClip {
        sigma_low: f64,
        sigma_high: f64,
    },
    WinsorizedSigma {
        sigma_low: f64,
        sigma_high: f64,
    },
    LinearFitClip {
        sigma_low: f64,
        sigma_high: f64,
    },
    /// Min/max clipping (M4c, ruling R-M4c-1). Defaults when chosen:
    /// `low = 1`, `high = 1`.
    MinMax {
        low: usize,
        high: usize,
    },
    /// Generalized extreme studentized deviate test (M4c, ruling R-M4c-1).
    /// Defaults when chosen: `outliersFraction = 0.3`, `alpha = 0.05`,
    /// `lowRelaxation = 1.5`.
    Esd {
        outliers_fraction: f64,
        alpha: f64,
        low_relaxation: f64,
    },
    /// Robust Chauvenet Rejection (M4c, ruling R-M4c-1). Default when
    /// chosen: `limit = 0.5` (Chauvenet's criterion).
    Rcr {
        limit: f64,
    },
}

impl RejectionChoice {
    /// `n < 8` → percentile 0.2/0.1; `8 ≤ n < 20` → Winsorized 4.0/3.0;
    /// `n ≥ 20` → linear fit 5.0/3.5 (spec §6.3's Auto rule). Every other
    /// choice passes its parameters through unchanged.
    ///
    /// The ladder is deliberately unchanged by M4c (ruling R-M4c-1): min/max,
    /// ESD and RCR are user choices only, so no existing group's rejection
    /// moves because they exist.
    pub fn resolve(self, n: usize) -> Rejection {
        match self {
            RejectionChoice::Auto => {
                if n < 8 {
                    Rejection::PercentileClip {
                        low: 0.2,
                        high: 0.1,
                    }
                } else if n < 20 {
                    Rejection::WinsorizedSigma {
                        sigma_low: 4.0,
                        sigma_high: 3.0,
                    }
                } else {
                    Rejection::LinearFitClip {
                        sigma_low: 5.0,
                        sigma_high: 3.5,
                    }
                }
            }
            RejectionChoice::None => Rejection::None,
            RejectionChoice::PercentileClip { low, high } => {
                Rejection::PercentileClip { low, high }
            }
            RejectionChoice::SigmaClip {
                sigma_low,
                sigma_high,
            } => Rejection::SigmaClip {
                sigma_low,
                sigma_high,
            },
            RejectionChoice::WinsorizedSigma {
                sigma_low,
                sigma_high,
            } => Rejection::WinsorizedSigma {
                sigma_low,
                sigma_high,
            },
            RejectionChoice::LinearFitClip {
                sigma_low,
                sigma_high,
            } => Rejection::LinearFitClip {
                sigma_low,
                sigma_high,
            },
            RejectionChoice::MinMax { low, high } => Rejection::MinMax { low, high },
            RejectionChoice::Esd {
                outliers_fraction,
                alpha,
                low_relaxation,
            } => Rejection::Esd {
                outliers_fraction,
                alpha,
                low_relaxation,
            },
            RejectionChoice::Rcr { limit } => Rejection::Rcr { limit },
        }
    }
}

/// One frame of a group, ready to integrate: its geometry, its Plan 2
/// measurement and weight, and the bookkeeping the group-level header
/// (`DATE-OBS` span, exposure) needs.
pub struct StackFrame {
    pub path: PathBuf,
    /// Subject → reference (identity for the reference frame).
    pub map: PixelMap,
    pub measurement: FrameMeasurement,
    pub weight: FrameWeight,
    pub exposure_s: f64,
    /// `DATE-OBS` as stored (ISO-8601 text), for the header's earliest/latest.
    pub date_obs: Option<String>,
    /// The catalog frame id, for the Task 6a fallback warning only — no
    /// other field on this type carries identity. [`integrate_planes`]
    /// carries it into the `frame_id` [`MaterializedFrame`] builds, so
    /// `RegisteredSource::open_materialized`'s fallback `warn!` (fix round
    /// 2, ruling R-TA-6 I1) can name the frame it warped instead of
    /// reading it verbatim.
    pub frame_id: i64,
    /// The materialized registered artifact (Task 6a, spec §3.7) — already
    /// warped into the group's reference geometry, read verbatim by
    /// [`integrate_planes`]/`normalize_frame` in place of re-warping `path`
    /// through `map`. `None` when no fresh artifact was on disk at snapshot
    /// time (`stacking::run::GroupMember` resolves this once, the same
    /// `is_fresh` rule every other cached stage uses) — every reader then
    /// falls back to the on-the-fly warp, exactly as before this field
    /// existed.
    pub registered_path: Option<PathBuf>,
}

pub struct GroupInput<'a> {
    pub frames: &'a [StackFrame],
    /// Index into `frames` of the reference (its measurement normalizes the others).
    pub reference: usize,
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub interpolation: Interpolation,
    pub clamping: f32,
    pub integration: &'a IntegrationConfig,
    pub normalization: &'a NormalizationConfig,
    /// M2 Task 7: per-frame local-normalization grids, indexed like `frames`
    /// (not like a group's `included`/`frame_indices` subset) — `stacking::run`
    /// reads these back from cached `.athln` artifacts once stage 6 has run
    /// for the group; `None` for a frame with no sidecar (LN never ran for
    /// it, it drives rejection only and its own `normalize_frame` call
    /// failed, or it overlaps the LN reference on under `ln::LN_MIN_OVERLAP`
    /// and keeps global normalization — plan 6b-3 D7). The whole field is `None` when local normalization never ran
    /// for this group at all (disabled, or a ruling-R3-shaped fallback to
    /// global normalization) — `integrate_group` forwards it verbatim to
    /// [`integrate_planes`], which already treats `None` the same way for
    /// every M1 caller.
    pub ln: Option<&'a [Option<LnFrameGrids>]>,
    /// M3 Task 2 (spec §6.2, ruling R-M3-8): the run's per-run rejection-
    /// bitmap set for this group, when drizzle wants the survivor mask
    /// (`drizzle.enabled && drizzle.useRejection`) — `stacking::run` (Task
    /// 5) creates it (sized to the INCLUDED frame count, in engine order)
    /// before calling `integrate_group`; `None` for every other caller,
    /// including the LN reference builder, which never writes bitmaps.
    /// `integrate_group` checks `rej.frames()` against the post-min-weight-
    /// drop included count before handing it to `integrate_planes`.
    pub rej: Option<&'a RejBitmapSet>,
    /// Whether the large-scale SECOND pass should write a bitmap set of its
    /// own (`rej/<…>/pass2/`, ruling R-T3-1) — i.e. whether anything will
    /// ever READ it.
    ///
    /// The only reader is the Drizzle stage, which is handed pass 2's files
    /// instead of pass 1's precisely because they describe the master it is
    /// depositing into (see `run::drizzle_rejection_paths`). So this flag
    /// carries the run's own `drizzle.enabled && drizzle.useRejection`
    /// decision — the SAME condition that creates `rej` in the first place
    /// — and with drizzle off a large-scale run writes two bitmap sets, not
    /// three: the first pass's `.rej`, its filtered `.rejl` sibling, and
    /// nothing under `pass2/`. That third set is ≈ 3.3 MB per frame per
    /// plane at 26 Mpx (≈ 600 MB on a 208-frame group) that no code would
    /// open.
    ///
    /// `false` for every caller that is not a run: the LN reference builder
    /// and the tests never pass a `rej` set at all, so the flag has nothing
    /// to gate.
    pub second_pass_bitmaps: bool,
    /// Plan 6b-3 D4, spec §5.7: keep plane 0's geometric coverage in
    /// [`GroupOutput::coverage`] — the coverage map, and the crop's
    /// rectangle. `false` for every caller that wants neither.
    pub coverage_map: bool,
    /// Plan 6b-3 D4 refinement (review I1): measure the master's statistics
    /// (`master_location`, `_scale`, `_noise`, `_psf_snr`, `snr_gain`, FWHM,
    /// eccentricity) on covered pixels only — a grid master's uncovered
    /// pixels are 0.0 and would read as data. Implies the plane-0 count.
    /// The rectangle is the largest one at least half the group's included
    /// frames cover (`ceil(n / 2)`), falling back to one frame when that is
    /// under [`MIN_STATS_RECT_PX`] (final review A1).
    /// `false` for a frame set, whose statistics never move.
    pub stats_on_covered: bool,
}

/// [`GroupStats::snr_gain_version`] of a gain measured now: `1` = the paired
/// SNR factor `√(PSF SNR master / PSF SNR best sub)` (v0.7.0). Absent (`0`)
/// = the pre-v0.7.0 unpaired power ratio. Bump it when the formula changes
/// again, and teach `src/components/stacking/snrGain.ts` the new value.
pub const SNR_GAIN_VERSION: u32 = 1;

/// The smallest covered rectangle (per side, px) a grid master's statistics
/// are measured on (plan 6b-3 D4 refinement). The noise and PSF estimators
/// need a real image: B3-spline smoothing reflects two samples past an edge
/// (a 1-2 px row panics) and both want at least 100 samples. A narrower
/// covered rectangle fails the group instead.
pub const MIN_STATS_RECT_PX: usize = 16;

// `Clone, Serialize, Deserialize, ts_rs::TS` pulled forward from Task 9's own
// ts_export registration work (spec plan, "GroupStats (integrate.rs — derive
// TS)"): Plan 5a Task 6's `stacking::provenance::SummaryGroup.stats` is
// `Option<GroupStats>`, and `RunSummary` (the run's `summary_json` /
// `runs/run-<id>.json` document) must serialize as a whole — the derive is a
// compile-time necessity now, not a design choice made early. Task 9 still
// owns registering the type into `ts_export.rs`'s `stacking.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GroupStats {
    /// The group's total frame count, before the min-weight drop.
    pub frames: usize,
    pub included: usize,
    pub dropped_below_min_weight: usize,
    /// `IntegrationRecipe::describe()` of the resolved recipe.
    pub recipe: String,
    /// `Σ rejected_low / Σ samples_per_frame`, over every included frame and
    /// plane (the engine's `rejected_low`/`samples_per_frame` denominator —
    /// see `StackOutput`'s doc — NOT `base.rejected_fraction`'s, which
    /// counts algorithm rejections only).
    pub rejected_low_fraction: f64,
    pub rejected_high_fraction: f64,
    /// Per included frame (engine order), `Σ rejected / Σ samples` over
    /// every plane — the same range-plus-algorithm rejection the group
    /// fractions above count, just narrowed to one frame instead of the
    /// whole group.
    pub rejected_fraction_per_frame: Vec<f64>,
    /// MRS noise of the master per plane, native `[0, 1]` units.
    /// `measure_plane` already reports `ChannelMeasurement.noise` in native
    /// units (it divides the ADU-scaled MRS estimate back down by
    /// `measure::ADU_SCALE` internally, see `measure_plane`'s own code) —
    /// no further conversion happens here.
    pub master_noise: Vec<f64>,
    pub master_location: Vec<f64>,
    pub master_scale: Vec<f64>,
    /// Per plane, of the included frame with the highest `weight.normalized_mean`.
    pub best_sub_noise: Vec<f64>,
    pub master_psf_snr: Vec<f64>,
    pub best_sub_psf_snr: Vec<f64>,
    /// How many times the master's star SNR beats the best sub's, per
    /// plane — an SNR (amplitude) factor, `√(PSF SNR master / PSF SNR best
    /// sub)`, since PSF SNR is TFlux²/σ² (a power ratio). The two PSF SNRs
    /// are a PAIRED measurement ([`paired_snr_gain`]): the best sub read
    /// through the same registration warp the master was built from, both
    /// measured with the same options on the same rectangle — a smoothing
    /// interpolation lowers a warped image's noise, so a raw sub against
    /// the master over-states the gain (2026-10-25: set 109 read 751.98×
    /// and 777.33× for 208 and 160 frames; the ratio was a power ratio of a
    /// warped master over a raw sub). They are not `master_psf_snr` /
    /// `best_sub_psf_snr` above, which stay each image's own measurement.
    /// `0.0` (with a warn) when the pair cannot be measured.
    pub snr_gain: Vec<f64>,
    /// Which formula [`Self::snr_gain`] was measured with:
    /// [`SNR_GAIN_VERSION`] for the paired SNR factor above. A stats document
    /// stored before v0.7.0 has no such key and reads `0`: its `snr_gain` is
    /// the old unpaired power ratio (set 109's 751.98×), so the Results panel
    /// shows "—" for it instead of passing it off as the same metric.
    #[serde(default)]
    pub snr_gain_version: u32,
    pub master_fwhm_px: Vec<f64>,
    pub master_eccentricity: Vec<f64>,
    /// `Σ exposure_i · weight_i` over included frames, `weight_i` the mean
    /// over planes of frame i's normalized weight (`FrameWeight::normalized_mean`).
    pub weighted_exposure_s: f64,
    pub total_exposure_s: f64,
    pub read_ms: u64,
    pub combine_ms: u64,
    pub bytes_read: u64,
    /// Included frames integrated with an LN grid (M2 Task 7) — `0` when the
    /// caller passes no grids at all (`GroupInput.ln: None`: local
    /// normalization off for this group, or `stacking::run` never resolved
    /// any), otherwise the count of included frames whose own `ln[i]` was
    /// `Some`.
    pub ln_frames: usize,
    /// M4c Task 3: the fraction of all (frame, plane, pixel) samples the
    /// PROCESSED bitmaps forced out before the second pass's algorithm ran
    /// — `None` when large-scale rejection did not run for this group at
    /// all (off, no bitmap set, or the first pass's bitmaps could not be
    /// trusted; `stacking::run` turns the last two into a run warning).
    #[serde(default)]
    pub large_scale_rejected_fraction: Option<f64>,
}

#[derive(Debug)]
pub struct GroupOutput {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    /// Planar, `channels × width × height`.
    pub data: Vec<f32>,
    pub rejection_low: Option<Vec<f32>>,
    pub rejection_high: Option<Vec<f32>>,
    /// Indices into the input `frames` that were integrated (after the min-weight drop), in engine order.
    pub included: Vec<usize>,
    pub stats: GroupStats,
    /// M3 Task 2: per plane (outer, `channels` long), per included frame
    /// (inner, engine order — same order as `included`), the OUTPUT
    /// normalization pair `integrate_planes` computed for it (the same
    /// values it fed `StackParams::output`). Task 3's drizzle driver uses
    /// this as the ruling-R-M3-5 fallback pair for a frame with no LN grid.
    pub output_pairs: Vec<Vec<NormalizationPair>>,
    /// M4c Task 3, fix round 1 (ruling R-T3-1): the large-scale SECOND pass
    /// ran AND wrote a complete rejection-bitmap set of its own
    /// ([`RejBitmapSet::second_pass_path`]). `true` is the ONLY state in
    /// which those files describe the master this call returned — drizzle
    /// reads them then, and nothing else (`stacking::run`); `false` covers
    /// both "no second pass" (the pass-1 `.rej` set still describes the
    /// master) and "the second pass ran but its own bitmaps are missing or
    /// incomplete", which the run turns into a drizzle skip rather than
    /// letting drizzle trust bits that describe a different integration.
    pub second_pass_rej_ok: bool,
    /// Plane 0's per-pixel count of frames with a finite registered sample
    /// (`Some` when [`GroupInput::coverage_map`] was set). The pass whose
    /// output the master is (the second, when large-scale rejection ran)
    /// supplies it; both passes see the same geometry.
    pub coverage: Option<Vec<f32>>,
    /// The rectangle the master's statistics were measured on, `Some` only
    /// when [`GroupInput::stats_on_covered`] was set (a grid master): the
    /// largest rectangle every pixel of which at least half the frames cover
    /// (one frame when that is too small), already past
    /// [`MIN_STATS_RECT_PX`]. A grid drizzle measures its own statistics on
    /// this rectangle scaled by the drizzle factor (plan 6b-3 ruling (b)).
    pub measure_rect: Option<crate::stacking::crop::CropRect>,
}

pub struct GroupProgress<'a> {
    /// `(plane_index, planes_total)` at the start of each plane.
    pub on_plane: &'a (dyn Fn(usize, usize) + Sync),
    pub engine: EngineProgress<'a>,
}

/// The two `GroupInput` sanity checks every direct consumer of its
/// `reference`/`channels` fields needs before it can safely index into
/// `frames` with them: the reference index must be in range, and every
/// frame's own measurement must carry as many channels as the group
/// declares (a short one would silently index out of bounds inside
/// `integrate_planes`'s per-plane loop otherwise). Shared by
/// `integrate_group` and `stacking::ln::reference::build_reference` (M2
/// Task 4) so the two checks — and their error texts — cannot drift apart.
pub(crate) fn validate_group_input(input: &GroupInput<'_>) -> Result<(), IntegrationError> {
    let frames = input.frames;
    if input.reference >= frames.len() {
        return Err(IntegrationError::BadInput(format!(
            "reference index {} out of range for {} frames",
            input.reference,
            frames.len()
        )));
    }
    for (i, f) in frames.iter().enumerate() {
        if f.measurement.channels.len() != input.channels {
            return Err(IntegrationError::BadInput(format!(
                "frame {i} ({}) has {} measured channels, the group has {}",
                f.path.display(),
                f.measurement.channels.len(),
                input.channels
            )));
        }
    }
    Ok(())
}

/// Wraps one channel's LN grid as an `integration::engine::LocalNormRowFactory`
/// (M2 Task 6, fix round 1, item 3): calling the returned factory produces a
/// FRESH `LnScratch` and a `FnMut(y_abs, a_row, b_row)` that owns it
/// exclusively, evaluating the row via [`LnGrid::evaluate_row_into`] (see
/// [`LnGrid::evaluate_row`]'s own doc, which names exactly this reuse — one
/// scratch across every row a worker evaluates — as the band loop's job).
/// The engine calls this factory once per worker thread (never per row or
/// pixel), so every worker gets its own private scratch with no lock: a
/// single shared closure behind a `Mutex<LnScratch>` (the fix round 1 review
/// finding) would instead serialize every worker through one frame's
/// scratch, capping parallelism at the frame count.
fn local_norm_row_factory<'g>(grid: &'g LnGrid) -> impl Fn() -> Box<LocalNormRow<'g>> + Sync + 'g {
    move || {
        let mut scratch = LnScratch::for_grid(grid);
        Box::new(move |y: usize, a_row: &mut [f32], b_row: &mut [f32]| {
            grid.evaluate_row_into(y, a_row, b_row, &mut scratch);
        })
    }
}

/// What [`integrate_planes`] counts per output pixel (plan 6b-3 D4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CoverageOptions {
    /// Count plane 0's geometric coverage (one plane: debayered planes share
    /// the footprint; a Bayer mosaic is one plane).
    pub count: bool,
    /// Leave every plane's uncovered pixels NaN (a project's LN reference).
    pub empty_as_nan: bool,
}

/// [`GroupStats::snr_gain`], per plane: `√(PSF SNR master / PSF SNR best
/// sub)` from a paired measurement.
///
/// PSF SNR is `TFlux² / σ²`, a power ratio, so the gain in SNR is its square
/// root. And the two must be measured alike: the master is an average of
/// WARPED frames, and the registration interpolation (the default cubic
/// B-spline smooths, `1/6 4/6 1/6`) lowers the noise of every frame it
/// resamples — the best sub's own measurement, taken on its raw pixels
/// before registration, has none of that. So the best sub is read here
/// through the very source [`integrate_planes`] read it from
/// ([`materialized_frame`]: its registered artifact or the on-the-fly warp),
/// and both images are measured with `measure` (the master's options) on
/// one rectangle: the largest one inside `base` (the master's own
/// statistics rectangle) where both are finite — a warped frame is NaN
/// outside its footprint, and TFlux sums over the stars in the rectangle,
/// so the areas must match.
///
/// One frame plane resident at a time (plus its crop). `0.0` for every
/// plane, with a warn, when the common rectangle is under
/// [`MIN_STATS_RECT_PX`]; `0.0` for a plane whose pair has no usable PSF SNR.
fn paired_snr_gain(
    input: &GroupInput<'_>,
    best: &StackFrame,
    master: &[f32],
    base: crate::stacking::crop::CropRect,
    measure: &MeasureOptions,
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
) -> Result<Vec<f64>, IntegrationError> {
    use crate::integration::banded::BandPlanes;
    use crate::integration::source::FrameSource;
    use crate::stacking::crop::{crop_plane, largest_covered_rect, CropRect};

    let (w, h, channels) = (input.width, input.height, input.channels);
    let plane_len = w * h;
    let mut src = RegisteredSource::open_materialized(
        &[materialized_frame(best)],
        w,
        h,
        0,
        input.interpolation,
        input.clamping,
    )?
    .with_pool(Arc::clone(pool));
    let mut band = BandPlanes::new(&src);
    let mut plane = vec![0f32; plane_len];
    let mut rect: Option<CropRect> = None;
    let mut sub_planes: Vec<Vec<f32>> = Vec::with_capacity(channels);
    for p in 0..channels {
        if cancel.load(Ordering::Relaxed) {
            warn!(plane = p, "group integration cancelled");
            return Err(IntegrationError::Cancelled);
        }
        src.set_plane(p)?;
        src.read_band_with_progress(0, h, &mut band, 1, &|_: u64| {}, cancel)?;
        band.decode_frame_into(0, &mut plane);
        if p == 0 {
            let mut both = vec![f32::NAN; plane_len];
            for y in base.y0..(base.y0 + base.height).min(h) {
                for x in base.x0..(base.x0 + base.width).min(w) {
                    let k = y * w + x;
                    if plane[k].is_finite() && master[k].is_finite() {
                        both[k] = 1.0;
                    }
                }
            }
            rect = largest_covered_rect(&both, w, h, 1)
                .filter(|r| r.width >= MIN_STATS_RECT_PX && r.height >= MIN_STATS_RECT_PX);
            if rect.is_none() {
                warn!(
                    frame_id = best.frame_id,
                    "snr_gain not measured: the best sub and the master share no usable rectangle; reporting 0.0"
                );
                return Ok(vec![0.0; channels]);
            }
        }
        let r = rect.expect("set on plane 0");
        sub_planes.push(crop_plane(&plane, w, r));
    }
    drop(plane);
    drop(band);
    drop(src);
    let r = rect.expect("channels >= 1");

    let measure_pair = |planes: &(dyn Fn(usize) -> Vec<f32> + Sync)| {
        pool.install(|| {
            measure_planes_diagnosed(
                channels,
                r.width,
                r.height,
                measure,
                None,
                SeedOverride::Fast,
                false,
                // Both images are resampled: mixed pixels (ruling C-10).
                crate::star_measure::PixelNoise::Mixed,
                None,
                |p| {
                    if cancel.load(Ordering::Relaxed) {
                        warn!(plane = p, "group integration cancelled");
                        return Err(IntegrationError::Cancelled);
                    }
                    Ok(Cow::Owned(planes(p)))
                },
            )
        })
    };
    let master_m =
        measure_pair(&|p| crop_plane(&master[p * plane_len..(p + 1) * plane_len], w, r))?;
    let sub_m = measure_pair(&|p| sub_planes[p].clone())?;

    Ok(master_m
        .iter()
        .zip(sub_m.iter())
        .enumerate()
        .map(|(p, (m, s))| {
            let (master_snr, sub_snr) = (m.measurement.channel.psf_snr, s.measurement.channel.psf_snr);
            let gain = if sub_snr > 0.0 { (master_snr / sub_snr).sqrt() } else { f64::NAN };
            debug!(
                plane = p,
                frame_id = best.frame_id,
                master_psf_snr = master_snr,
                best_sub_psf_snr = sub_snr,
                crop_x0 = r.x0,
                crop_y0 = r.y0,
                crop_width = r.width,
                crop_height = r.height,
                snr_gain = gain,
                "snr gain measured"
            );
            if gain.is_finite() {
                gain
            } else {
                warn!(
                    plane = p,
                    "snr_gain is not finite (the paired best sub has no usable PSF SNR); reporting 0.0"
                );
                0.0
            }
        })
        .collect())
}

/// How [`integrate_planes`] reads one frame: its materialized registered
/// artifact when there is one, else the on-the-fly warp of `path` through
/// `map`. Shared with the LN reference's saturation map
/// (`ln::reference::build_reference_saturation`, L-23), which must read
/// exactly the pixels the reference integrated.
pub(crate) fn materialized_frame(frame: &StackFrame) -> MaterializedFrame {
    MaterializedFrame {
        registered_path: frame.registered_path.clone(),
        frame_id: Some(frame.frame_id),
        fallback: RegisteredFrame {
            path: frame.path.clone(),
            map: frame.map.clone(),
        },
    }
}

/// The per-plane engine loop shared by [`integrate_group`] and the LN
/// reference builder (`stacking::ln::reference::build_reference`, M2 Task
/// 4): for every plane, builds the rejection/output normalization pairs of
/// `frame_indices` against `input`'s own normalization reference
/// (`input.frames[input.reference]` — the anchor is always the group's
/// reference, whether or not it is itself one of `frame_indices`), opens a
/// `RegisteredSource` over exactly that subset and runs the weighted engine.
/// `weights[k]` is frame `frame_indices[k]`'s per-channel weight (length
/// `input.channels`; a missing channel weighs zero, same convention as the
/// caller building it) — callers with a real per-frame weight vector pass it
/// through unchanged, callers that want equal weighting (the LN reference)
/// pass all-`1.0` rows. `output_mode`/`rejection_mode`/`recipe`/`write_maps`
/// are explicit rather than read from `input.normalization`/`input.integration`
/// so a caller can force plain global normalization (the LN reference always
/// does) independently of what the group itself is configured to use —
/// `local_for_output` (M2) joins them for the same reason: the LN reference
/// forces it `false` (its own doc: local normalization "doesn't exist yet at
/// reference-build time"). `local_for_rejection` is not a separate parameter
/// — it is `rejection_mode == RejectionNormalization::Local`, since that
/// enum value already says the same thing a caller would otherwise have to
/// repeat.
///
/// `ln` (M2) is `Some` per-frame [`LnFrameGrids`], indexed like
/// `input.frames` (not `frame_indices`) — `None`, the whole option, is every
/// caller today; a frame's own entry is `None` when it has no sidecar. Per
/// plane, this wraps `ln[i].channels[p]` (`i` = the frame's index into
/// `input.frames`) as a `LocalNormRow` via [`local_norm_row`] and hands
/// the resulting per-frame-indices row of closures to
/// [`crate::integration::engine::StackParams::local`] — engine.rs never
/// depends on `LnGrid` directly (see `LocalNormRow`'s own doc for why).
///
/// `rej` (M3 Task 2) is `Some` when the caller wants per-frame rejection
/// bitmaps written for this group — every M1/M2 caller and the LN reference
/// builder pass `None`. Bound per plane via [`RejBitmapSet::plane_sink`] and
/// handed to [`crate::integration::engine::StackParams::rejection_bits`];
/// the caller (`integrate_group`) is responsible for checking
/// `rej.frames() == frame_indices.len()` before calling this — a mismatch
/// here would otherwise surface as an opaque `IntegrationError::BadInput`
/// from deep inside `integrate_stack` instead of a clear one at the group
/// boundary.
///
/// Returns one [`StackOutput`] per plane (in channel order) alongside the
/// OUTPUT normalization pair this call resolved for every entry of
/// `frame_indices`, per plane — the same pairs each plane fed
/// `StackParams::output` — for the caller to fold into
/// [`GroupOutput::output_pairs`] (Task 3's drizzle driver reads them back as
/// its ruling-R-M3-5 fallback pair). No stats/accumulation/writing beyond
/// that: the rest of the tail is the caller's job.
#[allow(clippy::too_many_arguments)]
pub(crate) fn integrate_planes<'g>(
    input: &GroupInput<'_>,
    frame_indices: &[usize],
    weights: &[Vec<f32>],
    output_mode: OutputNormalization,
    rejection_mode: RejectionNormalization,
    local_for_output: bool,
    ln: Option<&'g [Option<LnFrameGrids>]>,
    rej: Option<&RejBitmapSet>,
    forced: Option<&RejForcedSource>,
    recipe: IntegrationRecipe,
    write_maps: bool,
    coverage: CoverageOptions,
    // An owned `Arc`, not a bare `&rayon::ThreadPool` (perf tier 1 Task 2):
    // this is the one place a `RegisteredSource` gets built for group
    // integration, and `RegisteredSource::with_pool` needs a clonable
    // handle to outlive the call the pool reference alone cannot provide.
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
    progress: &GroupProgress<'_>,
    io: IoPolicy,
) -> Result<(Vec<StackOutput>, Vec<Vec<NormalizationPair>>), IntegrationError> {
    if weights.len() != frame_indices.len() {
        return Err(IntegrationError::BadInput(format!(
            "{} frame indices but {} weight rows",
            frame_indices.len(),
            weights.len()
        )));
    }
    // Fix round 1, item 4: validate every provided grid's shape BEFORE any
    // rayon worker ever touches it. `LnGrid::evaluate_row_into` `assert!`s
    // `y < ref_height` and the row buffers' length against `ref_width` —
    // both would otherwise panic INSIDE a worker thread the first time a
    // mismatched grid's row got evaluated (Task 7 feeds cached, on-disk
    // sidecars into `ln`, which is exactly the kind of input this needs to
    // refuse cleanly rather than crash on).
    if let Some(grids) = ln {
        if grids.len() != input.frames.len() {
            return Err(IntegrationError::BadInput(format!(
                "local-normalization grids for {} frames, the group has {}",
                grids.len(),
                input.frames.len()
            )));
        }
        for (i, g) in grids.iter().enumerate() {
            let Some(fg) = g else { continue };
            if fg.channels.len() < input.channels {
                return Err(IntegrationError::BadInput(format!(
                    "frame {i} ({}) has {} LN channels, the group has {}",
                    input.frames[i].path.display(),
                    fg.channels.len(),
                    input.channels
                )));
            }
            for (c, grid) in fg.channels.iter().take(input.channels).enumerate() {
                if grid.ref_width != input.width || grid.ref_height != input.height {
                    return Err(IntegrationError::BadInput(format!(
                        "frame {i} ({}) LN grid channel {c} is {}x{}, the group is {}x{}",
                        input.frames[i].path.display(),
                        grid.ref_width,
                        grid.ref_height,
                        input.width,
                        input.height
                    )));
                }
            }
        }
    }
    let frames = input.frames;
    let n = frame_indices.len();
    let local_for_rejection = rejection_mode == RejectionNormalization::Local;
    let mut outputs = Vec::with_capacity(input.channels);
    let mut output_pairs_out: Vec<Vec<NormalizationPair>> = Vec::with_capacity(input.channels);

    // M4c Task 4 fix round 3, ruling R-T4-7: ONE source for the whole
    // group, re-pointed at each plane through `set_plane`, rather than a
    // fresh one per plane. The frame list does not depend on `p` — same
    // paths, same maps — so opening it three times for an OSC group only
    // re-opened every reader and, since M4c, rebuilt every frame's
    // displacement grid twice over for nothing. `set_plane` existed for
    // exactly this and had no production caller until now.
    //
    // Perf tier A Task 6a: read each frame's MATERIALIZED registered
    // artifact when `stacking::run` found one fresh at snapshot time
    // (`StackFrame::registered_path`), falling back per frame to the
    // on-the-fly warp otherwise — `RegisteredSource::open_materialized`
    // owns the readiness check and the ONE fallback `warn!`, see its own
    // doc.
    let registered_frames: Vec<MaterializedFrame> = frame_indices
        .iter()
        .map(|&i| materialized_frame(&frames[i]))
        .collect();
    let mut src = RegisteredSource::open_materialized(
        &registered_frames,
        input.width,
        input.height,
        0,
        input.interpolation,
        input.clamping,
    )?
    .with_pool(Arc::clone(pool));

    for p in 0..input.channels {
        if cancel.load(Ordering::Relaxed) {
            warn!(plane = p, "group integration cancelled");
            return Err(IntegrationError::Cancelled);
        }
        (progress.on_plane)(p, input.channels);

        let ref_ls = frames[input.reference].measurement.channels[p].location_scale();
        let mut rejection_pairs = Vec::with_capacity(n);
        let mut output_pairs = Vec::with_capacity(n);
        let mut plane_weights = Vec::with_capacity(n);
        // M2: this plane's local-normalization row-evaluator FACTORIES, one
        // per entry of `frame_indices` (fix round 1, item 3 — `StackParams::local`
        // now carries factories, not evaluators: the engine calls one once
        // per worker thread, never sharing an evaluator, so no per-frame
        // lock is needed). `local_factories` owns the boxed factory
        // closures (each wraps one frame's already-validated `LnGrid` for
        // channel `p`), `local_refs` is the `&dyn Fn` view `StackParams::local`
        // actually wants. Built even when `ln` is `None` (every entry then
        // comes out `None` too) so the plumbing has one shape regardless —
        // negligible cost, `n` is small.
        let mut local_factories: Vec<Option<Box<LocalNormRowFactory<'_>>>> = Vec::with_capacity(n);
        for (k, &i) in frame_indices.iter().enumerate() {
            let f = &frames[i];
            let frame_ls = f.measurement.channels[p].location_scale();
            let rp = rejection_pair(ref_ls, frame_ls, rejection_mode);
            let op = output_pair(ref_ls, frame_ls, output_mode);
            rejection_pairs.push(rp);
            output_pairs.push(op);
            plane_weights.push(weights[k].get(p).copied().unwrap_or(0.0));
            let grid = ln
                .and_then(|grids| grids.get(i))
                .and_then(|g| g.as_ref())
                .map(|fg| &fg.channels[p]);
            local_factories.push(grid.map(|grid| {
                Box::new(local_norm_row_factory(grid)) as Box<LocalNormRowFactory<'_>>
            }));
        }
        let local_refs: Vec<Option<&LocalNormRowFactory<'_>>> =
            local_factories.iter().map(|o| o.as_deref()).collect();

        src.set_plane(p)?;

        // M3 Task 2: this plane's rejection-bit sink, when the caller asked
        // for one — an owned `Option<RejPlaneSink>` so `StackParams` can
        // borrow a `&dyn RejectionBitSink` from it for the `integrate_stack`
        // call just below.
        let sink = rej.map(|r| r.plane_sink(p));
        // M4c Task 3: and this plane's forced-rejection source, when the
        // caller is running the SECOND pass over a group's processed
        // (`.rejl`) bitmaps — owned here for the same borrow reason.
        let forced_plane = forced.map(|f| f.plane_source(p));
        let params = StackParams {
            rejection: &rejection_pairs,
            output: &output_pairs,
            weights: &plane_weights,
            range_low: input.integration.range_low.map(|v| v as f32),
            range_high: input.integration.range_high.map(|v| v as f32),
            rejection_maps: write_maps,
            local: ln.is_some().then_some(local_refs.as_slice()),
            local_for_rejection,
            local_for_output,
            rejection_bits: sink.as_ref().map(|s| s as &dyn RejectionBitSink),
            // M4c Task 3: the second pass's forced bits, bound to this
            // plane the same way the sink above is.
            forced_rejection: forced_plane.as_ref().map(|f| f as &dyn RejectionBitSource),
            coverage: coverage.count && p == 0,
            empty_as_nan: coverage.empty_as_nan,
        };
        // `EngineProgress` itself is not `Copy` — only its two `&dyn Fn`
        // fields are — so it must be rebuilt (not read) from
        // `progress.engine` each plane, since we only hold
        // `&GroupProgress`, not an owned one.
        let engine_progress = EngineProgress {
            on_band: progress.engine.on_band,
            on_combine: progress.engine.on_combine,
        };
        let out = integrate_stack(&src, &params, recipe, pool, cancel, engine_progress, io)?;
        outputs.push(out);
        output_pairs_out.push(output_pairs);
    }

    // Perf tier A M1 fix wave: this call's own LN row-buffer pool (one
    // `(Vec<f32>, Vec<f32>)` per frame, per WORKER thread) is done growing
    // the moment the last plane above returns — release it now rather than
    // letting it sit resident for the rest of the process. Guarded on `ln`
    // itself (not, say, `local_for_output || local_for_rejection`): a
    // non-LN call's `RowState::local` is always empty and its `Drop` never
    // touches the thread-local (see its own doc), so the broadcast would
    // find nothing to clear — skip the cross-thread round trip entirely.
    if ln.is_some() {
        pool.broadcast(|_| crate::integration::engine::release_ln_row_buffers());
    }

    Ok((outputs, output_pairs_out))
}

/// The min-weight drop's DECISION (spec §6.2): a frame whose lowest
/// per-channel normalized weight sits below `min_weight` never joins the
/// stack; the reference is exempt (dropping it would leave nothing to
/// normalize the rest against). Returns the included frame indices into
/// `frames`, in `frames`' own order.
///
/// Extracted (M3 Task 5) so `stacking::run` can compute the SAME set BEFORE
/// calling [`integrate_group`], to size and order a group's rejection-bitmap
/// set to match what `integrate_group` will actually integrate — the two
/// must never disagree about which frames survive the floor, so this is the
/// only place the rule itself is expressed; both callers pass identical
/// inputs and get an identical answer, deterministically.
pub(crate) fn included_after_min_weight(
    frames: &[StackFrame],
    reference: usize,
    min_weight: f64,
) -> Vec<usize> {
    frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let wmin = f
                .weight
                .normalized
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min);
            (wmin >= min_weight || i == reference).then_some(i)
        })
        .collect()
}

/// How many times [`integrate_planes`] runs for a group (M4c Task 3,
/// ruling R-M4c-4): twice when large-scale rejection is on AND the run
/// created a rejection-bitmap set for the group to derive the structures
/// from (without one there is nothing to filter), once otherwise.
///
/// The ONE place the rule is expressed — `stacking::run` calls it too, to
/// size the Integrate stage's own progress space before `integrate_group`
/// runs, so the two can never disagree about how many plane-slots a group's
/// integration will report.
pub(crate) fn large_scale_passes(integration: &IntegrationConfig, has_rej: bool) -> usize {
    if integration.large_scale.enabled && has_rej {
        2
    } else {
        1
    }
}

/// Filters every included frame's first-pass bitmap to its large-scale
/// structures, writes each as its `.rejl` sibling and reads the set back as
/// the engine's forced-rejection source (M4c Task 3).
///
/// Parallel over FRAMES on the caller's pool, one frame's bitmap in RAM per
/// worker at a time (a 6248x4176 mono bitmap is ≈ 3.3 MB, and
/// [`process_large_scale`] works in two `u8` scratch planes of the same
/// geometry) — never every frame's at once. The `.rejl` files are written
/// rather than kept in memory because they are also what the drizzle stage
/// reads back (the same bits the master was built with) and what a
/// `keepAll` run leaves behind for inspection.
/// `IntegrationError::Cancelled` is returned distinctly — it is the RUN's
/// own cancel, which must propagate rather than degrade into "large-scale
/// rejection skipped"; every other failure comes back as
/// [`IntegrationError::Decode`] carrying the `anyhow` chain, and the caller
/// keeps the first pass's master.
fn build_forced_source(
    rej_set: &RejBitmapSet,
    input: &GroupInput<'_>,
    included_count: usize,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
) -> Result<RejForcedSource, IntegrationError> {
    use rayon::prelude::*;
    let cfg = input.integration.large_scale;
    let (w, h, c) = (input.width, input.height, input.channels);
    pool.install(|| {
        (0..included_count)
            .into_par_iter()
            .map(|k| {
                // Checked per FRAME: the filter is a couple of O(pixels)
                // passes per plane, so this bounds how long a cancel waits
                // to a single frame's own filtering.
                if cancel.load(Ordering::Relaxed) {
                    return Err(IntegrationError::Cancelled);
                }
                let bits = RejBitmap::read(rej_set.path(k), w, h, c)
                    .map_err(|e| IntegrationError::Decode(format!("{e:#}")))?;
                let processed = process_large_scale(&bits, cfg.protected_layers, cfg.growth);
                processed
                    .write(rej_set.processed_path(k).as_path())
                    .map_err(|e| IntegrationError::Decode(format!("{e:#}")))
            })
            .collect::<Result<(), IntegrationError>>()
    })?;
    let paths: Vec<PathBuf> = (0..included_count)
        .map(|k| rej_set.processed_path(k))
        .collect();
    RejForcedSource::load(&paths, w, h, c).map_err(|e| IntegrationError::Decode(format!("{e:#}")))
}

/// Integrates one group, plane by plane (spec §6.1–6.3): resolves the Auto
/// rejection rule, drops any frame below the weight floor, builds the
/// rejection/output normalization pairs from the reference frame's
/// measurement, and runs the weighted engine once per plane through a
/// `RegisteredSource`. `frames.len() ≥ 3` after the drop, or this refuses.
#[allow(clippy::too_many_arguments)]
pub fn integrate_group(
    input: &GroupInput<'_>,
    measure: &MeasureOptions,
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
    progress: &GroupProgress<'_>,
    io: IoPolicy,
) -> Result<GroupOutput, IntegrationError> {
    let group_start = Instant::now();
    let frames = input.frames;
    if frames.len() < 3 {
        return Err(IntegrationError::BadInput(format!(
            "{} frames, need at least 3 to integrate a group",
            frames.len()
        )));
    }
    validate_group_input(input)?;

    // Min-weight drop (spec §6.2): the DECISION lives in
    // `included_after_min_weight` (M3 Task 5) — `stacking::run` calls the
    // same function, with the same inputs, to size and order a group's
    // rejection-bitmap set BEFORE this call ever runs, so the two must never
    // compute two different answers. The per-frame `warn!`s below are this
    // function's own diagnostics over the extracted result, not a second
    // copy of the drop rule itself.
    let min_weight = input.integration.min_weight;
    let included = included_after_min_weight(frames, input.reference, min_weight);
    let mut included_mask = vec![false; frames.len()];
    for &i in &included {
        included_mask[i] = true;
    }
    let mut dropped_below_min_weight = 0usize;
    for (i, f) in frames.iter().enumerate() {
        if !included_mask[i] {
            let wmin = f
                .weight
                .normalized
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min);
            dropped_below_min_weight += 1;
            warn!(
                path = %f.path.display(),
                weight = wmin,
                min_weight,
                "frame dropped below the minimum weight"
            );
        } else if i == input.reference {
            let wmin = f
                .weight
                .normalized
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min);
            if wmin < min_weight {
                warn!(
                    path = %f.path.display(),
                    weight = wmin,
                    min_weight,
                    "reference frame is below the minimum weight but is never dropped"
                );
            }
        }
    }
    if included.len() < 3 {
        return Err(IntegrationError::BadInput(format!(
            "fewer than 3 frames after the weight floor ({})",
            included.len()
        )));
    }
    let included_count = included.len();

    // M3 Task 2: a rejection-bitmap set is sized (and its stems ordered) to
    // the frames the RUN expected to be included at the time it created it
    // (Task 5) — the min-weight drop above must not silently shift that
    // count out from under it, or `RejPlaneSink::record_band` would write
    // bits for a frame index that doesn't exist in `set` (or leave a
    // trailing file no included frame ever fills).
    if let Some(rej_set) = input.rej {
        if rej_set.frames() != included_count {
            return Err(IntegrationError::BadInput(format!(
                "rejection bitmap set has {} frames, {included_count} included after the weight floor",
                rej_set.frames()
            )));
        }
        // Fix round 1, I2: the frame-count check above catches a size
        // mismatch, but a set built for the wrong WIDTH/HEIGHT/CHANNELS
        // (e.g. a stale set from a previous, differently-sized group) would
        // otherwise sail through here — `RejPlaneSink::record_band`'s
        // offset arithmetic runs past its own `set_len`'d end, `write_at`
        // silently EXTENDS the file, and the mistake would only surface
        // much later when `RejBitmap::read` reports a length/geometry
        // mismatch — after the group's whole integration has already been
        // paid for. Refused here instead, before any pixel work runs.
        if rej_set.width() != input.width
            || rej_set.height() != input.height
            || rej_set.channels() != input.channels
        {
            return Err(IntegrationError::BadInput(format!(
                "rejection bitmap set geometry {}x{}x{} != group geometry {}x{}x{}",
                rej_set.width(),
                rej_set.height(),
                rej_set.channels(),
                input.width,
                input.height,
                input.channels
            )));
        }
    }

    // A frame whose own weight vector doesn't match the group's channel
    // count silently weighs zero on the missing planes (see the per-plane
    // loop below) — worth a warn per frame, once, not once per plane.
    for &i in &included {
        let f = &frames[i];
        if f.weight.normalized.len() != input.channels {
            warn!(
                path = %f.path.display(),
                weight_channels = f.weight.normalized.len(),
                channels = input.channels,
                "weight channel count differs from the group; missing channels weigh zero"
            );
        }
    }

    let recipe = IntegrationRecipe {
        combination: input.integration.combination,
        rejection: input.integration.rejection.resolve(included_count),
    };
    info!(frames = included_count, recipe = %recipe.describe(), "group integration started");

    // The included frame with the highest normalized-mean weight (spec
    // §4.4's "best sub"), fixed once for the whole group — `best_by_weight`
    // is the same helper Plan 2's selection stage uses, restricted here to
    // the frames this group actually kept.
    let weight_snapshot: Vec<FrameWeight> = frames.iter().map(|f| f.weight.clone()).collect();
    let included_mask: Vec<bool> = (0..frames.len()).map(|i| included.contains(&i)).collect();
    let star_counts: Vec<usize> = frames.iter().map(|f| f.measurement.min_stars()).collect();
    let best_idx = best_by_weight(&weight_snapshot, &included_mask, &star_counts)
        .expect("included has at least 3 frames");

    let plane_pixels = input.width * input.height;
    let mut data = Vec::with_capacity(plane_pixels * input.channels);
    let mut rejection_low = input
        .integration
        .write_rejection_maps
        .then(|| Vec::with_capacity(plane_pixels * input.channels));
    let mut rejection_high = input
        .integration
        .write_rejection_maps
        .then(|| Vec::with_capacity(plane_pixels * input.channels));

    let mut rejected_low_total = 0u64;
    let mut rejected_high_total = 0u64;
    let mut samples_total = 0u64;
    let mut per_frame_samples = vec![0u64; included_count];
    let mut per_frame_rejected = vec![0u64; included_count];
    let mut read_ms_total = 0u64;
    let mut combine_ms_total = 0u64;
    let mut bytes_read_total = 0u64;

    let mut master_noise = Vec::with_capacity(input.channels);
    let mut master_location = Vec::with_capacity(input.channels);
    let mut master_scale = Vec::with_capacity(input.channels);
    let mut best_sub_noise = Vec::with_capacity(input.channels);
    let mut master_psf_snr = Vec::with_capacity(input.channels);
    let mut best_sub_psf_snr = Vec::with_capacity(input.channels);
    let mut snr_gain = Vec::with_capacity(input.channels);
    let mut master_fwhm_px = Vec::with_capacity(input.channels);
    let mut master_eccentricity = Vec::with_capacity(input.channels);

    // Per-channel weight row for each included frame, in `included` order —
    // the same `.get(p).copied().unwrap_or(0.0)` convention the per-plane
    // loop used inline before this was pulled out into `integrate_planes`
    // (a frame whose own weight vector is short weighs zero on the missing
    // channels, warned above).
    let weights_per_frame: Vec<Vec<f32>> = included
        .iter()
        .map(|&i| {
            let f = &frames[i];
            (0..input.channels)
                .map(|p| f.weight.normalized.get(p).copied().unwrap_or(0.0) as f32)
                .collect()
        })
        .collect();

    // M4c Task 3: with large-scale rejection on, `integrate_planes` runs
    // twice and the plane-progress space is twice as wide — pass 1 reports
    // planes `0..channels`, pass 2 `channels..2·channels`, both against the
    // same doubled total. `stacking::run` derives the SAME total from
    // [`large_scale_passes`], so its own band/combine ticks (which read the
    // plane index this `on_plane` stamps) stay on one scale.
    let passes = large_scale_passes(input.integration, input.rej.is_some());
    let planes_total = input.channels * passes;
    let on_plane_pass1 = |p: usize, _total: usize| (progress.on_plane)(p, planes_total);
    let progress_pass1 = GroupProgress {
        on_plane: &on_plane_pass1,
        engine: EngineProgress {
            on_band: progress.engine.on_band,
            on_combine: progress.engine.on_combine,
        },
    };

    let (mut outputs, mut output_pairs) = integrate_planes(
        input,
        &included,
        &weights_per_frame,
        input.normalization.output,
        input.normalization.rejection,
        // M2 Task 7: `input.ln` carries whatever real per-frame grids
        // `stacking::run` resolved for this group (`None` when local
        // normalization never ran for it at all) — `local_for_output` is
        // `input.normalization.local.enabled` regardless of whether any
        // frame actually has a grid; a frame with none falls back to its
        // global pair either way (see `integrate_planes`'s own doc).
        input.normalization.local.enabled,
        input.ln,
        input.rej,
        None,
        recipe,
        input.integration.write_rejection_maps,
        CoverageOptions {
            count: input.coverage_map || input.stats_on_covered,
            empty_as_nan: false,
        },
        pool,
        cancel,
        &progress_pass1,
        io,
    )?;

    // M4c Task 3, the second pass (spec §6.2, ruling R-M4c-4): every
    // included frame's first-pass bitmap is filtered to its large-scale
    // structures, written as a `.rejl` sibling, and forced out of a fresh
    // integration — whose output REPLACES the first pass's. Pass 1's own
    // I/O cost is folded into the group's totals below, so the group still
    // reports what the run actually paid.
    let mut large_scale_rejected_fraction: Option<f64> = None;
    let mut second_pass_rej_ok = false;
    let mut pass_one_cost = (0u64, 0u64, 0u64);
    if passes > 1 {
        let rej_set = input
            .rej
            .expect("large_scale_passes only returns 2 when a bitmap set exists");
        // B3's latch (M3): a WRITE fault mid-pass-1 means the bitmaps are
        // incomplete, so the structures derived from them would be wrong —
        // the group keeps pass 1's master, with the reason logged here and
        // surfaced as a run warning by `stacking::run` (which sees
        // `large_scale_rejected_fraction: None` against an enabled config).
        if let Some(reason) = rej_set.failure() {
            warn!(
                error = %reason,
                "large-scale rejection skipped: the first pass's rejection bitmaps are incomplete"
            );
        } else {
            match build_forced_source(rej_set, input, included_count, pool, cancel) {
                Ok(forced) => {
                    pass_one_cost = outputs.iter().fold((0, 0, 0), |acc, o| {
                        (
                            acc.0 + o.base.read_duration.as_millis() as u64,
                            acc.1 + o.base.combine_duration.as_millis() as u64,
                            acc.2 + o.base.bytes_read,
                        )
                    });
                    let forced_fraction = forced.forced_fraction();
                    info!(
                        frames = included_count,
                        protected_layers = input.integration.large_scale.protected_layers,
                        growth = input.integration.large_scale.growth,
                        large_scale_forced = forced_fraction,
                        "large-scale rejection: re-integrating with the processed bitmaps"
                    );
                    let channels = input.channels;
                    let on_plane_pass2 =
                        |p: usize, _total: usize| (progress.on_plane)(channels + p, planes_total);
                    let progress_pass2 = GroupProgress {
                        on_plane: &on_plane_pass2,
                        engine: EngineProgress {
                            on_band: progress.engine.on_band,
                            on_combine: progress.engine.on_combine,
                        },
                    };
                    // Fix round 1, ruling R-T3-1: pass 2 gets a sink of its
                    // OWN — a second, freshly-created set under
                    // `rej/run-<id>/<group>/pass2/`. Its bits are the
                    // master's real rejected set (the forced structures the
                    // engine records as rejections, plus whatever pass 2's
                    // own algorithm and range tests rejected), which is
                    // what drizzle must see; the `.rejl` files stay as the
                    // intermediate that produced the forced set. Never a
                    // rewrite of pass 1's own files: `record_band` skips a
                    // frame with no bits in a band, trusting `create`'s
                    // zero-fill, so pass 1's bits would survive wherever
                    // pass 2 had none. A create failure is not fatal — pass
                    // 2 still runs and the master is still correct; only
                    // drizzle loses its input, which `stacking::run` turns
                    // into the same "drizzle skipped for this group"
                    // degradation a pass-1 bitmap failure already gets.
                    //
                    // …but only when something will READ it: the Drizzle
                    // stage is the one consumer, so a standalone
                    // large-scale run (drizzle off) writes no third set at
                    // all — see `GroupInput::second_pass_bitmaps`.
                    let second_set = if !input.second_pass_bitmaps {
                        None
                    } else {
                        match rej_set.create_second_pass() {
                            Ok(set) => Some(set),
                            Err(e) => {
                                warn!(
                                    error = %format!("{e:#}"),
                                    "the second pass's rejection bitmaps could not be created; \
                                     the master is unaffected"
                                );
                                None
                            }
                        }
                    };
                    let (second, pairs) = integrate_planes(
                        input,
                        &included,
                        &weights_per_frame,
                        input.normalization.output,
                        input.normalization.rejection,
                        input.normalization.local.enabled,
                        input.ln,
                        second_set.as_ref(),
                        Some(&forced),
                        recipe,
                        input.integration.write_rejection_maps,
                        CoverageOptions {
                            count: input.coverage_map || input.stats_on_covered,
                            empty_as_nan: false,
                        },
                        pool,
                        cancel,
                        &progress_pass2,
                        io,
                    )?;
                    // A WRITE fault mid-pass-2 latches the same way pass
                    // 1's does, and makes that set untrustworthy for the
                    // same reason.
                    second_pass_rej_ok = match second_set.as_ref().and_then(|s| s.failure()) {
                        Some(reason) => {
                            warn!(
                                error = %reason,
                                "the second pass's rejection bitmaps are incomplete; \
                                 the master is unaffected"
                            );
                            false
                        }
                        None => second_set.is_some(),
                    };
                    outputs = second;
                    output_pairs = pairs;
                    large_scale_rejected_fraction = Some(forced_fraction);
                }
                // The run's own cancel propagates; anything else keeps the
                // first pass's master with a warning (`stacking::run`
                // turns the `None` fraction into a run warning).
                Err(IntegrationError::Cancelled) => {
                    warn!("group integration cancelled");
                    return Err(IntegrationError::Cancelled);
                }
                Err(e) => warn!(
                    error = %e,
                    "large-scale rejection skipped: the rejection bitmaps could not be processed"
                ),
            }
        }
    }
    read_ms_total += pass_one_cost.0;
    combine_ms_total += pass_one_cost.1;
    bytes_read_total += pass_one_cost.2;

    let mut coverage: Option<Vec<f32>> = None;
    let mut measure_rect: Option<crate::stacking::crop::CropRect> = None;
    for (p, mut out) in outputs.into_iter().enumerate() {
        if p == 0 {
            coverage = out.coverage.take();
        }
        // `integrate_planes` already checks `cancel` before each plane's own
        // engine work; this check stops the tail at the next plane boundary
        // the way the pre-extraction single loop did, and the master
        // measurement after this loop checks it again before each plane.
        if cancel.load(Ordering::Relaxed) {
            warn!(plane = p, "group integration cancelled");
            return Err(IntegrationError::Cancelled);
        }
        // `integrate_planes` no longer exposes a per-plane wall-clock split
        // (it opens ONE `RegisteredSource` for the group — ruling R-T4-7 —
        // and runs every plane's `set_plane` + `integrate_stack` before
        // this tail even starts), so the logged `duration_ms` below is
        // now `out.base`'s own read+combine time plus this tail's own
        // elapsed time — the dominant costs, not the source's
        // (comparatively negligible) setup — rather than the true
        // open-to-tail span the pre-extraction code measured.
        let stats_start = Instant::now();

        read_ms_total += out.base.read_duration.as_millis() as u64;
        combine_ms_total += out.base.combine_duration.as_millis() as u64;
        bytes_read_total += out.base.bytes_read;

        data.extend_from_slice(&out.base.data);
        if let Some(acc) = rejection_low.as_mut() {
            acc.extend_from_slice(
                out.rejection_low
                    .as_deref()
                    .expect("engine returns maps when rejection_maps is set"),
            );
        }
        if let Some(acc) = rejection_high.as_mut() {
            acc.extend_from_slice(
                out.rejection_high
                    .as_deref()
                    .expect("engine returns maps when rejection_maps is set"),
            );
        }

        rejected_low_total += out.rejected_low;
        rejected_high_total += out.rejected_high;
        let plane_samples: u64 = out.samples_per_frame.iter().sum();
        samples_total += plane_samples;
        for k in 0..included_count {
            per_frame_samples[k] += out.samples_per_frame.get(k).copied().unwrap_or(0);
            per_frame_rejected[k] += out.rejected_per_frame.get(k).copied().unwrap_or(0);
        }

        // Master measurement (spec §6) runs after this loop, every plane at
        // once (below); its rectangle is chosen here, on plane 0.
        // Plan 6b-3 D4 refinement (review I1, final review A1): on a grid the
        // statistics come from the largest rectangle every pixel of which at
        // least half the group's frames cover (`k = ceil(n / 2)`; plane 0's
        // count, the planes share the footprint), so the deep centre of the
        // stack is what they describe and not the one-frame fringe a wide
        // offset frame adds. When that rectangle is empty or under
        // [`MIN_STATS_RECT_PX`] on a side the measurement falls back to
        // `k = 1` (a `warn!` names both sizes). The group then fails,
        // visibly, when no frame covers the grid (the master would be all
        // zeros) or when the chosen rectangle is under [`MIN_STATS_RECT_PX`]:
        // whole-plane statistics would read the uncovered 0.0 as data.
        if p == 0 && input.stats_on_covered {
            let big_enough = |r: &crate::stacking::crop::CropRect| {
                r.width >= MIN_STATS_RECT_PX && r.height >= MIN_STATS_RECT_PX
            };
            let half = u32::try_from(included_count.div_ceil(2))
                .unwrap_or(u32::MAX)
                .max(1);
            let rect_at = |k: u32| {
                coverage.as_deref().and_then(|c| {
                    crate::stacking::crop::largest_covered_rect(c, input.width, input.height, k)
                })
            };
            let mut depth = half;
            measure_rect = rect_at(half).filter(big_enough);
            if measure_rect.is_none() {
                let one = rect_at(1);
                if let (true, Some(fallback)) = (half > 1, one) {
                    let deep = rect_at(half);
                    warn!(
                        count = half,
                        width = deep.map_or(0, |r| r.width),
                        height = deep.map_or(0, |r| r.height),
                        crop_width = fallback.width,
                        crop_height = fallback.height,
                        "statistics rectangle too small at half coverage; measured at one frame"
                    );
                }
                depth = 1;
                measure_rect = one;
            }
            match measure_rect {
                Some(r) if big_enough(&r) => {
                    debug!(
                        crop_x0 = r.x0,
                        crop_y0 = r.y0,
                        crop_width = r.width,
                        crop_height = r.height,
                        count = depth,
                        "master statistics on the covered rectangle"
                    )
                }
                Some(r) => {
                    return Err(IntegrationError::BadInput(format!(
                        "the covered part of the grid is only {} x {} px, below the {} x {} px \
                         minimum for the master's statistics",
                        r.width, r.height, MIN_STATS_RECT_PX, MIN_STATS_RECT_PX
                    )));
                }
                None => {
                    return Err(IntegrationError::BadInput(
                        "no frame of this group covers the grid".to_string(),
                    ));
                }
            }
        }
        let plane_duration_ms = (out.base.read_duration
            + out.base.combine_duration
            + stats_start.elapsed())
        .as_millis() as u64;
        // Perf tier A Task 0 (audit §3.3, item I11): the plane's total
        // pixel-stack count — every combine call, one per output pixel —
        // is what `rejection_iters_total`/`medfit_evals_total` divide by
        // for the means below.
        let plane_pixels = (out.base.width * out.base.height).max(1) as f64;
        debug!(
            plane = p,
            rejected_low = out.rejected_low,
            rejected_high = out.rejected_high,
            read_ms = out.base.read_duration.as_millis() as u64,
            combine_ms = out.base.combine_duration.as_millis() as u64,
            combine_cpu_ms = out.base.combine_cpu_duration.as_millis() as u64,
            rejection_iters_mean = out.base.rejection_iters_total as f64 / plane_pixels,
            medfit_evals_mean = out.base.medfit_evals_total as f64 / plane_pixels,
            duration_ms = plane_duration_ms,
            "plane integrated"
        );
    }

    // Master measurement (spec §6), every plane through the one multi-plane
    // entry (Plan A Task 8 fix wave, owner ruling 2026-10-09): a 3-plane
    // master is a debayered OSC group's (Q3) and gets the cross-plane rule,
    // so every plane's verdict pass runs before any plane's statistics —
    // hence here, after the plane loop, on `data` (on the covered rectangle
    // when one was chosen). The entry wants
    // `Option<&Arc<rayon::ThreadPool>>`, which the group driver's plain
    // `&rayon::ThreadPool` can't supply — it runs inside `pool.install`
    // instead so its own parallel work still lands on the caller's pool,
    // with `None` for the explicit handle. `cancel` is checked before each
    // plane is handed over.
    let plane_len = input.width * input.height;
    let crop = measure_rect.filter(|r| !r.is_full(input.width, input.height));
    let (measure_w, measure_h) = crop.map_or((input.width, input.height), |r| (r.width, r.height));
    // FM-28: the master is always measured at All. `fitStars` is a per-frame speed control; the master is one
    // high-SNR image, and its FWHM / eccentricity / star count must stay comparable across K.
    let master_measure = MeasureOptions {
        fit_stars: None,
        ..measure.clone()
    };
    let measured = pool.install(|| {
        measure_planes_diagnosed(
            input.channels,
            measure_w,
            measure_h,
            &master_measure,
            None,
            SeedOverride::Fast,
            false,
            // A master is integrated / drizzled: its pixels are mixed (ruling C-10).
            crate::star_measure::PixelNoise::Mixed,
            None,
            |p| {
                if cancel.load(Ordering::Relaxed) {
                    warn!(plane = p, "group integration cancelled");
                    return Err(IntegrationError::Cancelled);
                }
                let plane_data = &data[p * plane_len..(p + 1) * plane_len];
                Ok(match crop {
                    Some(r) => Cow::Owned(crate::stacking::crop::crop_plane(
                        plane_data,
                        input.width,
                        r,
                    )),
                    None => Cow::Borrowed(plane_data),
                })
            },
        )
    })?;
    for (p, measured) in measured.iter().enumerate() {
        let cm = &measured.measurement.channel;
        master_noise.push(cm.noise);
        master_location.push(cm.location);
        master_scale.push(cm.scale);
        master_fwhm_px.push(cm.fwhm_px);
        master_eccentricity.push(cm.eccentricity);
        master_psf_snr.push(cm.psf_snr);

        let best_ch = &frames[best_idx].measurement.channels[p];
        best_sub_noise.push(best_ch.noise);
        best_sub_psf_snr.push(best_ch.psf_snr);
        debug!(
            plane = p,
            duration_ms = measured.wall_ms,
            "master plane measured"
        );
    }

    snr_gain.extend(paired_snr_gain(
        input,
        &frames[best_idx],
        &data,
        measure_rect.unwrap_or(crate::stacking::crop::CropRect::full(
            input.width,
            input.height,
        )),
        &master_measure,
        pool,
        cancel,
    )?);

    let rejected_fraction_per_frame: Vec<f64> = (0..included_count)
        .map(|k| {
            let s = per_frame_samples[k];
            if s == 0 {
                0.0
            } else {
                per_frame_rejected[k] as f64 / s as f64
            }
        })
        .collect();

    let total_exposure_s: f64 = included.iter().map(|&i| frames[i].exposure_s).sum();
    let weighted_exposure_s: f64 = included
        .iter()
        .map(|&i| frames[i].exposure_s * frames[i].weight.normalized_mean)
        .sum();

    let rejected_low_fraction = rejected_low_total as f64 / samples_total.max(1) as f64;
    let rejected_high_fraction = rejected_high_total as f64 / samples_total.max(1) as f64;

    info!(
        frames = included_count,
        planes = input.channels,
        rejected_fraction = rejected_low_fraction + rejected_high_fraction,
        duration_ms = group_start.elapsed().as_millis() as u64,
        "group integration finished"
    );

    // M2 Task 7: `input.ln` is `Some` only when `stacking::run` resolved
    // real per-frame grids for this group (see `GroupInput.ln`'s own doc) —
    // count how many of the INCLUDED frames (post min-weight drop) actually
    // carried one.
    let ln_frames = input
        .ln
        .map(|grids| {
            included
                .iter()
                .filter(|&&i| grids.get(i).is_some_and(|g| g.is_some()))
                .count()
        })
        .unwrap_or(0);

    Ok(GroupOutput {
        width: input.width,
        height: input.height,
        channels: input.channels,
        data,
        rejection_low,
        rejection_high,
        included,
        output_pairs,
        second_pass_rej_ok,
        // The count taken only for the statistics is never a coverage map.
        coverage: if input.coverage_map { coverage } else { None },
        measure_rect,
        stats: GroupStats {
            frames: frames.len(),
            included: included_count,
            dropped_below_min_weight,
            recipe: recipe.describe(),
            rejected_low_fraction,
            rejected_high_fraction,
            rejected_fraction_per_frame,
            master_noise,
            master_location,
            master_scale,
            best_sub_noise,
            master_psf_snr,
            best_sub_psf_snr,
            snr_gain,
            snr_gain_version: SNR_GAIN_VERSION,
            master_fwhm_px,
            master_eccentricity,
            weighted_exposure_s,
            total_exposure_s,
            read_ms: read_ms_total,
            combine_ms: combine_ms_total,
            bytes_read: bytes_read_total,
            ln_frames,
            large_scale_rejected_fraction,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::{Linear, LinearKind};
    use crate::integration::storage_class::StorageClass;
    use crate::stacking::measure::{measure_frame, measure_plane, ChannelMeasurement};
    use crate::stacking::rej::RejBitmap;
    use crate::test_support::{add_noise, centroid, gaussian_field};

    fn pool() -> Arc<rayon::ThreadPool> {
        Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
        )
    }

    fn io(band_budget_bytes: usize) -> IoPolicy {
        IoPolicy {
            band_budget_bytes,
            read_concurrency: 2,
            storage: StorageClass::Local,
            prefetch_local: true,
        }
    }

    fn identity_map() -> PixelMap {
        PixelMap::linear(Linear::identity()).unwrap()
    }

    fn shift_map(dx: f64, dy: f64) -> PixelMap {
        let fwd = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, -dx], [0.0, 1.0, -dy], [0.0, 0.0, 1.0]],
        };
        PixelMap::linear(fwd).unwrap()
    }

    fn weight(w: f64, channels: usize) -> FrameWeight {
        FrameWeight {
            channels: vec![w; channels],
            normalized: vec![w; channels],
            mean: w,
            normalized_mean: w,
            missing: None,
        }
    }

    fn dummy_measurement(channels: usize) -> FrameMeasurement {
        FrameMeasurement {
            width: 10,
            height: 10,
            channels: vec![ChannelMeasurement::default(); channels],
            duration_ms: 0,
        }
    }

    fn nop_plane() -> impl Fn(usize, usize) {
        |_, _| {}
    }
    fn nop_band() -> impl Fn(usize, usize, u64, u64) {
        |_, _, _, _| {}
    }

    #[test]
    fn auto_rule_follows_the_group_size() {
        assert_eq!(
            RejectionChoice::Auto.resolve(3),
            Rejection::PercentileClip {
                low: 0.2,
                high: 0.1
            }
        );
        assert_eq!(
            RejectionChoice::Auto.resolve(7),
            Rejection::PercentileClip {
                low: 0.2,
                high: 0.1
            }
        );
        assert_eq!(
            RejectionChoice::Auto.resolve(8),
            Rejection::WinsorizedSigma {
                sigma_low: 4.0,
                sigma_high: 3.0
            }
        );
        assert_eq!(
            RejectionChoice::Auto.resolve(19),
            Rejection::WinsorizedSigma {
                sigma_low: 4.0,
                sigma_high: 3.0
            }
        );
        assert_eq!(
            RejectionChoice::Auto.resolve(20),
            Rejection::LinearFitClip {
                sigma_low: 5.0,
                sigma_high: 3.5
            }
        );
        assert_eq!(
            RejectionChoice::Auto.resolve(208),
            Rejection::LinearFitClip {
                sigma_low: 5.0,
                sigma_high: 3.5
            }
        );
        assert_eq!(
            RejectionChoice::SigmaClip {
                sigma_low: 2.0,
                sigma_high: 2.5
            }
            .resolve(500),
            Rejection::SigmaClip {
                sigma_low: 2.0,
                sigma_high: 2.5
            }
        );
    }

    /// M4c Task 1 (ruling R-M4c-1): min/max, ESD and RCR are USER choices.
    /// Each new arm passes its own parameters straight through, and the Auto
    /// ladder above never resolves to one of them at any group size.
    #[test]
    fn the_new_rejection_choices_pass_through_and_auto_never_selects_them() {
        assert_eq!(
            RejectionChoice::MinMax { low: 1, high: 1 }.resolve(200),
            Rejection::MinMax { low: 1, high: 1 }
        );
        assert_eq!(
            RejectionChoice::Esd {
                outliers_fraction: 0.3,
                alpha: 0.05,
                low_relaxation: 1.5
            }
            .resolve(64),
            Rejection::Esd {
                outliers_fraction: 0.3,
                alpha: 0.05,
                low_relaxation: 1.5
            }
        );
        assert_eq!(
            RejectionChoice::Rcr { limit: 0.5 }.resolve(9),
            Rejection::Rcr { limit: 0.5 }
        );
        for n in [1usize, 2, 3, 7, 8, 19, 20, 64, 208, 1000] {
            assert!(
                matches!(
                    RejectionChoice::Auto.resolve(n),
                    Rejection::PercentileClip { .. }
                        | Rejection::WinsorizedSigma { .. }
                        | Rejection::LinearFitClip { .. }
                ),
                "Auto must not select a M4c algorithm (n = {n})"
            );
        }
    }

    /// camelCase on the wire with the R-M4c-1 defaults, and a round trip
    /// through `serde_json` for each new arm.
    #[test]
    fn the_new_rejection_choices_use_the_spec_wire_names() {
        let c: RejectionChoice =
            serde_json::from_str(r#"{"method":"minMax","low":1,"high":1}"#).unwrap();
        assert_eq!(c, RejectionChoice::MinMax { low: 1, high: 1 });
        let c: RejectionChoice = serde_json::from_str(
            r#"{"method":"esd","outliersFraction":0.3,"alpha":0.05,"lowRelaxation":1.5}"#,
        )
        .unwrap();
        assert_eq!(
            c,
            RejectionChoice::Esd {
                outliers_fraction: 0.3,
                alpha: 0.05,
                low_relaxation: 1.5
            }
        );
        let c: RejectionChoice = serde_json::from_str(r#"{"method":"rcr","limit":0.5}"#).unwrap();
        assert_eq!(c, RejectionChoice::Rcr { limit: 0.5 });
        for choice in [
            RejectionChoice::MinMax { low: 1, high: 1 },
            RejectionChoice::Esd {
                outliers_fraction: 0.3,
                alpha: 0.05,
                low_relaxation: 1.5,
            },
            RejectionChoice::Rcr { limit: 0.5 },
        ] {
            let v = serde_json::to_value(choice).unwrap();
            assert_eq!(
                serde_json::from_value::<RejectionChoice>(v.clone()).unwrap(),
                choice,
                "{v}"
            );
        }
    }

    #[test]
    fn config_serde_names_match_the_spec() {
        let d: IntegrationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(d.combination, Combination::Average);
        assert_eq!(d.rejection, RejectionChoice::Auto);
        assert_eq!(d.min_weight, 0.005);
        assert_eq!(d.range_low, Some(0.0));
        assert_eq!(d.range_high, None);
        assert!(!d.write_rejection_maps);
        assert_eq!(d.large_scale, LargeScaleRejection::default());
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["rejection"]["method"], "auto");
        assert_eq!(j["minWeight"], 0.005);
        assert_eq!(j["writeRejectionMaps"], false);
        // M4c Task 3 (spec §9.2, ruling R-M4c-4): one `enabled`, not the
        // spec's original `low`/`high` pair — the bitmap is one bit per
        // pixel and cannot carry the side (see `LargeScaleRejection`).
        assert_eq!(j["largeScale"]["enabled"], false);
        assert_eq!(j["largeScale"]["protectedLayers"], 2);
        assert_eq!(j["largeScale"]["growth"], 2);
        let with_large: IntegrationConfig = serde_json::from_str(
            r#"{"largeScale":{"enabled":true,"protectedLayers":3,"growth":1}}"#,
        )
        .unwrap();
        assert_eq!(
            with_large.large_scale,
            LargeScaleRejection {
                enabled: true,
                protected_layers: 3,
                growth: 1
            }
        );
        let explicit: IntegrationConfig = serde_json::from_str(
            r#"{"rejection":{"method":"linearFitClip","sigmaLow":5.0,"sigmaHigh":3.5},"rangeHigh":0.98}"#,
        )
        .unwrap();
        assert_eq!(
            explicit.rejection,
            RejectionChoice::LinearFitClip {
                sigma_low: 5.0,
                sigma_high: 3.5
            }
        );
        assert_eq!(explicit.range_high, Some(0.98));
        let n: NormalizationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(
            serde_json::to_value(&n).unwrap()["output"],
            "additiveWithScaling"
        );
        assert_eq!(
            serde_json::to_value(&n).unwrap()["rejection"],
            "scaleZeroOffset"
        );
        assert_eq!(serde_json::to_value(&n).unwrap()["scaleEstimator"], "bwmv");
        assert_eq!(n.local, LocalNormalizationConfig::default());
        let nv = serde_json::to_value(&n).unwrap();
        assert_eq!(nv["local"]["enabled"], false);
        assert_eq!(nv["local"]["scale"], 1024);
        assert_eq!(nv["local"]["scaleMode"], "auto");
        assert_eq!(nv["local"]["gridSize"], 4);
        assert_eq!(nv["local"]["referenceFrames"], 20);
        assert_eq!(nv["local"]["psfModel"], "auto");
        assert_eq!(nv["local"]["localScale"], false);
        let with_local: NormalizationConfig = serde_json::from_str(
            r#"{"local":{"enabled":true,"scale":512,"referenceFrames":8,"psfModel":"moffat4","localScale":true}}"#,
        )
        .unwrap();
        assert_eq!(
            with_local.local,
            LocalNormalizationConfig {
                enabled: true,
                scale: 512,
                // A document stored without `scaleMode` decodes to `None`
                // here; `resolve_config` fills it (L-9).
                scale_mode: None,
                grid_size: 4,
                reference_frames: 8,
                psf_model: crate::stacking::psf_signal::PsfModel::Moffat4,
                local_scale: true,
                star_selection: LnStarSelection::default(),
                // 2026-10-06: F1 growth curve is the default (L-24).
                flux_estimator: LnFluxEstimator::CommonAperture,
                aperture: LnAperture::GrowthCurve,
            }
        );
    }

    /// 2026-10-06 (L-24): a stored `normalization.local` with neither
    /// `fluxEstimator` nor `aperture` resolves to F1 at the growth curve; a
    /// document that names `psfSignal` keeps it.
    #[test]
    fn a_stored_local_document_without_an_estimator_decodes_to_f1_growth_curve() {
        let bare: NormalizationConfig =
            serde_json::from_str(r#"{"local":{"enabled":true,"scale":512}}"#).unwrap();
        assert_eq!(bare.local.flux_estimator, LnFluxEstimator::CommonAperture);
        assert_eq!(bare.local.aperture, LnAperture::GrowthCurve);
        let psf: NormalizationConfig = serde_json::from_str(
            r#"{"local":{"enabled":true,"fluxEstimator":"psfSignal"}}"#,
        )
        .unwrap();
        assert_eq!(psf.local.flux_estimator, LnFluxEstimator::PsfSignal);
    }

    /// M2 fix round 1, item 1(b): `integrate_group` no longer refuses
    /// `normalization.rejection == Local` — the M1 guard predates the real
    /// `GroupInput.ln` conduit (M2 Task 7) and `integrate_planes` already
    /// derives `local_for_rejection` from `rejection_mode` and validates
    /// every grid's shape up front, so there is nothing left for this
    /// function to refuse. A real 4-frame group, every frame carrying a
    /// (trivial, identity `A=1`/`B=0`) grid, integrates cleanly with
    /// `Local` rejection normalization selected, and `GroupStats.ln_frames`
    /// counts every included frame.
    #[test]
    fn a_local_rejection_group_integrates() {
        const W: usize = 96;
        const H: usize = 64;
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|i| {
                let mut d = vec![0.1f32; W * H];
                add_noise(&mut d, 0.003, 950 + i as u64);
                let p = dir.path().join(format!("f{i}.fits"));
                write_fits_f32(&p, W, H, 1, &d, &[]).unwrap();
                p
            })
            .collect();
        let measurements: Vec<_> = paths
            .iter()
            .map(|p| {
                measure_frame(p, &MeasureOptions::default(), None, &AtomicBool::new(false)).unwrap()
            })
            .collect();
        let frames: Vec<StackFrame> = paths
            .iter()
            .cloned()
            .zip(measurements)
            .map(|(path, measurement)| StackFrame {
                path,
                map: identity_map(),
                measurement,
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900002,
                registered_path: None,
            })
            .collect();

        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig {
            rejection: RejectionNormalization::Local,
            ..NormalizationConfig::default()
        };
        // A grid per frame, every channel identity (`A = 1`, `B = 0`) — this
        // test is about the M1 refusal being gone, not about LN's own
        // numerical behaviour (Task 6's engine tests already cover that).
        let grids: Vec<Option<LnFrameGrids>> = (0..frames.len())
            .map(|_| {
                Some(LnFrameGrids {
                    channels: vec![LnGrid::constant(W, H, 128, 1.0, 0.0)],
                })
            })
            .collect();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: Some(&grids),
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let out = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap();
        assert_eq!(out.stats.included, 4, "{:?}", out.stats);
        assert_eq!(
            out.stats.ln_frames, out.stats.included,
            "every included frame carried a grid: {:?}",
            out.stats
        );
    }

    /// Fix round 1, item 4: an LN grid whose `ref_width`/`ref_height` don't
    /// match the group's declared geometry must be refused up front — before
    /// `RegisteredSource::open` or any rayon worker ever reads it —
    /// `LnGrid::evaluate_row_into` only `assert!`s the mismatch, which would
    /// otherwise panic inside a worker thread the first time this frame's
    /// row got evaluated. Calls `integrate_planes` directly (no file I/O:
    /// the validation runs before any path is opened).
    #[test]
    fn a_mismatched_ln_grid_geometry_is_refused_before_any_worker_runs() {
        let frames = vec![
            StackFrame {
                path: PathBuf::from("a.fits"),
                map: identity_map(),
                measurement: dummy_measurement(1),
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900003,
                registered_path: None,
            },
            StackFrame {
                path: PathBuf::from("b.fits"),
                map: identity_map(),
                measurement: dummy_measurement(1),
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900004,
                registered_path: None,
            },
        ];
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: 20,
            height: 20,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        // Frame 1's grid is built for a 10x10 reference geometry — the
        // group above declares 20x20.
        let bad_grid = LnGrid::constant(10, 10, 128, 1.0, 0.0);
        let grids: Vec<Option<LnFrameGrids>> = vec![
            None,
            Some(LnFrameGrids {
                channels: vec![bad_grid],
            }),
        ];
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let result = integrate_planes(
            &input,
            &[0, 1],
            &[vec![1.0], vec![1.0]],
            OutputNormalization::default(),
            RejectionNormalization::default(),
            false,
            Some(&grids),
            None,
            None,
            IntegrationRecipe::average(Rejection::None),
            false,
            CoverageOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(1_000_000),
        );
        match result {
            Err(IntegrationError::BadInput(msg)) => {
                assert!(msg.contains("10x10"), "{msg}");
                // A5: `msg.contains('1')` was vacuous — "10x10" itself
                // contains '1'. Assert the frame index text precisely
                // instead ("frame 1 (" — `format!("frame {i} (…")`).
                assert!(
                    msg.contains("frame 1 ("),
                    "error should name frame 1 specifically: {msg}"
                );
            }
            Err(other) => panic!("expected BadInput, got {other:?}"),
            Ok(_) => panic!("expected the mismatched grid to be refused"),
        }
    }

    #[test]
    fn frames_below_min_weight_are_dropped_and_fewer_than_three_is_refused() {
        // 96x64: small enough to measure fast, large enough that the
        // background-mesh model (128px cell) degrades gracefully rather
        // than erroring — this test only cares about the min-weight gate,
        // not registration or normalization accuracy, so a flat noisy field
        // (no stars) is enough.
        const W: usize = 96;
        const H: usize = 64;
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|i| {
                let mut d = vec![0.1f32; W * H];
                add_noise(&mut d, 0.003, 900 + i as u64);
                let p = dir.path().join(format!("f{i}.fits"));
                write_fits_f32(&p, W, H, 1, &d, &[]).unwrap();
                p
            })
            .collect();
        let measurements: Vec<_> = paths
            .iter()
            .map(|p| {
                measure_frame(p, &MeasureOptions::default(), None, &AtomicBool::new(false)).unwrap()
            })
            .collect();

        let build = |norm_weights: [f64; 4]| -> Vec<StackFrame> {
            paths
                .iter()
                .cloned()
                .zip(measurements.iter().cloned())
                .zip(norm_weights)
                .map(|((path, measurement), w)| StackFrame {
                    path,
                    map: identity_map(),
                    measurement,
                    weight: weight(w, 1),
                    exposure_s: 60.0,
                    date_obs: None,
                    frame_id: 900005,
                    registered_path: None,
                })
                .collect()
        };

        let integration = IntegrationConfig::default(); // min_weight = 0.005
        let normalization = NormalizationConfig::default();
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };

        // 1.0 / 0.8 / 0.5 / 0.001 — the last drops, three remain.
        let frames = build([1.0, 0.8, 0.5, 0.001]);
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let out = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap();
        assert_eq!(out.included, vec![0, 1, 2]);
        assert_eq!(out.stats.included, 3);
        assert_eq!(out.stats.dropped_below_min_weight, 1);

        // 1.0 / 0.001 / 0.001 / 0.001 — only the reference clears the floor.
        let frames = build([1.0, 0.001, 0.001, 0.001]);
        let input = GroupInput {
            frames: &frames,
            ..input
        };
        let err = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap_err();
        match err {
            IntegrationError::BadInput(msg) => assert!(msg.contains("fewer than 3"), "{msg}"),
            other => panic!("expected BadInput, got {other:?}"),
        }
    }

    /// A stats document stored before v0.7.0 has no `snrGainVersion` key: it
    /// reads `0`, so its old power-ratio `snrGain` is told apart from a gain
    /// measured now (the Results panel shows "—" for it). A current document
    /// round-trips its version.
    #[test]
    fn a_pre_v070_stats_document_reads_snr_gain_version_0() {
        let legacy = r#"{"frames":208,"included":208,"droppedBelowMinWeight":0,"recipe":"r",
            "rejectedLowFraction":0.0,"rejectedHighFraction":0.0,"rejectedFractionPerFrame":[],
            "masterNoise":[0.001],"masterLocation":[0.1],"masterScale":[0.01],"bestSubNoise":[0.01],
            "masterPsfSnr":[5305.74],"bestSubPsfSnr":[7.0557],"snrGain":[751.98],
            "masterFwhmPx":[2.5],"masterEccentricity":[0.4],"weightedExposureS":1.0,
            "totalExposureS":1.0,"readMs":0,"combineMs":0,"bytesRead":0,"lnFrames":0}"#;
        let old: GroupStats = serde_json::from_str(legacy).unwrap();
        assert_eq!(old.snr_gain_version, 0);
        assert_eq!(old.snr_gain, vec![751.98]);

        let mut now = old;
        now.snr_gain_version = SNR_GAIN_VERSION;
        let json = serde_json::to_string(&now).unwrap();
        assert!(json.contains(r#""snrGainVersion":1"#), "{json}");
        let back: GroupStats = serde_json::from_str(&json).unwrap();
        assert_eq!(back.snr_gain_version, SNR_GAIN_VERSION);
    }

    /// The SNR gain is an SNR factor, measured like for like (2026-10-25:
    /// set 109 read 751.98× for 208 frames). Eight equal frames — same
    /// noise, same weight, sub-pixel dithers — integrated through the
    /// smoothing cubic B-spline the registration defaults to: averaging N
    /// equal frames raises the SNR by √N, and the gain must land there. The
    /// old figure (a PSF SNR ratio, i.e. a power ratio, of the warped
    /// master over the raw best sub) is far past even N here.
    #[test]
    fn snr_gain_of_n_equal_frames_is_about_root_n() {
        const FW: usize = 400;
        const FH: usize = 300;
        const BG: f32 = 0.08;
        const NOISE: f32 = 0.01;
        let stars: Vec<(f64, f64, f64)> = (0..3)
            .flat_map(|j| {
                (0..4).map(move |i| (50.0 + i as f64 * 100.0, 50.0 + j as f64 * 100.0, 0.25))
            })
            .collect();
        let shifts: [(f64, f64); 8] = [
            (0.0, 0.0),
            (0.5, 0.5),
            (1.3, -0.7),
            (-2.25, 1.6),
            (0.8, 2.4),
            (-1.1, -1.9),
            (2.7, 0.35),
            (-0.45, -2.6),
        ];
        let dir = tempfile::tempdir().unwrap();
        let frames: Vec<StackFrame> = shifts
            .iter()
            .enumerate()
            .map(|(i, &(dx, dy))| {
                let shifted: Vec<(f64, f64, f64)> =
                    stars.iter().map(|&(x, y, a)| (x + dx, y + dy, a)).collect();
                let mut data = gaussian_field(FW, FH, &shifted, 1.8, BG);
                add_noise(&mut data, NOISE, 700 + i as u64);
                let path = dir.path().join(format!("n{i}.fits"));
                write_fits_f32(&path, FW, FH, 1, &data, &[]).unwrap();
                let map = if i == 0 {
                    identity_map()
                } else {
                    shift_map(dx, dy)
                };
                let measurement = measure_frame(
                    &path,
                    &MeasureOptions::default(),
                    None,
                    &AtomicBool::new(false),
                )
                .unwrap();
                StackFrame {
                    path,
                    map,
                    measurement,
                    weight: weight(1.0, 1),
                    exposure_s: 60.0,
                    date_obs: None,
                    frame_id: 910_000 + i as i64,
                    registered_path: None,
                }
            })
            .collect();

        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: FW,
            height: FH,
            channels: 1,
            interpolation: Interpolation::BicubicBSpline,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let out = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool(),
            &AtomicBool::new(false),
            &progress,
            io(40_000_000),
        )
        .unwrap();
        assert_eq!(out.stats.included, 8);
        assert_eq!(out.stats.snr_gain_version, SNR_GAIN_VERSION);
        let gain = out.stats.snr_gain[0];
        let root_n = 8f64.sqrt();
        let old = out.stats.master_psf_snr[0] / out.stats.best_sub_psf_snr[0];
        assert!(
            gain > 0.75 * root_n && gain < 1.25 * root_n,
            "gain {gain}, √N {root_n} (the old power ratio of the warped master over the raw sub: {old})"
        );
        assert!(old > 8.0, "the old figure overshot even N: {old}");
    }

    const SHIFTED_EXPOSURES: [f64; 3] = [60.0, 90.0, 120.0];
    const SHIFTED_WEIGHTS: [f64; 3] = [1.0, 0.8, 0.6];

    /// The three shifted frames of the level test: (0,0), (2.4,-1.3) and a
    /// (-5,3)-shifted frame at twice the level, 400x300, written to `dir`.
    fn three_shifted_frames(dir: &std::path::Path) -> Vec<StackFrame> {
        const FW: usize = 400;
        const FH: usize = 300;
        const BG: f32 = 0.08;
        const SIGMA: f64 = 1.8;
        const NOISE: f32 = 0.003;
        let stars: Vec<(f64, f64, f64)> = (0..3)
            .flat_map(|j| {
                (0..4).map(move |i| (50.0 + i as f64 * 100.0, 50.0 + j as f64 * 100.0, 0.25))
            })
            .collect();
        let write_frame =
            |name: &str, dx: f64, dy: f64, scale: f64, seed: u64| -> (PathBuf, PixelMap) {
                let shifted: Vec<(f64, f64, f64)> = stars
                    .iter()
                    .map(|&(x, y, a)| (x + dx, y + dy, a * scale))
                    .collect();
                let mut data = gaussian_field(FW, FH, &shifted, SIGMA, BG * scale as f32);
                // The brighter frame's noise scales with its level too — a
                // frame that is "twice the level" is twice the level end to
                // end, which is what actually exercises the
                // AdditiveWithScaling pair's scale term below.
                add_noise(&mut data, NOISE * scale as f32, seed);
                let path = dir.join(name);
                write_fits_f32(&path, FW, FH, 1, &data, &[]).unwrap();
                let map = if dx == 0.0 && dy == 0.0 {
                    identity_map()
                } else {
                    shift_map(dx, dy)
                };
                (path, map)
            };

        let (p0, m0) = write_frame("s0.fits", 0.0, 0.0, 1.0, 501);
        let (p1, m1) = write_frame("s1.fits", 2.4, -1.3, 1.0, 502);
        let (p2, m2) = write_frame("s2.fits", -5.0, 3.0, 2.0, 503);

        let meas = |p: &PathBuf| {
            measure_frame(p, &MeasureOptions::default(), None, &AtomicBool::new(false)).unwrap()
        };
        let (m0meas, m1meas, m2meas) = (meas(&p0), meas(&p1), meas(&p2));

        let (exposures, weights) = (SHIFTED_EXPOSURES, SHIFTED_WEIGHTS);
        let frames = vec![
            StackFrame {
                path: p0,
                map: m0,
                measurement: m0meas,
                weight: weight(weights[0], 1),
                exposure_s: exposures[0],
                date_obs: None,
                frame_id: 900006,
                registered_path: None,
            },
            StackFrame {
                path: p1,
                map: m1,
                measurement: m1meas,
                weight: weight(weights[1], 1),
                exposure_s: exposures[1],
                date_obs: None,
                frame_id: 900007,
                registered_path: None,
            },
            StackFrame {
                path: p2,
                map: m2,
                measurement: m2meas,
                weight: weight(weights[2], 1),
                exposure_s: exposures[2],
                date_obs: None,
                frame_id: 900008,
                registered_path: None,
            },
        ];
        frames
    }

    #[test]
    fn integrating_a_shifted_brighter_frame_lands_on_the_reference_level() {
        // Fixture bounds picked for this test: 400x300 (the 200x150 size
        // used by the registered-source tests is too small for the
        // background-mesh model's 128px cell to say much), 12 stars on a
        // 4x3 grid, amplitude 0.25 against a noise sigma of 0.003 — SNR far
        // above the detector's default `min_snr` of 5.0, so all three
        // frames and the master fit reliably. "twice the level" (spec
        // sketch) is background 0.08 -> 0.16 with amplitude scaled the same
        // way, not literally "100" — the brief's number was illustrative.
        const FW: usize = 400;
        const FH: usize = 300;
        const BG: f32 = 0.08;
        let stars: Vec<(f64, f64, f64)> = (0..3)
            .flat_map(|j| {
                (0..4).map(move |i| (50.0 + i as f64 * 100.0, 50.0 + j as f64 * 100.0, 0.25))
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let frames = three_shifted_frames(dir.path());
        let (exposures, weights) = (SHIFTED_EXPOSURES, SHIFTED_WEIGHTS);

        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: FW,
            height: FH,
            channels: 1,
            interpolation: Interpolation::Lanczos3,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let out = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(40_000_000),
        )
        .unwrap();

        assert_eq!(out.included, vec![0, 1, 2]);
        assert_eq!((out.width, out.height, out.channels), (FW, FH, 1));

        // A 38×38 star-free patch: ~1400 pixels bring σ of the mean to ≈
        // 4e-5, twenty times inside the 1% bound; a single pixel's σ (≈
        // 1.7e-3) was half the bound.
        let mut patch_sum = 0.0f64;
        let mut patch_n = 0usize;
        for y in 2..40 {
            for x in 2..40 {
                patch_sum += out.data[y * FW + x] as f64;
                patch_n += 1;
            }
        }
        let patch_mean = patch_sum / patch_n as f64;
        assert!(
            (patch_mean - BG as f64).abs() < 0.01 * BG as f64,
            "background {patch_mean} vs {BG}"
        );

        // Measured centroid error tops out around 0.054 px on this fixture
        // (three-way weighted average of independently-noised, sub-pixel
        // shifted, Lanczos3-warped copies of the same field) — looser than
        // the single-frame 0.02 px bound `registered_source.rs` gets on a
        // noiseless fixture, so the bound here is 0.1 px, not 0.05.
        for &(sx, sy, _) in &stars {
            let (cx, cy) = centroid(&out.data, FW, sx, sy, 7, BG);
            assert!(
                (cx - sx).abs() < 0.1 && (cy - sy).abs() < 0.1,
                "star ({sx},{sy}) centroid ({cx},{cy})"
            );
        }

        assert!(
            out.stats.master_noise[0] > 0.0,
            "master_noise {}",
            out.stats.master_noise[0]
        );
        assert!(
            out.stats.snr_gain[0] > 1.0,
            "snr_gain {}",
            out.stats.snr_gain[0]
        );
        // `snr_gain` is an SNR factor (the square root of the paired PSF
        // SNR ratio, `paired_snr_gain`), so three frames cannot lift it
        // much past √3 ≈ 1.73 — the margin is the fit noise of a 12-star
        // fixture. `snr_gain_of_n_equal_frames_is_about_root_n` pins the
        // value itself.
        assert!(
            out.stats.snr_gain[0] < 2.0,
            "gain {} — a 3-frame stack cannot run away past √3",
            out.stats.snr_gain[0]
        );

        // Frame 2 is genuinely twice the level (signal AND noise), so the
        // AdditiveWithScaling pair that maps it back onto the reference
        // must actually use a scale term near 0.5, not fall back to
        // identity — measured 0.50 on this fixture.
        let pair = output_pair(
            frames[0].measurement.channels[0].location_scale(),
            frames[2].measurement.channels[0].location_scale(),
            OutputNormalization::AdditiveWithScaling,
        );
        assert!((pair.scale - 0.5).abs() < 0.1, "scale term {}", pair.scale);

        let expect_weighted =
            exposures[0] * weights[0] + exposures[1] * weights[1] + exposures[2] * weights[2];
        assert!((out.stats.weighted_exposure_s - expect_weighted).abs() < 1e-9);
        assert!((out.stats.total_exposure_s - exposures.iter().sum::<f64>()).abs() < 1e-9);
    }

    /// Plan 6b-3 D4, spec §5.7: `GroupInput.coverage_map` makes the group
    /// carry plane 0's coverage — whole frames covered everywhere inside,
    /// fewer at a shifted frame's edge — and the master is bit-identical
    /// with it off.
    #[test]
    fn the_group_coverage_is_a_side_output() {
        const FW: usize = 400;
        const FH: usize = 300;
        let dir = tempfile::tempdir().unwrap();
        let frames = three_shifted_frames(dir.path());
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let make_input = |coverage_map: bool| GroupInput {
            frames: &frames,
            reference: 0,
            width: FW,
            height: FH,
            channels: 1,
            interpolation: Interpolation::BicubicBSpline,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map,
            stats_on_covered: false,
        };
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let pool = pool();
        let off = integrate_group(
            &make_input(false),
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(1 << 24),
        )
        .unwrap();
        let on = integrate_group(
            &make_input(true),
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(1 << 24),
        )
        .unwrap();
        assert!(off.coverage.is_none());
        for (a, b) in off.data.iter().zip(&on.data) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        let cov = on.coverage.expect("the coverage plane");
        assert_eq!(cov.len(), FW * FH);
        assert!(cov
            .iter()
            .all(|&c| c == c.trunc() && (0.0..=3.0).contains(&c)));
        assert_eq!(
            cov[(FH / 2) * FW + FW / 2],
            3.0,
            "the centre is covered by every frame"
        );
        assert!(cov[0] < 3.0, "the (-5, 3)-shifted frame leaves the corner");
    }

    /// Runs `three_shifted_frames` through `integrate_group` with
    /// `stats_on_covered` set, every frame's map moved by `shift` (a
    /// whole-frame shift leaves a sliver, or nothing, covered).
    fn integrate_on_covered(
        coverage_map: bool,
        shift: Option<(f64, f64)>,
    ) -> Result<GroupOutput, IntegrationError> {
        let dir = tempfile::tempdir().unwrap();
        let mut frames = three_shifted_frames(dir.path());
        if let Some((dx, dy)) = shift {
            for f in &mut frames {
                f.map = shift_map(dx, dy);
            }
        }
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: 400,
            height: 300,
            channels: 1,
            interpolation: Interpolation::BicubicBSpline,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map,
            stats_on_covered: true,
        };
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool(),
            &AtomicBool::new(false),
            &progress,
            io(1 << 24),
        )
    }

    /// Review I1, plan 6b-3: with `stats_on_covered` the master's statistics
    /// are those of its largest covered rectangle, not of the whole plane,
    /// and no map is returned unless `coverage_map` asked for one.
    #[test]
    fn stats_on_covered_measure_the_covered_rectangle() {
        let out = integrate_on_covered(false, Some((200.0, 0.0))).unwrap();
        assert!(
            out.coverage.is_none(),
            "the count taken for the statistics is no map"
        );
        let with_map = integrate_on_covered(true, Some((200.0, 0.0))).unwrap();
        let cov = with_map.coverage.as_deref().expect("a coverage map");
        let rect = crate::stacking::crop::largest_covered_rect(cov, 400, 300, 1).unwrap();
        assert!(
            rect.width < 300 && rect.height == 300,
            "a partial rectangle: {rect:?}"
        );
        let sub = crate::stacking::crop::crop_plane(&out.data, 400, rect);
        let expect = measure_plane(
            &sub,
            rect.width,
            rect.height,
            &MeasureOptions::default(),
            None,
        );
        assert_eq!(out.stats.master_noise[0].to_bits(), expect.noise.to_bits());
        assert_eq!(
            out.stats.master_psf_snr[0].to_bits(),
            expect.psf_snr.to_bits()
        );
        let whole = measure_plane(&out.data, 400, 300, &MeasureOptions::default(), None);
        assert_ne!(
            out.stats.master_noise[0].to_bits(),
            whole.noise.to_bits(),
            "the fixture discriminates the rectangle from the whole plane"
        );
    }

    /// `stats_on_covered` and a coverage map over one source file placed at
    /// each of `shifts` (a frame per shift, so the group has `shifts.len()`
    /// frames); returns the output and everything `warn!`/`debug!` wrote.
    fn integrate_shifts(shifts: &[(f64, f64)]) -> (Result<GroupOutput, IntegrationError>, String) {
        let dir = tempfile::tempdir().unwrap();
        let base = three_shifted_frames(dir.path());
        let frames: Vec<StackFrame> = shifts
            .iter()
            .enumerate()
            .map(|(i, &(dx, dy))| StackFrame {
                path: base[0].path.clone(),
                map: shift_map(dx, dy),
                measurement: base[0].measurement.clone(),
                weight: weight(1.0, 1),
                exposure_s: 30.0,
                date_obs: None,
                frame_id: 910000 + i as i64,
                registered_path: None,
            })
            .collect();
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: 400,
            height: 300,
            channels: 1,
            interpolation: Interpolation::BicubicBSpline,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: true,
            stats_on_covered: true,
        };
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let mut result = None;
        let logs = crate::stacking::solve::tests::captured_logs(|| {
            result = Some(integrate_group(
                &input,
                &MeasureOptions::default(),
                &pool(),
                &AtomicBool::new(false),
                &progress,
                io(1 << 24),
            ));
        });
        (result.unwrap(), logs)
    }

    /// Final review A1: many narrow central frames plus one wide offset
    /// frame. The statistics describe the deep centre (the largest
    /// rectangle at least half the frames cover), not the one-frame fringe
    /// the offset frame adds to the footprint; the master's pixels are the
    /// same either way.
    #[test]
    fn stats_rectangle_is_the_deep_centre_not_the_wide_footprint() {
        // Four frames at +100 px cover columns 100.., one at -100 covers ..300.
        let shifts = [(100.0, 0.0); 4]
            .into_iter()
            .chain([(-100.0, 0.0)])
            .collect::<Vec<_>>();
        let (out, logs) = integrate_shifts(&shifts);
        let out = out.unwrap();
        let cov = out.coverage.as_deref().expect("a coverage map");
        let deep = crate::stacking::crop::largest_covered_rect(cov, 400, 300, 3).unwrap();
        let wide = crate::stacking::crop::largest_covered_rect(cov, 400, 300, 1).unwrap();
        assert!(
            deep.width < wide.width,
            "the fixture discriminates: deep {deep:?} vs wide {wide:?}"
        );
        assert_eq!(out.measure_rect, Some(deep), "k = ceil(5 / 2) = 3");
        let sub = crate::stacking::crop::crop_plane(&out.data, 400, deep);
        let expect = measure_plane(
            &sub,
            deep.width,
            deep.height,
            &MeasureOptions::default(),
            None,
        );
        assert_eq!(out.stats.master_noise[0].to_bits(), expect.noise.to_bits());
        let sub_wide = crate::stacking::crop::crop_plane(&out.data, 400, wide);
        let on_wide = measure_plane(
            &sub_wide,
            wide.width,
            wide.height,
            &MeasureOptions::default(),
            None,
        );
        assert_ne!(
            out.stats.master_noise[0].to_bits(),
            on_wide.noise.to_bits(),
            "the wide footprint would have measured something else"
        );
        assert!(
            !logs.contains("statistics rectangle too small"),
            "no fallback here: {logs}"
        );
        assert!(
            logs.contains("master statistics on the covered rectangle"),
            "{logs}"
        );
    }

    /// Final review A1: when the half-coverage rectangle is under
    /// `MIN_STATS_RECT_PX` (or empty) the statistics fall back to one frame
    /// of coverage with a warning naming both sizes (a group integrates from
    /// three frames up, so `k >= 2` always; the no-fallback case is the deep
    /// centre test above).
    #[test]
    fn stats_rectangle_falls_back_to_one_frame_with_a_warning() {
        // Three frames, two of them sharing a 5-px strip: depth 2 is a sliver.
        let (out, logs) = integrate_shifts(&[(0.0, 0.0), (395.0, 0.0), (395.0, 0.0)]);
        let out = out.unwrap();
        let cov = out.coverage.as_deref().expect("a coverage map");
        let one = crate::stacking::crop::largest_covered_rect(cov, 400, 300, 1).unwrap();
        assert_eq!(out.measure_rect, Some(one), "fell back to k = 1");
        assert!(
            logs.contains("statistics rectangle too small at half coverage"),
            "{logs}"
        );
        assert!(logs.contains("count=2"), "names the depth: {logs}");

        // Two disjoint 150-px footprints and one frame off the grid: no
        // pixel reaches depth 2 at all, still a fallback and not an error.
        let (out, logs) = integrate_shifts(&[(250.0, 0.0), (-250.0, 0.0), (450.0, 0.0)]);
        let out = out.unwrap();
        let cov = out.coverage.as_deref().unwrap();
        assert!(crate::stacking::crop::largest_covered_rect(cov, 400, 300, 2).is_none());
        assert_eq!(
            out.measure_rect,
            crate::stacking::crop::largest_covered_rect(cov, 400, 300, 1)
        );
        assert!(
            logs.contains("width=0") && logs.contains("height=0"),
            "the missing deep rectangle reads 0 x 0: {logs}"
        );
        assert!(
            logs.contains("statistics rectangle too small at half coverage"),
            "{logs}"
        );
    }

    /// A covered rectangle under `MIN_STATS_RECT_PX` fails the group with a
    /// message that names it, never a panic in the estimators.
    #[test]
    fn a_sliver_of_coverage_fails_the_group_with_its_own_message() {
        // Every frame moved 397 px right: only a 3-px-wide column is covered.
        match integrate_on_covered(false, Some((397.0, 0.0))) {
            Err(IntegrationError::BadInput(m)) => {
                assert!(m.contains("covered part of the grid is only"), "{m}")
            }
            other => panic!("expected a BadInput, got {:?}", other.map(|o| o.width)),
        }
    }

    /// No frame covering the grid at all fails the group, with its own text.
    #[test]
    fn empty_coverage_fails_the_group() {
        match integrate_on_covered(false, Some((5000.0, 0.0))) {
            Err(IntegrationError::BadInput(m)) => {
                assert!(m.contains("no frame of this group covers the grid"), "{m}")
            }
            other => panic!("expected a BadInput, got {:?}", other.map(|o| o.width)),
        }
    }

    #[test]
    fn a_three_plane_group_integrates_plane_by_plane_with_one_map_set_per_plane() {
        const W: usize = 48;
        const H: usize = 36;
        let levels = [0.1f32, 0.2, 0.3];
        let dir = tempfile::tempdir().unwrap();
        let mut base = Vec::with_capacity(3 * W * H);
        for &lvl in &levels {
            base.extend(std::iter::repeat(lvl).take(W * H));
        }
        let mut frames = Vec::new();
        for i in 0..3u64 {
            let mut data = base.clone();
            add_noise(&mut data, 0.0005, 700 + i); // tiny noise so scale isn't exactly zero
            let path = dir.path().join(format!("rgb{i}.fits"));
            write_fits_f32(&path, W, H, 3, &data, &[]).unwrap();
            let measurement = measure_frame(
                &path,
                &MeasureOptions::default(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
            frames.push(StackFrame {
                path,
                map: identity_map(),
                measurement,
                weight: weight(1.0, 3),
                exposure_s: 30.0,
                date_obs: None,
                frame_id: 900009,
                registered_path: None,
            });
        }

        let mut integration = IntegrationConfig::default();
        integration.write_rejection_maps = true;
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: 3,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let planes_seen = std::sync::Mutex::new(Vec::new());
        let on_plane = |p: usize, total: usize| planes_seen.lock().unwrap().push((p, total));
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let out = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap();

        assert_eq!(out.data.len(), 3 * W * H);
        for (p, &lvl) in levels.iter().enumerate() {
            let plane = &out.data[p * W * H..(p + 1) * W * H];
            let mean: f32 = plane.iter().sum::<f32>() / plane.len() as f32;
            assert!((mean - lvl).abs() < 0.01, "plane {p} mean {mean} vs {lvl}");
        }
        assert_eq!(out.rejection_low.as_ref().unwrap().len(), 3 * W * H);
        assert_eq!(out.rejection_high.as_ref().unwrap().len(), 3 * W * H);
        assert_eq!(*planes_seen.lock().unwrap(), vec![(0, 3), (1, 3), (2, 3)]);
    }

    /// Plan A Task 8 fix wave (owner ruling 2026-10-09, review I-1): a 3-plane master's statistics are the multi-plane
    /// entry's — a star clipped on G alone is flagged on R and B too and leaves their master FWHM.
    #[test]
    fn a_three_plane_masters_statistics_go_through_the_multi_plane_entry() {
        use crate::stacking::measure::{measure_plane_diagnosed, tests::cross_plane_fixture};
        let (planes, w, h) = cross_plane_fixture();
        let dir = tempfile::tempdir().unwrap();
        let mut frames = Vec::new();
        for i in 0..3u64 {
            let mut data: Vec<f32> = planes.iter().flatten().copied().collect();
            add_noise(&mut data, 0.0005, 910 + i);
            let path = dir.path().join(format!("osc{i}.fits"));
            write_fits_f32(&path, w, h, 3, &data, &[]).unwrap();
            let measurement = measure_frame(
                &path,
                &MeasureOptions::default(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
            frames.push(StackFrame {
                path,
                map: identity_map(),
                measurement,
                weight: weight(1.0, 3),
                exposure_s: 30.0,
                date_obs: None,
                frame_id: 900021 + i as i64,
                registered_path: None,
            });
        }
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: w,
            height: h,
            channels: 3,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let out = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool(),
            &AtomicBool::new(false),
            &progress,
            io(1 << 24),
        )
        .unwrap();
        let n = w * h;
        let master = |p: usize| &out.data[p * n..(p + 1) * n];
        let multi = measure_planes_diagnosed(
            3,
            w,
            h,
            &MeasureOptions::default(),
            None,
            SeedOverride::Fast,
            false,
            crate::star_measure::PixelNoise::Mixed,
            None,
            |p| Ok::<_, std::convert::Infallible>(Cow::Borrowed(master(p))),
        )
        .unwrap();
        for p in 0..3 {
            let c = &multi[p].measurement.channel;
            assert_eq!(
                out.stats.master_fwhm_px[p].to_bits(),
                c.fwhm_px.to_bits(),
                "plane {p}"
            );
            assert_eq!(
                out.stats.master_eccentricity[p].to_bits(),
                c.eccentricity.to_bits(),
                "plane {p}"
            );
        }
        for p in [0, 2] {
            assert!(multi[p].measurement.cross_flags >= 1, "plane {p}");
            let (alone, _, _) = measure_plane_diagnosed(
                master(p),
                w,
                h,
                &MeasureOptions::default(),
                None,
                SeedOverride::Fast,
                false,
                true,
                crate::star_measure::PixelNoise::Mixed,
                None,
                None,
            );
            assert_ne!(
                out.stats.master_fwhm_px[p].to_bits(),
                alone.channel.fwhm_px.to_bits(),
                "plane {p}: the cross flag moves the master's FWHM"
            );
        }
    }

    /// FM-28 (F1 review I1): the master's statistics are measured at All whatever `fitStars` the frames used.
    #[test]
    fn master_measurement_ignores_fit_stars() {
        use crate::stacking::measure::tests::cross_plane_fixture;
        let (planes, w, h) = cross_plane_fixture();
        let dir = tempfile::tempdir().unwrap();
        let mut frames = Vec::new();
        for i in 0..3u64 {
            let mut data: Vec<f32> = planes.iter().flatten().copied().collect();
            add_noise(&mut data, 0.0005, 910 + i);
            let path = dir.path().join(format!("osc{i}.fits"));
            write_fits_f32(&path, w, h, 3, &data, &[]).unwrap();
            let measurement = measure_frame(
                &path,
                &MeasureOptions::default(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
            frames.push(StackFrame {
                path,
                map: identity_map(),
                measurement,
                weight: weight(1.0, 3),
                exposure_s: 30.0,
                date_obs: None,
                frame_id: 900031 + i as i64,
                registered_path: None,
            });
        }
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: w,
            height: h,
            channels: 3,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let run = |measure: &MeasureOptions| {
            let on_plane = nop_plane();
            let on_band = nop_band();
            let progress = GroupProgress {
                on_plane: &on_plane,
                engine: EngineProgress {
                    on_band: &on_band,
                    on_combine: &on_band,
                },
            };
            integrate_group(
                &input,
                measure,
                &pool(),
                &AtomicBool::new(false),
                &progress,
                io(1 << 24),
            )
            .unwrap()
        };
        let all = run(&MeasureOptions::default());
        let k = run(&MeasureOptions {
            fit_stars: Some(1),
            ..MeasureOptions::default()
        });
        for p in 0..3 {
            assert_eq!(
                k.stats.master_fwhm_px[p].to_bits(),
                all.stats.master_fwhm_px[p].to_bits(),
                "plane {p} fwhm"
            );
            assert_eq!(
                k.stats.master_eccentricity[p].to_bits(),
                all.stats.master_eccentricity[p].to_bits(),
                "plane {p} ecc"
            );
        }
    }

    /// M3 Task 2, brief tests (g) + (h): `integrate_group` with a real
    /// `RejBitmapSet` writes bitmaps whose total rejected-bit count matches
    /// the engine's own raw `rejected_per_frame` (obtained from a second,
    /// bitmap-less `integrate_planes` call on the same fixture — the sink's
    /// presence never changes what gets rejected, engine.rs's own sink test
    /// pins that directly), and `output_pairs` has the shape/contract
    /// `stats::output_pair` documents: one entry per channel, one per
    /// included frame, the reference frame's own pair `(scale 1, offset 0)`
    /// under the default `AdditiveWithScaling` output mode.
    #[test]
    fn integrate_group_writes_rejection_bitmaps_matching_the_engines_rejected_count_and_output_pairs(
    ) {
        const W: usize = 32;
        const H: usize = 24;
        let dir = tempfile::tempdir().unwrap();
        let hot_idx = 3usize;
        let paths: Vec<_> = (0..4)
            .map(|i| {
                let mut d = vec![0.1f32; W * H];
                add_noise(&mut d, 0.0005, 2000 + i as u64);
                if i == hot_idx {
                    d[5 * W + 7] = 0.9;
                }
                let p = dir.path().join(format!("f{i}.fits"));
                write_fits_f32(&p, W, H, 1, &d, &[]).unwrap();
                p
            })
            .collect();
        let measurements: Vec<_> = paths
            .iter()
            .map(|p| {
                measure_frame(p, &MeasureOptions::default(), None, &AtomicBool::new(false)).unwrap()
            })
            .collect();
        let frames: Vec<StackFrame> = paths
            .iter()
            .cloned()
            .zip(measurements.iter().cloned())
            .map(|(path, measurement)| StackFrame {
                path,
                map: identity_map(),
                measurement,
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900010,
                registered_path: None,
            })
            .collect();

        let mut integration = IntegrationConfig::default();
        integration.rejection = RejectionChoice::SigmaClip {
            sigma_low: 3.0,
            sigma_high: 1.0,
        };
        let normalization = NormalizationConfig::default();
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };

        let input_plain = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };

        // The engine's own raw rejected count, from a bitmap-less
        // `integrate_planes` call directly (bypassing `integrate_group`'s
        // `GroupStats` fractions, which don't expose raw counts).
        let recipe = IntegrationRecipe {
            combination: integration.combination,
            rejection: integration.rejection.resolve(4),
        };
        let (raw_outputs, _) = integrate_planes(
            &input_plain,
            &[0, 1, 2, 3],
            &[vec![1.0], vec![1.0], vec![1.0], vec![1.0]],
            normalization.output,
            normalization.rejection,
            false,
            None,
            None,
            None,
            recipe,
            false,
            CoverageOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap();
        let expected_rejected: u64 = raw_outputs
            .iter()
            .map(|o| o.rejected_per_frame.iter().sum::<u64>())
            .sum();
        assert!(
            expected_rejected > 0,
            "the hot pixel must actually be rejected for this test to be meaningful"
        );

        let rej_dir = dir.path().join("rej");
        let stems: Vec<String> = (0..4).map(|i| format!("f{i}")).collect();
        let set = RejBitmapSet::create(&rej_dir, &stems, W, H, 1).unwrap();

        let input = GroupInput {
            rej: Some(&set),
            ..input_plain
        };
        let out = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap();
        assert_eq!(out.included, vec![0, 1, 2, 3]);

        // (h) output_pairs shape + the reference frame's own identity pair.
        assert_eq!(out.output_pairs.len(), 1, "one entry per channel");
        assert_eq!(out.output_pairs[0].len(), out.included.len());
        let ref_pos = out
            .included
            .iter()
            .position(|&i| i == input.reference)
            .unwrap();
        let ref_pair = out.output_pairs[0][ref_pos];
        assert_eq!(
            ref_pair.scale, 1.0,
            "AdditiveWithScaling: the reference frame's own pair is (scale 1, offset 0)"
        );
        assert_eq!(ref_pair.offset, 0.0);

        // (g) total bits set across every frame's bitmap equals the raw
        // engine rejected count above.
        let mut total_bits = 0u64;
        for i in 0..4 {
            let bm = RejBitmap::read(set.path(i), W, H, 1).unwrap();
            for y in 0..H {
                for x in 0..W {
                    if bm.is_rejected(0, x, y) {
                        total_bits += 1;
                    }
                }
            }
        }
        assert_eq!(total_bits, expected_rejected);
    }

    /// M3 Task 2, brief test (i): a rejection-bitmap set sized for the
    /// wrong frame count is refused before any pixel work runs.
    #[test]
    fn integrate_group_refuses_a_rej_set_with_the_wrong_frame_count() {
        const W: usize = 24;
        const H: usize = 16;
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|i| {
                let mut d = vec![0.1f32; W * H];
                add_noise(&mut d, 0.001, 3000 + i as u64);
                let p = dir.path().join(format!("g{i}.fits"));
                write_fits_f32(&p, W, H, 1, &d, &[]).unwrap();
                p
            })
            .collect();
        let measurements: Vec<_> = paths
            .iter()
            .map(|p| {
                measure_frame(p, &MeasureOptions::default(), None, &AtomicBool::new(false)).unwrap()
            })
            .collect();
        let frames: Vec<StackFrame> = paths
            .iter()
            .cloned()
            .zip(measurements.iter().cloned())
            .map(|(path, measurement)| StackFrame {
                path,
                map: identity_map(),
                measurement,
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900011,
                registered_path: None,
            })
            .collect();
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();

        let rej_dir = dir.path().join("rej");
        // Wrong frame count on purpose: 3 stems for a 4-frame, no-drop group.
        let stems: Vec<String> = (0..3).map(|i| format!("g{i}")).collect();
        let set = RejBitmapSet::create(&rej_dir, &stems, W, H, 1).unwrap();

        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: Some(&set),
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let err = integrate_group(
            &input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap_err();
        match err {
            IntegrationError::BadInput(msg) => {
                assert!(msg.contains('3') && msg.contains('4'), "{msg}");
            }
            other => panic!("expected BadInput, got {other:?}"),
        }

        // Fix round 1, I2: right frame count (4), wrong geometry — a stale
        // set from a differently-sized group must be refused just as
        // loudly, before any pixel work runs.
        let geom_dir = dir.path().join("rej-geom");
        let stems4: Vec<String> = (0..4).map(|i| format!("g{i}")).collect();
        let wrong_geom_set = RejBitmapSet::create(&geom_dir, &stems4, W / 2, H, 1).unwrap();
        let geom_input = GroupInput {
            rej: Some(&wrong_geom_set),
            ..input
        };
        let err = integrate_group(
            &geom_input,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap_err();
        match err {
            IntegrationError::BadInput(msg) => {
                assert!(msg.contains("geometry"), "{msg}");
            }
            other => panic!("expected BadInput, got {other:?}"),
        }
    }

    // ── M4c Task 3: the large-scale second pass ─────────────────────────

    /// `large_scale_passes` is the ONE expression of "does this group
    /// integrate twice" — `stacking::run` reads it too, for the Integrate
    /// stage's progress space.
    #[test]
    fn large_scale_passes_needs_both_the_flag_and_a_bitmap_set() {
        let mut cfg = IntegrationConfig::default();
        assert_eq!(large_scale_passes(&cfg, false), 1);
        assert_eq!(large_scale_passes(&cfg, true), 1, "off by default");
        cfg.large_scale.enabled = true;
        assert_eq!(
            large_scale_passes(&cfg, false),
            1,
            "no bitmaps to derive structures from → no second pass"
        );
        assert_eq!(large_scale_passes(&cfg, true), 2);
    }

    /// M4c Task 3 (ruling R-M4c-4) end to end through `integrate_group`: one
    /// frame of six carries a 5-px band at 6x the background with a 2-px
    /// SHOULDER on each side that the per-pixel test cannot see (the
    /// shoulder deviates 20 %, the percentile clip's high threshold is
    /// 50 %) — exactly the residual a real trail's faint edges leave.
    ///
    /// Without large-scale rejection the master keeps 1/6 of that shoulder;
    /// with it on, the band's own rejected rows survive the filter, the
    /// disc dilation by 2 px covers the shoulders, and the second pass
    /// forces them out — the master's shoulder rows come out at the other
    /// five frames' own level. Every `.rejl` sibling is written, and the
    /// pixels the forced set never touched stay BIT-identical between the
    /// two runs.
    #[test]
    fn large_scale_rejection_removes_a_bands_shoulder_that_the_per_pixel_test_misses() {
        const W: usize = 64;
        const H: usize = 64;
        const BASE: f32 = 0.1;
        const CORE: std::ops::Range<usize> = 28..33;
        const SHOULDER_LOW: std::ops::Range<usize> = 26..28;
        const SHOULDER_HIGH: std::ops::Range<usize> = 33..35;
        let dir = tempfile::tempdir().unwrap();
        let trail_idx = 3usize;
        // Fix round 1 (ruling R-T3-1): one frame ALSO carries a lone hot
        // pixel far from the trail — the per-pixel test rejects it in both
        // passes, the large-scale filter erases it as speckle, so it is
        // exactly the sample that separates "the forced set" from "the set
        // the master was built with".
        let hot_idx = 1usize;
        let (hot_x, hot_y) = (10usize, 50usize);
        let paths: Vec<_> = (0..6)
            .map(|i| {
                let mut d = vec![BASE; W * H];
                if i == trail_idx {
                    for y in CORE {
                        for x in 0..W {
                            d[y * W + x] = 0.6;
                        }
                    }
                    for y in SHOULDER_LOW.chain(SHOULDER_HIGH) {
                        for x in 0..W {
                            d[y * W + x] = 0.12;
                        }
                    }
                }
                if i == hot_idx {
                    d[hot_y * W + hot_x] = 0.9;
                }
                add_noise(&mut d, 0.0005, 7000 + i as u64);
                let p = dir.path().join(format!("t{i}.fits"));
                write_fits_f32(&p, W, H, 1, &d, &[]).unwrap();
                p
            })
            .collect();
        let frames: Vec<StackFrame> = paths
            .iter()
            .map(|p| StackFrame {
                path: p.clone(),
                map: identity_map(),
                measurement: measure_frame(
                    p,
                    &MeasureOptions::default(),
                    None,
                    &AtomicBool::new(false),
                )
                .unwrap(),
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
                frame_id: 900012,
                registered_path: None,
            })
            .collect();

        let mut integration = IntegrationConfig::default();
        // 50 % high threshold: the 0.6 core (500 %) is rejected, the 0.12
        // shoulder (20 %) is not.
        integration.rejection = RejectionChoice::PercentileClip {
            low: 0.2,
            high: 0.5,
        };
        let normalization = NormalizationConfig::default();
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress {
                on_band: &on_band,
                on_combine: &on_band,
            },
        };
        let input_off = GroupInput {
            frames: &frames,
            reference: 0,
            width: W,
            height: H,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map: false,
            stats_on_covered: false,
        };

        let off = integrate_group(
            &input_off,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap();
        assert_eq!(off.included.len(), 6);
        assert_eq!(
            off.stats.large_scale_rejected_fraction, None,
            "large-scale rejection is off for this run"
        );

        let mut integration_on = integration.clone();
        integration_on.large_scale = LargeScaleRejection {
            enabled: true,
            protected_layers: 2,
            growth: 2,
        };
        let rej_dir = dir.path().join("rej");
        let stems: Vec<String> = (0..6).map(|i| format!("t{i}")).collect();
        let set = RejBitmapSet::create(&rej_dir, &stems, W, H, 1).unwrap();
        let input_on = GroupInput {
            integration: &integration_on,
            rej: Some(&set),
            // This pin reads the second pass's own set, which is what a
            // drizzle-consuming run asks for.
            second_pass_bitmaps: true,
            ..input_off
        };
        let on = integrate_group(
            &input_on,
            &MeasureOptions::default(),
            &pool,
            &AtomicBool::new(false),
            &progress,
            io(20_000_000),
        )
        .unwrap();

        // Every included frame got a `.rejl` sibling, and only the trail
        // frame's own carries bits (the others' speckle is erased).
        for k in 0..6 {
            let path = set.processed_path(k);
            assert!(path.exists(), "missing processed sibling: {path:?}");
            let bm = RejBitmap::read(&path, W, H, 1).unwrap();
            let bits = bm.count();
            if k == trail_idx {
                assert!(
                    bits > 0,
                    "the trail frame's processed bitmap must carry bits"
                );
            } else {
                assert_eq!(
                    bits, 0,
                    "frame {k}'s speckle must be erased, got {bits} bits"
                );
            }
        }

        let forced = on
            .stats
            .large_scale_rejected_fraction
            .expect("the second pass ran");
        // 9 rows (5 core + 2 + 2 shoulder) x 64 px of one frame in six.
        let expected = 9.0 * W as f64 / (6.0 * W as f64 * H as f64);
        assert!(
            (forced - expected).abs() < 1e-9,
            "forced fraction {forced} != {expected}"
        );

        // Fix round 1 (ruling R-T3-1): the SECOND pass writes a bitmap set
        // of its own, and those bits are the master's real rejected set —
        // the forced structures PLUS what pass 2's own algorithm rejected,
        // which is what drizzle has to read. Checked three ways: it is a
        // superset of the forced set, it carries the lone hot pixel the
        // filter erased (so it is a STRICT superset — the distinction the
        // ruling exists for), and every frame's own bit count matches the
        // rejection `GroupStats` reports for that frame.
        assert!(on.second_pass_rej_ok, "the second pass wrote its own set");
        let mut second_total = 0u64;
        for k in 0..6 {
            let path = set.second_pass_path(k);
            assert!(path.exists(), "missing second-pass bitmap: {path:?}");
            let second = RejBitmap::read(&path, W, H, 1).unwrap();
            let processed = RejBitmap::read(&set.processed_path(k), W, H, 1).unwrap();
            for y in 0..H {
                for x in 0..W {
                    if processed.is_rejected(0, x, y) {
                        assert!(
                            second.is_rejected(0, x, y),
                            "frame {k} ({x}, {y}): a forced sample must be recorded as rejected"
                        );
                    }
                }
            }
            if k == hot_idx {
                assert!(
                    second.is_rejected(0, hot_x, hot_y),
                    "the hot pixel is an ALGORITHM rejection of the second pass and must be recorded"
                );
                assert!(
                    !processed.is_rejected(0, hot_x, hot_y),
                    "…and the filter erased it, which is why the two sets differ"
                );
            }
            // `rejected_fraction_per_frame` is that frame's rejected
            // samples over its own samples; every sample here is finite
            // (identity maps, one plane), so the denominator is `W · H`.
            let reported =
                (on.stats.rejected_fraction_per_frame[k] * (W * H) as f64).round() as u64;
            assert_eq!(
                second.count(),
                reported,
                "frame {k}: {} recorded bits against {reported} rejected samples in the stats",
                second.count()
            );
            second_total += second.count();
        }
        let forced_total = (forced * (6 * W * H) as f64).round() as u64;
        assert!(
            second_total > forced_total,
            "the second pass's set ({second_total} bits) must be a STRICT superset of the \
             forced set ({forced_total} bits)"
        );

        let row_mean = |data: &[f32], y: usize| -> f64 {
            data[y * W..(y + 1) * W]
                .iter()
                .map(|&v| v as f64)
                .sum::<f64>()
                / W as f64
        };
        let control = row_mean(&off.data, 5);
        for y in SHOULDER_LOW.chain(SHOULDER_HIGH) {
            let without = row_mean(&off.data, y);
            let with = row_mean(&on.data, y);
            assert!(
                (without - control) / control > 0.02,
                "row {y}: without large-scale rejection the shoulder must stay HIGH: \
                 {without} vs control {control}"
            );
            assert!(
                ((with - control) / control).abs() < 0.005,
                "row {y}: with large-scale rejection the shoulder must match the other frames: \
                 {with} vs control {control}"
            );
        }
        // The core rows were rejected per-pixel either way.
        for y in CORE {
            let without = row_mean(&off.data, y);
            let with = row_mean(&on.data, y);
            assert!(
                ((with - without) / control).abs() < 0.005,
                "row {y}: the core is rejected either way: {with} vs {without}"
            );
        }
        // Nothing outside the forced band moved at all.
        for y in 0..H {
            if (24..37).contains(&y) {
                continue;
            }
            for x in 0..W {
                assert_eq!(
                    on.data[y * W + x].to_bits(),
                    off.data[y * W + x].to_bits(),
                    "pixel ({x}, {y}) moved outside the forced band"
                );
            }
        }
    }
}
