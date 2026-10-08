//! One star-measurement path (spec `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` §2.1):
//! the stacking Measure stage's per-star fit, per-frame β, frame aggregates, noise and sky statistic as ONE module the
//! stacking adapter (`stacking::measure`), local normalization's refit and — from Plan C — the Analysis tab call.
//! `render`-gated like `analysis`; nothing here depends on `stacking` (the stacking-only seed sources, the peak and
//! structure detectors and the median prefilter, stay adapter-side and hand their seeds in as [`SeedSpec::Given`]).
//!
//! Plan A Task 2 moved the code here verbatim (no arithmetic changed, no operation reordered); the later tasks fix it
//! defect by defect against the synthetic truth lab.

use std::sync::Arc;
use std::time::Instant;

use crate::integration::stats::ScaleEstimator;

pub mod beta;
pub mod domain;
pub mod fit;
pub mod frame;
pub mod robust;

pub use beta::{AUTO_BETAS, AUTO_SAMPLE};
pub use domain::{
    background_residual, noise_model, noise_mrs, sky_level, to_working_units, Domain, NoiseModel,
    NoiseSource, SkyLevel, ADU_SCALE,
};
pub use fit::{fwtm_from_alpha, FitOutcome, FitParams, PsfModel, Seed, StarFit};
pub use frame::{frame_shape, signal_totals, FrameAggregates, SignalTotals, RCR_LIMIT};

/// Version of the measurement path's own behaviour — bumped whenever
/// `star_measure::fit`'s `fit_one`, `accept` or the aperture (or anything else in this module) change what a
/// fit accepts or measures; folded as `"measureVersion"` into every artifact hash that stores
/// fit-derived numbers (`measurement_subtree`, `registration_subtree`, `normalization_subtree`).
///
/// Without it a cached artifact cannot tell that the code which produced it
/// has changed: the config it was keyed on is untouched, but the numbers
/// would come out different today. M4a Task 2 is exactly that case — the
/// adaptive sampling region and the inner-region rule moved which stars are
/// accepted, for the quality measurement AND for local normalization's
/// PSF-flux scale, which fits through the same `fit_stars` with
/// `FitParams::default()` (ruling R-M4a-15).
///
/// 1 = the fixed `5σ` stamp with the `0.85·r` centre rule (M1-M3).
/// 2 = the adaptive sampling region + `inner_margin` (M4a Task 2).
/// 3 = no change to what a fit accepts — bumped so every cached `metrics`
/// artifact is treated as stale until it comes with a sibling `fits`
/// artifact (Tier C Task 1, spec §2.2.7): the two are written together from
/// this version on, and a document from before it never carries `fits` at
/// all. (Versions 1-3 were `PSF_FIT_VERSION`, formerly in `stacking::psf_signal`.)
/// 4 = Plan A, the one star-measurement path (this module, F-0…F-7; ruling
/// R-OSP-1, spec §4.4).
///
/// Policy: ONE bump per merged plan that changes what a fit accepts or
/// measures, never one per task; Plan C = 5. `AnalysisConfig::config_hash`
/// is Plan C's.
pub const MEASURE_VERSION: u32 = 4;

/// What the module is handed besides the plane (grows in Tasks 4–12). `Default` = `Domain::Calibrated` without a
/// gain, `PsfModel::Auto`, 24 576 stars, no seeds, `FitParams::default()`, no precomputed noise, `Bwmv` (ruling F14:
/// every test literal uses `..Default::default()` so a later field never breaks an earlier test).
#[derive(Debug, Clone, PartialEq)]
pub struct MeasureOptions {
    pub domain: Domain,
    pub psf_model: PsfModel,
    /// The detection cap of the module's own seed source (Task 6a). [`SeedSpec::Given`] seeds are fitted as handed
    /// in: the caller's detector has already applied its cap.
    pub max_stars: usize,
    pub seeds: SeedSpec,
    pub fit: FitParams,
    /// A noise model the caller already computed (the adapter's Median3 arm hands the MRS σ to its detector
    /// BEFORE detection); `None` = the module computes it. Nothing is computed twice (I-1).
    pub noise: Option<NoiseModel>,
    /// The scale estimator the adapter keeps in lockstep with normalization (PA-21).
    pub scale_estimator: ScaleEstimator,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        MeasureOptions {
            domain: Domain::Calibrated {
                gain_e_per_unit: None,
            },
            psf_model: PsfModel::Auto,
            max_stars: 24576,
            seeds: SeedSpec::None,
            fit: FitParams::default(),
            noise: None,
            scale_estimator: ScaleEstimator::Bwmv,
        }
    }
}

/// Wall time per phase the module owns (I-1); the adapter adds `read_ms` and `detect_ms` and keeps
/// `ChannelMeasurement.timings` exactly as today. A phase the module did not run (a precomputed noise model) reads 0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PhaseTimings {
    pub background_ms: u64,
    pub noise_ms: u64,
    pub fit_ms: u64,
}

