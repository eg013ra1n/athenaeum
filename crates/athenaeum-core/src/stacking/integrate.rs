//! Group integration driver (spec §9.2, §6.3): the `integration:`/
//! `normalization:` configuration, the Auto rejection rule resolved per
//! group size, and `integrate_group` — which turns Plan 2's per-frame
//! measurements and weights into the rejection/output normalization pairs
//! the weighted engine (`integration::engine::integrate_stack`) needs,
//! opens each plane through a `RegisteredSource` (spec §6.1's hybrid
//! materialization — transforms persist, pixels do not), and folds the
//! per-plane `StackOutput`s into one `GroupOutput`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::geometry::PixelMap;
use crate::integration::combine::{Combination, IntegrationRecipe, Rejection};
use crate::integration::engine::{
    integrate_stack, EngineProgress, LocalNormRow, LocalNormRowFactory, StackOutput, StackParams,
};
use crate::integration::io_policy::IoPolicy;
use crate::integration::registered_source::{RegisteredFrame, RegisteredSource};
use crate::integration::source::{RejectionBitSink, RejectionBitSource};
use crate::integration::stats::{
    output_pair, rejection_pair, NormalizationPair, OutputNormalization, RejectionNormalization,
    ScaleEstimator,
};
use crate::integration::IntegrationError;
use crate::resample::Interpolation;
use crate::stacking::ln::grid::LnScratch;
use crate::stacking::ln::{LnFrameGrids, LnGrid};
use crate::stacking::measure::{measure_plane, FrameMeasurement, MeasureOptions};
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
    /// Tile size in pixels for the local-normalization grid.
    pub scale: u32,
    pub reference_frames: u32,
    pub psf_model: crate::stacking::psf_signal::PsfModel,
    pub local_scale: bool,
}

