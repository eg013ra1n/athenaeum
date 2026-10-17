//! Per-frame, per-channel measurement of calibrated frames (spec §4.1):
//! detection, PSF fits, the PSF-signal totals, the large-scale background
//! residual, MRS noise, the normalization statistics and the classic SNR
//! weight — everything the weighting, selection and normalization stages
//! consume. Reads through `PlaneReader`, one plane at a time.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use astroimage::{DetectionLevels, ImageAnalyzer};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use super::prefilter::{self, SeedPrefilter};
use super::psf_signal::{self, FitParams, PsfModel, Seed, StarFit};
use super::structure::{self, StructureParams};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::{LocationScale, ScaleEstimator};
use crate::integration::IntegrationError;
use crate::stacking::config::SeedSource;
use crate::stacking::pass1_artifact::{Pass1ArtifactError, Pass1Head, SourceStamp};
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
    /// `Auto` (the default) is the continuous per-frame β from Plan A Task 9 (F-4, ruling R-OSP-6, the owner's E5): the
    /// median of the unflagged bright seeds' pixel-integrated bounded free-β fits (each on its verdict fit's own
    /// pixels), in `star_measure::BETA_BOUNDS`; `Moffat4` fits every star at 4.
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
    /// The two star populations (Plan A Task 13b, [`sm::population`]): `None` = every seed is fitted (the stacking Measure
    /// stage since route A, owner decision 2026-10-17, and the tests), `Some` = the pass fits the frame-statistic
    /// population and the star list only (the probes; the weight that needs it is a later cycle, STOP R13b-11).
    pub population: Option<StarPopulation>,
}

/// The star-list sizes of the Measure stage's populations (R13b-3): list (b) is the first `list_mono` seeds of a mono
/// plane, the first `list_colour` of a colour plane's (Register re-detects a colour frame on its luminance, so only
/// local normalization reads those).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarPopulation {
    /// N, the frame-statistic stars (population (a)).
    pub statistic: usize,
    pub list_mono: usize,
    pub list_colour: usize,
}

/// A mono plane's star list is this many times `registration.max_stars` (the top registration stars by signal lie
/// within seed rank 3 270-3 575 on set 204, against 2 x 2 000 = 4 000).
pub const MONO_LIST_PER_REGISTRATION_STAR: usize = 2;
/// A colour plane's star list: 4 x local normalization's 500-pair cap (its paired stars lie within seed rank 1 349).
pub const COLOUR_LIST_STARS: usize = 2000;

impl StarPopulation {
    /// The probe / lab sizes (the stacking Measure runs `Mode::All` and does not use a population) for a run whose registration keeps `registration_max_stars` stars.
    pub fn probe_default(registration_max_stars: usize) -> StarPopulation {
        StarPopulation {
            statistic: sm::population::STATISTIC_STARS,
            list_mono: MONO_LIST_PER_REGISTRATION_STAR * registration_max_stars,
            list_colour: COLOUR_LIST_STARS,
        }
    }
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
            population: None,
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
    /// The β every star of this plane was fitted at: under the stacking Measure stage's `Auto` the group β (Plan A
    /// Task 11b, owner decision R11b-12 — `psf_signal::group_beta` of the members' [`Self::beta_own`], the value
    /// `stacking_run_groups.beta` records and LN fits at), a group of one's own β; under `Moffat4` 4.0; elsewhere the
    /// plane's own `Auto` β.
    pub beta: f64,
    /// The plane's own β (F-4's E5 estimator, unchanged; Task 11b): a diagnostic, never consumed — reported beside
    /// `beta`. `Some` only from the two-pass group measure ([`measure_planes_pass1`]); `None` on every single-pass
    /// path (`Moffat4`, `Fixed`, the master statistics) and in a payload written before Task 11b — and then never
    /// serialized, so those payloads stay byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beta_own: Option<f64>,
    /// The plane's own β sits at a bound of [`crate::star_measure::BETA_BOUNDS`] (`star_measure::beta::at_bound`, the
    /// test that sets `StarFlags::BETA_AT_CAP` on a fit at a frame β): the own β is then a clamp, not an estimate.
    /// `false` wherever `beta_own` is `None` and in a payload written before this field; then never serialized, so
    /// those payloads stay byte-identical.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub beta_own_at_cap: bool,
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
    /// The plane's sub-step wall and thread-CPU timers (Plan A Task 13b, 13-P), reported in `"frame plane substeps"`
    /// only. Never compared, never persisted.
    pub sub_steps: sm::SubSteps,
    /// The field width the module's fit started from (Plan A Task 6b): the matched
    /// filter's own for [`SeedOverride::MatchedFilter`], else `field_width_of` the
    /// seeds. Reported, not persisted.
    pub field_width: sm::FieldWidth,
    /// The plane is defocused (owner ruling 2026-10-08): `fits` is empty and the zero-star path below takes it.
    /// Reported, not persisted.
    pub defocused: bool,
    /// The readable seeds refused `Donut` (the verdict's `n_donut`). Reported, not persisted.
    pub defocus_donuts: usize,
    /// The plane is trailed (Plan A Task 10b, F-10; rulings A6, Q1, Q5: more than half of at least 100 judged seeds
    /// refused `centreMoved` / `regionTooSmall` / `centreAtRim`): `fits` is empty and the zero-star path below takes it, as a defocused
    /// plane's. Reported, not persisted.
    pub trailed: bool,
    /// The verdict's input, the share of the judged seeds refused `centreMoved`, `regionTooSmall` or `centreAtRim` (A4-2; the probe's
    /// `trailShare`). Reported, not persisted.
    pub trail_share: f64,
    /// The seeds whose saturation verdict fit failed for a reason other than n ≤ p (F-3, Plan A Task 8, Q4; the
    /// probe's `verdictFitFailed`). Reported, not persisted.
    pub verdict_fit_failed: usize,
    /// The seeds flagged by the cross-plane rule (another plane's own flag at their position; Task 8 fix wave, owner
    /// ruling 2026-10-09; the probe's `crossFlags`); 0 on a single plane. Reported, not persisted.
    pub cross_flags: usize,
    /// Where `channel.beta` came from (F-4, Plan A Task 9; the probe's `betaSource`). Reported, not persisted.
    pub beta_source: sm::BetaSource,
    /// The plane's aperture correction every fit's `signal` carries (F-5, Plan A Task 10; the probe's
    /// `apertureCorrection`); `None` = `c = 1`. Reported, not persisted.
    pub aperture_correction: Option<f64>,
    /// The qualifying stars `aperture_correction` is the median over (the probe's `apertureStars`). Reported, not
    /// persisted.
    pub aperture_stars: usize,
    /// The standard error of `aperture_correction` as the median of its ratios (Plan A Task 13b; the probe's
    /// `apertureSe`). Reported, not persisted.
    pub aperture_se: Option<f64>,
    /// What the two populations did on the plane (Plan A Task 13b; the probe's selection counts). `None` = every seed
    /// was fitted. Reported, not persisted.
    pub population: Option<sm::PopulationReport>,
    /// The accepted fits without a ring sky B_i (Plan A Task 10b, F-10; the probe's `ringSkyMissing`). Reported, not
    /// persisted.
    pub ring_sky_missing: usize,
    /// The plane's near-clip floor in range units (Plan A Task 10c, R10c-10; the probe's `clipFloor`); `None` = no
    /// clipped evidence. Reported, not persisted.
    pub clip_floor: Option<f64>,
    /// How many pieces of evidence set `clip_floor` (the probe's `clipEvidence`). Reported, not persisted.
    pub clip_evidence: usize,
    /// The module's matched-filter report (Plan A Task 10b, F-10: σ_f propagated and measured, the noise gain, the
    /// structure seeds dropped, the extended flags; the probe's `sigmaF`, `sigmaFMeasured`, `noiseGain`,
    /// `structureDropped`, `extendedSeeds`); `None` for the other detectors. Reported, not persisted.
    pub detection: Option<sm::DetectionReport>,
    /// The gain the stars' SNR used, e- per ADU of the working-unit plane (Plan A Task 11, ruling R11-6: the header's
    /// `EGAIN x ATH_CSCL` over 65535; the probe's `snrGain`); `None` = no gain, the shot term was omitted. Reported,
    /// not persisted (not in [`ChannelMeasurement`], so no cached `metrics` payload changes).
    pub snr_gain: Option<f64>,
    /// The accepted fits without an SNR (no ring sky; the probe's `snrMissing`). Reported, not persisted.
    pub snr_missing: usize,
    /// The plane's median aperture SNR over the finite values of the fits the frame statistic counts (ruling R11-5; the
    /// probe's `snrMedian`); `None` = none. Reported, not persisted.
    pub snr_median: Option<f64>,
    /// The plane's median half-flux radius, px, over the same population (the probe's `hfrMedian`). Reported, not
    /// persisted.
    pub hfr_median: Option<f64>,
    /// How the SNR's noise correlation was obtained (the probe's `snrNoise`). Reported, not persisted.
    pub snr_noise: sm::SnrNoise,
    /// The `s` the shot term would use (the plane's CFA site share spreading it; the probe's `shotSpread`), reported
    /// even when the gain is absent and the shot term omitted. Not persisted.
    pub shot_spread: f64,
}

/// One plane's result from [`measure_planes_diagnosed`].
#[derive(Debug, Clone)]
pub struct MeasuredPlane {
    pub measurement: PlaneMeasurement,
    /// The seeds the fits were made from, as [`measure_plane_detailed`] returns them.
    pub seeds: Vec<Seed>,
    /// One record per seed, in seed order, when `diagnostics` is set; empty otherwise.
    pub diagnostics: Vec<sm::FitDiagnostics>,
    /// The plane's own wall time in the call: both of its halves, its `plane(p)` calls included. Reported.
    pub wall_ms: u64,
    /// The verdict pass's wall in ms, where the plane went through [`sm::prepare_plane`] (the group measure and the
    /// multi-plane path); `None` on the mono single-call path. Reported in `"frame plane measured"` only (13-P).
    pub verdict_ms: Option<u64>,
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
    let (pm, seeds, _) = measure_plane_diagnosed(
        data,
        w,
        h,
        opts,
        pool,
        seed_source,
        false,
        false,
        sm::PixelNoise::Unknown,
        None,
        None,
    );
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
/// (`star_measure::MeasureOptions::colour`). One plane on its own: no
/// cross-plane rule; every image of more than one plane goes through
/// [`measure_planes_diagnosed`], which sets `colour` itself.
/// `pixel_noise` (Task 10b A1c, ruling C-10): the caller's statement of the plane's noise correlation
/// ([`sm::PixelNoise`]); the convenience entries above say `Unknown` (the gain is read).
/// `gain_e_per_unit` (Task 11, ruling R11-6): e- per calibrated unit (`EGAIN x ATH_CSCL`, [`gain_from_header`]), the
/// SNR's shot term; `None` omits it. `cfa_site_share` (ruling R11-1): the plane's share of its own colour's CFA sites
/// (0.25 / 0.5 / 0.25 on a debayered RGGB plane), `None` = 1. Neither moves anything but the new SNR / HFR values.
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
    pixel_noise: sm::PixelNoise,
    gain_e_per_unit: Option<f64>,
    cfa_site_share: Option<f64>,
) -> (PlaneMeasurement, Vec<Seed>, Vec<sm::FitDiagnostics>) {
    let head = adapter_head(
        data,
        w,
        h,
        opts,
        pool,
        seed_source,
        diagnostics,
        colour,
        pixel_noise,
        gain_e_per_unit,
        cfa_site_share,
    );
    let m = sm::measure_plane(data, w, h, &head.module_opts, pool);
    adapter_tail(head, m)
}