/// Where the seeds come from. Task 2: the adapter detects (Peak/Structure) and passes them in.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SeedSpec {
    /// No seeds: nothing is fitted.
    #[default]
    None,
    /// Seeds from the caller's detector, expected brightest-first (the detector's order).
    Given(Vec<Seed>),
}

impl SeedSpec {
    fn seeds(&self) -> &[Seed] {
        match self {
            SeedSpec::None => &[],
            SeedSpec::Given(seeds) => seeds,
        }
    }
}

/// One plane's measurement (PA-1: the stacking adapter keeps its own `stacking::measure::PlaneMeasurement`). Energies
/// (`stars`, `frame`, `noise`) are in the module's working units; `sky` is in the plane's native units.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaneMeasurement {
    /// The accepted fits, after the ±1 px duplicate drop.
    pub stars: Vec<StarFit>,
    /// The β every star was fitted at.
    pub beta: f64,
    pub frame: FrameAggregates,
    pub noise: NoiseModel,
    pub sky: SkyLevel,
    /// Seeds handed to the fitter.
    pub seeds: usize,
    pub timings: PhaseTimings,
}

/// Measure one plane: the noise model (unless `opts.noise` carries one) and the fits on the working-unit copy, the
/// frame aggregates over the accepted fits, the sky statistic on the native plane. The fit runs on `pool` when given
/// one, as do the background model and MRS.
pub fn measure_plane(
    plane: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlaneMeasurement {
    let working = domain::to_working_units(plane, &opts.domain);
    let (noise, mut timings) = match opts.noise {
        Some(given) => (given, PhaseTimings::default()),
        None => domain::noise_model(&working, w, h, pool),
    };
    let seeds = opts.seeds.seeds();
    let t = Instant::now();
    let outcome = match pool {
        // Already inside this `install` — `fit_stars`'s own pool param gets
        // `None` so it does not nest a second, pointless `install` inside
        // this one (perf tier 1 Task 2).
        Some(p) => {
            p.install(|| fit::fit_stars(&working, w, h, seeds, opts.psf_model, &opts.fit, None))
        }
        None => fit::fit_stars(&working, w, h, seeds, opts.psf_model, &opts.fit, None),
    };
    timings.fit_ms = t.elapsed().as_millis() as u64;
    let frame = FrameAggregates::of(&outcome.fits);
    let sky = domain::sky_level(plane, w, h, &opts.domain, opts.scale_estimator);
    PlaneMeasurement {
        stars: outcome.fits,
        beta: outcome.beta,
        frame,
        noise,
        sky,
        seeds: seeds.len(),
        timings,
    }
}

/// The refit entry point LN uses (PA-6): every seed fitted at `beta`. The plane is in the module's working units
/// (ADU-scaled), exactly as `fit_stars_with_beta` takes it.
pub fn fit_stars_at(
    plane: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    fit: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> FitOutcome {
    fit::fit_stars_with_beta(plane, w, h, seeds, beta, fit, pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_field;

    #[cfg(feature = "solver")]
    #[test]
    fn module_measure_equals_the_adapter_bit_for_bit() {
        use crate::stacking::measure::{
            measure_plane_detailed, measure_plane_with_seeds, MeasureOptions as AdapterOptions,
            SeedSource,
        };
        let (data, w, h) = fixture_field(2026_09_19, 1.0, 0.002); // 150 Gaussian stars, σ 1.8 px, bg 0.08, noise 0.002
        let adapter = measure_plane_with_seeds(
            &data,
            w,
            h,
            &AdapterOptions::default(),
            None,
            SeedSource::Fast,
        );
        let (pm, seeds) = measure_plane_detailed(
            &data,
            w,
            h,
            &AdapterOptions::default(),
            None,
            SeedSource::Fast,
        );
        let opts = MeasureOptions {
            seeds: SeedSpec::Given(seeds),
            ..Default::default()
        };
        let m = measure_plane(&data, w, h, &opts, None);
        assert_eq!(m.stars, pm.fits);
        assert_eq!(m.beta, adapter.channel.beta);
        assert_eq!(m.frame.fwhm_px.unwrap(), adapter.channel.fwhm_px);
        assert_eq!(m.frame.tflux / ADU_SCALE as f64, adapter.channel.tflux);
        assert_eq!(m.noise.sigma / ADU_SCALE as f64, adapter.channel.noise);
        assert_eq!(m.sky.median, adapter.channel.median);
        assert_eq!(m.sky.scale, adapter.channel.scale);
        assert_eq!(m.sky.noise_scale_high, adapter.channel.noise_scale_high);
    }

    #[test]
    fn a_precomputed_noise_model_is_used_and_not_recomputed() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let given = NoiseModel {
            sigma: 123.0,
            source: NoiseSource::Mrs,
            m_star: 1.0,
            n_star: 2.0,
        };
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                noise: Some(given),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.noise, given);
        assert_eq!(m.timings.noise_ms, 0);
        let fresh = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        assert_ne!(fresh.noise.sigma, 123.0);
    }

    #[test]
    fn sky_level_clips_in_the_planes_own_domain() {
        // the same field as ADU: the RawAdu statistics equal the Calibrated ones × 65535 (clip [1, clip−1] ↔ [CLIP_LO, CLIP_HI])
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let adu: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let a = sky_level(
            &data,
            w,
            h,
            &Domain::Calibrated {
                gain_e_per_unit: None,
            },
            ScaleEstimator::Bwmv,
        );
        let b = sky_level(
            &adu,
            w,
            h,
            &Domain::RawAdu {
                clip: 65535.0,
                gain_e_per_adu: None,
            },
            ScaleEstimator::Bwmv,
        );
        assert!((b.median / ADU_SCALE as f64 - a.median).abs() < 1e-6 * a.median);
        assert!((b.mad / ADU_SCALE as f64 - a.mad).abs() < 1e-6 * a.mad);
        assert!(b.median > 1000.0, "the ADU sample is not empty");
    }

    /// A pin, not RED (ruling F28): after the move the wrapper and the entry point share one body. The real pins of
    /// this task are the probe parity gate and `measure.rs`'s literals.
    #[test]
    fn fit_stars_at_equals_fit_stars_with_beta() {
        use crate::test_support::{moffat_field, MoffatStar};
        let stars: Vec<MoffatStar> = (0..20)
            .map(|i| MoffatStar {
                x: 20.0 + (i % 5) as f64 * 40.3,
                y: 20.0 + (i / 5) as f64 * 40.7,
                amp: 0.2,
                alpha_x: 5.0,
                alpha_y: 5.0,
                theta: 0.0,
            })
            .collect();
        let data = moffat_field(220, 180, &stars, 4.0, 0.05);
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 1.0,
            })
            .collect();
        let a = fit_stars_at(&data, 220, 180, &seeds, 4.0, &FitParams::default(), None);
        let b = fit::fit_stars_with_beta(&data, 220, 180, &seeds, 4.0, &FitParams::default(), None);
        assert_eq!(a, b);
        assert_eq!(a.fits.len(), 20);
    }

    /// RawAdu's noise-scale factors are taken on `[2, clip − 2]` ADU, the unit domain's
    /// `[NOISE_CLIP_LO, NOISE_CLIP_HI]` in the plane's own counts: the same field as ADU gives the calibrated
    /// factors, location and scale × 65535 (the unit-domain range on an ADU sample would empty it and read 0).
    #[test]
    fn raw_adu_noise_scale_factors_location_and_scale_are_the_calibrated_ones_in_adu() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let adu: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let a = sky_level(
            &data,
            w,
            h,
            &Domain::Calibrated {
                gain_e_per_unit: None,
            },
            ScaleEstimator::Bwmv,
        );
        let b = sky_level(
            &adu,
            w,
            h,
            &Domain::RawAdu {
                clip: 65535.0,
                gain_e_per_adu: None,
            },
            ScaleEstimator::Bwmv,
        );
        let s = ADU_SCALE as f64;
        for (what, got, want) in [
            ("noise_scale_low", b.noise_scale_low, a.noise_scale_low),
            ("noise_scale_high", b.noise_scale_high, a.noise_scale_high),
            ("location", b.location, a.location),
            ("scale", b.scale, a.scale),
            ("median_mean_dev", b.median_mean_dev, a.median_mean_dev),
        ] {
            assert!(
                want > 0.0,
                "{what}: the calibrated value is measured ({want})"
            );
            assert!(
                (got / s - want).abs() < 1e-5 * want,
                "{what}: RawAdu {got} / 65535 vs Calibrated {want}"
            );
        }
    }

    /// The working units: a calibrated plane is the ADU-scaled copy the stacking Measure stage always made, a raw
    /// plane is used as it is.
    #[test]
    fn working_units_scale_a_calibrated_plane_and_borrow_a_raw_one() {
        let plane = [0.5f32, 1.0, 0.25];
        let cal = to_working_units(
            &plane,
            &Domain::Calibrated {
                gain_e_per_unit: None,
            },
        );
        assert_eq!(&*cal, &[0.5 * ADU_SCALE, ADU_SCALE, 0.25 * ADU_SCALE]);
        let raw = to_working_units(
            &plane,
            &Domain::RawAdu {
                clip: 65535.0,
                gain_e_per_adu: None,
            },
        );
        assert!(matches!(raw, std::borrow::Cow::Borrowed(_)));
    }

    /// No seeds, nothing fitted: the frame aggregates are `None`, the β is the `Auto` fallback 4.0, and the noise and
    /// sky are still measured.
    #[test]
    fn no_seeds_fit_nothing_and_still_measure_the_noise_and_the_sky() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let m = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        assert!(m.stars.is_empty());
        assert_eq!(m.seeds, 0);
        assert_eq!(m.beta, 4.0);
        assert_eq!((m.frame.fwhm_px, m.frame.eccentricity), (None, None));
        assert_eq!((m.frame.stars_fitted, m.frame.tflux), (0, 0.0));
        assert_eq!(m.noise.source, NoiseSource::Mrs);
        assert!((m.noise.sigma / ADU_SCALE as f64 - 0.002).abs() < 0.1 * 0.002);
        assert!(
            (m.sky.median - 0.08).abs() < 0.001,
            "median {}",
            m.sky.median
        );
    }
}
