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
use crate::integration::engine::{integrate_stack, EngineProgress, StackOutput, StackParams};
use crate::integration::io_policy::IoPolicy;
use crate::integration::registered_source::{RegisteredFrame, RegisteredSource};
use crate::integration::stats::{
    output_pair, rejection_pair, OutputNormalization, RejectionNormalization, ScaleEstimator,
};
use crate::integration::IntegrationError;
use crate::resample::Interpolation;
use crate::stacking::measure::{measure_plane, FrameMeasurement, MeasureOptions};
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
        }
    }
}

/// spec §9.2 `normalization:` (global part only; `local` rejection
/// normalization is M2 — see [`RejectionNormalization::Local`] — and is
/// carried here as an opaque, defaulted value so a stored config round-trips
/// even though M1 refuses it at [`integrate_group`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct NormalizationConfig {
    pub output: OutputNormalization,
    pub rejection: RejectionNormalization,
    /// Informational at this layer: the estimator that produced the
    /// stage-3 location/scale is `MeasureOptions::scale_estimator` — the
    /// orchestrator (Plan 5) keeps the two equal.
    pub scale_estimator: ScaleEstimator,
    /// Local (small-scale) normalization settings (spec §5.2, M2). Carried
    /// here as an opaque, defaulted block so a stored config round-trips
    /// before M2 lands — `integrate_group` never reads it.
    #[serde(default)]
    pub local: LocalNormalizationConfig,
}

/// spec §9.2 `normalization.local:` (M2). `enabled` stays `false` until the
/// local-normalization stage exists; every other field is carried so the
/// block round-trips through a stored config unchanged.
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
}

impl RejectionChoice {
    /// `n < 8` → percentile 0.2/0.1; `8 ≤ n < 20` → Winsorized 4.0/3.0;
    /// `n ≥ 20` → linear fit 5.0/3.5 (spec §6.3's Auto rule). Every other
    /// choice passes its parameters through unchanged.
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
}