/// The one multi-plane entry (F-3; Task 8 fix wave, owner ruling 2026-10-09): every plane of one image measured as
/// [`measure_plane_diagnosed`] measures one, with the cross-plane rule between the halves — every plane's verdict
/// pass ([`sm::prepare_plane`]), then [`sm::cross_plane_flags`] (a star flagged on any plane is flagged on every plane
/// at that position), then every plane's β sample, fits at β and frame statistic ([`sm::finish_plane`]). The frame
/// adapter ([`measure_frame`] and its siblings), the master statistics (`integrate`, `drizzle`, their covered-rectangle
/// crops included) and the lab's probe call it; one plane is exactly [`measure_plane_diagnosed`] without `colour`.
/// `colour` (Q3) is `n_planes == 3`: a 3-plane calibrated frame or master is a debayered OSC group's.
///
/// `pixel_noise` is the caller's statement for every plane of the image (ruling C-10): `White` for a calibrated sensor
/// frame (the Measure stage's mono plane), `Mixed` for a master (integrated, drizzled) or any debayered / resampled
/// image, `Unknown` otherwise; a debayered 3-plane image is `Mixed` whatever is stated.
///
/// `plane(p)` hands plane `p`, in its native units. On more than one plane it is called twice per plane, once per
/// half, so a caller reading from disk holds one plane at a time (`stacking::run`'s `MEASURE_PLANES_RESIDENT` stays
/// one plane's working set); the second half runs the planes last to first, so the last plane's data passes straight
/// from its first half to its second. An `Err` from `plane` ends the call with it (the frame adapter's cancel).
#[allow(clippy::too_many_arguments)]
pub fn measure_planes_diagnosed<'a, E>(
    n_planes: usize,
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedOverride,
    diagnostics: bool,
    pixel_noise: sm::PixelNoise,
    gain_e_per_unit: Option<f64>,
    mut plane: impl FnMut(usize) -> Result<Cow<'a, [f32]>, E>,
) -> Result<Vec<MeasuredPlane>, E> {
    if n_planes == 1 {
        let t = Instant::now();
        let data = plane(0)?;
        let _span = tracing::debug_span!("measure_plane", plane = 0).entered();
        let (measurement, seeds, diagnostics) = measure_plane_diagnosed(
            &data,
            w,
            h,
            opts,
            pool,
            seed_source,
            diagnostics,
            false,
            pixel_noise,
            gain_e_per_unit,
            None,
        );
        return Ok(vec![MeasuredPlane {
            measurement,
            seeds,
            diagnostics,
            wall_ms: t.elapsed().as_millis() as u64,
            verdict_ms: None,
        }]);
    }
    let colour = n_planes == 3;
    let mut walls = vec![Duration::ZERO; n_planes];
    let mut heads = Vec::with_capacity(n_planes);
    let mut preps = Vec::with_capacity(n_planes);
    let mut last: Option<Cow<'a, [f32]>> = None;
    for (p, wall) in walls.iter_mut().enumerate() {
        let t = Instant::now();
        let data = plane(p)?;
        let _span = tracing::debug_span!("measure_plane", plane = p).entered();
        let head = adapter_head(
            &data,
            w,
            h,
            opts,
            pool,
            seed_source,
            diagnostics,
            colour,
            pixel_noise,
            gain_e_per_unit,
            // A debayered RGGB colour image's planes 0 / 1 / 2 are R / G / B: the share of their own colour's sites.
            (colour && n_planes == 3).then(|| [0.25, 0.5, 0.25][p]),
        );
        preps.push(sm::prepare_plane(&data, w, h, &head.module_opts, pool));
        heads.push(head);
        if p + 1 == n_planes {
            last = Some(data);
        }
        *wall += t.elapsed();
    }
    sm::cross_plane_flags(&mut preps);
    let mut out: Vec<Option<MeasuredPlane>> = (0..n_planes).map(|_| None).collect();
    for p in (0..n_planes).rev() {
        let t = Instant::now();
        let data = match last.take() {
            Some(data) => data,
            None => plane(p)?,
        };
        let _span = tracing::debug_span!("measure_plane", plane = p).entered();
        let (head, prep) = match (heads.pop(), preps.pop()) {
            (Some(head), Some(prep)) => (head, prep),
            _ => unreachable!("one head and one prep per plane, taken last to first"),
        };
        let verdict_ms = prep.verdict_time().as_millis() as u64;
        let m = sm::finish_plane(&data, w, h, &head.module_opts, prep, pool);
        drop(data);
        let (measurement, seeds, diagnostics) = adapter_tail(head, m);
        walls[p] += t.elapsed();
        out[p] = Some(MeasuredPlane {
            measurement,
            seeds,
            diagnostics,
            wall_ms: walls[p].as_millis() as u64,
            verdict_ms: Some(verdict_ms),
        });
    }
    Ok(out.into_iter().flatten().collect())
}

/// One image's first pass of the stacking Measure stage's group measure (Plan A Task 11b, owner decision R11b-12):
/// every plane's adapter head (its detector and seeds) and verdict pass ([`sm::prepare_plane`], with
/// [`sm::cross_plane_flags`] between the planes of a multi-plane image), then every plane's own β
/// ([`sm::plane_beta`]: F-4's E5 estimator, unchanged). Holds no plane-sized buffer — the seeds and their per-seed
/// records, a few hundred bytes a seed — so a whole group's first passes can wait for the group β, which
/// [`measure_planes_pass2`] then fits every plane at.
pub struct PlanesPass1 {
    w: usize,
    h: usize,
    heads: Vec<AdapterHead>,
    preps: Vec<sm::PlanePrep>,
    beta_own: Vec<f64>,
    /// The wall of each plane's [`sm::plane_beta`], in ms. In memory only (13-P): a stored first pass read back holds 0.
    beta_ms: Vec<u64>,
    walls: Vec<Duration>,
}

impl PlanesPass1 {
    /// Every plane's own β, in plane order.
    pub fn beta_own(&self) -> &[f64] {
        &self.beta_own
    }

    /// The image's plane count.
    pub fn planes(&self) -> usize {
        self.beta_own.len()
    }
}

/// [`measure_planes_diagnosed`]'s first half for the group measure (Task 11b): the same heads and verdict passes with
/// the same arguments, then each plane's own β. `plane(p)` as there: one plane is read once (its β reads the copy
/// its verdict pass read), a plane of a multi-plane image twice (after every plane's verdict pass, the cross-plane
/// rule, last to first so the last plane passes straight through).
#[allow(clippy::too_many_arguments)]
pub fn measure_planes_pass1<'a, E>(
    n_planes: usize,
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedOverride,
    diagnostics: bool,
    pixel_noise: sm::PixelNoise,
    gain_e_per_unit: Option<f64>,
    mut plane: impl FnMut(usize) -> Result<Cow<'a, [f32]>, E>,
) -> Result<PlanesPass1, E> {
    // The arguments `measure_planes_diagnosed` hands its heads: one plane is mono (no colour, no site share).
    let colour = n_planes == 3;
    let mut walls = vec![Duration::ZERO; n_planes];
    let mut heads = Vec::with_capacity(n_planes);
    let mut preps = Vec::with_capacity(n_planes);
    let mut beta_own = vec![f64::NAN; n_planes];
    let mut beta_ms = vec![0u64; n_planes];
    let mut last: Option<Cow<'a, [f32]>> = None;
    for (p, wall) in walls.iter_mut().enumerate() {
        let t = Instant::now();
        let data = plane(p)?;
        let _span = tracing::debug_span!("measure_plane", plane = p).entered();
        let head = adapter_head(
            &data,
            w,
            h,
            opts,
            pool,
            seed_source,
            diagnostics,
            colour,
            pixel_noise,
            gain_e_per_unit,
            colour.then(|| [0.25, 0.5, 0.25][p]),
        );
        let prep = sm::prepare_plane(&data, w, h, &head.module_opts, pool);
        if n_planes == 1 {
            let tb = Instant::now();
            beta_own[p] = sm::plane_beta(&data, w, h, &head.module_opts, &prep, pool).0;
            beta_ms[p] = tb.elapsed().as_millis() as u64;
        } else if p + 1 == n_planes {
            last = Some(data);
        }
        heads.push(head);
        preps.push(prep);
        *wall += t.elapsed();
    }
    if n_planes > 1 {
        sm::cross_plane_flags(&mut preps);
        for p in (0..n_planes).rev() {
            let t = Instant::now();
            let data = match last.take() {
                Some(data) => data,
                None => plane(p)?,
            };
            let _span = tracing::debug_span!("measure_plane", plane = p).entered();
            let tb = Instant::now();
            beta_own[p] = sm::plane_beta(&data, w, h, &heads[p].module_opts, &preps[p], pool).0;
            beta_ms[p] = tb.elapsed().as_millis() as u64;
            walls[p] += t.elapsed();
        }
    }
    Ok(PlanesPass1 {
        w,
        h,
        heads,
        preps,
        beta_own,
        beta_ms,
        walls,
    })
}

/// The group measure's second pass (Task 11b): every plane of `pass1`'s image fitted at `betas[p]`
/// (`PsfModel::Fixed`, fit exactly like `Moffat4`'s fixed 4.0) from its own verdict pass — the group β on every plane
/// of a group's member, a group of one's own β per plane — through [`sm::finish_plane`] and the adapter's tail, as
/// [`measure_planes_diagnosed`]'s second half. Every fit, F, `c`, F-6 statistic and `ChannelMeasurement` comes from
/// here; `channel.beta` is `betas[p]` and `channel.beta_own` the plane's own β. `plane(p)` must hand the same bytes
/// the first pass read (the frame adapter's read guard spans both passes). `betas` must hold one β per plane
/// (`debug_assert!`; [`measure_frame_pass2`], the only production caller, refuses a mismatch with an error).
pub fn measure_planes_pass2<'a, E>(
    pass1: PlanesPass1,
    betas: &[f64],
    pool: Option<&Arc<rayon::ThreadPool>>,
    mut plane: impl FnMut(usize) -> Result<Cow<'a, [f32]>, E>,
) -> Result<Vec<MeasuredPlane>, E> {
    let PlanesPass1 {
        w,
        h,
        heads,
        preps,
        beta_own,
        beta_ms: _,
        walls,
    } = pass1;
    debug_assert_eq!(
        betas.len(),
        heads.len(),
        "one β per plane (checked by measure_frame_pass2)"
    );
    let mut out = Vec::with_capacity(heads.len());
    for (p, (mut head, prep)) in heads.into_iter().zip(preps).enumerate() {
        let t = Instant::now();
        let data = plane(p)?;
        let _span = tracing::debug_span!("measure_plane", plane = p).entered();
        head.module_opts.psf_model = PsfModel::Fixed(betas[p]);
        let verdict_ms = prep.verdict_time().as_millis() as u64;
        let m = sm::finish_plane(&data, w, h, &head.module_opts, prep, pool);
        drop(data);
        let (mut measurement, seeds, diagnostics) = adapter_tail(head, m);
        measurement.channel.beta_own = Some(beta_own[p]);
        measurement.channel.beta_own_at_cap = sm::beta::at_bound(beta_own[p]);
        out.push(MeasuredPlane {
            measurement,
            seeds,
            diagnostics,
            wall_ms: (walls[p] + t.elapsed()).as_millis() as u64,
            verdict_ms: Some(verdict_ms),
        });
    }
    Ok(out)
}

