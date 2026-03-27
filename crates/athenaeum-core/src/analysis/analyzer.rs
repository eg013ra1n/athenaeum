use anyhow::Result;
use astroimage::{ImageAnalyzer, ImageConverter, ImageMetadata, PixelData, AnalysisResult};
use std::sync::Arc;

use crate::models::{FrameAnalysis, StarMetric};
use super::config::AnalysisConfig;

/// Read raw pixel data from a FITS/XISF file.
/// Returns metadata (including flip_vertical) and pixel data for use with `analyze_frame_raw`.
pub fn read_raw(path: &str) -> Result<(ImageMetadata, PixelData)> {
    ImageConverter::read_raw(path).map_err(Into::into)
}

/// Build an `ImageAnalyzer` from an `AnalysisConfig`.
pub fn build_analyzer(
    config: &AnalysisConfig,
    thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> ImageAnalyzer {
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(config.detection_sigma as f32)
        .with_min_star_area(config.min_star_area as usize)
        .with_max_star_area(config.max_star_area as usize)
        .with_saturation_fraction(config.saturation_fraction as f32)
        .with_max_stars(config.max_stars as usize)
        .with_trail_threshold(config.trail_threshold as f32)
        .with_mrs_layers(config.mrs_layers as usize)
        .with_measure_cap(config.measure_cap as usize)
        .with_fit_max_iter(config.fit_max_iter as usize)
        .with_fit_tolerance(config.fit_tolerance)
        .with_fit_max_rejects(config.fit_max_rejects as usize);

    if let Some(pool) = thread_pool {
        analyzer = analyzer.with_thread_pool(pool);
    }
    analyzer
}

/// Analyze a single frame file (reads file internally).
/// Convenience wrapper around `read_raw` + `analyze_frame_raw`.
pub fn analyze_frame(
    path: &str,
    analyzer: &ImageAnalyzer,
    config_hash: &str,
) -> Result<(FrameAnalysis, Vec<StarMetric>, bool)> {
    let (meta, pixels) = read_raw(path)?;
    let flip_vertical = meta.flip_vertical;
    let (analysis, stars) = analyze_frame_raw(&meta, &pixels, analyzer, config_hash)?;
    Ok((analysis, stars, flip_vertical))
}

/// Analyze pre-loaded pixel data (no file I/O).
/// Use `read_raw()` to pre-load data, then pass it here for CPU-only analysis.
/// This enables overlapping I/O of the next frame with analysis of the current one.
pub fn analyze_frame_raw(
    meta: &ImageMetadata,
    pixels: &PixelData,
    analyzer: &ImageAnalyzer,
    config_hash: &str,
) -> Result<(FrameAnalysis, Vec<StarMetric>)> {
    let result = analyzer.analyze_raw(meta, pixels)?;
    Ok(map_result(result, config_hash))
}

/// Map rustafits AnalysisResult to Athenaeum models.
fn map_result(result: AnalysisResult, config_hash: &str) -> (FrameAnalysis, Vec<StarMetric>) {
    let stars: Vec<StarMetric> = result.stars.iter().map(|s| StarMetric {
        id: None,
        frame_analysis_id: 0,
        x: s.x as f64,
        y: s.y as f64,
        peak: s.peak as f64,
        flux: s.flux as f64,
        fwhm: s.fwhm as f64,
        fwhm_x: s.fwhm_x as f64,
        fwhm_y: s.fwhm_y as f64,
        eccentricity: s.eccentricity as f64,
        snr: s.snr as f64,
        hfr: s.hfr as f64,
        theta: s.theta as f64,
        beta: s.beta.map(|v| v as f64),
        fit_method: format!("{:?}", s.fit_method),
        fit_residual: s.fit_residual as f64,
    }).collect();

    let analysis = FrameAnalysis {
        id: None,
        frame_id: 0,
        file_id: 0,
        stars_detected: result.stars_detected as i64,
        median_fwhm: result.median_fwhm as f64,
        median_eccentricity: result.median_eccentricity as f64,
        median_snr: result.median_snr as f64,
        median_hfr: result.median_hfr as f64,
        frame_snr: result.frame_snr as f64,
        snr_weight: result.snr_weight as f64,
        psf_signal: result.psf_signal as f64,
        background: result.background as f64,
        noise: result.noise as f64,
        detection_threshold: result.detection_threshold as f64,
        width: result.width as i64,
        height: result.height as i64,
        source_channels: result.source_channels as i64,
        trail_r_squared: result.trail_r_squared as f64,
        possibly_trailed: result.possibly_trailed,
        median_beta: result.median_beta.map(|v| v as f64),
        quality_score: None,
        config_hash: Some(config_hash.to_string()),
        analyzed_at: chrono::Utc::now().to_rfc3339(),
    };

    (analysis, stars)
}

/// Compute relative quality scores for a batch of analyses.
/// Each metric is normalized to [0, 1] based on min/max within the set,
/// then combined with weighted sum.
pub fn compute_quality_scores(analyses: &mut [FrameAnalysis], config: &AnalysisConfig) {
    if analyses.len() < 2 {
        // With 0 or 1 frame, score is trivially 1.0
        for a in analyses.iter_mut() {
            a.quality_score = Some(1.0);
        }
        return;
    }

    let weights = config.scoring_weights.normalized();

    // Find min/max for each metric
    let mut fwhm_min = f64::MAX;
    let mut fwhm_max = f64::MIN;
    let mut ecc_min = f64::MAX;
    let mut ecc_max = f64::MIN;
    let mut snr_min = f64::MAX;
    let mut snr_max = f64::MIN;
    let mut stars_min = i64::MAX;
    let mut stars_max = i64::MIN;

    for a in analyses.iter() {
        fwhm_min = fwhm_min.min(a.median_fwhm);
        fwhm_max = fwhm_max.max(a.median_fwhm);
        ecc_min = ecc_min.min(a.median_eccentricity);
        ecc_max = ecc_max.max(a.median_eccentricity);
        snr_min = snr_min.min(a.snr_weight);
        snr_max = snr_max.max(a.snr_weight);
        stars_min = stars_min.min(a.stars_detected);
        stars_max = stars_max.max(a.stars_detected);
    }

    for a in analyses.iter_mut() {
        let norm_fwhm = normalize(a.median_fwhm, fwhm_min, fwhm_max);
        let norm_ecc = normalize(a.median_eccentricity, ecc_min, ecc_max);
        let norm_snr = normalize(a.snr_weight, snr_min, snr_max);
        let norm_stars = normalize(a.stars_detected as f64, stars_min as f64, stars_max as f64);

        // Lower FWHM/eccentricity = better → invert
        let score = weights.fwhm * (1.0 - norm_fwhm)
            + weights.eccentricity * (1.0 - norm_ecc)
            + weights.snr_weight * norm_snr
            + weights.star_count * norm_stars;

        a.quality_score = Some(score.clamp(0.0, 1.0));
    }
}

fn normalize(value: f64, min: f64, max: f64) -> f64 {
    if (max - min).abs() < f64::EPSILON {
        return 0.5; // All values are the same
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}
