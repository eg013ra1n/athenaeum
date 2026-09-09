//! Per-frame, per-channel measurement of calibrated frames (spec §4.1):
//! detection, PSF fits, the PSF-signal totals, the large-scale background
//! residual, MRS noise, the normalization statistics and the classic SNR
//! weight — everything the weighting, selection and normalization stages
//! consume. Reads through `PlaneReader`, one plane at a time.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use astroimage::ImageAnalyzer;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::psf_signal::{self, FitParams, PsfModel, Seed};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::{self, LocationScale, ScaleEstimator, CLIP_HI, CLIP_LO};
use crate::integration::IntegrationError;

/// Calibrated frames are float32 in `[0, 1]`; the detector and the MRS
/// estimator carry absolute floors tuned for 16-bit ADU data, so they see a
/// copy scaled by this factor. Every estimator this module reports is
/// scale-invariant (PSF Signal Weight, PSF SNR) or divided back into
/// native units.
pub const ADU_SCALE: f32 = 65535.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeasureOptions {
    pub psf_model: PsfModel,
    /// Detection cap (spec §9.2 `maxStars`).
    pub max_stars: usize,
    pub scale_estimator: ScaleEstimator,
    /// The sensitivity dial — the detector itself is threshold-free.
    pub min_snr: f32,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        MeasureOptions {
            psf_model: PsfModel::Auto,
            max_stars: 24576,
            scale_estimator: ScaleEstimator::Bwmv,
            min_snr: 5.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NoiseSource {
    #[default]
    Mrs,
    /// MRS was unavailable; `noise` is `N*`.
    BackgroundResidual,
}

/// One channel's measurement, in native `[0, 1]` units.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ChannelMeasurement {
    pub stars_detected: usize,
    pub stars_fitted: usize,
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

/// Measure one plane. Detection, fitting, the background model and MRS run
/// on an ADU-scaled copy; the sample statistics run on the native data.
pub fn measure_plane(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> ChannelMeasurement {
    let scaled: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();

    let mut analyzer = ImageAnalyzer::new()
        .with_max_stars(opts.max_stars.max(8))
        .with_centroid_refine(false);
    if let Some(p) = pool {
        analyzer = analyzer.with_thread_pool(Arc::clone(p));
    }
    let seeds: Vec<Seed> = match analyzer.detect_fast_data(&scaled, w, h, 1) {
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
    };
    let stars_detected = seeds.len();

    let params = FitParams::default();
    let outcome = match pool {
        Some(p) => {
            p.install(|| psf_signal::fit_stars(&scaled, w, h, &seeds, opts.psf_model, &params))
        }
        None => psf_signal::fit_stars(&scaled, w, h, &seeds, opts.psf_model, &params),
    };
    let totals = psf_signal::signal_totals(&outcome.fits);
    let (fwhm_px, eccentricity) = psf_signal::frame_shape(&outcome.fits).unwrap_or((0.0, 0.0));
    let bg = match pool {
        Some(p) => p.install(|| psf_signal::background_residual(&scaled, w, h)),
        None => psf_signal::background_residual(&scaled, w, h),
    };
    let (m_star, n_star) = match bg {
        Some(v) => v,
        None => {
            warn!(
                width = w,
                height = h,
                "large-scale background model unavailable; M* and N* are zero"
            );
            (0.0, 0.0)
        }
    };
    let (noise_adu, noise_source) = match psf_signal::noise_mrs(&scaled, w, h) {
        Some(n) => (n as f64, NoiseSource::Mrs),
        None => {
            warn!(
                n_star,
                "MRS noise unavailable; using the background residual scale"
            );
            (n_star, NoiseSource::BackgroundResidual)
        }
    };

    let sample = stats::stratified_sample(data, w, h);
    let clipped = stats::clip_sample(&sample, CLIP_LO, CLIP_HI);
    let (median, mad, median_mean_dev) = if clipped.is_empty() {
        warn!(
            samples = sample.len(),
            "no in-range samples after clipping; the plane may not be in [0, 1]"
        );
        (0.0, 0.0, 0.0)
    } else {
        let m = stats::median_of(&clipped);
        (
            m as f64,
            stats::mad_about(&clipped, m) as f64,
            stats::avg_dev_about(&clipped, m) as f64,
        )
    };
    let ls = stats::location_scale(&clipped, opts.scale_estimator).unwrap_or(LocationScale {
        location: median as f32,
        scale: 0.0,
    });
    let (noise_scale_low, noise_scale_high) = stats::noise_scale_factors(&sample)
        .map(|(a, b)| (a as f64, b as f64))
        .unwrap_or((0.0, 0.0));

    let s = ADU_SCALE as f64;
    let noise = noise_adu / s;
    let snr_weight = if noise > 0.0 {
        (median_mean_dev / noise).powi(2)
    } else {
        0.0
    };
    ChannelMeasurement {
        stars_detected,
        stars_fitted: outcome.fits.len(),
        beta: outcome.beta,
        fwhm_px,
        eccentricity,
        tflux: totals.tflux / s,
        tmean_flux: totals.tmean_flux / s,
        mean_flux_rejected: totals.rejected,
        m_star: m_star / s,
        n_star: n_star / s,
        noise,
        noise_source,
        median,
        mad,
        median_mean_dev,
        snr_weight,
        noise_scale_low,
        noise_scale_high,
        location: ls.location as f64,
        scale: ls.scale as f64,
        // Both estimators are scale-invariant, so the ADU-unit inputs give
        // the native-unit answer.
        psf_signal_weight: psf_signal::psf_signal_weight(
            totals.tflux,
            totals.tmean_flux,
            noise_adu,
            m_star,
        ),
        psf_snr: psf_signal::psf_snr(totals.tflux, noise_adu),
    }
}

/// Measure every plane of a calibrated frame; checks `cancel` before each
/// plane.
pub fn measure_frame(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FrameMeasurement, IntegrationError> {
    let start = Instant::now();
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    let mut channels = Vec::with_capacity(reader.channels());
    for plane in 0..reader.channels() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let data = reader.read_plane(plane)?;
        let t = Instant::now();
        let _span = tracing::debug_span!("measure_plane", path = %path.display(), plane).entered();
        let m = measure_plane(&data, w, h, opts, pool);
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
            duration_ms = t.elapsed().as_millis() as u64,
            "frame plane measured"
        );
        channels.push(m);
    }
    Ok(FrameMeasurement {
        width: w,
        height: h,
        channels,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::ransac::SplitMix64;
    use crate::test_support::{add_noise, gaussian_field};
    use std::path::PathBuf;

    /// 150 Gaussian stars (σ 1.8 px) on a jittered 15×10 grid, amplitudes
    /// `(0.1..0.3)·amp_scale`, background 0.08, Gaussian noise `noise`.
    fn field(seed: u64, amp_scale: f64, noise: f32) -> (Vec<f32>, usize, usize) {
        let (w, h) = (768, 512);
        let mut rng = SplitMix64(seed);
        let mut stars = Vec::new();
        for j in 0..10 {
            for i in 0..15 {
                let x = 40.0 + i as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
                let y = 40.0 + j as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
                stars.push((x, y, (0.1 + 0.2 * rng.next_f64()) * amp_scale));
            }
        }
        let mut data = gaussian_field(w, h, &stars, 1.8, 0.08);
        add_noise(&mut data, noise, seed + 100);
        (data, w, h)
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

    #[test]
    fn options_serde_defaults() {
        let d = MeasureOptions::default();
        assert_eq!(d.max_stars, 24576);
        assert_eq!(d.psf_model, PsfModel::Auto);
        assert_eq!(d.scale_estimator, ScaleEstimator::Bwmv);
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"psfModel\":\"auto\"") && json.contains("\"maxStars\":24576"));
        assert!(json.contains("\"scaleEstimator\":\"bwmv\"") && json.contains("\"minSnr\":5.0"));
        assert!(!json.contains("\"detectionSigma\""));
        assert_eq!(
            serde_json::from_str::<MeasureOptions>("{}").unwrap(),
            MeasureOptions::default()
        );
        let c: ChannelMeasurement = serde_json::from_str("{\"starsFitted\":3}").unwrap();
        assert_eq!(c.stars_fitted, 3);
        assert_eq!(c.noise_source, NoiseSource::Mrs);
    }
}