/// The adapter's seed detector, resolved from the options and the probe's override.
#[derive(Clone, Copy, PartialEq)]
enum Detector {
    Peak,
    Full,
    Structure,
    Matched(f64),
}

/// The detector the options and the probe's override select. `Fast` means "whatever the options say", so the two never
/// contradict each other silently. The one rule [`adapter_head`] and a stored first pass's rebuild
/// ([`FramePass1::from_record`]) share.
fn resolve_detector(seed_source: SeedOverride, opts: &MeasureOptions) -> Detector {
    match seed_source {
        SeedOverride::Fast => match opts.seed_source {
            SeedSource::MatchedFilter => Detector::Matched(opts.seed_threshold),
            SeedSource::Peak => Detector::Peak,
            SeedSource::Structure => Detector::Structure,
        },
        SeedOverride::Full => Detector::Full,
        SeedOverride::Structure => Detector::Structure,
        SeedOverride::MatchedFilter { threshold } => Detector::Matched(threshold),
    }
}

/// The module's options for one plane (Plan A Task 11c, R11c-1): the ONE construction [`adapter_head`] and a stored
/// first pass's rebuild share. `seeds` are the ones the adapter's detector handed in (ignored for the matched filter,
/// which detects inside the module); `noise` is the Median3 arm's precomputed model. The stored first pass rebuilds
/// with `noise = None`: [`sm::finish_plane`] reads only `domain`, `psf_model`, `fit`, `mask`, `diagnostics` and
/// `cfa_site_share` of these options, and the model it needs is in the stored `PlanePrep`.
#[allow(clippy::too_many_arguments)]
fn module_options(
    opts: &MeasureOptions,
    detector: Detector,
    seeds: Vec<Seed>,
    noise: Option<sm::NoiseModel>,
    diagnostics: bool,
    colour: bool,
    pixel_noise: sm::PixelNoise,
    gain_e_per_unit: Option<f64>,
    cfa_site_share: Option<f64>,
) -> sm::MeasureOptions {
    let mode = match opts.population {
        Some(p) => sm::Mode::Population {
            statistic: p.statistic,
            list: if colour { p.list_colour } else { p.list_mono },
        },
        None => sm::Mode::All,
    };
    sm::MeasureOptions {
        domain: sm::Domain::Calibrated { gain_e_per_unit },
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
        pixel_noise,
        cfa_site_share,
        mode,
    }
}

/// What the adapter decides and measures before the module (its detector, the seeds it hands in, the Median3 arm's
/// noise model), kept until the module's result is mapped onto a [`PlaneMeasurement`].
struct AdapterHead {
    detector: Detector,
    module_opts: sm::MeasureOptions,
    adapter_detect_ms: u64,
    own_phases: Option<sm::PhaseTimings>,
}

/// The adapter's head: the detector, the seeds it hands the module (the matched filter runs inside the module) and
/// the module's options.
#[allow(clippy::too_many_arguments)]
fn adapter_head(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedOverride,
    diagnostics: bool,
    colour: bool,
    pixel_noise: sm::PixelNoise,
    gain_e_per_unit: Option<f64>,
    cfa_site_share: Option<f64>,
) -> AdapterHead {
    // A run selects its detector through the config's `seed_source`; `seed_override` is the probe-level override
    // the dev harnesses pass. `Fast` means "whatever the options say", so the two never contradict each other
    // silently.
    let detector = resolve_detector(seed_source, opts);

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
    let module_opts = module_options(
        opts,
        detector,
        seeds,
        noise,
        diagnostics,
        colour,
        pixel_noise,
        gain_e_per_unit,
        cfa_site_share,
    );
    AdapterHead {
        detector,
        module_opts,
        adapter_detect_ms,
        own_phases,
    }
}

/// The adapter's tail: the module's measurement mapped onto a [`PlaneMeasurement`], with the seeds it fitted and its
/// per-seed records.
fn adapter_tail(
    head: AdapterHead,
    m: sm::PlaneMeasurement,
) -> (PlaneMeasurement, Vec<Seed>, Vec<sm::FitDiagnostics>) {
    let AdapterHead {
        detector,
        module_opts,
        adapter_detect_ms,
        own_phases,
    } = head;
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
        // The two-pass group measure sets it (`measure_planes_pass2`); a single pass reads no own β of its own.
        beta_own: None,
        beta_own_at_cap: false,
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
            sub_steps: m.sub_steps,
            field_width: m.field_width,
            defocused: m.defocused,
            defocus_donuts: m.defocus_donuts,
            trailed: m.trailed,
            trail_share: m.trail_share,
            verdict_fit_failed: m.verdict_fit_failed,
            cross_flags: m.cross_flags,
            beta_source: m.beta_source,
            aperture_correction: m.aperture_correction,
            aperture_stars: m.aperture_stars,
            aperture_se: m.aperture_se,
            population: m.population,
            ring_sky_missing: m.ring_sky_missing,
            clip_floor: m.clip_floor,
            clip_evidence: m.clip_evidence,
            detection: m.detection,
            snr_gain: m.snr_gain,
            snr_missing: m.snr_missing,
            snr_median: m.frame.snr_median,
            hfr_median: m.frame.hfr_median,
            snr_noise: m.snr_noise,
            shot_spread: m.shot_spread,
        },
        seeds,
        m.diagnostics,
    )
}

/// The calibrated frame's gain in e- per CALIBRATED unit, read from its header (Plan A Task 11, ruling R11-6, PA-22):
/// `EGAIN` (e- per ADU of the raw sensor) times `ATH_CSCL` (the calibrated light's unit divisor, ADU per unit). `ATH_CFNM`
/// is the flat's normalisation and plays no part. No camera table: a frame without either card, or one that is not a
/// positive number, has no gain (`None`: the SNR omits its shot term and the frame line says so); an unreadable header
/// is logged, never silent.
pub fn gain_from_header(path: &Path) -> Option<f64> {
    let is_xisf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xisf"));
    let (egain, cscl) = if is_xisf {
        match crate::fits_parser::extract_xisf_header(path) {
            Ok(text) => (
                header_text_f64(&text, "EGAIN"),
                header_text_f64(&text, "ATH_CSCL"),
            ),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "snr: the XISF header could not be read; no gain");
                return None;
            }
        }
    } else {
        match crate::fits_parser::FitsHeader::from_path(path) {
            Ok(h) => (h.get_f64("EGAIN"), h.get_f64("ATH_CSCL")),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "snr: the FITS header could not be read; no gain");
                return None;
            }
        }
    };
    let gain = egain.zip(cscl).map(|(e, c)| e * c);
    if gain.is_none() {
        debug!(path = %path.display(), "snr: no EGAIN and ATH_CSCL in the header; the shot term is omitted");
    }
    gain.filter(|g| g.is_finite() && *g > 0.0)
}

/// The real value of `key` in the header text `extract_xisf_header` returns (`NAME = value` lines).
fn header_text_f64(text: &str, key: &str) -> Option<f64> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        v.split('/')
            .next()?
            .trim()
            .trim_matches('\'')
            .trim()
            .parse::<f64>()
            .ok()
    })
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

