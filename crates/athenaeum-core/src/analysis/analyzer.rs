//! The Analysis tab's measurement: a raw frame measured through the one star path (Plan C, Task C5).
//!
//! [`measure_raw_frame`] runs `star_measure` on the frame (mono, a Bayer mosaic's green sites, or a debayered file's G
//! plane); [`map_measurement`] writes the result into the `FrameAnalysis` / `StarMetric` rows. Positions stay in FILE
//! row order (unflipped); [`analyze_frame`] returns `flip_vertical` for the overlay.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use super::cfa;
use super::config::{AnalysisConfig, AnalysisPsfModel};
use super::input::{read_raw_frame, RawFrame, RawPlanes};
use super::trail::trail_r_squared;
use crate::models::{FrameAnalysis, FrameVerdict, StarMetric};
use crate::star_measure::{
    self, frame::finite_median, BetaSource, MeasureOptions, Mode, PixelNoise, PlaneMeasurement, PsfModel, SeedSpec,
    StarFit, MEASURE_VERSION,
};

/// Measure a raw frame (PC-4, PC-5): mono -> `measure_plane`; a Bayer mosaic -> [`cfa::measure_green_sites`]; a
/// debayered file -> `measure_plane` on its G plane (`colour`, mixed pixel noise, green share 0.5).
pub fn measure_raw_frame(
    frame: &RawFrame,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlaneMeasurement {
    measure_raw_frame_with(frame, config, pool, false)
}

/// [`measure_raw_frame`] with the module's per-seed diagnostics (`diagnostics`) for the probe's refusal lines; the
/// numbers are the same.
pub fn measure_raw_frame_with(
    frame: &RawFrame,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
) -> PlaneMeasurement {
    let psf_model = match config.psf_model {
        AnalysisPsfModel::Moffat4 => PsfModel::Moffat4,
        AnalysisPsfModel::Auto => PsfModel::Auto,
    };
    let base = MeasureOptions {
        domain: frame.domain,
        psf_model,
        seeds: SeedSpec::MatchedFilter {
            threshold: config.detection_threshold,
        },
        mode: Mode::Budget {
            budget: config.max_stars as usize,
        },
        diagnostics,
        ..Default::default()
    };
    match &frame.planes {
        RawPlanes::Mono(plane) => {
            let opts = MeasureOptions {
                pixel_noise: PixelNoise::White,
                ..base
            };
            star_measure::measure_plane(plane, frame.w, frame.h, &opts, pool)
        }
        RawPlanes::Cfa { mosaic, pattern } => {
            cfa::measure_green_sites(mosaic, frame.w, frame.h, *pattern, &base, pool)
        }
        RawPlanes::Rgb(planes) => {
            let opts = MeasureOptions {
                colour: true,
                pixel_noise: PixelNoise::Mixed,
                cfa_site_share: Some(0.5),
                ..base
            };
            star_measure::measure_plane(&planes[1], frame.w, frame.h, &opts, pool)
        }
    }
}

/// A non-finite number written as 0.0, so no column of a row is NaN.
fn finite_or_zero(x: f64) -> f64 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

/// `stars_detected` (R-PC-5 / R-PC-8), one rule for every layout: the matched filter's seeds not flagged `extended`.
/// `seeds` is the detection's seed count on the module's own sources and the handed count on the CFA path.
fn point_like_seeds(pm: &PlaneMeasurement) -> i64 {
    pm.seeds.saturating_sub(pm.extended_seeds) as i64
}

/// Write a measurement into the Analysis rows (spec 4.2 with PC-6..PC-9).
///
/// Medians come from the module's frame aggregates and read 0.0 when the frame has no fit in its statistic (PC-7);
/// `snr_weight`, `psf_signal` and `trail_r_squared` read the same fits. `median_beta` is the frame's fitted beta under
/// `Auto` and NULL otherwise (PC-9). A star's non-finite `snr` / `hfr` is `None` (PC-8). The row carries the
/// measurement version, the refusal count and the frame verdict (PC-13): trailed / defocused as the module judged, else
/// no_stars when no fit stands in the frame statistic, else none.
pub fn map_measurement(
    pm: &PlaneMeasurement,
    frame: &RawFrame,
    config: &AnalysisConfig,
    config_hash: &str,
) -> (FrameAnalysis, Vec<StarMetric>) {
    let stars: Vec<StarMetric> = pm
        .stars
        .iter()
        .map(|f| StarMetric {
            id: None,
            frame_analysis_id: 0,
            x: f.x,
            y: f.y,
            peak: f.amplitude,
            flux: f.signal,
            fwhm: f.fwhm(),
            fwhm_x: f.fwhm_x,
            fwhm_y: f.fwhm_y,
            eccentricity: f.eccentricity(),
            snr: f.snr.is_finite().then_some(f.snr),
            hfr: f.hfr.is_finite().then_some(f.hfr),
            theta: f.theta,
            beta: Some(f.beta),
            fit_method: "moffat".to_string(),
            fit_residual: f.residual,
        })
        .collect();

    let stat: Vec<&StarFit> = pm.stars.iter().filter(|f| f.flags.in_statistic()).collect();
    let background = finite_or_zero(pm.sky.median);
    let noise = finite_or_zero(pm.noise.sigma);
    let frame_snr = if noise > 0.0 { background / noise } else { 0.0 };
    let (snr_weight, psf_signal) = if noise > 0.0 && !stat.is_empty() {
        let flux = finite_median(stat.iter().map(|f| f.signal)).unwrap_or(0.0);
        let amp = finite_median(stat.iter().map(|f| f.amplitude)).unwrap_or(0.0);
        let w = if background > 0.0 {
            flux * flux / (noise * noise * background)
        } else {
            0.0
        };
        (finite_or_zero(w), finite_or_zero(amp / noise))
    } else {
        (0.0, 0.0)
    };
    let median_fwhm = pm.frame.fwhm_px.unwrap_or(0.0);
    let thetas: Vec<f64> = stat.iter().map(|f| f.theta).collect();
    let median_beta = match (config.psf_model, pm.beta_source) {
        (AnalysisPsfModel::Auto, BetaSource::Fitted { .. }) => Some(pm.beta),
        _ => None,
    };

    let frame_verdict = if pm.trailed {
        Some(FrameVerdict::Trailed)
    } else if pm.defocused {
        Some(FrameVerdict::Defocused)
    } else if stat.is_empty() {
        Some(FrameVerdict::NoStars)
    } else {
        None
    };

    let analysis = FrameAnalysis {
        id: None,
        frame_id: 0,
        file_id: 0,
        stars_detected: point_like_seeds(pm),
        median_fwhm: finite_or_zero(median_fwhm),
        median_eccentricity: finite_or_zero(pm.frame.eccentricity.unwrap_or(0.0)),
        median_snr: finite_or_zero(pm.frame.snr_median.unwrap_or(0.0)),
        median_hfr: finite_or_zero(pm.frame.hfr_median.unwrap_or(0.0)),
        frame_snr,
        snr_weight,
        psf_signal,
        background,
        noise,
        detection_threshold: config.detection_threshold,
        width: frame.w as i64,
        height: frame.h as i64,
        source_channels: frame.channels as i64,
        trail_r_squared: finite_or_zero(trail_r_squared(&thetas, median_fwhm)),
        possibly_trailed: pm.trailed,
        median_beta,
        quality_score: None,
        config_hash: Some(config_hash.to_string()),
        analyzed_at: chrono::Utc::now().to_rfc3339(),
        measure_version: Some(MEASURE_VERSION),
        stars_refused: Some(pm.refused.total() as i64),
        frame_verdict,
    };
    (analysis, stars)
}

/// Analyze an already-read frame (CPU only, so the batch keeps its read overlap): measure, then map.
pub fn analyze_raw_frame(
    frame: &RawFrame,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    config_hash: &str,
) -> (FrameAnalysis, Vec<StarMetric>) {
    let pm = measure_raw_frame(frame, config, pool);
    map_measurement(&pm, frame, config, config_hash)
}

/// Analyze a single frame file (reads it with [`read_raw_frame`]). Returns the frame's `flip_vertical` for the overlay;
/// the star positions are in file row order.
pub fn analyze_frame(
    path: &str,
    config: &AnalysisConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    config_hash: &str,
) -> Result<(FrameAnalysis, Vec<StarMetric>, bool)> {
    let mut frame = read_raw_frame(Path::new(path))?;
    frame.drop_unmeasured_planes();
    let (analysis, stars) = analyze_raw_frame(&frame, config, pool, config_hash);
    Ok((analysis, stars, frame.flip_vertical))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::Domain;
    use crate::test_support::{add_noise, moffat_field, MoffatStar};
    use astroimage::BayerPattern;

    const RAW: Domain = Domain::RawAdu {
        clip: 65535.0,
        full_scale: 65535.0,
        gain_e_per_adu: None,
    };

    fn star(x: f64, y: f64, amp: f64, fwhm: f64) -> MoffatStar {
        // Moffat beta 4: FWHM = 2 alpha sqrt(2^(1/4) - 1).
        let alpha = fwhm / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
        MoffatStar {
            x,
            y,
            amp,
            alpha_x: alpha,
            alpha_y: alpha,
            theta: 0.0,
        }
    }

    /// `n` stars on a jittered grid of a `w` x `h` plane, amplitudes 500..2000.
    fn grid_stars(n: usize, w: usize, h: usize, fwhm: f64, seed: u64) -> Vec<MoffatStar> {
        let mut rng = crate::geometry::ransac::SplitMix64(seed);
        let cols = ((n as f64 * w as f64 / h as f64).sqrt().ceil() as usize).max(1);
        let rows = n.div_ceil(cols);
        let (cw, ch) = (w as f64 / cols as f64, h as f64 / rows as f64);
        (0..n)
            .map(|i| {
                let (c, r) = ((i % cols) as f64, (i / cols) as f64);
                let x = c * cw + 12.0 + rng.next_f64() * (cw - 24.0);
                let y = r * ch + 12.0 + rng.next_f64() * (ch - 24.0);
                star(x, y, 500.0 + rng.next_f64() * 1500.0, fwhm)
            })
            .collect()
    }

    fn mono_frame(n: usize, w: usize, h: usize, sky: f32, seed: u64) -> RawFrame {
        let stars = grid_stars(n, w, h, 3.5, seed);
        let mut p = moffat_field(w, h, &stars, 4.0, sky);
        add_noise(&mut p, 5.0, seed + 1);
        RawFrame {
            planes: RawPlanes::Mono(p),
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        }
    }

    fn cfg() -> AnalysisConfig {
        AnalysisConfig::default()
    }

    fn all_finite(a: &FrameAnalysis) {
        for (name, v) in [
            ("median_fwhm", a.median_fwhm),
            ("median_eccentricity", a.median_eccentricity),
            ("median_snr", a.median_snr),
            ("median_hfr", a.median_hfr),
            ("frame_snr", a.frame_snr),
            ("snr_weight", a.snr_weight),
            ("psf_signal", a.psf_signal),
            ("background", a.background),
            ("noise", a.noise),
            ("detection_threshold", a.detection_threshold),
            ("trail_r_squared", a.trail_r_squared),
        ] {
            assert!(v.is_finite(), "{name} = {v}");
        }
    }

    #[test]
    fn analysis_of_a_synthetic_mono_frame_maps_every_column() {
        let frame = mono_frame(300, 1024, 768, 1000.0, 7);
        let (a, stars) = analyze_raw_frame(&frame, &cfg(), None, "h");
        all_finite(&a);
        assert!(
            (a.median_fwhm - 3.5).abs() < 0.07,
            "median_fwhm {}",
            a.median_fwhm
        );
        assert!(a.median_eccentricity < 0.1, "ecc {}", a.median_eccentricity);
        assert!(a.stars_detected >= 285, "detected {}", a.stars_detected);
        assert!(!stars.is_empty());
        assert!(stars.iter().all(|s| s.fit_method == "moffat"));
        assert_eq!(a.frame_snr, a.background / a.noise);
        assert!((a.background - 1000.0).abs() < 10.0, "bg {}", a.background);
        assert!(a.snr_weight > 0.0 && a.psf_signal > 0.0);
        assert_eq!((a.width, a.height, a.source_channels), (1024, 768, 1));
        assert_eq!(a.detection_threshold, 6.0);
        assert_eq!(a.config_hash.as_deref(), Some("h"));
        assert!(a.median_beta.is_none());
        assert!(!a.possibly_trailed);
        for s in &stars {
            assert!(s.fwhm_x >= s.fwhm_y && s.eccentricity >= 0.0 && s.eccentricity < 1.0);
            assert!(s.x.is_finite() && s.y.is_finite() && s.peak > 0.0 && s.flux > 0.0);
        }
    }

    /// R-PC-5 / R-PC-8: `stars_detected` counts the matched filter's point-like seeds. A 3F Gaussian knot is a seed the
    /// module flags `extended`: it is fitted but is not counted, in every layout (mono, a mosaic's green sites, a
    /// debayered file's G plane), so the count with the knot equals the count without it.
    #[test]
    fn stars_detected_leaves_knots_out_in_every_layout() {
        use crate::test_support::gaussian_field;
        let (w, h) = (320, 240);
        let stars = grid_stars(40, w, h, 3.5, 21);
        let knot = (160.3, 120.8);
        let g = gaussian_field(w, h, &[(knot.0, knot.1, 400.0)], 3.0 * 3.5 / 2.354_820_045, 0.0);
        let plane = |with_knot: bool, seed: u64| {
            let mut p = moffat_field(w, h, &stars, 4.0, 1000.0);
            if with_knot {
                for (d, b) in p.iter_mut().zip(&g) {
                    *d += b;
                }
            }
            add_noise(&mut p, 6.0, seed);
            p
        };
        let frame = |planes: RawPlanes, channels| RawFrame {
            planes,
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels,
        };
        let make = |name: &str, k: bool| match name {
            "mono" => frame(RawPlanes::Mono(plane(k, 5)), 1),
            "cfa" => frame(
                RawPlanes::Cfa {
                    mosaic: plane(k, 5),
                    pattern: BayerPattern::Rggb,
                },
                1,
            ),
            _ => frame(
                RawPlanes::Rgb([plane(false, 3), plane(k, 5), plane(false, 4)]),
                3,
            ),
        };
        for name in ["mono", "cfa", "rgb"] {
            let (plain, knotted) = (make(name, false), make(name, true));
            let pm = measure_raw_frame(&knotted, &cfg(), None);
            assert!(pm.extended_seeds >= 1, "{name}: the knot is flagged extended");
            if name == "cfa" {
                assert!(pm.detected.is_empty(), "{name}: handed seeds leave `detected` empty");
            } else {
                assert_eq!(pm.detected.len(), pm.seeds, "{name}");
            }
            let (a, _) = analyze_raw_frame(&plain, &cfg(), None, "h");
            let (b, _) = analyze_raw_frame(&knotted, &cfg(), None, "h");
            assert_eq!(b.stars_detected, (pm.seeds - pm.extended_seeds) as i64, "{name}");
            assert_eq!(b.stars_detected, a.stars_detected, "{name}: the knot is not counted");
            assert!(b.stars_detected >= 30, "{name}: {}", b.stars_detected);
        }
    }

    #[test]
    fn a_frame_without_fits_writes_zero_medians() {
        let (w, h) = (400, 300);
        let mut noise = vec![1000.0f32; w * h];
        add_noise(&mut noise, 10.0, 3);
        let donut: Vec<f32> = crate::test_support::donut_field(400, 200, 5)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        for (planes, w, h) in [(noise, w, h), (donut, 400, 200)] {
            let frame = RawFrame {
                planes: RawPlanes::Mono(planes),
                w,
                h,
                domain: RAW,
                flip_vertical: false,
                channels: 1,
            };
            let (a, stars) = analyze_raw_frame(&frame, &cfg(), None, "h");
            all_finite(&a);
            assert!(stars.is_empty());
            assert_eq!(
                (a.median_fwhm, a.median_eccentricity, a.median_snr, a.median_hfr),
                (0.0, 0.0, 0.0, 0.0)
            );
        }
    }

    #[test]
    fn a_trailed_frame_is_flagged_with_no_measured_star() {
        let p: Vec<f32> = crate::test_support::trailed_field(600, 450, 9)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        let frame = RawFrame {
            planes: RawPlanes::Mono(p),
            w: 600,
            h: 450,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        };
        let (a, stars) = analyze_raw_frame(&frame, &cfg(), None, "h");
        all_finite(&a);
        assert!(a.possibly_trailed);
        assert!(stars.is_empty());
        assert_eq!(
            (a.median_fwhm, a.median_eccentricity, a.median_snr, a.median_hfr),
            (0.0, 0.0, 0.0, 0.0)
        );
        assert_eq!(a.trail_r_squared, 0.0);
    }

    fn mono(p: Vec<f32>, w: usize, h: usize) -> RawFrame {
        RawFrame {
            planes: RawPlanes::Mono(p),
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        }
    }

    #[test]
    fn the_analyzer_fills_measure_version_and_stars_refused() {
        let scaled = |p: Vec<f32>| p.iter().map(|v| v * 65535.0).collect::<Vec<f32>>();
        let noise = {
            let mut n = vec![1000.0f32; 400 * 300];
            add_noise(&mut n, 10.0, 3);
            mono(n, 400, 300)
        };
        let trailed = mono(scaled(crate::test_support::trailed_field(600, 450, 9)), 600, 450);
        let clean = mono_frame(120, 640, 480, 1000.0, 11);
        for frame in [&noise, &trailed, &clean] {
            let pm = measure_raw_frame(frame, &cfg(), None);
            let (a, _) = map_measurement(&pm, frame, &cfg(), "h");
            assert_eq!(a.measure_version, Some(MEASURE_VERSION));
            // The sum of the refusal reasons; the budget's not-selected / trimmed are not in it.
            assert_eq!(a.stars_refused, Some(pm.refused.by_reason.values().sum::<usize>() as i64));
        }
        let pm = measure_raw_frame(&trailed, &cfg(), None);
        let (a, _) = map_measurement(&pm, &trailed, &cfg(), "h");
        assert!(a.stars_refused.unwrap() > 0, "a trailed frame's fits are refused: {:?}", a.stars_refused);
    }

    #[test]
    fn a_non_finite_star_snr_or_hfr_maps_to_none() {
        let frame = mono_frame(60, 512, 384, 1000.0, 13);
        let mut pm = measure_raw_frame(&frame, &cfg(), None);
        assert!(pm.stars.len() >= 3);
        pm.stars[0].snr = f64::NAN;
        pm.stars[1].hfr = f64::INFINITY;
        let (_, stars) = map_measurement(&pm, &frame, &cfg(), "h");
        assert_eq!((stars[0].snr, stars[0].hfr.is_some()), (None, true));
        assert_eq!((stars[1].hfr, stars[1].snr.is_some()), (None, true));
        assert!(stars[2].snr.is_some() && stars[2].hfr.is_some());
    }

    #[test]
    fn the_analyzer_records_the_frame_verdict() {
        let (w, h) = (400, 300);
        let mut n = vec![1000.0f32; w * h];
        add_noise(&mut n, 10.0, 3);
        let donut: Vec<f32> = crate::test_support::donut_field(400, 200, 5)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        let trailed: Vec<f32> = crate::test_support::trailed_field(600, 450, 9)
            .iter()
            .map(|v| v * 65535.0)
            .collect();
        let verdict = |f: RawFrame| analyze_raw_frame(&f, &cfg(), None, "h").0;
        let t = verdict(mono(trailed, 600, 450));
        assert_eq!((t.frame_verdict, t.possibly_trailed), (Some(FrameVerdict::Trailed), true));
        assert_eq!(verdict(mono(donut, 400, 200)).frame_verdict, Some(FrameVerdict::Defocused));
        assert_eq!(verdict(mono(n, w, h)).frame_verdict, Some(FrameVerdict::NoStars));
        assert_eq!(verdict(mono_frame(120, 640, 480, 1000.0, 11)).frame_verdict, None);
    }

    #[test]
    fn moffat4_writes_no_median_beta_and_auto_writes_the_fitted_one() {
        let frame = mono_frame(120, 640, 480, 1000.0, 11);
        let (m4, _) = analyze_raw_frame(&frame, &cfg(), None, "h");
        assert_eq!(m4.median_beta, None);
        let auto = AnalysisConfig {
            psf_model: AnalysisPsfModel::Auto,
            ..cfg()
        };
        let (a, _) = analyze_raw_frame(&frame, &auto, None, "h");
        let b = a.median_beta.expect("Auto writes the fitted beta");
        assert!(b.is_finite() && (b - 4.0).abs() < 1.0, "beta {b}");
    }

    fn site(p: BayerPattern, x: usize, y: usize) -> char {
        let (ex, ey) = (x % 2 == 0, y % 2 == 0);
        match (p, ex, ey) {
            (BayerPattern::Rggb, true, true) => 'R',
            (BayerPattern::Rggb, false, false) => 'B',
            _ => 'G',
        }
    }

    #[test]
    fn an_osc_mosaic_is_measured_on_green_and_a_three_plane_file_on_g() {
        // The mosaic: scene stars (flux 1.0 on G) x site gains + sky per site + noise.
        let (w, h) = (256, 256);
        let truth = (100.3, 80.7);
        let mut stars = vec![star(truth.0, truth.1, 3000.0, 4.0)];
        let mut rng = crate::geometry::ransac::SplitMix64(11);
        while stars.len() < 41 {
            let x = 16.0 + rng.next_f64() * (w as f64 - 32.0);
            let y = 16.0 + rng.next_f64() * (h as f64 - 32.0);
            if stars.iter().any(|s| (s.x - x).hypot(s.y - y) < 20.0) {
                continue;
            }
            stars.push(star(x, y, 250.0 + rng.next_f64() * 500.0, 3.6 + rng.next_f64() * 0.8));
        }
        let scene = moffat_field(w, h, &stars, 4.0, 0.0);
        let mut m: Vec<f32> = (0..w * h)
            .map(|i| {
                let (gain, sky) = match site(BayerPattern::Rggb, i % w, i / w) {
                    'R' => (0.5, 900.0),
                    'B' => (0.4, 800.0),
                    _ => (1.0, 1000.0),
                };
                scene[i] * gain + sky
            })
            .collect();
        add_noise(&mut m, 6.0, 5);
        let frame = RawFrame {
            planes: RawPlanes::Cfa {
                mosaic: m,
                pattern: BayerPattern::Rggb,
            },
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 1,
        };
        let (a, ms) = analyze_raw_frame(&frame, &cfg(), None, "h");
        all_finite(&a);
        let best = ms
            .iter()
            .min_by(|p, q| {
                (p.x - truth.0)
                    .hypot(p.y - truth.1)
                    .total_cmp(&(q.x - truth.0).hypot(q.y - truth.1))
            })
            .expect("a measured star");
        let d = (best.x - truth.0).hypot(best.y - truth.1);
        assert!(d < 0.1, "bright star {d} px off");
        assert_eq!(a.source_channels, 1);
        assert!(a.stars_detected >= ms.len() as i64 && a.stars_detected >= 35);

        // Three planes: 40 common stars, one only in G, one only in R.
        let (w, h) = (320, 240);
        let common = grid_stars(40, w, h, 3.5, 21);
        // Two free spots, 25 px from every common star and from each other.
        let mut free: Vec<(f64, f64)> = Vec::new();
        let mut rng = crate::geometry::ransac::SplitMix64(77);
        while free.len() < 2 {
            let (x, y) = (20.0 + rng.next_f64() * 280.0, 20.0 + rng.next_f64() * 200.0);
            if common
                .iter()
                .map(|s| (s.x, s.y))
                .chain(free.iter().copied())
                .all(|(a, b)| (a - x).hypot(b - y) > 25.0)
            {
                free.push((x, y));
            }
        }
        let g_only = star(free[0].0, free[0].1, 1500.0, 3.5);
        let r_only = star(free[1].0, free[1].1, 1500.0, 3.5);
        let plane = |extra: Option<MoffatStar>, seed| {
            let mut s = common.clone();
            s.extend(extra);
            let mut p = moffat_field(w, h, &s, 4.0, 1000.0);
            add_noise(&mut p, 10.0, seed);
            p
        };
        let frame = RawFrame {
            planes: RawPlanes::Rgb([plane(Some(r_only), 1), plane(Some(g_only), 2), plane(None, 3)]),
            w,
            h,
            domain: RAW,
            flip_vertical: false,
            channels: 3,
        };
        let (a, ms) = analyze_raw_frame(&frame, &cfg(), None, "h");
        assert_eq!(a.source_channels, 3);
        let near = |s: &MoffatStar| ms.iter().any(|m| (m.x - s.x).hypot(m.y - s.y) < 0.5);
        assert!(near(&g_only), "the G star is measured");
        assert!(!near(&r_only), "the R star is absent");
    }
}