impl Default for LocalNormalizationConfig {
    fn default() -> Self {
        LocalNormalizationConfig {
            enabled: false,
            scale: 1024,
            reference_frames: 20,
            psf_model: crate::stacking::psf_signal::PsfModel::Auto,
            local_scale: false,
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
    /// it, or it drives rejection only and its own `normalize_frame` call
    /// failed). The whole field is `None` when local normalization never ran
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
}

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
    /// `master_psf_snr / best_sub_psf_snr`; `0.0` (with a warn) when the
    /// best sub's own PSF SNR is not positive.
    pub snr_gain: Vec<f64>,
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
    pool: &rayon::ThreadPool,
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
        let mut registered_frames = Vec::with_capacity(n);
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
            registered_frames.push(RegisteredFrame {
                path: f.path.clone(),
                map: f.map.clone(),
            });
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

        let src = RegisteredSource::open(
            &registered_frames,
            input.width,
            input.height,
            p,
            input.interpolation,
            input.clamping,
        )?;

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
    RejForcedSource::load(&paths, w, h, c)
        .map_err(|e| IntegrationError::Decode(format!("{e:#}")))
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
    pool: &rayon::ThreadPool,
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
        if rej_set.width() != input.width || rej_set.height() != input.height || rej_set.channels() != input.channels
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
                    let second_set = match rej_set.create_second_pass() {
                        Ok(set) => Some(set),
                        Err(e) => {
                            warn!(
                                error = %format!("{e:#}"),
                                "the second pass's rejection bitmaps could not be created; \
                                 the master is unaffected"
                            );
                            None
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

    for (p, out) in outputs.into_iter().enumerate() {
        // `integrate_planes` already checks `cancel` before each plane's own
        // engine work, but the tail below runs a real (non-trivial) master
        // measurement per plane — without this check here, a cancel raised
        // while `integrate_planes` was finishing its last plane would still
        // let every remaining tail iteration run its measurement before this
        // function notices, instead of stopping at the next plane boundary
        // the way the pre-extraction single loop did.
        if cancel.load(Ordering::Relaxed) {
            warn!(plane = p, "group integration cancelled");
            return Err(IntegrationError::Cancelled);
        }
        // `integrate_planes` no longer exposes a per-plane wall-clock split
        // (it runs every plane's `RegisteredSource::open` + `integrate_stack`
        // before this tail even starts), so the logged `duration_ms` below is
        // now `out.base`'s own read+combine time plus this tail's own
        // elapsed time — the dominant costs, not `RegisteredSource::open`'s
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

        // Master measurement (spec §6): `measure_plane` wants
        // `Option<&Arc<rayon::ThreadPool>>`, which the group driver's plain
        // `&rayon::ThreadPool` can't supply — run it inside `pool.install`
        // instead so its own parallel work still lands on the caller's
        // pool, and pass `None` for the explicit handle.
        let plane_data = &out.base.data;
        let cm =
            pool.install(|| measure_plane(plane_data, input.width, input.height, measure, None));
        master_noise.push(cm.noise);
        master_location.push(cm.location);
        master_scale.push(cm.scale);
        master_fwhm_px.push(cm.fwhm_px);
        master_eccentricity.push(cm.eccentricity);
        master_psf_snr.push(cm.psf_snr);

        let best_ch = &frames[best_idx].measurement.channels[p];
        best_sub_noise.push(best_ch.noise);
        best_sub_psf_snr.push(best_ch.psf_snr);
        let gain = if best_ch.psf_snr > 0.0 {
            cm.psf_snr / best_ch.psf_snr
        } else {
            f64::NAN
        };
        let gain = if gain.is_finite() {
            gain
        } else {
            warn!(
                plane = p,
                "snr_gain is not finite (the best sub has no usable PSF SNR); reporting 0.0"
            );
            0.0
        };
        snr_gain.push(gain);

        let plane_duration_ms = (out.base.read_duration
            + out.base.combine_duration
            + stats_start.elapsed())
        .as_millis() as u64;
        debug!(
            plane = p,
            rejected_low = out.rejected_low,
            rejected_high = out.rejected_high,
            duration_ms = plane_duration_ms,
            "plane integrated"
        );
    }

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
    use crate::stacking::measure::{measure_frame, ChannelMeasurement};
    use crate::stacking::rej::RejBitmap;
    use crate::test_support::{add_noise, centroid, gaussian_field};

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
    }

    fn io(band_budget_bytes: usize) -> IoPolicy {
        IoPolicy {
            band_budget_bytes,
            read_concurrency: 2,
            storage: StorageClass::Local,
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
                reference_frames: 8,
                psf_model: crate::stacking::psf_signal::PsfModel::Moffat4,
                local_scale: true,
            }
        );
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
            },
            StackFrame {
                path: PathBuf::from("b.fits"),
                map: identity_map(),
                measurement: dummy_measurement(1),
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
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
        const SIGMA: f64 = 1.8;
        const NOISE: f32 = 0.003;
        let stars: Vec<(f64, f64, f64)> = (0..3)
            .flat_map(|j| {
                (0..4).map(move |i| (50.0 + i as f64 * 100.0, 50.0 + j as f64 * 100.0, 0.25))
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();

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
                let path = dir.path().join(name);
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

        let exposures = [60.0, 90.0, 120.0];
        let weights = [1.0, 0.8, 0.6];
        let frames = vec![
            StackFrame {
                path: p0,
                map: m0,
                measurement: m0meas,
                weight: weight(weights[0], 1),
                exposure_s: exposures[0],
                date_obs: None,
            },
            StackFrame {
                path: p1,
                map: m1,
                measurement: m1meas,
                weight: weight(weights[1], 1),
                exposure_s: exposures[1],
                date_obs: None,
            },
            StackFrame {
                path: p2,
                map: m2,
                measurement: m2meas,
                weight: weight(weights[2], 1),
                exposure_s: exposures[2],
                date_obs: None,
            },
        ];

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
        // `psf_snr` is already TFlux²/σ_N² (PSFSNR_NUM·TFlux² over
        // PSFSNR_DEN·σ_N² — see `psf_signal::psf_snr`), so for N
        // near-equally-weighted, near-equal-noise frames the natural
        // ceiling on `snr_gain` is N itself (noise variance drops by N,
        // and gain is a ratio of variances), not √N — measured 3.14 on
        // this fixture (weights 1.0/0.8/0.6, effective N ≈ 2.9), just
        // over the N=3 ceiling from the master's own fit noise. The
        // sanity check is against an unbounded/runaway gain, not a tight
        // theoretical bound.
        assert!(
            out.stats.snr_gain[0] < 3.5,
            "gain {} — a 3-frame stack cannot run away past its own frame count",
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
    fn integrate_group_writes_rejection_bitmaps_matching_the_engines_rejected_count_and_output_pairs() {
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
            })
            .collect();

        let mut integration = IntegrationConfig::default();
        integration.rejection = RejectionChoice::SigmaClip { sigma_low: 3.0, sigma_high: 1.0 };
        let normalization = NormalizationConfig::default();
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress { on_band: &on_band, on_combine: &on_band },
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

        let input = GroupInput { rej: Some(&set), ..input_plain };
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
        let ref_pos = out.included.iter().position(|&i| i == input.reference).unwrap();
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
        };
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress { on_band: &on_band, on_combine: &on_band },
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
        let geom_input = GroupInput { rej: Some(&wrong_geom_set), ..input };
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
            })
            .collect();

        let mut integration = IntegrationConfig::default();
        // 50 % high threshold: the 0.6 core (500 %) is rejected, the 0.12
        // shoulder (20 %) is not.
        integration.rejection = RejectionChoice::PercentileClip { low: 0.2, high: 0.5 };
        let normalization = NormalizationConfig::default();
        let pool = pool();
        let on_plane = nop_plane();
        let on_band = nop_band();
        let progress = GroupProgress {
            on_plane: &on_plane,
            engine: EngineProgress { on_band: &on_band, on_combine: &on_band },
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
                assert!(bits > 0, "the trail frame's processed bitmap must carry bits");
            } else {
                assert_eq!(bits, 0, "frame {k}'s speckle must be erased, got {bits} bits");
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
            let reported = (on.stats.rejected_fraction_per_frame[k] * (W * H) as f64).round() as u64;
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
            data[y * W..(y + 1) * W].iter().map(|&v| v as f64).sum::<f64>() / W as f64
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