/// The file's identity as the plane reader sees it: size and modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl FileIdentity {
    fn of(path: &Path) -> std::io::Result<FileIdentity> {
        let meta = std::fs::metadata(path)?;
        Ok(FileIdentity {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// Guards the multi-plane reader (Task 8 close, review M-4): a plane is read once per half, so a file rewritten
/// between the two reads would mix two images in one measurement. The identity is taken at the first read; any later
/// read of a different identity is an error (logged here), never a mixed measurement.
struct ReadGuard<'a> {
    path: &'a Path,
    first: Option<FileIdentity>,
}

impl<'a> ReadGuard<'a> {
    fn new(path: &'a Path) -> Self {
        ReadGuard { path, first: None }
    }

    /// Call before every plane read.
    fn check(&mut self) -> Result<(), IntegrationError> {
        let now = FileIdentity::of(self.path).map_err(|e| {
            error!(path = %self.path.display(), error = %e, "measure: the frame cannot be stat-ed between plane reads");
            IntegrationError::Io(e)
        })?;
        match self.first {
            None => {
                self.first = Some(now);
                Ok(())
            }
            Some(first) if first == now => Ok(()),
            Some(first) => {
                error!(
                    path = %self.path.display(),
                    size_first = first.len,
                    size_now = now.len,
                    "measure: the frame changed between its two plane reads; the measurement is refused"
                );
                Err(IntegrationError::Io(std::io::Error::other(format!(
                    "{} changed between the two reads of one measurement (size {} -> {}, modified time {:?} -> {:?}); \
                     the measurement is refused rather than mixing two versions of the file",
                    self.path.display(),
                    first.len,
                    now.len,
                    first.modified,
                    now.modified
                ))))
            }
        }
    }
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
    let n = reader.channels();
    // Every plane through the one multi-plane entry (Task 8 fix wave): a 3-plane calibrated frame is a debayered OSC
    // group's (Q3, all 8 neighbours) and gets the cross-plane rule. `cancel` is checked before each read; a plane is
    // read once per half, so one plane is held at a time.
    let mut read_ms = vec![0u64; n];
    let mut guard = ReadGuard::new(path);
    let _span = tracing::debug_span!("measure_frame", path = %path.display()).entered();
    let planes = measure_planes_diagnosed(
        n,
        w,
        h,
        opts,
        pool,
        seed_source,
        false,
        frame_pixel_noise(n),
        gain_from_header(path),
        |plane| read_guarded(&reader, &mut guard, cancel, &mut read_ms, plane),
    )?;
    Ok(assemble_frame(
        path,
        opts,
        w,
        h,
        planes,
        &read_ms,
        start.elapsed(),
    ))
}

/// The Measure stage measures calibrated sensor frames (ruling C-10): one plane is white, a debayered 3-plane frame
/// is mixed.
fn frame_pixel_noise(n: usize) -> sm::PixelNoise {
    match n {
        1 => sm::PixelNoise::White,
        3 => sm::PixelNoise::Mixed,
        _ => sm::PixelNoise::Unknown,
    }
}

/// One plane of the frame for the measurement: `cancel` checked first, the read guard's identity check, the read
/// timed into `read_ms[plane]`.
fn read_guarded(
    reader: &PlaneReader,
    guard: &mut ReadGuard<'_>,
    cancel: &AtomicBool,
    read_ms: &mut [u64],
    plane: usize,
) -> Result<Cow<'static, [f32]>, IntegrationError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    guard.check()?;
    let t = Instant::now();
    let data = reader.read_plane(plane)?;
    read_ms[plane] += t.elapsed().as_millis() as u64;
    Ok(Cow::Owned(data))
}

/// Plan A Task 13b (13-P): one plane's sub-step profile as two debug lines, `"frame plane substeps"` (the plane-wide
/// passes) and `"frame plane substeps fit"` (the fit tail). Each step reports its wall `<step>_us` and the thread CPU
/// `<step>_cpu_us` it used, summed over the pool tasks that did the work; `sat_wait_us` is a wall alone. A plane whose
/// first pass was read back from a stored `.athm` reports the second pass's steps only.
fn log_substeps(path: &Path, plane: usize, s: &sm::SubSteps) {
    use sm::Step::*;
    let (w, c) = (|k| s.wall_us(k), |k| s.cpu_us(k));
    debug!(
        path = %path.display(),
        plane,
        work_copy_us = w(WorkCopy),
        work_copy_cpu_us = c(WorkCopy),
        sky_level_us = w(SkyLevel),
        sky_level_cpu_us = c(SkyLevel),
        sat_plane_us = w(SatPlane),
        sat_plane_cpu_us = c(SatPlane),
        sat_wait_us = w(SatWait),
        mesh_us = w(Mesh),
        mesh_cpu_us = c(Mesh),
        residual_us = w(Residual),
        residual_cpu_us = c(Residual),
        mrs_us = w(Mrs),
        mrs_cpu_us = c(Mrs),
        detect_prep_us = w(DetectPrep),
        detect_prep_cpu_us = c(DetectPrep),
        detect_pass1_us = w(DetectPass1),
        detect_pass1_cpu_us = c(DetectPass1),
        detect_widths_us = w(DetectWidths),
        detect_widths_cpu_us = c(DetectWidths),
        detect_pass2_us = w(DetectPass2),
        detect_pass2_cpu_us = c(DetectPass2),
        detect_knots_us = w(DetectKnots),
        detect_knots_cpu_us = c(DetectKnots),
        "frame plane substeps"
    );
    debug!(
        path = %path.display(),
        plane,
        signal_totals_us = w(SignalTotals),
        signal_totals_cpu_us = c(SignalTotals),
        rcr_us = w(Rcr),
        rcr_cpu_us = c(Rcr),
        ring_sky_us = w(RingSky),
        ring_sky_cpu_us = c(RingSky),
        aperture_c_us = w(ApertureC),
        aperture_c_cpu_us = c(ApertureC),
        snr_hfr_us = w(SnrHfr),
        snr_hfr_cpu_us = c(SnrHfr),
        "frame plane substeps fit"
    );
}

/// The measured planes as the frame's [`FrameMeasurement`] and its per-plane fits, with the frame adapter's per-plane
/// log lines (the defocused and trailed verdicts, `"frame plane measured"`).
fn assemble_frame(
    path: &Path,
    opts: &MeasureOptions,
    w: usize,
    h: usize,
    planes: Vec<MeasuredPlane>,
    read_ms: &[u64],
    elapsed: Duration,
) -> (FrameMeasurement, Vec<Vec<StarFit>>) {
    let mut channels = Vec::with_capacity(planes.len());
    let mut fits_by_plane: Vec<Vec<StarFit>> = Vec::with_capacity(planes.len());
    for (plane, measured) in planes.into_iter().enumerate() {
        let pm = measured.measurement;
        let mut m = pm.channel;
        m.timings.read_ms = read_ms[plane];
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
        // Plan A Task 10b (F-10): the trailed verdict, logged here for the same reason.
        if pm.trailed {
            warn!(
                outcome = "trailed",
                trail_share = pm.trail_share,
                stars_detected = m.stars_detected,
                path = %path.display(),
                plane,
                "measure: the plane is trailed; no star is measured"
            );
        }
        debug!(
            path = %path.display(),
            plane,
            stars_detected = m.stars_detected,
            stars_fitted = m.stars_fitted,
            beta = m.beta,
            beta_own = m.beta_own,
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
            verdict_ms = measured.verdict_ms,
            duration_ms = measured.wall_ms,
            "frame plane measured"
        );
        log_substeps(path, plane, &pm.sub_steps);
        channels.push(m);
        fits_by_plane.push(pm.fits);
    }
    (
        FrameMeasurement {
            width: w,
            height: h,
            channels,
            duration_ms: elapsed.as_millis() as u64,
        },
        fits_by_plane,
    )
}

/// A calibrated frame's first pass of the Measure stage's group measure (Plan A Task 11b, owner decision R11b-12):
/// [`measure_planes_pass1`] over the frame's planes as [`measure_frame_with_fits`] reads them, plus the file's
/// identity at that read, so the second pass refuses a file rewritten in between rather than fitting one file's
/// records on another's pixels.
pub struct FramePass1 {
    planes: PlanesPass1,
    identity: Option<FileIdentity>,
    read_ms: Vec<u64>,
    elapsed: Duration,
    /// The frame's header gain, the one every plane's options were built with (stored: the rebuild needs it).
    gain: Option<f64>,
    /// Test-only: counts this value among the live first passes of its calibrated directory ([`pass1_peak`]).
    #[cfg(test)]
    _live: Pass1Live,
}

/// Test-only: one entry per [`measure_frame_pass1`] call — the one entry of all first-pass work (Task 11c, bar 2). A
/// test reads the entries under its own temp tree, as `FITS_WRITE_FAILURE_LOG` is read.
#[cfg(test)]
pub(crate) static PASS1_LOG: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

/// Test-only: `(directory, remaining)` — once `remaining` first passes of frames under the directory have finished,
/// the run's cancel flag is set (a deterministic mid-phase-1 cancel; Task 11c review I2). The artifact is written by
/// the caller after `measure_frame_pass1` returns, so the cancel lands between that write and the next frame.
#[cfg(test)]
pub(crate) static PASS1_CANCEL_AFTER: std::sync::Mutex<Vec<(std::path::PathBuf, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// Test-only: per calibrated directory, `(live, peak)` count of [`FramePass1`] values (Task 11c, bar 3).
#[cfg(test)]
static PASS1_LIVE: std::sync::Mutex<Vec<(std::path::PathBuf, usize, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// Test-only: a live [`FramePass1`]'s registration in [`PASS1_LIVE`], undone on drop.
#[cfg(test)]
struct Pass1Live(std::path::PathBuf);

#[cfg(test)]
impl Pass1Live {
    fn new(frame: &Path) -> Pass1Live {
        let dir = frame.parent().unwrap_or(frame).to_path_buf();
        let mut all = PASS1_LIVE.lock().unwrap();
        match all.iter_mut().find(|(d, _, _)| *d == dir) {
            Some((_, live, peak)) => {
                *live += 1;
                *peak = (*peak).max(*live);
            }
            None => all.push((dir.clone(), 1, 1)),
        }
        Pass1Live(dir)
    }
}

#[cfg(test)]
impl Drop for Pass1Live {
    fn drop(&mut self) {
        let mut all = PASS1_LIVE.lock().unwrap();
        if let Some((_, live, _)) = all.iter_mut().find(|(d, _, _)| *d == self.0) {
            *live -= 1;
        }
    }
}

/// Test-only: the most [`FramePass1`] values of `dir` (a calibrated directory) ever alive at once.
#[cfg(test)]
pub(crate) fn pass1_peak(dir: &Path) -> usize {
    PASS1_LIVE
        .lock()
        .unwrap()
        .iter()
        .find(|(d, _, _)| d == dir)
        .map_or(0, |(_, _, peak)| *peak)
}

impl FramePass1 {
    /// Every plane's own β, in plane order.
    pub fn beta_own(&self) -> &[f64] {
        self.planes.beta_own()
    }

    /// The calibrated file's identity at this first pass's first plane read (Plan A Task 11c, R11c-4).
    pub(crate) fn source(&self) -> Option<SourceStamp> {
        self.identity.map(stamp_of)
    }

    /// What a controller reads without the body (Task 11c, R11c-2).
    pub(crate) fn head(&self) -> Pass1Head {
        Pass1Head {
            source: self.source(),
            width: self.planes.w as u32,
            height: self.planes.h as u32,
            beta_own: self.planes.beta_own.clone(),
        }
    }

    /// The records a stored first pass keeps (Task 11c, R11c-1), borrowed: nothing is cloned to be written.
    pub(crate) fn body(&self) -> Pass1BodyRef<'_> {
        Pass1BodyRef {
            gain: self.gain,
            read_ms: &self.read_ms,
            elapsed: self.elapsed,
            planes: self
                .planes
                .heads
                .iter()
                .zip(&self.planes.preps)
                .zip(&self.planes.walls)
                .map(|((head, prep), &wall)| PlaneRecordRef {
                    prep,
                    adapter_detect_ms: head.adapter_detect_ms,
                    own_phases: head.own_phases,
                    wall,
                })
                .collect(),
        }
    }

    /// A stored first pass read back (Task 11c, R11c-1 / R11c-4): the heads rebuilt from `opts`, the plane index, the
    /// plane count and the stored gain through the ONE [`module_options`] builder, the read guard seeded with the
    /// identity of `calibrated` NOW, which must equal the stored stamp ([`Pass1ArtifactError::Stale`] otherwise).
    pub(crate) fn from_record(
        head: Pass1Head,
        body: Pass1Body,
        calibrated: &Path,
        opts: &MeasureOptions,
    ) -> Result<FramePass1, Pass1ArtifactError> {
        let n = head.beta_own.len();
        if n == 0 || body.planes.len() != n || body.read_ms.len() != n {
            return Err(Pass1ArtifactError::Truncated(format!(
                "{} planes in the head, {} records and {} read times in the body",
                n,
                body.planes.len(),
                body.read_ms.len()
            )));
        }
        let now = FileIdentity::of(calibrated)?;
        if head.source != Some(stamp_of(now)) {
            return Err(Pass1ArtifactError::Stale {
                stored: head.source,
                now: Some(stamp_of(now)),
            });
        }
        let detector = resolve_detector(SeedOverride::Fast, opts);
        let colour = n == 3;
        let mut heads = Vec::with_capacity(n);
        let mut preps = Vec::with_capacity(n);
        let mut walls = Vec::with_capacity(n);
        for (p, record) in body.planes.into_iter().enumerate() {
            let seeds = match detector {
                Detector::Matched(_) => Vec::new(),
                _ => record.prep.seeds().to_vec(),
            };
            heads.push(AdapterHead {
                detector,
                module_opts: module_options(
                    opts,
                    detector,
                    seeds,
                    None,
                    false,
                    colour,
                    frame_pixel_noise(n),
                    body.gain,
                    colour.then(|| [0.25, 0.5, 0.25][p]),
                ),
                adapter_detect_ms: record.adapter_detect_ms,
                own_phases: record.own_phases,
            });
            preps.push(record.prep);
            walls.push(record.wall);
        }
        Ok(FramePass1 {
            planes: PlanesPass1 {
                w: head.width as usize,
                h: head.height as usize,
                heads,
                preps,
                beta_own: head.beta_own,
                beta_ms: vec![0; n],
                walls,
            },
            identity: Some(now),
            read_ms: body.read_ms,
            elapsed: body.elapsed,
            gain: body.gain,
            #[cfg(test)]
            _live: Pass1Live::new(calibrated),
        })
    }
}

/// One plane's stored first-pass record, borrowed for writing (Task 11c, R11c-1): the `PlanePrep` verbatim, the head's
/// detection time and own phase times, the plane's pass-1 wall. The detector and the module options are not here:
/// they are rebuilt from the options, the plane index and count and the stored gain.
#[derive(Serialize)]
pub(crate) struct PlaneRecordRef<'a> {
    pub(crate) prep: &'a sm::PlanePrep,
    pub(crate) adapter_detect_ms: u64,
    pub(crate) own_phases: Option<sm::PhaseTimings>,
    pub(crate) wall: Duration,
}

/// A frame's stored first-pass body, borrowed for writing (the postcard bytes are those of [`Pass1Body`]).
#[derive(Serialize)]
pub(crate) struct Pass1BodyRef<'a> {
    pub(crate) gain: Option<f64>,
    pub(crate) read_ms: &'a [u64],
    pub(crate) elapsed: Duration,
    pub(crate) planes: Vec<PlaneRecordRef<'a>>,
}

