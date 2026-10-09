//! Per-frame, per-channel measurement of calibrated frames (spec §4.1):
//! detection, PSF fits, the PSF-signal totals, the large-scale background
//! residual, MRS noise, the normalization statistics and the classic SNR
//! weight — everything the weighting, selection and normalization stages
//! consume. Reads through `PlaneReader`, one plane at a time.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use astroimage::{DetectionLevels, ImageAnalyzer};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::prefilter::{self, SeedPrefilter};
use super::psf_signal::{self, FitParams, PsfModel, Seed, StarFit};
use super::structure::{self, StructureParams};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::{LocationScale, ScaleEstimator};
use crate::integration::IntegrationError;
use crate::stacking::config::SeedSource;
use crate::star_measure as sm;

// `ADU_SCALE` (calibrated frames are float32 in `[0, 1]`; the detector and
// the MRS estimator see a copy scaled by it) and `NoiseSource` moved to the
// star-measurement module (Plan A Task 2, ruling F43); re-exported so every
// `measure::ADU_SCALE` caller and `ChannelMeasurement`'s serialized enum are
// untouched.
pub use crate::star_measure::domain::{NoiseSource, ADU_SCALE};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeasureOptions {
    pub psf_model: PsfModel,
    /// Detection cap (spec §9.2 `maxStars`).
    pub max_stars: usize,
    pub scale_estimator: ScaleEstimator,
    /// The sensitivity dial — a flux-SNR floor on the detections the fitter
    /// is seeded with (the detector's own `flux / sqrt(flux + pi*r_ap^2*sigma^2)`).
    ///
    /// Any value at or below 10 is inert: the detector's own scan already
    /// refuses a detection unless `snr > 10`, so the shipped default of 5
    /// never removes anything (measured — `--min-snr 10` is bit-identical to
    /// `--min-snr 5` on 94 real frames). Only a value above 10 cuts.
    pub min_snr: f32,
    /// What the seed DETECTION runs on (math reference §5.1). The PSF fits,
    /// the aperture flux, the background model and the noise estimate always
    /// see the untouched plane, whatever this says.
    pub seed_prefilter: SeedPrefilter,
    /// Star-detection threshold for the seed population, in sigma above the
    /// LOCAL background (spec §9.2 `measurement.detectionSigma`). The
    /// detector's two ladder levels become `background + k*noise` with
    /// `k = detection_sigma` and `k/2` — the fainter level the scan retries
    /// at, half the primary the way the rank budget's `24*max_stars` is four
    /// times its `6*max_stars`.
    ///
    /// Noise-relative is the point: the rank budget this replaces set the
    /// levels from a fixed bright-pixel count, so a sharp, bright-sky night
    /// (higher peaks for the same flux) handed the estimator several times
    /// the seed population a soft, dark one did, and the frame ranking
    /// inverted against the external reference (M4a Task 2, ruling
    /// R-M4a-1).
    pub detection_sigma: f32,
    /// WHICH detector finds the seeds (spec §2.2 `measurement.seedSource`,
    /// rulings R-OSP-2 and R-M4c-11). `MatchedFilter` reads `seed_threshold`;
    /// `Peak` reads `detection_sigma` and `seed_prefilter`; `Structure` reads
    /// `structure` instead — the detectors share no dial.
    pub seed_source: SeedSource,
    /// The matched filter's threshold in units of the filtered plane's noise
    /// (ruling R-OSP-2), read when `seed_source` is `MatchedFilter`.
    pub seed_threshold: f64,
    /// The structure detector's own dials, used only when `seed_source`
    /// is `Structure`. Not a `StackingConfig` field: a run selects the
    /// detector, never its internals (M4c Task 0 calibrated them once).
    pub structure: StructureParams,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        MeasureOptions {
            psf_model: PsfModel::Auto,
            max_stars: 24576,
            scale_estimator: ScaleEstimator::Bwmv,
            min_snr: 5.0,
            seed_prefilter: DEFAULT_SEED_PREFILTER,
            detection_sigma: DEFAULT_DETECTION_SIGMA,
            seed_source: SeedSource::MatchedFilter,
            seed_threshold: sm::seeds::DEFAULT_THRESHOLD,
            structure: StructureParams::default(),
        }
    }
}

/// Default for [`MeasureOptions::detection_sigma`] and
/// [`crate::stacking::config::MeasurementConfig::detection_sigma`].
///
/// Calibrated in M4a Task 2 against an external reference's own per-frame
/// PSF-fit counts and frame ranking on 368 real frames (a mono and an OSC
/// group over three nights), sweeping `k` from 5 to 32.
///
/// It is deliberately far above the "5σ detection" convention, and that is
/// not a contradiction: this threshold is compared against a star's PEAK
/// pixel over the local background in units of the PER-PIXEL noise, while a
/// structure-based detector's sensitivity aggregates a whole star's pixels.
/// 20 is where our peak threshold reproduces that detector's answer best:
/// every mono night's fit count lands inside `[0.7, 1.4]` of the
/// reference's (1.01 / 1.05 / 0.82), and it is the only value tested at
/// which BOTH groups' top-20 frame rankings agree with the reference
/// (18/20 mono, 14/20 OSC — and the OSC top 20 draws from the same two
/// nights the reference draws from). Below ~16 the population runs 15-30 %
/// rich and the sharpest night crowds out the ranking; above ~24 that
/// night is pushed off the top entirely and the soft nights start losing
/// more than a quarter of their stars.
pub const DEFAULT_DETECTION_SIGMA: f32 = 20.0;

/// Default for [`MeasureOptions::seed_prefilter`] and
/// [`crate::stacking::config::MeasurementConfig::seed_prefilter`].
pub const DEFAULT_SEED_PREFILTER: SeedPrefilter = SeedPrefilter::None;

/// Per-phase wall time of one plane's measurement (perf tier 1 Task 0).
/// Skipped by serde: the cached `metrics` artifact payload must not change
/// shape, and a timing is not a measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeasureTimings {
    pub read_ms: u64,
    pub background_ms: u64,
    pub noise_ms: u64,
    pub detect_ms: u64,
    pub fit_ms: u64,
}

/// One channel's measurement, in native `[0, 1]` units.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ChannelMeasurement {
    pub stars_detected: usize,
    pub stars_fitted: usize,
    /// Seeds the PSF fitter refused, each for a counted reason (F-0, Plan A
    /// Task 4): `stars_fitted + stars_refused == stars_detected`. `0` in a
    /// `metrics` payload written before that task.
    pub stars_refused: usize,
    /// `stars_refused` by `star_measure::RefusalReason`, keyed by its
    /// camelCase name (`{"regionTooSmall": 12, "duplicate": 3}`); empty in a
    /// payload written before Plan A Task 4.
    pub refused_by_reason: BTreeMap<String, usize>,
    /// Of `stars_fitted`, the fits not flagged saturated (F-3, Plan A Task 8): the population `fwhm_px` and
    /// `eccentricity` are measured over. `0` in a payload written before that task.
    pub stars_unsaturated: usize,
    pub beta: f64,
    pub fwhm_px: f64,
    pub eccentricity: f64,
    pub tflux: f64,
    pub tmean_flux: f64,
    /// Mean fluxes RCR flagged in the PSF-signal totals (spec §1.1).
    pub mean_flux_rejected: usize,
    pub m_star: f64,
    pub n_star: f64,
    pub noise: f64,
    pub noise_source: NoiseSource,
    pub median: f64,
    pub mad: f64,
    pub median_mean_dev: f64,
    /// Classic `MedianMeanDev² / Noise²`.
    pub snr_weight: f64,
    pub noise_scale_low: f64,
    pub noise_scale_high: f64,
    /// Normalization location (median) and two-sided scale.
    pub location: f64,
    pub scale: f64,
    pub psf_signal_weight: f64,
    pub psf_snr: f64,
    /// Per-phase wall time of this plane's measurement (perf tier 1 Task 0).
    /// Not part of the cached `metrics` artifact payload — a timing, not a
    /// measurement.
    #[serde(skip)]
    pub timings: MeasureTimings,
}