pub struct GroupProgress<'a> {
    /// `(plane_index, planes_total)` at the start of each plane.
    pub on_plane: &'a (dyn Fn(usize, usize) + Sync),
    pub engine: EngineProgress<'a>,
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
/// does) independently of what the group itself is configured to use.
/// Returns one [`StackOutput`] per plane, in channel order — no
/// stats/accumulation/writing: that tail is the caller's job.
#[allow(clippy::too_many_arguments)]
pub(crate) fn integrate_planes(
    input: &GroupInput<'_>,
    frame_indices: &[usize],
    weights: &[Vec<f32>],
    output_mode: OutputNormalization,
    rejection_mode: RejectionNormalization,
    recipe: IntegrationRecipe,
    write_maps: bool,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &GroupProgress<'_>,
    io: IoPolicy,
) -> Result<Vec<StackOutput>, IntegrationError> {
    if weights.len() != frame_indices.len() {
        return Err(IntegrationError::BadInput(format!(
            "{} frame indices but {} weight rows",
            frame_indices.len(),
            weights.len()
        )));
    }
    let frames = input.frames;
    let n = frame_indices.len();
    let mut outputs = Vec::with_capacity(input.channels);

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
        for (k, &i) in frame_indices.iter().enumerate() {
            let f = &frames[i];
            let frame_ls = f.measurement.channels[p].location_scale();
            // `RejectionNormalization::Local` was refused before this call
            // (`integrate_group` validates it up front; the LN reference
            // never passes it), so every remaining mode returns `Some`.
            let rp = rejection_pair(ref_ls, frame_ls, rejection_mode)
                .expect("Local rejection normalization was refused before this call");
            let op = output_pair(ref_ls, frame_ls, output_mode);
            rejection_pairs.push(rp);
            output_pairs.push(op);
            plane_weights.push(weights[k].get(p).copied().unwrap_or(0.0));
            registered_frames.push(RegisteredFrame {
                path: f.path.clone(),
                map: f.map.clone(),
            });
        }

        let src = RegisteredSource::open(
            &registered_frames,
            input.width,
            input.height,
            p,
            input.interpolation,
            input.clamping,
        )?;

        let params = StackParams {
            rejection: &rejection_pairs,
            output: &output_pairs,
            weights: &plane_weights,
            range_low: input.integration.range_low.map(|v| v as f32),
            range_high: input.integration.range_high.map(|v| v as f32),
            rejection_maps: write_maps,
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
    }

    Ok(outputs)
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
    if input.normalization.rejection == RejectionNormalization::Local {
        return Err(IntegrationError::BadInput(
            "local normalization is M2; use scaleZeroOffset or equalizeFluxes".into(),
        ));
    }

    // Min-weight drop (spec §6.2): a frame whose lowest per-channel
    // normalized weight sits below the floor never joins the stack. The
    // reference is exempt — dropping it would leave nothing to normalize
    // the rest against.
    let min_weight = input.integration.min_weight;
    let mut included: Vec<usize> = Vec::with_capacity(frames.len());
    let mut dropped_below_min_weight = 0usize;
    for (i, f) in frames.iter().enumerate() {
        let wmin = f
            .weight
            .normalized
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        let below = wmin < min_weight;
        if below && i == input.reference {
            warn!(
                path = %f.path.display(),
                weight = wmin,
                min_weight,
                "reference frame is below the minimum weight but is never dropped"
            );
        } else if below {
            dropped_below_min_weight += 1;
            warn!(
                path = %f.path.display(),
                weight = wmin,
                min_weight,
                "frame dropped below the minimum weight"
            );
            continue;
        }
        included.push(i);
    }
    if included.len() < 3 {
        return Err(IntegrationError::BadInput(format!(
            "fewer than 3 frames after the weight floor ({})",
            included.len()
        )));
    }
    let included_count = included.len();

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

    let outputs = integrate_planes(
        input,
        &included,
        &weights_per_frame,
        input.normalization.output,
        input.normalization.rejection,
        recipe,
        input.integration.write_rejection_maps,
        pool,
        cancel,
        progress,
        io,
    )?;

    for (p, out) in outputs.into_iter().enumerate() {
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

        let plane_duration_ms =
            (out.base.read_duration + out.base.combine_duration + stats_start.elapsed())
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

    Ok(GroupOutput {
        width: input.width,
        height: input.height,
        channels: input.channels,
        data,
        rejection_low,
        rejection_high,
        included,
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

    #[test]
    fn config_serde_names_match_the_spec() {
        let d: IntegrationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(d.combination, Combination::Average);
        assert_eq!(d.rejection, RejectionChoice::Auto);
        assert_eq!(d.min_weight, 0.005);
        assert_eq!(d.range_low, Some(0.0));
        assert_eq!(d.range_high, None);
        assert!(!d.write_rejection_maps);
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["rejection"]["method"], "auto");
        assert_eq!(j["minWeight"], 0.005);
        assert_eq!(j["writeRejectionMaps"], false);
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

    #[test]
    fn local_rejection_normalization_is_refused_in_m1() {
        // No file I/O needed: validation rejects `Local` before any frame's
        // path is ever opened, so dummy paths and measurements are enough.
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
            StackFrame {
                path: PathBuf::from("c.fits"),
                map: identity_map(),
                measurement: dummy_measurement(1),
                weight: weight(1.0, 1),
                exposure_s: 60.0,
                date_obs: None,
            },
        ];
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig {
            rejection: RejectionNormalization::Local,
            ..NormalizationConfig::default()
        };
        let input = GroupInput {
            frames: &frames,
            reference: 0,
            width: 10,
            height: 10,
            channels: 1,
            interpolation: Interpolation::Bilinear,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
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
            io(1_000_000),
        )
        .unwrap_err();
        match err {
            IntegrationError::BadInput(msg) => {
                assert!(msg.contains("local normalization"), "{msg}")
            }
            other => panic!("expected BadInput, got {other:?}"),
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
}