/// [`PlaneRecordRef`], read back.
#[derive(Deserialize)]
pub(crate) struct PlaneRecord {
    pub(crate) prep: sm::PlanePrep,
    pub(crate) adapter_detect_ms: u64,
    pub(crate) own_phases: Option<sm::PhaseTimings>,
    pub(crate) wall: Duration,
}

/// [`Pass1BodyRef`], read back.
#[derive(Deserialize)]
pub(crate) struct Pass1Body {
    pub(crate) gain: Option<f64>,
    pub(crate) read_ms: Vec<u64>,
    pub(crate) elapsed: Duration,
    pub(crate) planes: Vec<PlaneRecord>,
}

impl Pass1Body {
    /// The borrowed view whose bytes are this value's.
    #[cfg(test)]
    pub(crate) fn view(&self) -> Pass1BodyRef<'_> {
        Pass1BodyRef {
            gain: self.gain,
            read_ms: &self.read_ms,
            elapsed: self.elapsed,
            planes: self
                .planes
                .iter()
                .map(|r| PlaneRecordRef {
                    prep: &r.prep,
                    adapter_detect_ms: r.adapter_detect_ms,
                    own_phases: r.own_phases,
                    wall: r.wall,
                })
                .collect(),
        }
    }
}

/// Whether a stored head describes `calibrated` as it is now (the controller's check in step (0), Plan A Task 11c,
/// R11c-5): the stamp equals the file's identity now ([`Pass1ArtifactError::Stale`] otherwise) and the geometry and
/// the plane count are the file's own (a header probe, no pixel read).
pub(crate) fn check_pass1_head(
    head: &Pass1Head,
    calibrated: &Path,
) -> Result<(), Pass1ArtifactError> {
    let now = stamp_of(FileIdentity::of(calibrated)?);
    if head.source != Some(now) {
        return Err(Pass1ArtifactError::Stale {
            stored: head.source,
            now: Some(now),
        });
    }
    let reader = PlaneReader::open(calibrated).map_err(|e| {
        Pass1ArtifactError::Io(std::io::Error::other(format!("the calibrated file: {e}")))
    })?;
    let file = (reader.width(), reader.height(), reader.channels());
    let stored = (
        head.width as usize,
        head.height as usize,
        head.beta_own.len(),
    );
    if file != stored {
        return Err(Pass1ArtifactError::Mismatch(format!(
            "the head says {stored:?} (width, height, planes), the file is {file:?}"
        )));
    }
    Ok(())
}

/// A read guard identity as the stored stamp: the size and the modification time as signed nanoseconds since the Unix
/// epoch.
fn stamp_of(id: FileIdentity) -> SourceStamp {
    SourceStamp {
        len: id.len,
        modified_ns: id
            .modified
            .map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
                Ok(after) => after.as_nanos() as i128,
                Err(before) => -(before.duration().as_nanos() as i128),
            }),
    }
}