impl ChannelMeasurement {
    pub fn location_scale(&self) -> LocationScale {
        LocationScale {
            location: self.location as f32,
            scale: self.scale as f32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMeasurement {
    pub width: usize,
    pub height: usize,
    pub channels: Vec<ChannelMeasurement>,
    pub duration_ms: u64,
}

impl FrameMeasurement {
    fn mean_of(&self, f: impl Fn(&ChannelMeasurement) -> f64) -> f64 {
        if self.channels.is_empty() {
            0.0
        } else {
            self.channels.iter().map(f).sum::<f64>() / self.channels.len() as f64
        }
    }
    pub fn mean_fwhm_px(&self) -> f64 {
        self.mean_of(|c| c.fwhm_px)
    }
    pub fn mean_eccentricity(&self) -> f64 {
        self.mean_of(|c| c.eccentricity)
    }
    /// Mean over planes of `ChannelMeasurement::median` — the frame's sky
    /// background level, native `[0, 1]` units. This is the `sky[i]`
    /// [`crate::stacking::weights::sky_penalized_order`] penalizes the
    /// normalization anchor / LN-reference-member ranking on (spec §4.4,
    /// ruling R-M3-17 v2).
    pub fn mean_median(&self) -> f64 {
        self.mean_of(|c| c.median)
    }
    pub fn mean_psf_signal_weight(&self) -> f64 {
        self.mean_of(|c| c.psf_signal_weight)
    }
    pub fn min_stars(&self) -> usize {
        self.channels
            .iter()
            .map(|c| c.stars_fitted)
            .min()
            .unwrap_or(0)
    }
}

/// One plane's [`ChannelMeasurement`] plus the [`StarFit`]s the PSF fitter
/// accepted for it (Tier C Task 1, spec §2.2.1) — `measure_plane` and
/// `measure_frame`/`measure_frame_with_seeds` keep discarding `fits`
/// exactly as they always have (they return `channel` alone); the one new
/// caller that wants both is [`measure_frame_with_fits`], which Measure's
/// stage 3 uses to persist `fits` as the on-disk `fits` artifact
/// (`stacking::fits_artifact`) beside `metrics`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaneMeasurement {
    pub channel: ChannelMeasurement,
    pub fits: Vec<StarFit>,
    /// The field width the module's fit started from (Plan A Task 6b): the matched
    /// filter's own for [`SeedOverride::MatchedFilter`], else `field_width_of` the
    /// seeds. Reported, not persisted.
    pub field_width: sm::FieldWidth,
    /// The plane is defocused (owner ruling 2026-10-08): `fits` is empty and the zero-star path below takes it.
    /// Reported, not persisted.
    pub defocused: bool,
    /// The readable seeds refused `Donut` (the verdict's `n_donut`). Reported, not persisted.
    pub defocus_donuts: usize,
    /// The seeds whose saturation verdict fit failed for a reason other than n ≤ p (F-3, Plan A Task 8, Q4; the
    /// probe's `verdictFitFailed`). Reported, not persisted.
    pub verdict_fit_failed: usize,
}

/// A probe-level override of the seed detector (ruling F26). A run never passes one: it selects its detector
/// through the config's [`SeedSource`] (`MeasureOptions::seed_source`), and `Fast` means exactly that. The other
/// variants force one detector whatever the options say (the dev harnesses' `--seeds`). `PartialEq` only since Plan A
/// Task 6a: `MatchedFilter` carries an `f64`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SeedOverride {
    /// Whatever `MeasureOptions::seed_source` says: the matched filter at `seed_threshold`, the peak detector
    /// (`ImageAnalyzer::detect_fast_data` at `detection_sigma` and `seed_prefilter`) or the structure map.
    Fast,
    /// `ImageAnalyzer::analyze_data` — the full two-pass detect+measure
    /// pipeline; its per-star metrics become the seeds instead.
    Full,
    /// [`crate::stacking::structure::detect_structures`] — the structure-map
    /// detector (M4c Task 0). `Fast` resolves to this whenever
    /// [`MeasureOptions::seed_source`] says so, so a caller only passes it
    /// explicitly to override a config that says otherwise.
    Structure,
    /// The star-measurement module's two-pass matched filter at the field
    /// width ([`crate::star_measure::seeds::detect`], Plan A Task 6a), its
    /// `threshold` in units of the filtered plane's measured noise, whatever
    /// the options say.
    MatchedFilter { threshold: f64 },
}

/// Measure one plane. Detection, fitting, the background model and MRS run
/// on an ADU-scaled copy; the sample statistics run on the native data. The
/// fit, the noise, the frame aggregates and the sample statistics are the
/// star-measurement module's ([`crate::star_measure::measure_plane`], Plan A
/// Task 2); this adapter reads the plane, runs the configured seed detector
/// and maps the module's result onto `ChannelMeasurement`.
pub fn measure_plane(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> ChannelMeasurement {
    measure_plane_with_seeds(data, w, h, opts, pool, SeedOverride::Fast).channel
}

/// As `measure_plane`, but the seeds handed to the PSF fitter come from the
/// chosen detector instead of always `detect_fast_data`, and the fitter's
/// accepted [`StarFit`]s ride alongside the measurement (Tier C Task 1)
/// instead of being dropped after `signal_totals`/`frame_shape` read them.
pub fn measure_plane_with_seeds(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedOverride,
) -> PlaneMeasurement {
    measure_plane_detailed(data, w, h, opts, pool, seed_source).0
}

/// [`measure_plane_with_seeds`], also returning the seeds the fits were made
/// from (star measurement part 1: the lab's `star_probe` dumps them). Same
/// call, same numbers; the run never reads the seeds.
pub fn measure_plane_detailed(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedOverride,
) -> (PlaneMeasurement, Vec<Seed>) {
    let (pm, seeds, _) = measure_plane_diagnosed(data, w, h, opts, pool, seed_source, false, false);
    (pm, seeds)
}

/// [`measure_plane_detailed`], also returning the module's per-seed outcome
/// records (F-0, Plan A Task 4) when `diagnostics` is set — one per seed, in
/// the returned seeds' order (empty otherwise). Same call, same numbers: the
/// records are a report the lab's `star_probe --diagnostics` writes; the run
/// never asks for them.
///
/// `colour` (F-3, Plan A Task 8, Q3): the plane is one of a debayered colour
/// frame's, so the saturation test's plateau needs all 8 neighbours
/// (`star_measure::MeasureOptions::colour`). [`measure_frame_inner`] sets it
/// for a 3-plane calibrated frame (a debayered OSC group's), the probe for a
/// 3-plane input; [`measure_plane_detailed`] and its wrappers measure one
/// plane on its own and pass `false`.
#[allow(clippy::too_many_arguments)]
pub fn measure_plane_diagnosed(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedOverride,
    diagnostics: bool,
    colour: bool,
) -> (PlaneMeasurement, Vec<Seed>, Vec<sm::FitDiagnostics>) {
    // A run selects its detector through the config's `seed_source`; `seed_override` is the probe-level override
    // the dev harnesses pass. `Fast` means "whatever the options say", so the two never contradict each other
    // silently.
    #[derive(Clone, Copy, PartialEq)]
    enum Detector {
        Peak,
        Full,
        Structure,
        Matched(f64),
    }
    let detector = match seed_source {
        SeedOverride::Fast => match opts.seed_source {
            SeedSource::MatchedFilter => Detector::Matched(opts.seed_threshold),
            SeedSource::Peak => Detector::Peak,
            SeedSource::Structure => Detector::Structure,
        },
        SeedOverride::Full => Detector::Full,
        SeedOverride::Structure => Detector::Structure,
        SeedOverride::MatchedFilter { threshold } => Detector::Matched(threshold),
    };

    // The ADU-scaled copy exists only for the detectors that read it (Task 7, C5). The matched filter runs inside
    // the module on the module's own working copy, the only one on that path.
    let scaled: Option<Vec<f32>> = match detector {
        Detector::Matched(_) => None,
        _ => Some(data.iter().map(|v| v * ADU_SCALE).collect()),
    };
    let scaled_ref: &[f32] = scaled.as_deref().unwrap_or(&[]);

    // The pre-filtered peak detection needs the noise σ BEFORE it runs, and
    // from the UNFILTERED plane: a pre-filtered detection copy has its own,
    // smaller noise, and a level derived from it would move down with the
    // stars the filter attenuates (M4a Task 2 fix round 2, ruling
    // R-M4a-14). That one arm measures the background model and MRS here,
    // on the same ADU-scaled bytes the module would, and hands the result
    // to the module, which then does not measure it again (I-1); every
    // other arm lets the module measure it. Either way the two run exactly
    // once, background first, and give the reported `noise`/`m_star`/
    // `n_star` (Plan A Task 2).
    let precomputed: Option<(sm::NoiseModel, sm::PhaseTimings)> =
        match (detector, opts.seed_prefilter) {
            (Detector::Peak, SeedPrefilter::Median3) => {
                Some(sm::noise_model(scaled_ref, w, h, pool))
            }
            _ => None,
        };

    let t = Instant::now();
    // The matched filter runs inside the module (its own working copy, its own field width); the other detectors'
    // seeds are handed in and get the module's `field_width_of`.
    let seeds: Vec<Seed> = match detector {
        Detector::Peak => {
            // Detection may run on a pre-filtered copy; EVERYTHING after it
            // (the fits below, `background_residual`, `noise_mrs`, the
            // sample statistics) stays on the untouched plane.
            let detect_on = match opts.seed_prefilter {
                SeedPrefilter::None => None,
                // Perf tier 1 Task 2 fix round 1, item 4: `median3` is
                // itself `par_chunks_mut`-parallel — the one unwrapped
                // parallel call left in this function, routed like its
                // three siblings (`background_residual`, `noise_mrs`,
                // `fit_stars`) above/below.
                SeedPrefilter::Median3 => Some(match pool {
                    Some(p) => p.install(|| prefilter::median3(scaled_ref, w, h)),
                    None => prefilter::median3(scaled_ref, w, h),
                }),
            };
            let detect_data: &[f32] = detect_on.as_deref().unwrap_or(scaled_ref);
            // Noise-relative levels, not the detector's default rank budget:
            // the seed population has to be set by how far above THIS
            // frame's sky a star stands, not by a bright-pixel count that a
            // brighter sky silently deepens.
            //
            // With a pre-filter on, the σ must come from the UNFILTERED
            // plane and the levels are handed over in ADU: the 3×3 median
            // attenuates the noise (≈ 0.42×) as well as the stars, so a
            // level the detector measured on the filtered copy would fall
            // with the peaks and the filter would cancel itself — which is
            // exactly what fix round 1 measured (ruling R-M4a-14).
            let noise_adu = precomputed.as_ref().map_or(0.0, |(n, _)| n.sigma);
            let levels = match opts.seed_prefilter {
                SeedPrefilter::None => DetectionLevels::NoiseRelative {
                    k1: opts.detection_sigma,
                    k2: opts.detection_sigma * 0.5,
                },
                SeedPrefilter::Median3 if noise_adu > 0.0 => DetectionLevels::Absolute {
                    above_bg_1: opts.detection_sigma * noise_adu as f32,
                    above_bg_2: opts.detection_sigma * 0.5 * noise_adu as f32,
                },
                SeedPrefilter::Median3 => {
                    // Neither MRS nor the background residual produced a
                    // scale — there is no unfiltered σ to hold the level
                    // still, so fall back to the filtered copy's own.
                    warn!(
                        "no unfiltered noise estimate; the pre-filtered detection loses its threshold anchor"
                    );
                    DetectionLevels::NoiseRelative {
                        k1: opts.detection_sigma,
                        k2: opts.detection_sigma * 0.5,
                    }
                }
            };
            let mut analyzer = ImageAnalyzer::new()
                .with_max_stars(opts.max_stars.max(8))
                .with_centroid_refine(false)
                .with_detection_levels(levels);
            if let Some(p) = pool {
                analyzer = analyzer.with_thread_pool(Arc::clone(p));
            }
            match analyzer.detect_fast_data(detect_data, w, h, 1) {
                Ok(r) => r
                    .stars
                    .iter()
                    .filter(|s| s.snr >= opts.min_snr && s.peak > 0.0)
                    .map(|s| Seed {
                        x: s.x as f64,
                        y: s.y as f64,
                        peak: s.peak as f64,
                        flux: s.flux as f64,
                    })
                    .collect(),
                Err(e) => {
                    warn!(error = %e, "star detection failed; the plane measures as starless");
                    Vec::new()
                }
            }
        }
        Detector::Full => {
            // `measure_cap` defaults to 500 and truncates AFTER measurement
            // (unlike `with_max_stars`, which caps detection) — left at the
            // default, the full path only ever measures the 500 brightest
            // detections regardless of `opts.max_stars`, starving PSFSW/PSF
            // SNR (both quadratic in fitted-star count) against the fast
            // arm's much larger seed set. 0 measures every detection.
            let mut analyzer = ImageAnalyzer::new()
                .with_max_stars(opts.max_stars)
                .with_measure_cap(0);
            if let Some(p) = pool {
                analyzer = analyzer.with_thread_pool(Arc::clone(p));
            }
            match analyzer.analyze_data(scaled_ref, w, h, 1) {
                Ok(r) => r
                    .stars
                    .iter()
                    .filter(|s| s.snr >= opts.min_snr && s.peak > 0.0)
                    .map(|s| Seed {
                        x: s.x as f64,
                        y: s.y as f64,
                        peak: s.peak as f64,
                        flux: s.flux as f64,
                    })
                    .collect(),
                Err(e) => {
                    warn!(error = %e, "full analysis failed; the plane measures as starless");
                    Vec::new()
                }
            }
        }
        Detector::Structure => {
            // The map is built from the same ADU-scaled copy everything
            // else measures, so the saturation limit — a `[0, 1]` fraction
            // in the config — is scaled with it. The detector carries the
            // reference's own hot-pixel median internally, so
            // `seed_prefilter` is NOT consulted here: a second median on
            // top of that one would be a different filter, not a stronger
            // one. `min_snr` is likewise inert — this detector's own
            // sensitivity IS its SNR gate.
            let p = StructureParams {
                upper_limit: opts.structure.upper_limit * ADU_SCALE,
                ..opts.structure
            };
            let mut seeds = match pool {
                Some(pl) => pl.install(|| structure::detect_structures(scaled_ref, w, h, &p)),
                None => structure::detect_structures(scaled_ref, w, h, &p),
            };
            // Brightest first already, so the cap keeps the brightest.
            seeds.truncate(opts.max_stars.max(8));
            seeds
        }
        // The module detects at the head of its own call (below).
        Detector::Matched(_) => Vec::new(),
    };
    let adapter_detect_ms = t.elapsed().as_millis() as u64;
    // The detectors were this copy's only reader; the module makes its own
    // working-unit copy of `data` (the same `v * ADU_SCALE` bytes), so the
    // plane is never held twice.
    drop(scaled);

    let (noise, own_phases) = match precomputed {
        Some((model, phases)) => (Some(model), Some(phases)),
        None => (None, None),
    };
    let module_opts = sm::MeasureOptions {
        domain: sm::Domain::Calibrated {
            gain_e_per_unit: None,
        },
        psf_model: opts.psf_model,
        max_stars: opts.max_stars,
        seeds: match detector {
            Detector::Matched(threshold) => sm::SeedSpec::MatchedFilter { threshold },
            _ => sm::SeedSpec::Given(seeds),
        },
        fit: FitParams::default(),
        noise,
        scale_estimator: opts.scale_estimator,
        diagnostics,
        mask: None,
        field_width: None,
        colour,
    };
    let m = sm::measure_plane(data, w, h, &module_opts, pool);
    let seeds = match module_opts.seeds {
        sm::SeedSpec::Given(seeds) => seeds,
        // The matched filter ran inside the module; it hands its seeds back.
        sm::SeedSpec::MatchedFilter { .. } => m.detected,
        sm::SeedSpec::None => Vec::new(),
    };
    let stars_detected = m.seeds;
    let detect_ms = match detector {
        Detector::Matched(_) => m.timings.detect_ms,
        _ => adapter_detect_ms,
    };
    // The background/noise phases are whoever measured them: the Median3
    // arm above, else the module.
    let phases = own_phases.unwrap_or(m.timings);

    let s = ADU_SCALE as f64;
    let noise = m.noise.sigma / s;
    let snr_weight = if noise > 0.0 {
        (m.sky.median_mean_dev / noise).powi(2)
    } else {
        0.0
    };
    let channel = ChannelMeasurement {
        stars_detected,
        stars_fitted: m.frame.stars_fitted,
        stars_refused: m.refused.total(),
        refused_by_reason: m.refused.by_name(),
        stars_unsaturated: m.frame.stars_unsaturated,
        beta: m.beta,
        fwhm_px: m.frame.fwhm_px.unwrap_or(0.0),
        eccentricity: m.frame.eccentricity.unwrap_or(0.0),
        tflux: m.frame.tflux / s,
        tmean_flux: m.frame.tmean_flux / s,
        mean_flux_rejected: m.frame.mean_flux_rejected,
        m_star: m.noise.m_star / s,
        n_star: m.noise.n_star / s,
        noise,
        noise_source: m.noise.source,
        median: m.sky.median,
        mad: m.sky.mad,
        median_mean_dev: m.sky.median_mean_dev,
        snr_weight,
        noise_scale_low: m.sky.noise_scale_low,
        noise_scale_high: m.sky.noise_scale_high,
        location: m.sky.location,
        scale: m.sky.scale,
        // Both estimators are scale-invariant, so the ADU-unit inputs give
        // the native-unit answer.
        psf_signal_weight: psf_signal::psf_signal_weight(
            m.frame.tflux,
            m.frame.tmean_flux,
            m.noise.sigma,
            m.noise.m_star,
        ),
        psf_snr: psf_signal::psf_snr(m.frame.tflux, m.noise.sigma),
        timings: MeasureTimings {
            read_ms: 0,
            background_ms: phases.background_ms,
            noise_ms: phases.noise_ms,
            detect_ms,
            fit_ms: m.timings.fit_ms,
        },
    };
    (
        PlaneMeasurement {
            channel,
            fits: m.stars,
            field_width: m.field_width,
            defocused: m.defocused,
            defocus_donuts: m.defocus_donuts,
            verdict_fit_failed: m.verdict_fit_failed,
        },
        seeds,
        m.diagnostics,
    )
}

/// Measure every plane of a calibrated frame; checks `cancel` before each
/// plane.
pub fn measure_frame(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FrameMeasurement, IntegrationError> {
    measure_frame_with_seeds(path, opts, pool, cancel, SeedOverride::Fast)
}

/// As `measure_frame`, but every plane is seeded via `measure_plane_with_seeds`.
pub fn measure_frame_with_seeds(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
    seed_source: SeedOverride,
) -> Result<FrameMeasurement, IntegrationError> {
    measure_frame_inner(path, opts, pool, cancel, seed_source).map(|(m, _)| m)
}

/// As `measure_frame`, but also returns every plane's accepted [`StarFit`]s
/// (Tier C Task 1), indexed like `FrameMeasurement::channels` — `fits[i]`
/// is what fed `channels[i]`. The one caller that needs them today is
/// Measure's own stage 3 (`stacking::run`), which persists each plane's
/// list as the on-disk `fits` artifact beside `metrics`; every other
/// `measure_frame`/`measure_frame_with_seeds` caller is untouched by this
/// function's existence.
pub fn measure_frame_with_fits(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<(FrameMeasurement, Vec<Vec<StarFit>>), IntegrationError> {
    measure_frame_inner(path, opts, pool, cancel, SeedOverride::Fast)
}

fn measure_frame_inner(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
    seed_source: SeedOverride,
) -> Result<(FrameMeasurement, Vec<Vec<StarFit>>), IntegrationError> {
    let start = Instant::now();
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    // F-3 (Plan A Task 8, Q3): a 3-plane calibrated frame is a debayered OSC group's, whose saturation test needs all
    // 8 neighbours of the seed pixel; a mono frame has one plane.
    let colour = reader.channels() == 3;
    let mut channels = Vec::with_capacity(reader.channels());
    let mut fits_by_plane: Vec<Vec<StarFit>> = Vec::with_capacity(reader.channels());
    for plane in 0..reader.channels() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let t = Instant::now();
        let data = reader.read_plane(plane)?;
        let read_ms = t.elapsed().as_millis() as u64;
        let _span = tracing::debug_span!("measure_plane", path = %path.display(), plane).entered();
        let pm = measure_plane_diagnosed(&data, w, h, opts, pool, seed_source, false, colour).0;
        let mut m = pm.channel;
        m.timings.read_ms = read_ms;
        // Once per plane, from the measure path only (Task 6b fix wave A, review M1): the fit reports the verdict, this
        // is the one place that knows the file.
        if pm.defocused {
            warn!(
                outcome = "defocused",
                count = pm.defocus_donuts,
                stars_detected = m.stars_detected,
                path = %path.display(),
                plane,
                "measure: the plane is defocused; no star is measured"
            );
        }
        debug!(
            path = %path.display(),
            plane,
            stars_detected = m.stars_detected,
            stars_fitted = m.stars_fitted,
            beta = m.beta,
            fwhm_px = m.fwhm_px,
            eccentricity = m.eccentricity,
            noise = m.noise,
            psf_signal_weight = m.psf_signal_weight,
            psf_snr = m.psf_snr,
            detection_sigma = opts.detection_sigma,
            seed_prefilter = opts.seed_prefilter.as_str(),
            seed_detector = opts.seed_source.as_str(),
            seed_threshold = opts.seed_threshold,
            read_ms = m.timings.read_ms,
            background_ms = m.timings.background_ms,
            noise_ms = m.timings.noise_ms,
            detect_ms = m.timings.detect_ms,
            fit_ms = m.timings.fit_ms,
            duration_ms = t.elapsed().as_millis() as u64,
            "frame plane measured"
        );
        channels.push(m);
        fits_by_plane.push(pm.fits);
    }
    Ok((
        FrameMeasurement {
            width: w,
            height: h,
            channels,
            duration_ms: start.elapsed().as_millis() as u64,
        },
        fits_by_plane,
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::test_support::add_noise;
    use std::path::PathBuf;

    /// 150 Gaussian stars (σ 1.8 px) on a jittered 15×10 grid, amplitudes
    /// `(0.1..0.3)·amp_scale`, background 0.08, Gaussian noise `noise` —
    /// [`crate::test_support::fixture_field`] (moved there in Plan A Task 2,
    /// ruling F44, so the star-measurement module's own tests build it too).
    fn field(seed: u64, amp_scale: f64, noise: f32) -> (Vec<f32>, usize, usize) {
        crate::test_support::fixture_field(seed, amp_scale, noise)
    }

    fn write(dir: &Path, name: &str, planes: &[&[f32]], w: usize, h: usize) -> PathBuf {
        let mut all = Vec::new();
        for p in planes {
            all.extend_from_slice(p);
        }
        let path = dir.join(name);
        write_fits_f32(&path, w, h, planes.len(), &all, &[]).unwrap();
        path
    }

    /// Zeroes wall-clock timings before a structural equality check — two
    /// independent calls of the same math never share a timing, so a pin
    /// comparing whole `ChannelMeasurement`s has to ignore it (perf tier 1
    /// Task 0).
    fn without_timings(mut c: ChannelMeasurement) -> ChannelMeasurement {
        c.timings = MeasureTimings::default();
        c
    }

    #[test]
    fn measure_plane_detailed_returns_the_seeds_it_fitted_and_the_same_measurement() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let opts = MeasureOptions::default();
        let plain = measure_plane_with_seeds(&data, w, h, &opts, None, SeedOverride::Fast);
        let (detailed, seeds) =
            measure_plane_detailed(&data, w, h, &opts, None, SeedOverride::Fast);
        assert_eq!(seeds.len(), detailed.channel.stars_detected);
        assert!(!seeds.is_empty());
        for f in &detailed.fits {
            assert!(
                seeds.iter().any(|sd| (sd.x - f.x).hypot(sd.y - f.y) <= 2.0),
                "fit at ({}, {}) has no seed within 2 px",
                f.x,
                f.y
            );
        }
        assert_eq!(detailed.fits, plain.fits);
        assert_eq!(
            format!("{:?}", without_timings(detailed.channel.clone())),
            format!("{:?}", without_timings(plain.channel.clone()))
        );
    }

    #[test]
    fn measures_a_synthetic_mono_frame() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "mono.fits", &[&data], w, h);
        let m = measure_frame(
            &path,
            &MeasureOptions::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!((m.width, m.height, m.channels.len()), (w, h, 1));
        let c = &m.channels[0];
        assert!(c.stars_detected >= c.stars_fitted);
        assert!(c.stars_fitted >= 120, "fitted {}", c.stars_fitted);
        let fwhm = 2.3548 * 1.8;
        assert!((c.fwhm_px - fwhm).abs() < 0.1 * fwhm, "fwhm {}", c.fwhm_px);
        assert!(c.eccentricity < 0.15, "ecc {}", c.eccentricity);
        assert!(c.beta >= 6.0, "beta {}", c.beta);
        assert!((c.noise - 0.002).abs() < 0.1 * 0.002, "noise {}", c.noise);
        assert_eq!(c.noise_source, NoiseSource::Mrs);
        assert!((c.median - 0.08).abs() < 0.001, "median {}", c.median);
        assert!((c.scale - 0.002).abs() < 0.1 * 0.002, "scale {}", c.scale);
        assert!((c.n_star - 0.002).abs() < 0.15 * 0.002, "N* {}", c.n_star);
        assert!(c.m_star > 0.0 && c.tflux > 0.0 && c.tmean_flux > 0.0);
        assert!(c.psf_signal_weight > 0.0 && c.psf_signal_weight.is_finite());
        assert!(c.psf_snr > 0.0 && c.snr_weight > 0.0);
        assert!(c.noise_scale_low > 0.0 && c.noise_scale_high > 0.0);
        assert!(m.duration_ms < 60_000);
        assert_eq!(m.min_stars(), c.stars_fitted);
    }

    /// Perf tier 1 Task 0: the phase splits `measure_plane` (in-memory, no
    /// `PlaneReader`) reports must sum to no more than the plane's own wall
    /// time, and a plane measured from memory (not `measure_frame`) never
    /// carries a read — `measure_frame_with_seeds` is the only place that
    /// sets `timings.read_ms`.
    #[test]
    fn measure_timings_are_components_of_the_plane_duration() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let opts = MeasureOptions::default();
        let t = std::time::Instant::now();
        let m = measure_plane(&data, w, h, &opts, None);
        let elapsed_ms = t.elapsed().as_millis() as u64;
        let sum =
            m.timings.background_ms + m.timings.noise_ms + m.timings.detect_ms + m.timings.fit_ms;
        assert!(
            sum <= elapsed_ms + 1,
            "phase sum {sum} exceeds the plane's own {elapsed_ms}"
        );
        assert_eq!(
            m.timings.read_ms, 0,
            "a plane measured from memory has no read"
        );
    }

    #[test]
    fn seed_source_fast_is_bit_identical_to_measure_plane() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let opts = MeasureOptions::default();
        let a = measure_plane(&data, w, h, &opts, None);
        let b = measure_plane_with_seeds(&data, w, h, &opts, None, SeedOverride::Fast).channel;
        let stars_fitted = a.stars_fitted;
        assert_eq!(without_timings(a), without_timings(b));
        assert!(stars_fitted > 0, "fixture should fit stars: {stars_fitted}");
    }

    #[test]
    fn psf_signal_weight_scales_with_signal_squared_and_inverse_noise_squared() {
        let dir = tempfile::tempdir().unwrap();
        let opts = MeasureOptions {
            psf_model: PsfModel::Moffat4,
            ..MeasureOptions::default()
        };
        let run = |name: &str, amp: f64, noise: f32| {
            let (d, w, h) = field(11, amp, noise);
            let p = write(dir.path(), name, &[&d], w, h);
            measure_frame(&p, &opts, None, &AtomicBool::new(false))
                .unwrap()
                .channels
                .remove(0)
        };
        let a = run("a.fits", 1.0, 0.002);
        let b = run("b.fits", 2.0, 0.002);
        let c = run("c.fits", 1.0, 0.004);
        let same_stars = |x: &ChannelMeasurement, y: &ChannelMeasurement| {
            (x.stars_fitted as f64 - y.stars_fitted as f64).abs() <= 0.05 * x.stars_fitted as f64
        };
        assert!(
            same_stars(&a, &b) && same_stars(&a, &c),
            "{} {} {}",
            a.stars_fitted,
            b.stars_fitted,
            c.stars_fitted
        );
        let r = b.psf_signal_weight / a.psf_signal_weight;
        assert!(r > 3.4 && r < 4.6, "signal ×2 → PSFSW ratio {r}");
        let r = b.psf_snr / a.psf_snr;
        assert!(r > 3.6 && r < 4.4, "signal ×2 → PSFSNR ratio {r}");
        let r = c.psf_signal_weight / a.psf_signal_weight;
        assert!(r > 0.19 && r < 0.31, "noise ×2 → PSFSW ratio {r}");
        let r = c.psf_snr / a.psf_snr;
        assert!(r > 0.2 && r < 0.3, "noise ×2 → PSFSNR ratio {r}");
    }

    #[test]
    fn rgb_frames_measure_each_plane_and_honour_cancel() {
        let (r, w, h) = field(3, 1.0, 0.002);
        let g: Vec<f32> = r.iter().map(|v| v * 0.5).collect();
        let b = r.clone();
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "rgb.fits", &[&r, &g, &b], w, h);
        let opts = MeasureOptions {
            psf_model: PsfModel::Moffat4,
            ..MeasureOptions::default()
        };
        let m = measure_frame(&path, &opts, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(m.channels.len(), 3);
        let (cr, cg, cb) = (&m.channels[0], &m.channels[1], &m.channels[2]);
        assert!((cg.median - 0.5 * cr.median).abs() < 0.001);
        assert!(
            (cg.psf_signal_weight - cr.psf_signal_weight).abs() < 0.15 * cr.psf_signal_weight,
            "PSFSW is scale-invariant: {} vs {}",
            cg.psf_signal_weight,
            cr.psf_signal_weight
        );
        assert!((cb.stars_fitted as i64 - cr.stars_fitted as i64).abs() <= 2);
        assert!((cb.psf_signal_weight - cr.psf_signal_weight).abs() < 1e-3 * cr.psf_signal_weight);
        assert!(m.mean_fwhm_px() > 3.0 && m.min_stars() >= 120);
        assert!(m.mean_eccentricity() < 0.15 && m.mean_psf_signal_weight() > 0.0);
        assert!(matches!(
            measure_frame(&path, &opts, None, &AtomicBool::new(true)),
            Err(IntegrationError::Cancelled)
        ));
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"psfSignalWeight\"") && json.contains("\"noiseSource\":\"mrs\""));
        // serde_json's default float parser is best-effort (bit-exact needs
        // its `float_roundtrip` feature), so compare within 1 ULP-scale slack.
        let back: FrameMeasurement = serde_json::from_str(&json).unwrap();
        assert_eq!(
            (back.width, back.height, back.duration_ms),
            (m.width, m.height, m.duration_ms)
        );
        assert_eq!(back.channels.len(), m.channels.len());
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-12 * b.abs().max(1e-300);
        for (a, b) in back.channels.iter().zip(&m.channels) {
            assert_eq!(
                (a.stars_detected, a.stars_fitted, a.noise_source),
                (b.stars_detected, b.stars_fitted, b.noise_source)
            );
            for (x, y) in [
                (a.beta, b.beta),
                (a.fwhm_px, b.fwhm_px),
                (a.eccentricity, b.eccentricity),
                (a.tflux, b.tflux),
                (a.tmean_flux, b.tmean_flux),
                (a.m_star, b.m_star),
                (a.n_star, b.n_star),
                (a.noise, b.noise),
                (a.median, b.median),
                (a.mad, b.mad),
                (a.median_mean_dev, b.median_mean_dev),
                (a.snr_weight, b.snr_weight),
                (a.noise_scale_low, b.noise_scale_low),
                (a.noise_scale_high, b.noise_scale_high),
                (a.location, b.location),
                (a.scale, b.scale),
                (a.psf_signal_weight, b.psf_signal_weight),
                (a.psf_snr, b.psf_snr),
            ] {
                assert!(close(x, y), "{x} vs {y}");
            }
        }
        let bad = dir.path().join("nope.txt");
        std::fs::write(&bad, b"x").unwrap();
        assert!(matches!(
            measure_frame(&bad, &opts, None, &AtomicBool::new(false)),
            Err(IntegrationError::BadInput(_))
        ));
    }

    /// Tier C Task 1: `measure_plane_with_seeds` hands back the same fits
    /// count `ChannelMeasurement::stars_fitted` reports, and
    /// `measure_frame_with_fits` indexes them per plane exactly like
    /// `FrameMeasurement::channels` — the shape Measure's stage 3 relies on
    /// to write one `.athf` file per plane. `measure_frame` (the discarding
    /// path every other caller still uses) measures byte-identically to the
    /// fits-returning one.
    #[test]
    fn measure_plane_and_frame_expose_the_accepted_fits() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let opts = MeasureOptions::default();
        let pm = measure_plane_with_seeds(&data, w, h, &opts, None, SeedOverride::Fast);
        assert_eq!(pm.fits.len(), pm.channel.stars_fitted);
        assert!(!pm.fits.is_empty(), "fixture should fit stars");

        let r = data;
        let g: Vec<f32> = r.iter().map(|v| v * 0.5).collect();
        let b = r.clone();
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "rgb.fits", &[&r, &g, &b], w, h);
        let frame_opts = MeasureOptions {
            psf_model: PsfModel::Moffat4,
            ..MeasureOptions::default()
        };
        let (m, fits) =
            measure_frame_with_fits(&path, &frame_opts, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(fits.len(), m.channels.len());
        for (channel, plane_fits) in m.channels.iter().zip(&fits) {
            assert_eq!(channel.stars_fitted, plane_fits.len());
        }
        // `measure_frame` (the discarding path every other caller still
        // uses) measures identically — timings and `duration_ms` are
        // wall-clock, not measurement output (same convention as
        // `without_timings` above), so they're zeroed before comparing.
        fn without_frame_timings(mut f: FrameMeasurement) -> FrameMeasurement {
            for c in &mut f.channels {
                c.timings = MeasureTimings::default();
            }
            f.duration_ms = 0;
            f
        }
        let discarding = measure_frame(&path, &frame_opts, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(without_frame_timings(discarding), without_frame_timings(m));
    }

    #[test]
    fn a_starless_plane_measures_with_zero_weight() {
        let (w, h) = (256, 256);
        let mut data = vec![0.1f32; w * h];
        add_noise(&mut data, 0.002, 41);
        let c = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        // A noise peak or two may survive the fitter; a real field scores
        // around 1e-3 on this fixture family, a noise fit around 1e-10.
        assert!(
            c.stars_fitted <= 3,
            "noise peaks fitted as stars: {}",
            c.stars_fitted
        );
        assert!(
            c.psf_signal_weight < 1e-6 && c.psf_snr < 1e-6,
            "{} {}",
            c.psf_signal_weight,
            c.psf_snr
        );
        if c.stars_fitted == 0 {
            assert_eq!((c.fwhm_px, c.eccentricity), (0.0, 0.0));
        }
        assert!((c.noise - 0.002).abs() < 0.1 * 0.002, "noise {}", c.noise);
        assert!((c.location - 0.1).abs() < 0.001, "location {}", c.location);
    }

    /// The threshold is a real dial: a high sigma keeps only the bright
    /// end of the field, and the estimator's answer moves with the
    /// population but not pathologically (PSFSW is quadratic in signal and
    /// inverse-quadratic in noise — a 3x band bounds it).
    #[test]
    fn detection_sigma_steers_the_seed_population() {
        // The fixture's faintest star is amplitude 0.1 on noise 0.002, i.e.
        // 50 sigma, and the SECOND (fainter) level sits at half the dial —
        // so the dial has to pass 100 before it cuts into the population at
        // all. At 160 the deep level is 80 sigma and keeps roughly the
        // brightest 70% (amplitudes are uniform on [0.1, 0.3]).
        const HIGH_SIGMA: f32 = 160.0;
        let (data, w, h) = field(7, 1.0, 0.002);
        let low = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seed_source: SeedSource::Peak,
                detection_sigma: 5.0,
                ..MeasureOptions::default()
            },
            None,
        );
        let high = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seed_source: SeedSource::Peak,
                detection_sigma: HIGH_SIGMA,
                ..MeasureOptions::default()
            },
            None,
        );
        assert!(
            low.stars_fitted >= 120,
            "sigma 5 keeps the fixture's pinned population: {}",
            low.stars_fitted
        );
        assert!(
            high.stars_fitted < low.stars_fitted,
            "sigma {HIGH_SIGMA} must fit strictly fewer: {} vs {}",
            high.stars_fitted,
            low.stars_fitted
        );
        let r = high.psf_signal_weight / low.psf_signal_weight;
        assert!(
            r > 1.0 / 3.0 && r < 3.0,
            "PSFSW sensitivity to the population is bounded: ratio {r} ({} vs {} fits)",
            high.stars_fitted,
            low.stars_fitted
        );
    }

    /// The shipping path (`SeedPrefilter::None`) pinned by its NUMBERS, not
    /// by a delegate equality — `measure_plane` forwards to
    /// `measure_plane_with_seeds`, so comparing the two can never fail.
    ///
    /// These four are what `field(7, 1.0, 0.002)` — 150 Gaussian stars,
    /// σ 1.8 px, background 0.08, noise 0.002 — measures through the
    /// default options, recorded 2026-09-11 (M4a Task 2 fix round 3). Any
    /// later change to detection, the fit region, the acceptance rules or
    /// the estimator moves at least one of them; that is the point. A task
    /// that changes one on purpose says so and rewrites the literal.
    ///
    /// Rewritten once, by Tier C item C3 (the Moffat LM arithmetic — one
    /// `powf` per sample-iteration instead of three, `(1+Q)^(-β-1)` as
    /// `(1+Q)^(-β)/(1+Q)`, `powi` at integral β). It moved `fwhm_px` by ONE
    /// ulp, 4.122_658_320_014_302_55 → 4.122_658_320_014_304; every other
    /// field here — the fit counts, β and `psf_signal_weight` — is
    /// bit-identical across that change, which is why
    /// the fit-version constant (3 then; now `star_measure::MEASURE_VERSION`) was NOT bumped (no cached `metrics`,
    /// `fits` or `.athln` artifact is made wrong by a last-bit FWHM).
    ///
    /// Rewritten by Plan A Task 5 (F-2, the stamp-centred amplitude-normalised fit with the corrected θ column):
    /// `psf_signal_weight` and `fwhm_px` moved by design; the counts and β did not.
    ///
    /// Rewritten by Plan A Task 6b (F-1/F-7: the per-star width start, the region that scales with the star, the rim
    /// background start): `psf_signal_weight` and `fwhm_px` moved by design; the counts and β did not. Rewritten again by
    /// Task 6b round 1 (ruling R1: the width start is the star's own half-maximum width where readable) and round 3
    /// (ruling R9: the start shape from the region's starting square): the same two.
    #[test]
    fn the_shipped_none_path_measures_exactly_these_numbers() {
        let (data, w, h) = field(7, 1.0, 0.002);
        // Task 7 (R-OSP-2): the product default is the matched filter now (its numbers are pinned in
        // `the_matched_filter_default_measures_exactly_these_numbers`); these literals are the PEAK detector's, kept.
        let opts = MeasureOptions {
            seed_source: SeedSource::Peak,
            ..MeasureOptions::default()
        };
        assert_eq!(opts.seed_prefilter, SeedPrefilter::None);
        let c = measure_plane(&data, w, h, &opts, None);
        assert_eq!(
            (c.stars_detected, c.stars_fitted),
            (150, 150),
            "the whole fixture field is detected and fitted"
        );
        assert_eq!(c.beta, 10.0, "Auto resolves to the widest β on Gaussians");
        // F-1 (Task 6b round 3, ruling R9): the start shape is read from the region's starting square; the previous
        // literal was 1.390_799_753_469_352e-3 (round 1, the own half-maximum width start), before it
        // 1.387_793_525_585_654_9e-3 (Task 6b's first round), 1.387_179_128_762_254_4e-3 (Task 5) and
        // 1.387_068_045_273_481_56e-3.
        assert_eq!(c.psf_signal_weight, 1.390_901_839_370_229e-3);
        // `fwhm_px` is a shape aggregate over 150 Moffat fits whose arithmetic
        // (Tier C C3: `powi`/`powf`, the reused residual) rounds differently
        // per platform's libm — the first push after C3 read 4.122_658_320_014_302_5
        // on the Windows runner against this Mac's ..._304, one ulp. A last-bit
        // FWHM is not a contract, so this aggregate is pinned to a few ulp,
        // never exactly; the fields above stay exact on every platform.
        // F-3 (Task 8, ruling R-OSP-5): the frame statistic reads the unflagged fits only — 141 of the 150 here (the
        // brightest stars' verdict fits top the plane's range, by design); the fits, β and the totals did not move. The
        // previous literal was 4.129_401_659_376_219 (Task 6b round 3).
        // F-1 (Task 6b round 3, ruling R9): as above; the literal before it was 4.129_358_265_048_832 (round 1), before
        // it 4.123_653_811_131_548 (Task 6b's first round), 4.123_100_751_260_767 (Task 5) and 4.122_658_320_014_304.
        assert_eq!(c.stars_unsaturated, 141);
        assert_ulp_close(c.fwhm_px, 4.129_098_214_007_167_5, "fwhm_px");
        // The delegate equality, kept as a cheap structural check — it is
        // not the pin above. Timings are wall-clock, not measurement output,
        // so they're zeroed before the comparison (perf tier 1 Task 0).
        assert_eq!(
            without_timings(c),
            without_timings(
                measure_plane_with_seeds(&data, w, h, &opts, None, SeedOverride::Fast).channel
            )
        );
    }

    /// With the filter on, the levels are anchored on the unfiltered noise
    /// — so the filter's own attenuation of the noise cannot move them.
    /// On this fixture the median suppresses the (σ 1.8 px, well-sampled)
    /// stars only mildly, so the population survives; what the test pins is
    /// that the two settings are genuinely different measurements and that
    /// the filtered one does not collapse.
    #[test]
    fn the_median_prefilter_keeps_a_well_sampled_population() {
        let (data, w, h) = field(7, 1.0, 0.002);
        // The pre-filter is the peak detector's dial (Task 7): both arms run it explicitly.
        let plain = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seed_source: SeedSource::Peak,
                ..MeasureOptions::default()
            },
            None,
        );
        let filtered = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seed_source: SeedSource::Peak,
                seed_prefilter: SeedPrefilter::Median3,
                ..MeasureOptions::default()
            },
            None,
        );
        // Timings are wall-clock, not measurement output, so they're zeroed
        // before the comparison (perf tier 1 Task 0) — otherwise two
        // independent calls almost always differ in at least one phase
        // timing and this pin would pass no matter what the measurement
        // numbers say.
        assert_ne!(
            without_timings(plain.clone()),
            without_timings(filtered.clone()),
            "the pre-filter must change something"
        );
        assert!(
            filtered.stars_fitted >= 100,
            "a well-sampled field survives the median: {} vs {}",
            filtered.stars_fitted,
            plain.stars_fitted
        );
        // Everything downstream of detection reads the untouched plane, so
        // the noise and the background model are bit-identical.
        assert_eq!(filtered.noise, plain.noise);
        assert_eq!(
            (filtered.m_star, filtered.n_star),
            (plain.m_star, plain.n_star)
        );
        assert_eq!(filtered.median, plain.median);
    }

    /// The structure detector is a real, selectable second seed source:
    /// it fits the fixture's stars, it is NOT the same measurement as the
    /// peak detector's, and everything downstream of detection still reads
    /// the untouched plane (so the noise, the background model and the
    /// sample statistics are bit-identical between the two).
    #[test]
    fn the_structure_detector_seeds_a_measurement() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let peak = MeasureOptions {
            seed_source: SeedSource::Peak,
            ..MeasureOptions::default()
        };
        let structure = MeasureOptions {
            seed_source: SeedSource::Structure,
            ..MeasureOptions::default()
        };
        let a = measure_plane(&data, w, h, &peak, None);
        let b = measure_plane(&data, w, h, &structure, None);
        assert!(
            b.stars_fitted >= 100,
            "the structure map must seed the fixture's field: {} fits of {} seeds",
            b.stars_fitted,
            b.stars_detected
        );
        // Timings are wall-clock, not measurement output, so they're zeroed
        // before the comparison (perf tier 1 Task 0) — otherwise two
        // independent calls almost always differ in at least one phase
        // timing and this pin would pass no matter what the measurement
        // numbers say.
        assert_ne!(
            without_timings(a.clone()),
            without_timings(b.clone()),
            "the two detectors are different measurements"
        );
        assert_eq!(b.noise, a.noise);
        assert_eq!((b.m_star, b.n_star), (a.m_star, a.n_star));
        assert_eq!((b.median, b.mad, b.location), (a.median, a.mad, a.location));
        assert!(b.psf_signal_weight > 0.0 && b.psf_snr > 0.0);
        // The config selects the detector; `SeedOverride::Fast` follows it,
        // and naming the source explicitly is the same measurement. Timings
        // are wall-clock, not measurement output, so they're zeroed before
        // the comparison (perf tier 1 Task 0).
        assert_eq!(
            without_timings(b),
            without_timings(
                measure_plane_with_seeds(&data, w, h, &peak, None, SeedOverride::Structure).channel
            )
        );
        // ... while an explicit `Full` still wins over the config.
        assert_eq!(
            without_timings(
                measure_plane_with_seeds(&data, w, h, &structure, None, SeedOverride::Full).channel
            ),
            without_timings(
                measure_plane_with_seeds(&data, w, h, &peak, None, SeedOverride::Full).channel
            )
        );
    }

    #[test]
    fn options_serde_defaults() {
        let d = MeasureOptions::default();
        assert_eq!(d.max_stars, 24576);
        assert_eq!(d.psf_model, PsfModel::Auto);
        assert_eq!(d.scale_estimator, ScaleEstimator::Bwmv);
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"psfModel\":\"auto\"") && json.contains("\"maxStars\":24576"));
        assert!(json.contains("\"scaleEstimator\":\"bwmv\"") && json.contains("\"minSnr\":5.0"));
        // M4a Task 2 (ruling R-M4a-1): the seed threshold is a real option
        // now — this used to assert the field's ABSENCE.
        assert_eq!(d.detection_sigma, DEFAULT_DETECTION_SIGMA);
        assert!(json.contains("\"detectionSigma\":20.0"));
        assert_eq!(d.seed_prefilter, DEFAULT_SEED_PREFILTER);
        assert!(json.contains("\"seedPrefilter\":\"none\""));
        // M4c Task 0 (ruling R-M4c-11): which detector, and its own dials.
        // Task 7 (R-OSP-2): the default source is the module's matched filter.
        assert_eq!(d.seed_source, SeedSource::MatchedFilter);
        assert_eq!(d.seed_threshold, sm::seeds::DEFAULT_THRESHOLD);
        assert!(json.contains("\"seedSource\":\"matchedFilter\""));
        assert_eq!(d.structure, StructureParams::default());
        assert_eq!(
            serde_json::from_str::<MeasureOptions>("{}").unwrap(),
            MeasureOptions::default()
        );
        let c: ChannelMeasurement = serde_json::from_str("{\"starsFitted\":3}").unwrap();
        assert_eq!(c.stars_fitted, 3);
        assert_eq!(c.noise_source, NoiseSource::Mrs);
    }

    /// Plan A Task 4 (F-0): the channel accounts for every seed it detected,
    /// its per-reason counts sum to `stars_refused`, the two keys reach the
    /// `metrics` payload, and a payload written before the task decodes with
    /// no refusals. Two stars sit 4 px from the right and bottom edges: the
    /// adapter's own detector seeds them, and their width-start box (radius
    /// ⌈2·F_field⌉ ≈ 9 px since Plan A Task 6b; the nominal stamp before it)
    /// leaves the image, so the fitter refuses them `outsideImage` and the
    /// accounting is never the trivial `stars_refused: 0`.
    #[test]
    fn channel_accounts_for_every_seed_and_an_old_payload_decodes() {
        let (mut data, w, h) = field(7, 1.0, 0.002);
        for &(sx, sy) in &[(763.0f64, 250.0f64), (250.0, 507.0)] {
            for y in 0..h {
                for x in 0..w {
                    let r2 = (x as f64 - sx).powi(2) + (y as f64 - sy).powi(2);
                    data[y * w + x] += (0.25 * (-r2 / (2.0 * 1.8 * 1.8)).exp()) as f32;
                }
            }
        }
        let c = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        assert!(c.stars_detected > 0);
        assert!(c.stars_refused >= 1, "the edge stars are refused: {c:?}");
        assert!(
            c.refused_by_reason
                .get("outsideImage")
                .copied()
                .unwrap_or(0)
                >= 1,
            "{:?}",
            c.refused_by_reason
        );
        assert_eq!(c.stars_fitted + c.stars_refused, c.stars_detected);
        assert_eq!(c.refused_by_reason.values().sum::<usize>(), c.stars_refused);
        let json = serde_json::to_string(&c).unwrap();
        assert!(
            json.contains("\"starsRefused\":") && json.contains("\"refusedByReason\":"),
            "{json}"
        );
        let old: ChannelMeasurement =
            serde_json::from_str("{\"starsDetected\":5,\"starsFitted\":3}").unwrap();
        assert_eq!((old.stars_refused, old.refused_by_reason.len()), (0, 0));
    }

    /// Tier A task-3b pin (controller ruling R-TA-2): `measure_plane_with_seeds`
    /// must return the identical `ChannelMeasurement` (timings excluded, per
    /// `without_timings`) before and after the `estimate_noise_mrs`
    /// residency rewrite (rustafits ping-pong buffers + bit-packed
    /// `sig_mask`) and this module's own scope-narrowing pass. The Debug
    /// string is the pin — captured verbatim from the CURRENT (pre-task-3b)
    /// code, so any drift in any field (not just the ones this task
    /// deliberately touches) fails loudly.
    ///
    /// Rewritten once, by Tier C item C3 (the Moffat LM arithmetic). Of the
    /// 22 fields exactly TWO moved, both shape aggregates over the same 150
    /// fits: `fwhm_px` 4.121_806_182_224_867_5 → 4.121_806_182_224_867 (one
    /// ulp) and `eccentricity` 0.095_246_020_871_035_72 →
    /// 0.095_246_020_871_036_08 (3.8e-15 relative — a difference of
    /// similar quantities, so it amplifies the same last-bit move).
    /// `stars_fitted`, `beta`, `tflux`, `m_star`, `n_star`,
    /// `psf_signal_weight` and `psf_snr` are bit-identical.
    ///
    /// Plan A Task 4 (F-0) added two fields to the literal, `stars_refused: 0`
    /// and `refused_by_reason: {}` (150 seeds, 150 fits: nothing refused);
    /// every number in it is unchanged.
    ///
    /// Rewritten by Plan A Task 5 (F-2, the stamp-centred amplitude-normalised fit with the corrected θ column): the
    /// fit-derived fields `fwhm_px`, `eccentricity`, `tflux`, `tmean_flux`, `psf_signal_weight` and `psf_snr` moved
    /// by design; the counts, β and every noise/sky field did not.
    ///
    /// Rewritten by Plan A Task 6b (F-1/F-7: the per-star width start, the region that scales with the star, the rim
    /// background start): the same six fit-derived fields moved by design; the counts, β and every noise/sky field
    /// did not. Rewritten again by Task 6b round 1 (ruling R1: the own half-maximum width start) and round 3 (ruling R9:
    /// the start shape from the region's starting square): the same six.
    #[test]
    fn measure_plane_with_seeds_pinned_before_task_3b_rewrite() {
        let (data, w, h) = field(2026_09_19, 1.0, 0.002);
        // Task 7 (R-OSP-2): the PEAK detector's pin, kept as it was; the matched-filter default is pinned beside it.
        let opts = MeasureOptions {
            seed_source: SeedSource::Peak,
            ..MeasureOptions::default()
        };
        let mut m = without_timings(
            measure_plane_with_seeds(&data, w, h, &opts, None, SeedOverride::Fast).channel,
        );
        // The two shape aggregates over the 150 Moffat fits round differently
        // per platform's libm since Tier C C3 (the first CI run after it read
        // fwhm_px 4.121_806_182_224_867_5 on Windows and eccentricity
        // ..._035_98 / ..._036_19 on Windows / Linux against this Mac's
        // ..._036_08 — 1e-16 absolute, 4e-15 relative). They are pinned to a
        // few ulp and then snapped to the pinned value, so the remaining 20
        // fields are compared EXACTLY through one Debug string as before.
        // F-1 (Task 6b round 3, ruling R9: the start shape from the starting square); the previous literal was
        // 4.130_191_893_023_729 (round 1), before it 4.123_144_488_377_148 (Task 6b's first round),
        // 4.121_938_449_255_613 (Task 5) and 4.121_806_182_224_867.
        // F-3 (Task 8, ruling R-OSP-5): the statistic's population is the 141 unflagged fits of 150 (the fits did not
        // move); the previous literals were fwhm_px 4.130_443_961_173_142 and eccentricity 0.113_127_912_625_514_87
        // (Task 6b round 3).
        assert_ulp_close(m.fwhm_px, 4.130_618_758_639_208_5, "fwhm_px");
        // As above; the literal before Task 8 was 0.113_127_912_625_514_87, before it 0.110_290_631_118_640_29 (round 1),
        // 0.109_077_618_457_969_11 (Task 6b's first round), 0.111_709_924_171_636_75 (Task 5) and
        // 0.095_246_020_871_036_08.
        assert_ulp_close(m.eccentricity, 0.113_726_031_990_419_54, "eccentricity");
        m.fwhm_px = 4.130_618_758_639_208_5;
        m.eccentricity = 0.113_726_031_990_419_54;
        // F-1 (Task 6b round 3, ruling R9): the start shape from the region's starting square moved the fit; round 1
        // read fwhm_px 4.130191893023729, eccentricity 0.11029063111864029, tflux 571.0240326481198, tmean_flux
        // 11.813958121959663, psf_signal_weight 0.0014726432553409737, psf_snr 0.002168467672133753.
        // F-1/F-7 (Task 6b round 1, ruling R1): the own half-maximum width start moved the fit; Task 6b's first round
        // read fwhm_px 4.123144488377148, eccentricity 0.10907761845796911, tflux 569.3936624492295, tmean_flux
        // 11.820962540092, psf_signal_weight 0.001469309237358735, psf_snr 0.002156102665484917. Before it (Task 6b,
        // the box moments, from Task 5's): fwhm_px 4.121938449255613, eccentricity 0.11170992417163675, tflux
        // 568.9959995966527, tmean_flux 11.82062196789549, psf_signal_weight 0.0014682407736153376, psf_snr
        // 0.0021530920856719002 (every other field unchanged). Task 5's own move (the corrected Moffat θ column,
        // attribution.txt) was from fwhm_px 4.121806182224867, eccentricity 0.09524602087103608, tflux
        // 569.0781285580445, tmean_flux 11.823163448679521, psf_signal_weight 0.0014687684228686841, psf_snr
        // 0.0021537136857493117.
        assert_eq!(
            format!("{m:?}"),
            "ChannelMeasurement { stars_detected: 150, stars_fitted: 150, \
stars_refused: 0, refused_by_reason: {}, stars_unsaturated: 141, \
beta: 10.0, fwhm_px: 4.1306187586392085, eccentricity: 0.11372603199041954, \
tflux: 571.0507421478552, tmean_flux: 11.813155657437393, \
mean_flux_rejected: 0, m_star: 0.0013608953886758983, \
n_star: 0.002002734525205997, noise: 0.0019919854930511845, \
noise_source: Mrs, median: 0.08010764420032501, \
mad: 0.001409422606229782, median_mean_dev: 0.003131831530481577, \
snr_weight: 2.471863255689976, noise_scale_low: 0.0010744471801444888, \
noise_scale_high: 0.0024544917978346348, location: 0.08010764420032501, \
scale: 0.0020880382508039474, psf_signal_weight: 0.001472612103695384, \
psf_snr: 0.0021686705358941864, timings: MeasureTimings { read_ms: 0, \
background_ms: 0, noise_ms: 0, detect_ms: 0, fit_ms: 0 } }"
        );
    }

    /// Task 7 (R-OSP-2, ruling F14): the shipped default is the module's matched filter. The Peak literals of
    /// `the_shipped_none_path_measures_exactly_these_numbers` were psf_signal_weight 1.390_901_839_370_229e-3 and
    /// fwhm_px 4.129_401_659_376_219 on this fixture; the counts and β did not move.
    #[test]
    fn the_matched_filter_default_measures_exactly_these_numbers() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let c = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        assert_eq!((c.stars_detected, c.stars_fitted), (150, 150));
        assert_eq!(c.beta, 10.0);
        assert_eq!(c.psf_signal_weight, 1.391_065_217_128_910_3e-3);
        // F-3 (Task 8, ruling R-OSP-5): the statistic over the 141 unflagged fits of 150; the previous literal was
        // 4.129_369_725_278_327.
        assert_eq!(c.stars_unsaturated, 141);
        assert_ulp_close(c.fwhm_px, 4.129_063_469_967_53, "fwhm_px");
    }

    /// Task 7 (ruling F14): the full-record pin on the matched-filter default; the Peak record is
    /// `measure_plane_with_seeds_pinned_before_task_3b_rewrite` (its fwhm_px 4.130_443_961_173_142, eccentricity
    /// 0.113_127_912_625_514_87, psf_signal_weight 0.001_472_612_103_695_384, psf_snr 0.002_168_670_535_894_186_4).
    #[test]
    fn measure_plane_with_seeds_matched_filter_default_record() {
        let (data, w, h) = field(2026_09_19, 1.0, 0.002);
        let mut m = without_timings(
            measure_plane_with_seeds(
                &data,
                w,
                h,
                &MeasureOptions::default(),
                None,
                SeedOverride::Fast,
            )
            .channel,
        );
        // The two shape aggregates are pinned to a few ulp and snapped, as in the Peak record. F-3 (Task 8, ruling
        // R-OSP-5): over the 141 unflagged fits of 150 (the fits did not move); the previous literals were fwhm_px
        // 4.130_362_798_202_614 and eccentricity 0.111_119_809_985_610_2.
        assert_ulp_close(m.fwhm_px, 4.130_521_827_498_531, "fwhm_px");
        assert_ulp_close(m.eccentricity, 0.112_086_634_567_625_24, "eccentricity");
        m.fwhm_px = 4.130_521_827_498_531;
        m.eccentricity = 0.112_086_634_567_625_24;
        assert_eq!(
            format!("{m:?}"),
            "ChannelMeasurement { stars_detected: 150, stars_fitted: 150, \
stars_refused: 0, refused_by_reason: {}, stars_unsaturated: 141, \
beta: 10.0, fwhm_px: 4.130521827498531, eccentricity: 0.11208663456762524, \
tflux: 571.0623650541556, tmean_flux: 11.813897354284132, \
mean_flux_rejected: 0, m_star: 0.0013608953886758983, \
n_star: 0.002002734525205997, noise: 0.0019919854930511845, \
noise_source: Mrs, median: 0.08010764420032501, \
mad: 0.001409422606229782, median_mean_dev: 0.003131831530481577, \
snr_weight: 2.471863255689976, noise_scale_low: 0.0010744471801444888, \
noise_scale_high: 0.0024544917978346348, location: 0.08010764420032501, \
scale: 0.0020880382508039474, psf_signal_weight: 0.0014727345373840676, \
psf_snr: 0.0021687588170541364, timings: MeasureTimings { read_ms: 0, \
background_ms: 0, noise_ms: 0, detect_ms: 0, fit_ms: 0 } }"
        );
    }

    /// A few ulp of tolerance for the one class of pinned number whose last
    /// bits legitimately differ between platforms: an aggregate over Moffat
    /// fits (`powf`/`exp` in the platform libm). 1e-12 relative is ≈ 4 500
    /// ulp at these magnitudes — far above any libm difference (the measured
    /// ones are 1–2 ulp) and far below anything the pipeline could notice.
    fn assert_ulp_close(got: f64, want: f64, what: &str) {
        let rel = ((got - want) / want).abs();
        assert!(
            rel <= 1e-12,
            "{what}: got {got:?}, pinned {want:?} (relative {rel:.3e} > 1e-12)"
        );
    }
}