/// [`measure_frame_with_fits`]'s first half for the group measure (Task 11b): every plane's verdict pass and own β.
pub fn measure_frame_pass1(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FramePass1, IntegrationError> {
    #[cfg(test)]
    PASS1_LOG.lock().unwrap().push(path.to_path_buf());
    let start = Instant::now();
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    let n = reader.channels();
    let mut read_ms = vec![0u64; n];
    let mut guard = ReadGuard::new(path);
    let gain = gain_from_header(path);
    let _span = tracing::debug_span!("measure_frame_pass1", path = %path.display()).entered();
    let planes = measure_planes_pass1(
        n,
        w,
        h,
        opts,
        pool,
        SeedOverride::Fast,
        false,
        frame_pixel_noise(n),
        gain,
        |plane| read_guarded(&reader, &mut guard, cancel, &mut read_ms, plane),
    )?;
    for (plane, (prep, wall)) in planes.preps.iter().zip(&planes.walls).enumerate() {
        let (verdict_fits, verdict_iterations) = prep.verdict_iterations();
        debug!(
            path = %path.display(),
            plane,
            seeds = prep.seeds().len(),
            verdict_ms = prep.verdict_time().as_millis() as u64,
            verdict_fits,
            verdict_iterations,
            beta_ms = planes.beta_ms[plane],
            beta_own = planes.beta_own[plane],
            duration_ms = wall.as_millis() as u64,
            "frame plane first pass"
        );
    }
    #[cfg(test)]
    {
        let mut hooks = PASS1_CANCEL_AFTER.lock().unwrap();
        if let Some((_, remaining)) = hooks.iter_mut().find(|(dir, _)| path.starts_with(dir)) {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
    Ok(FramePass1 {
        planes,
        identity: guard.first,
        read_ms,
        elapsed: start.elapsed(),
        gain,
        #[cfg(test)]
        _live: Pass1Live::new(path),
    })
}

/// [`measure_frame_with_fits`]'s second half for the group measure (Task 11b): every plane of `pass1` fitted at
/// `betas[p]` ([`measure_planes_pass2`]), the same frame result and log lines; the read guard carries the first
/// pass's identity, so a file that changed since is refused (logged), never measured half-and-half. `opts` are the
/// first pass's. `duration_ms` is both passes' own time.
pub fn measure_frame_pass2(
    path: &Path,
    pass1: FramePass1,
    betas: &[f64],
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<(FrameMeasurement, Vec<Vec<StarFit>>), IntegrationError> {
    let start = Instant::now();
    if betas.len() != pass1.planes.planes() {
        error!(
            path = %path.display(),
            betas = betas.len(),
            planes = pass1.planes.planes(),
            "measure: one beta per plane required; the measurement is refused"
        );
        return Err(IntegrationError::Io(std::io::Error::other(format!(
            "{}: {} betas for {} planes (one per plane required)",
            path.display(),
            betas.len(),
            pass1.planes.planes()
        ))));
    }
    let FramePass1 {
        planes,
        identity,
        mut read_ms,
        elapsed,
        ..
    } = pass1;
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    if (w, h, reader.channels()) != (planes.w, planes.h, planes.planes()) {
        error!(
            path = %path.display(),
            "measure: the frame's geometry changed between the two passes; the measurement is refused"
        );
        return Err(IntegrationError::Io(std::io::Error::other(format!(
            "{} changed between the two passes of one measurement (now {}x{}x{}, was {}x{}x{})",
            path.display(),
            w,
            h,
            reader.channels(),
            planes.w,
            planes.h,
            planes.planes()
        ))));
    }
    let mut guard = ReadGuard {
        path,
        first: identity,
    };
    let _span = tracing::debug_span!("measure_frame_pass2", path = %path.display()).entered();
    let measured = measure_planes_pass2(planes, betas, pool, |plane| {
        read_guarded(&reader, &mut guard, cancel, &mut read_ms, plane)
    })?;
    Ok(assemble_frame(
        path,
        opts,
        w,
        h,
        measured,
        &read_ms,
        elapsed + start.elapsed(),
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
    pub(crate) fn field(seed: u64, amp_scale: f64, noise: f32) -> (Vec<f32>, usize, usize) {
        crate::test_support::fixture_field(seed, amp_scale, noise)
    }

    /// M-4: the reader seam is the guard (`measure_planes_diagnosed` takes any `plane(p)` closure, and
    /// `measure_frame_inner`'s is `guard.check()` then the read), so the test drives the guard on a real file: stable
    /// across reads is fine, a rewritten file (new size) is an error naming the path, and the error is final.
    #[test]
    fn the_read_guard_refuses_a_file_changed_between_reads() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("frame.bin");
        std::fs::write(&f, [0u8; 64]).unwrap();
        let mut g = ReadGuard::new(&f);
        g.check().unwrap();
        g.check().unwrap();
        std::fs::write(&f, [1u8; 80]).unwrap();
        let err = g.check().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("changed between the two reads") && msg.contains("frame.bin"),
            "{msg}"
        );
        assert!(msg.contains("64 -> 80"), "{msg}");
        // A vanished file is an error with the io cause, not a silent pass.
        std::fs::remove_file(&f).unwrap();
        assert!(matches!(g.check(), Err(IntegrationError::Io(_))));
    }

    pub(crate) fn write(dir: &Path, name: &str, planes: &[&[f32]], w: usize, h: usize) -> PathBuf {
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

    /// The cross-plane fixture (Plan A Task 8 fix wave): three planes of 24 Gaussian stars (σ 1.8 px, amplitude 0.2) on
    /// a 6×4 grid of a 320×256 plane, background 0.08, noise 0.002, and one Moffat star (α 3, β 4) at (160.3, 215.7),
    /// 50 px below the grid: clipped at 0.95 on G (plane 1; a full plateau, flagged by its own verdict) and unclipped at
    /// amplitude 0.45 on R and B (no plateau, and under the range the single hot pixel at (290, 225) sets on those
    /// planes, so their own verdicts leave it unflagged).
    pub(crate) fn cross_plane_fixture() -> ([Vec<f32>; 3], usize, usize) {
        let (w, h) = (320usize, 256usize);
        let grid: Vec<(f64, f64, f64)> = (0..4)
            .flat_map(|j| {
                (0..6).map(move |i| (35.0 + 50.0 * i as f64, 30.0 + 45.0 * j as f64, 0.2))
            })
            .collect();
        let plane = |amp: f64, clip: bool, seed: u64| {
            let mut d = crate::test_support::gaussian_field(w, h, &grid, 1.8, 0.08);
            let star = crate::test_support::moffat_field(
                w,
                h,
                &[crate::test_support::MoffatStar {
                    x: 160.3,
                    y: 215.7,
                    amp,
                    alpha_x: 3.0,
                    alpha_y: 3.0,
                    theta: 0.0,
                }],
                4.0,
                0.0,
            );
            for (v, s) in d.iter_mut().zip(&star) {
                *v += s;
            }
            if clip {
                for v in d.iter_mut() {
                    *v = v.min(0.95);
                }
            } else {
                d[225 * w + 290] = 0.95;
            }
            add_noise(&mut d, 0.002, seed);
            d
        };
        (
            [
                plane(0.45, false, 31),
                plane(3.0, true, 32),
                plane(0.45, false, 33),
            ],
            w,
            h,
        )
    }

    /// The fixture star's fit on one plane and its verdict (from the diagnostics).
    fn fixture_star(m: &MeasuredPlane) -> (StarFit, sm::SaturationVerdict) {
        let fits = &m.measurement.fits;
        let star = fits
            .iter()
            .position(|f| (f.x - 160.3).abs() < 1.5 && (f.y - 215.7).abs() < 1.5)
            .expect("the star is fitted");
        let verdict = m
            .diagnostics
            .iter()
            .find(|d| matches!(d.outcome, sm::Outcome::Accepted { star: s } if s == star))
            .and_then(|d| d.verdict)
            .expect("an accepted star has a verdict");
        (fits[star], verdict)
    }

    /// Plan A Task 8 fix wave, owner ruling 2026-10-09: on a multi-plane image a star flagged on any plane is flagged on
    /// every plane at that position, before any plane's statistics; the frame adapter goes through the same entry.
    #[test]
    fn a_star_clipped_on_g_alone_is_flagged_on_all_three_planes_and_left_out_of_the_r_and_b_statistics(
    ) {
        let (planes, w, h) = cross_plane_fixture();
        let opts = MeasureOptions::default();
        let multi = measure_planes_diagnosed(
            3,
            w,
            h,
            &opts,
            None,
            SeedOverride::Fast,
            true,
            sm::PixelNoise::Mixed,
            None,
            |p| Ok::<_, std::convert::Infallible>(Cow::Borrowed(planes[p].as_slice())),
        )
        .unwrap();
        let (g, vg) = fixture_star(&multi[1]);
        assert!(vg.plateau && vg.saturated && !vg.cross, "G: {vg:?}");
        assert!(g.flags.saturated());
        for p in [0, 2] {
            let (star, v) = fixture_star(&multi[p]);
            assert!(!v.plateau && v.saturated && v.cross, "plane {p}: {v:?}");
            assert!(star.flags.saturated(), "plane {p}");
            assert!(multi[p].measurement.cross_flags >= 1, "plane {p}");
            // the same plane on its own: its own verdict leaves the star unflagged and its statistic counts it
            let (alone, _, _) = measure_plane_diagnosed(
                &planes[p],
                w,
                h,
                &opts,
                None,
                SeedOverride::Fast,
                false,
                true,
                sm::PixelNoise::Mixed,
                None,
                None,
            );
            let mine = alone
                .fits
                .iter()
                .find(|f| (f.x - 160.3).abs() < 1.5 && (f.y - 215.7).abs() < 1.5)
                .expect("the star is fitted alone");
            assert!(!mine.flags.saturated(), "plane {p} alone");
            let c = &multi[p].measurement.channel;
            assert_eq!(
                c.stars_unsaturated + 1,
                alone.channel.stars_unsaturated,
                "plane {p}"
            );
            assert_ne!(
                c.fwhm_px.to_bits(),
                alone.channel.fwhm_px.to_bits(),
                "plane {p}"
            );
            let unflagged: Vec<&StarFit> = multi[p]
                .measurement
                .fits
                .iter()
                .filter(|f| !f.flags.saturated())
                .collect();
            let (fwhm, _) = sm::frame_shape(unflagged.iter().copied()).expect("unflagged fits");
            assert_eq!(fwhm.to_bits(), c.fwhm_px.to_bits(), "plane {p}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "osc.fits",
            &[&planes[0], &planes[1], &planes[2]],
            w,
            h,
        );
        let (frame, fits) =
            measure_frame_with_fits(&path, &opts, None, &AtomicBool::new(false)).unwrap();
        for p in 0..3 {
            assert_eq!(
                without_timings(frame.channels[p].clone()),
                without_timings(multi[p].measurement.channel.clone()),
                "plane {p}"
            );
            assert_eq!(fits[p], multi[p].measurement.fits, "plane {p}");
        }
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
        // F-4 (Task 9, ruling R-OSP-6): β is continuous and these Gaussian stars read near the cap (the previous literal
        // was 10.0, the discrete ladder's widest). The fits now run at a non-integral β, on `powf` throughout where
        // β 10 ran on `powi`, so β and the fit-derived numbers are pinned to a few ulp, like `fwhm_px` below.
        // E5 (the owner's ruling 2026-10-09): the frame β is the median of the sample's PIXEL-INTEGRATED fits, so it
        // moved 99.288_572_538_934_94 → this (the verdict fit, its flags and the fits at β are the point model as
        // before; these point-sampled Gaussian stars still read near the cap).
        assert_ulp_close(c.beta, 99.507_410_046_446_08, "beta");
        // F-4: the FWTM aperture follows β; the previous literal was 1.390_901_839_370_229e-3 (Task 6b round 3).
        // F-1 (Task 6b round 3, ruling R9): the start shape is read from the region's starting square; the literal
        // before it was 1.390_799_753_469_352e-3 (round 1, the own half-maximum width start), before it
        // 1.387_793_525_585_654_9e-3 (Task 6b's first round), 1.387_179_128_762_254_4e-3 (Task 5) and
        // 1.387_068_045_273_481_56e-3.
        // E5: follows β; the previous literal was 1.387_279_026_471_343_5e-3 (Task 9's first build).
        assert_ulp_close(
            c.psf_signal_weight,
            1.702_475_827_736_932_3e-3,
            "psf_signal_weight",
        );
        // F-5 (Task 10, ruling F14): the star signal is the model flux in the FWTM ellipse x the plane's aperture
        // correction, so it moved by design: the previous literal was 1.387_282_647_214_539e-3 (E5); the counts, beta
        // and fwhm_px did not move. F-5 rounds 3/4 (owner ruling R6: `c`'s rim is the far ring [10F, 12F] mean outside
        // every seed's 3F disc, clipped candidates screened) moved it again from 1.697_635_877_922_333_6e-3 (round 2's
        // [6F, 8F] median); the counts, beta and fwhm_px did not move.
        // `fwhm_px` is a shape aggregate over 150 Moffat fits whose arithmetic
        // (Tier C C3: `powi`/`powf`, the reused residual) rounds differently
        // per platform's libm — the first push after C3 read 4.122_658_320_014_302_5
        // on the Windows runner against this Mac's ..._304, one ulp. A last-bit
        // FWHM is not a contract, so this aggregate is pinned to a few ulp,
        // never exactly; the fields above stay exact on every platform.
        // F-4 (Task 9): the fits at β ≈ 99.3 read the Gaussian truth 2.3548·1.8 = 4.2387 at −0.22 % (β 10: −2.59 %),
        // and the bounded verdict fit's amplitude on these Gaussian stars tops the plane's range on 6 of 150 (was 9:
        // the free fit stopped at its β 20, overshooting the peak), so the statistic reads 144; the previous literals
        // were 141 and 4.129_098_214_007_167_5 (Task 8).
        // F-3 (Task 8, ruling R-OSP-5): the frame statistic reads the unflagged fits only (the brightest stars'
        // verdict fits top the plane's range, by design). The literal before Task 8 was 4.129_401_659_376_219 (Task 6b
        // round 3).
        // F-1 (Task 6b round 3, ruling R9): as above; the literal before it was 4.129_358_265_048_832 (round 1), before
        // it 4.123_653_811_131_548 (Task 6b's first round), 4.123_100_751_260_767 (Task 5) and 4.122_658_320_014_304.
        // E5: follows β (the 144 unflagged are unchanged); the previous literal was 4.229_318_383_224_882.
        assert_eq!(c.stars_unsaturated, 144);
        assert_ulp_close(c.fwhm_px, 4.229_343_705_168_795, "fwhm_px");
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
        // F-4 (Task 9, ruling R-OSP-6): β is continuous (≈ 99.6 on these Gaussian stars), so the fits run on `powf`
        // throughout and every fit-derived field — β, the two shape aggregates, the totals and the two estimators —
        // is pinned to a few ulp and snapped; the counts, noise and sky fields stay exact. Task 8's record read
        // stars_unsaturated 141, beta 10.0, fwhm_px 4.130_618_758_639_208_5, eccentricity 0.113_726_031_990_419_54,
        // tflux 571.050_742_147_855_2, tmean_flux 11.813_155_657_437_393, psf_signal_weight 0.001_472_612_103_695_384,
        // psf_snr 0.002_168_670_535_894_186_4; the bounded verdict fit flags 1 of 150 here (was 9). E5 (the owner's
        // ruling 2026-10-09: the frame β is the median of the sample's pixel-integrated fits) moved Task 9's first
        // build's beta 99.560_183_873_386_31, fwhm_px 4.229_139_719_359_381, eccentricity
        // 0.111_005_918_977_448_41, tflux 562.456_944_818_559_5, tmean_flux 11.956_347_340_224_768,
        // psf_signal_weight 0.001_468_032_078_137_789, psf_snr 0.002_103_888_624_833_64 (the counts and flags did not move).
        // F-3 (Task 8): the literal before Task 8 was fwhm_px 4.130_443_961_173_142 (Task 6b round 3).
        // As above; the eccentricity before Task 8 was 0.113_127_912_625_514_87, before it 0.110_290_631_118_640_29
        // (round 1), 0.109_077_618_457_969_11 (Task 6b's first round), 0.111_709_924_171_636_75 (Task 5) and
        // 0.095_246_020_871_036_08.
        // F-5 (Task 10, ruling F14): tflux, tmean_flux, psf_signal_weight and psf_snr read the new star signal (model
        // flux in the FWTM ellipse x the aperture correction): E5's record read tflux 562.457_275_266_816_2, tmean_flux
        // 11.956_329_869_046_785, psf_signal_weight 0.001_468_030_795_460_277_5, psf_snr 0.002_103_891_096_939_422; the
        // counts, beta, fwhm_px, eccentricity and every noise/sky field did not move. F-5 rounds 3/4 (owner ruling R6: the
        // far-ring rim, the clip screen) moved the same four from round 2's tflux 620.461_907_374_79, tmean_flux
        // 13.189_488_893_656_062, psf_signal_weight 0.001_786_449_849_924_526_7, psf_snr 0.002_560_203_138_709_018_7;
        // nothing else moved.
        for (got, want, what) in [
            (&mut m.beta, 99.485_664_214_790_5, "beta"),
            (&mut m.fwhm_px, 4.229_131_188_127_607, "fwhm_px"),
            (
                &mut m.eccentricity,
                0.110_520_527_232_258_43,
                "eccentricity",
            ),
            (&mut m.tflux, 626.686_097_226_946_8, "tflux"),
            (&mut m.tmean_flux, 13.321_799_809_042_265, "tmean_flux"),
            (
                &mut m.psf_signal_weight,
                0.001_822_471_318_034_213_3,
                "psf_signal_weight",
            ),
            (&mut m.psf_snr, 0.002_611_826_348_685_623, "psf_snr"),
        ] {
            assert_ulp_close(*got, want, what);
            *got = want;
        }
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
stars_refused: 0, refused_by_reason: {}, stars_unsaturated: 149, \
beta: 99.4856642147905, beta_own: None, beta_own_at_cap: false, fwhm_px: 4.229131188127607, eccentricity: 0.11052052723225843, \
tflux: 626.6860972269468, tmean_flux: 13.321799809042265, \
mean_flux_rejected: 0, m_star: 0.0013608953886758983, \
n_star: 0.002002734525205997, noise: 0.0019919854930511845, \
noise_source: Mrs, median: 0.08010764420032501, \
mad: 0.001409422606229782, median_mean_dev: 0.003131831530481577, \
snr_weight: 2.471863255689976, noise_scale_low: 0.0010744471801444888, \
noise_scale_high: 0.0024544917978346348, location: 0.08010764420032501, \
scale: 0.0020880382508039474, psf_signal_weight: 0.0018224713180342133, \
psf_snr: 0.002611826348685623, timings: MeasureTimings { read_ms: 0, \
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
        // F-4 (Task 9, ruling R-OSP-6): β is continuous, near the cap on these Gaussian stars, and the fit-derived
        // numbers are pinned to a few ulp (the fits run on `powf`); the previous literals were beta 10.0,
        // psf_signal_weight 1.391_065_217_128_910_3e-3, stars_unsaturated 141 (the bounded verdict fit flags 6, was 9)
        // and fwhm_px 4.129_063_469_967_53.
        // E5 (the owner's ruling 2026-10-09, the pixel-integrated sample fit): β 99.246_318_146_702_68 → this,
        // psf_signal_weight 1.387_250_758_422_298e-3 → this, fwhm_px 4.229_284_321_565_829 → below.
        assert_ulp_close(c.beta, 99.358_444_715_324_14, "beta");
        assert_ulp_close(
            c.psf_signal_weight,
            1.702_397_814_933_499_6e-3,
            "psf_signal_weight",
        );
        // F-5 (Task 10, ruling F14): the star signal is the model flux in the FWTM ellipse x the plane's aperture
        // correction; the previous literal was 1.387_249_747_235_731e-3 (E5). The counts, beta and fwhm_px did not move.
        // F-5 rounds 3/4 (owner ruling R6: the far-ring rim, the clip screen): moved from 1.697_649_473_105_291_6e-3
        // (round 2); stars_unsaturated, beta and fwhm_px did not move.
        // F-3 (Task 8, ruling R-OSP-5): the statistic over the unflagged fits; the literal before Task 8 was
        // 4.129_369_725_278_327.
        assert_eq!(c.stars_unsaturated, 144);
        assert_ulp_close(c.fwhm_px, 4.229_303_258_914_085_5, "fwhm_px");
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
        // The fit-derived fields are pinned to a few ulp and snapped, as in the Peak record (F-4, Task 9: the continuous
        // β runs the fits on `powf`). Task 8's record read stars_unsaturated 141, beta 10.0, fwhm_px
        // 4.130_521_827_498_531, eccentricity 0.112_086_634_567_625_24, tflux 571.062_365_054_155_6, tmean_flux
        // 11.813_897_354_284_132, psf_signal_weight 0.001_472_734_537_384_067_6, psf_snr 0.002_168_758_817_054_136_4.
        // E5 (the owner's ruling 2026-10-09: the pixel-integrated sample fit) moved Task 9's first build's beta
        // 99.522_522_213_158_42, fwhm_px 4.229_136_928_323_991_5, eccentricity 0.111_441_301_280_083_44,
        // tflux 562.457_809_883_053_6, tmean_flux 11.956_339_865_260_546, psf_signal_weight
        // 0.001_468_033_418_188_667_2, psf_snr 0.002_103_895_096_442_766_7 (the counts and flags did not move).
        // F-3 (Task 8, ruling R-OSP-5): the literals before it were fwhm_px 4.130_362_798_202_614 and eccentricity
        // 0.111_119_809_985_610_2.
        // F-5 (Task 10, ruling F14): the same four fields read the new star signal; the record before read tflux
        // 562.457_716_878_329_7, tmean_flux 11.956_344_823_185_944, psf_signal_weight 0.001_468_033_784_191_355_4,
        // psf_snr 0.002_103_894_400_667_322; the counts, beta, fwhm_px, eccentricity and noise/sky fields did not move.
        // F-5 rounds 3/4 (owner ruling R6: the far-ring rim, the clip screen) moved the same four from round 2's tflux
        // 620.456_295_156_370_2, tmean_flux 13.189_375_194_910_12, psf_signal_weight 0.001_786_418_291_297_927_3,
        // psf_snr 0.002_560_156_823_684_486_6; the counts, beta, fwhm_px, eccentricity and noise/sky fields did not move.
        for (got, want, what) in [
            (&mut m.beta, 99.543_280_832_639_97, "beta"),
            (&mut m.fwhm_px, 4.229_139_285_746_762, "fwhm_px"),
            (
                &mut m.eccentricity,
                0.111_441_722_284_783_02,
                "eccentricity",
            ),
            (&mut m.tflux, 626.222_210_579_139_6, "tflux"),
            (&mut m.tmean_flux, 13.311_944_378_987_553, "tmean_flux"),
            (
                &mut m.psf_signal_weight,
                0.001_819_775_024_070_364_4,
                "psf_signal_weight",
            ),
            (&mut m.psf_snr, 0.002_607_961_118_702_720_6, "psf_snr"),
        ] {
            assert_ulp_close(*got, want, what);
            *got = want;
        }
        assert_eq!(
            format!("{m:?}"),
            "ChannelMeasurement { stars_detected: 150, stars_fitted: 150, \
stars_refused: 0, refused_by_reason: {}, stars_unsaturated: 149, \
beta: 99.54328083263997, beta_own: None, beta_own_at_cap: false, fwhm_px: 4.229139285746762, eccentricity: 0.11144172228478302, \
tflux: 626.2222105791396, tmean_flux: 13.311944378987553, \
mean_flux_rejected: 0, m_star: 0.0013608953886758983, \
n_star: 0.002002734525205997, noise: 0.0019919854930511845, \
noise_source: Mrs, median: 0.08010764420032501, \
mad: 0.001409422606229782, median_mean_dev: 0.003131831530481577, \
snr_weight: 2.471863255689976, noise_scale_low: 0.0010744471801444888, \
noise_scale_high: 0.0024544917978346348, location: 0.08010764420032501, \
scale: 0.0020880382508039474, psf_signal_weight: 0.0018197750240703644, \
psf_snr: 0.0026079611187027206, timings: MeasureTimings { read_ms: 0, \
background_ms: 0, noise_ms: 0, detect_ms: 0, fit_ms: 0 } }"
        );
    }

    /// A few ulp of tolerance for the one class of pinned number whose last
    /// bits legitimately differ between platforms: an aggregate over Moffat
    /// fits (`powf`/`exp` in the platform libm). 1e-12 relative is ≈ 4 500
    /// ulp at these magnitudes — far above any libm difference (the measured
    /// ones are 1–2 ulp) and far below anything the pipeline could notice.
    ///
    /// Since F-4 (Plan A Task 9) the pinned β on these Gaussian fixtures is the
    /// median of 8-parameter free-β solves that stop on the Gaussian ridge,
    /// where ln β trades against α and the problem is ill-conditioned: a 1-ulp
    /// `exp`/`powf` difference can flip one LM accept/reject step and move a
    /// solve's β by far more than an ulp (the review's focus 4). The tolerance
    /// is kept at 1e-12 on purpose; the Windows and Linux runners are the
    /// check before the main push (an owner push item), and a measured spread
    /// there is what would loosen it.
    fn assert_ulp_close(got: f64, want: f64, what: &str) {
        let rel = ((got - want) / want).abs();
        assert!(
            rel <= 1e-12,
            "{what}: got {got:?}, pinned {want:?} (relative {rel:.3e} > 1e-12)"
        );
    }

    #[test]
    fn the_adapter_reads_egain_and_cscl_into_the_gain() {
        use crate::fits_writer::{Card, CardValue};
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.fits");
        let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
        let cards = [
            Card::new("EGAIN", CardValue::Real(50000.0 / 65535.0)).unwrap(),
            Card::new("ATH_CSCL", CardValue::Real(65535.0)).unwrap(),
            Card::new("ATH_CFNM", CardValue::Real(3000.0)).unwrap(),
        ];
        write_fits_f32(&p, w, h, 1, &data, &cards).unwrap();
        assert!(
            (gain_from_header(&p).unwrap() - 50000.0).abs() < 1e-6,
            "EGAIN x ATH_CSCL, CFNM ignored (PA-22)"
        );
        let q = d.path().join("n.fits");
        write_fits_f32(&q, w, h, 1, &data, &[]).unwrap();
        assert_eq!(gain_from_header(&q), None);
        let r = d.path().join("only_egain.fits");
        write_fits_f32(&r, w, h, 1, &data, &cards[..1]).unwrap();
        assert_eq!(gain_from_header(&r), None, "ATH_CSCL missing: no gain");
        assert_eq!(
            gain_from_header(&d.path().join("missing.fits")),
            None,
            "an unreadable file has none (logged)"
        );
    }

    #[test]
    fn a_debayered_plane_spreads_the_shot_term_by_its_site_share() {
        // F-6 (Task 11, ruling R11-1): a colour image's planes 0 / 1 / 2 carry s = 4 / 2 / 4 (RGGB site shares 0.25 /
        // 0.5 / 0.25), a single plane 1; each star's SNR is S / sigma_S with sigma_S^2 = s S / g + kappa N sigma^2 (1 +
        // N / n) recomputed from the terms to 1e-9, the correlation the plane's own read.
        let (w, h) = (768usize, 512usize);
        let planes: Vec<Vec<f32>> = (0..3)
            .map(|p| crate::test_support::fixture_field(30 + p as u64, 1.0, 0.002).0)
            .collect();
        let gain = 50000.0;
        let opts = MeasureOptions::default();
        let multi = measure_planes_diagnosed(
            3,
            w,
            h,
            &opts,
            None,
            SeedOverride::Fast,
            false,
            sm::PixelNoise::Mixed,
            Some(gain),
            |p| Ok::<_, std::convert::Infallible>(Cow::Borrowed(planes[p].as_slice())),
        )
        .unwrap();
        for (p, want) in [4.0, 2.0, 4.0].into_iter().enumerate() {
            let m = &multi[p].measurement;
            assert_eq!(m.shot_spread, want, "plane {p}");
            assert_eq!(m.snr_noise, sm::SnrNoise::Read, "plane {p}");
            assert!(
                (m.snr_gain.unwrap() - gain / ADU_SCALE as f64).abs() < 1e-12,
                "plane {p}"
            );
            assert!(m.fits.len() > 50, "plane {p}: {} fits", m.fits.len());
            let working: Vec<f32> = planes[p].iter().map(|v| v * ADU_SCALE).collect();
            let rho = sm::seeds::read_noise_correlation(
                &working,
                w,
                h,
                None,
                &sm::domain::background_mesh(&working, w, h, None, true),
            )
            .expect("the read")
            .rho();
            let noise = sm::ApertureNoise {
                rho: Some(rho),
                shot_spread: want,
            };
            let mut checked = 0;
            for f in m.fits.iter().filter(|f| f.snr.is_finite()).take(25) {
                let (snr, t) = sm::star_snr(&working, w, h, f, m.snr_gain, None, &noise);
                assert_eq!(snr, f.snr, "plane {p}: the fit's value is the function's");
                let var = t.shot_spread * t.s.max(0.0) / m.snr_gain.unwrap()
                    + t.kappa
                        * t.n_ap as f64
                        * t.sigma_sky.powi(2)
                        * (1.0 + t.n_ap as f64 / f64::from(t.n_sky));
                assert!(((t.s / var.sqrt()) / f.snr - 1.0).abs() < 1e-9, "plane {p}");
                assert!(t.kappa > 0.5, "plane {p}: kappa {}", t.kappa);
                checked += 1;
            }
            assert!(checked >= 20, "plane {p}: {checked}");
        }
        let single = measure_planes_diagnosed(
            1,
            w,
            h,
            &opts,
            None,
            SeedOverride::Fast,
            false,
            sm::PixelNoise::White,
            Some(gain),
            |_| Ok::<_, std::convert::Infallible>(Cow::Borrowed(planes[0].as_slice())),
        )
        .unwrap();
        assert_eq!(single[0].measurement.shot_spread, 1.0);
        assert_eq!(single[0].measurement.snr_noise, sm::SnrNoise::White);
        assert!(
            single[0].measurement.snr_median.is_some()
                && single[0].measurement.hfr_median.is_some()
        );
    }

    /// Plan A Task 11b (owner decision R11b-12): a field of 30 round Moffat stars of FWHM 4 px and Moffat `beta` on a
    /// 480 x 400 plane, amplitudes 0.04 to 0.33 (one star at the plane maximum would make every equal star read
    /// saturated by the amplitude clause), background 0.08, Gaussian noise 0.002 — the two members of a synthetic
    /// group whose own βs differ.
    fn moffat_member(beta: f64, seed: u64) -> Vec<f32> {
        let alpha = 4.0 / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt());
        let stars: Vec<crate::test_support::MoffatStar> = (0..30)
            .map(|i| crate::test_support::MoffatStar {
                x: 60.0 + (i % 6) as f64 * 70.0 + 0.3,
                y: 60.0 + (i / 6) as f64 * 60.0 + 0.6,
                amp: 0.04 + 0.01 * i as f64,
                alpha_x: alpha,
                alpha_y: alpha,
                theta: 0.0,
            })
            .collect();
        let mut plane = crate::test_support::moffat_field(480, 400, &stars, beta, 0.08);
        crate::test_support::add_noise(&mut plane, 0.002, seed);
        plane
    }

    fn pass1_of(plane: &[f32], opts: &MeasureOptions) -> PlanesPass1 {
        measure_planes_pass1(
            1,
            480,
            400,
            opts,
            None,
            SeedOverride::Fast,
            false,
            sm::PixelNoise::White,
            None,
            |_| Ok::<_, std::convert::Infallible>(Cow::Borrowed(plane)),
        )
        .unwrap()
    }

    fn pass2_of(plane: &[f32], pass1: PlanesPass1, beta: f64) -> MeasuredPlane {
        measure_planes_pass2(pass1, &[beta], None, |_| {
            Ok::<_, std::convert::Infallible>(Cow::Borrowed(plane))
        })
        .unwrap()
        .remove(0)
    }

    fn single_pass(plane: &[f32], opts: &MeasureOptions) -> MeasuredPlane {
        measure_planes_diagnosed(
            1,
            480,
            400,
            opts,
            None,
            SeedOverride::Fast,
            false,
            sm::PixelNoise::White,
            None,
            |_| Ok::<_, std::convert::Infallible>(Cow::Borrowed(plane)),
        )
        .unwrap()
        .remove(0)
    }

    /// Plan A Task 13 (13-P): a frame's two passes log their profile at debug. `"frame plane first pass"` carries the
    /// seeds, the verdict pass's wall, fits and LM iterations, the β sample's wall, the own β and the plane's wall;
    /// `"frame plane measured"` gains `verdict_ms`, which is a share of `fit_ms`.
    #[test]
    fn the_two_passes_log_the_fit_profile() {
        let dir = tempfile::tempdir().unwrap();
        let plane = moffat_member(3.0, 11);
        let path = write(dir.path(), "profile.fits", &[plane.as_slice()], 480, 400);
        let opts = MeasureOptions::default();
        let cancel = AtomicBool::new(false);
        let mut result = None;
        let logs = crate::stacking::solve::tests::captured_logs(|| {
            let pass1 = measure_frame_pass1(&path, &opts, None, &cancel).unwrap();
            let beta = pass1.beta_own()[0];
            result =
                Some(measure_frame_pass2(&path, pass1, &[beta], &opts, None, &cancel).unwrap());
        });
        let (frame, _) = result.unwrap();
        let line = |msg: &str| {
            logs.lines()
                .find(|l| l.contains(msg))
                .unwrap_or_else(|| panic!("no {msg:?} line in:\n{logs}"))
                .to_string()
        };
        let first = line("frame plane first pass");
        for field in [
            "path=",
            "plane=0",
            "seeds=",
            "verdict_ms=",
            "verdict_fits=",
            "verdict_iterations=",
            "beta_ms=",
            "beta_own=",
            "duration_ms=",
        ] {
            assert!(first.contains(field), "{field} missing: {first}");
        }
        let field = |l: &str, name: &str| -> u64 {
            let at = l.find(&format!(" {name}=")).unwrap() + name.len() + 2;
            l[at..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        assert!(field(&first, "seeds") >= 20, "{first}");
        assert!(field(&first, "verdict_fits") >= 20, "{first}");
        assert!(
            field(&first, "verdict_iterations") >= field(&first, "verdict_fits"),
            "{first}"
        );
        let measured = line("frame plane measured");
        assert!(measured.contains("verdict_ms="), "{measured}");
        assert!(
            field(&measured, "verdict_ms") <= frame.channels[0].timings.fit_ms,
            "{measured}"
        );
        assert_eq!(
            field(&measured, "fit_ms"),
            frame.channels[0].timings.fit_ms,
            "{measured}"
        );
    }

    /// Plan A Task 13b (13-P): each plane logs its sub-step profile at debug, `"frame plane substeps"` (the plane-wide
    /// passes) and `"frame plane substeps fit"` (the fit tail), a wall `<step>_us` and a thread-CPU `<step>_cpu_us` per
    /// step, and the timers move no number.
    #[test]
    fn a_plane_logs_its_sub_step_profile() {
        let dir = tempfile::tempdir().unwrap();
        let plane = moffat_member(3.0, 11);
        let path = write(dir.path(), "substeps.fits", &[plane.as_slice()], 480, 400);
        let opts = MeasureOptions::default();
        let cancel = AtomicBool::new(false);
        let mut result = None;
        let logs = crate::stacking::solve::tests::captured_logs(|| {
            let pass1 = measure_frame_pass1(&path, &opts, None, &cancel).unwrap();
            let beta = pass1.beta_own()[0];
            result =
                Some(measure_frame_pass2(&path, pass1, &[beta], &opts, None, &cancel).unwrap());
        });
        let (frame, _) = result.unwrap();
        let line = |msg: &str, not: &str| {
            logs.lines()
                .find(|l| l.contains(msg) && (not.is_empty() || !l.contains(not)))
                .unwrap_or_else(|| panic!("no {msg:?} line in:\n{logs}"))
                .to_string()
        };
        let first = line("frame plane substeps", "substeps fit");
        let fit = line("frame plane substeps fit", "");
        for step in [
            "work_copy",
            "sky_level",
            "sat_plane",
            "mesh",
            "residual",
            "mrs",
            "detect_prep",
            "detect_pass1",
            "detect_widths",
            "detect_pass2",
            "detect_knots",
        ] {
            assert!(first.contains(&format!(" {step}_us=")), "{step}: {first}");
            assert!(
                first.contains(&format!(" {step}_cpu_us=")),
                "{step}: {first}"
            );
        }
        assert!(first.contains(" sat_wait_us="), "{first}");
        for step in ["signal_totals", "rcr", "ring_sky", "aperture_c", "snr_hfr"] {
            assert!(fit.contains(&format!(" {step}_us=")), "{step}: {fit}");
            assert!(fit.contains(&format!(" {step}_cpu_us=")), "{step}: {fit}");
        }
        assert!(
            first.contains("path=") && first.contains("plane=0"),
            "{first}"
        );
        let field = |l: &str, name: &str| -> u64 {
            let at = l.find(&format!(" {name}=")).unwrap() + name.len() + 2;
            l[at..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        // The RCR is a share of the totals; the plane-wide passes took real time on a real plane.
        assert!(
            field(&fit, "rcr_us") <= field(&fit, "signal_totals_us"),
            "{fit}"
        );
        assert!(
            field(&first, "mrs_us") > 0 && field(&first, "detect_pass1_us") > 0,
            "{first}"
        );
        #[cfg(unix)]
        {
            assert!(field(&first, "mrs_cpu_us") > 0, "{first}");
            assert!(field(&fit, "snr_hfr_cpu_us") > 0, "{fit}");
        }
        assert!(frame.channels[0].stars_fitted >= 20);
    }

    /// Task 11b: two members whose own βs differ (2.5 and 6 in the pixels) are both fitted at the group β, the lower
    /// median of their own βs: every fit, `channel.beta` and the frame F of each member is the one a single pass at
    /// `PsfModel::Fixed(group β)` measures, `beta_own` keeps each member's own β, and the member whose own β is not
    /// the group's reads a different F than its own-β single pass — the β ridge the group β removes from the
    /// comparison.
    #[test]
    fn a_two_member_group_is_fitted_at_the_shared_beta() {
        let opts = MeasureOptions::default();
        let planes = [moffat_member(2.5, 7), moffat_member(6.0, 8)];
        let firsts: Vec<PlanesPass1> = planes.iter().map(|p| pass1_of(p, &opts)).collect();
        let own: Vec<f64> = firsts.iter().map(|f| f.beta_own()[0]).collect();
        assert!(own[0] < 3.5 && own[1] > 4.5, "own betas {own:?}");
        let group = psf_signal::group_beta(&own);
        assert_eq!(
            group, own[0],
            "the lower median of two is the smaller own beta"
        );

        for ((plane, first), &own_beta) in planes.iter().zip(firsts).zip(&own) {
            let m = pass2_of(plane, first, group);
            assert_eq!(m.measurement.channel.beta, group);
            assert_eq!(m.measurement.channel.beta_own, Some(own_beta));
            assert_eq!(m.measurement.beta_source, sm::BetaSource::Fixed);
            assert!(
                m.measurement.fits.len() >= 25,
                "{} fits",
                m.measurement.fits.len()
            );
            assert!(m.measurement.fits.iter().all(|f| f.beta == group));

            let at_group = single_pass(
                plane,
                &MeasureOptions {
                    psf_model: PsfModel::Fixed(group),
                    ..opts
                },
            );
            // Debug-compared: a fit's NaN fields (no ring sky) are equal there, never under `==`.
            assert_eq!(
                format!("{:?}", m.measurement.fits),
                format!("{:?}", at_group.measurement.fits)
            );
            assert_eq!(
                m.measurement.channel.fwhm_px,
                at_group.measurement.channel.fwhm_px
            );
            assert_eq!(
                m.measurement.channel.tflux,
                at_group.measurement.channel.tflux
            );
            assert_eq!(
                m.measurement.aperture_correction,
                at_group.measurement.aperture_correction
            );
        }
        // The member at its own β 6 reads another F than at the shared β: the ridge is real on this field.
        let own_single = single_pass(&planes[1], &opts);
        assert_eq!(
            own_single.measurement.channel.beta, own[1],
            "pass 1 reads the single pass's own beta"
        );
        let shared = pass2_of(&planes[1], pass1_of(&planes[1], &opts), group);
        assert!(
            (shared.measurement.channel.fwhm_px / own_single.measurement.channel.fwhm_px - 1.0)
                .abs()
                > 1e-3,
            "F at the shared beta {} vs own {}",
            shared.measurement.channel.fwhm_px,
            own_single.measurement.channel.fwhm_px
        );
    }

    /// Task 11b: a group of one is fitted at its own β — the two passes at the member's own β reproduce the single
    /// `Auto` pass's fits and frame F exactly (only the β's source reads `fixed`).
    #[test]
    fn a_group_of_one_is_fitted_at_its_own_beta() {
        let opts = MeasureOptions::default();
        let plane = moffat_member(3.0, 9);
        let first = pass1_of(&plane, &opts);
        let own = first.beta_own()[0];
        let group = psf_signal::group_beta(&[own]);
        assert_eq!(group, own);
        let two = pass2_of(&plane, first, group);
        let single = single_pass(&plane, &opts);
        assert_eq!(single.measurement.channel.beta, own);
        assert_eq!(two.measurement.channel.beta, own);
        assert_eq!(two.measurement.channel.beta_own, Some(own));
        assert_eq!(
            format!("{:?}", two.measurement.fits),
            format!("{:?}", single.measurement.fits)
        );
        assert_eq!(
            two.measurement.channel.fwhm_px,
            single.measurement.channel.fwhm_px
        );
        assert_eq!(
            single.measurement.channel.beta_own, None,
            "a single pass reads no own beta of its own"
        );
    }
}
