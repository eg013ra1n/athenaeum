//! Frame quality measurement implementation
//!
//! This module provides pure Rust implementations for measuring image quality
//! metrics such as FWHM, roundness, and signal-to-noise ratio.

use crate::stacking::error::StackingResult;
use crate::stacking::fits::FitsImage;
use crate::stacking::stars::{find_stars, Star, StarFinderConfig, compute_median_fwhm, compute_average_roundness};
use crate::stacking::stats::{compute_background_noise, median};
use serde::{Deserialize, Serialize};

/// Quality metrics for a single frame
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityMetrics {
    /// Average FWHM in pixels
    pub fwhm: f64,
    /// FWHM in arcseconds (if plate scale known)
    pub fwhm_arcsec: Option<f64>,
    /// FWHM in X direction
    pub fwhm_x: f64,
    /// FWHM in Y direction
    pub fwhm_y: f64,
    /// Average roundness (1.0 = perfect circle)
    pub roundness: f64,
    /// Number of stars detected
    pub star_count: usize,
    /// Number of stars used for quality calculation (after filtering)
    pub stars_used: usize,
    /// Mean background level (normalized 0-1)
    pub background_level: f64,
    /// Background noise (standard deviation)
    pub background_noise: f64,
    /// Combined quality score (lower is better)
    pub quality_score: f64,
    /// Whether frame has saturated stars
    pub has_saturated_stars: bool,
    /// Average SNR of detected stars
    pub average_snr: f64,
}

impl QualityMetrics {
    /// Calculate quality score from metrics
    ///
    /// Score prioritizes:
    /// 1. Lower FWHM (sharper images)
    /// 2. More stars (better signal)
    /// 3. Lower noise
    /// 4. Better roundness (tracking)
    pub fn calculate_score(&self) -> f64 {
        // Base score is FWHM (lower is better)
        let fwhm_score = self.fwhm;

        // Penalty for few stars (less reliable measurement)
        let star_penalty = if self.stars_used > 0 {
            1.0 / (self.stars_used as f64).sqrt()
        } else {
            10.0 // Heavy penalty for no stars
        };

        // Penalty for high noise
        let noise_penalty = self.background_noise * 2.0;

        // Penalty for poor roundness (tracking issues)
        let roundness_penalty = (1.0 - self.roundness).abs() * 2.0;

        fwhm_score * (1.0 + star_penalty + noise_penalty + roundness_penalty)
    }
}

/// Configuration for quality measurement
#[derive(Debug, Clone)]
pub struct QualityConfig {
    /// Detection sigma threshold
    pub detection_sigma: f64,
    /// Maximum stars to analyze
    pub max_stars: usize,
    /// Minimum FWHM filter (pixels)
    pub min_fwhm: f64,
    /// Maximum FWHM filter (pixels)
    pub max_fwhm: f64,
    /// Minimum roundness for quality calculation
    pub min_roundness: f64,
    /// Minimum SNR for quality calculation
    pub min_snr: f64,
    /// Saturation threshold (0-1)
    pub saturation_threshold: f32,
}

impl Default for QualityConfig {
    fn default() -> Self {
        QualityConfig {
            detection_sigma: 3.0,
            max_stars: 500,
            min_fwhm: 1.0,
            max_fwhm: 20.0,
            min_roundness: 0.3,
            min_snr: 5.0,
            saturation_threshold: 0.98,
        }
    }
}

// =============================================================================
// Quality Measurement Functions
// =============================================================================

/// Measure quality metrics for a frame
pub fn measure_quality(img: &FitsImage, config: &QualityConfig) -> StackingResult<QualityMetrics> {
    // Configure star finder
    let star_config = StarFinderConfig {
        sigma_threshold: config.detection_sigma,
        max_stars: config.max_stars,
        min_fwhm: config.min_fwhm,
        max_fwhm: config.max_fwhm,
        min_roundness: config.min_roundness,
        min_snr: config.min_snr,
        search_radius: 3,
    };

    // Find stars
    let stars = find_stars(img, &star_config);

    // Compute background statistics
    let (background, noise) = compute_background_noise(img);

    // Check for saturation
    let has_saturated = img.data.iter().any(|&v| v > config.saturation_threshold);

    // If no stars found, return basic metrics
    if stars.is_empty() {
        return Ok(QualityMetrics {
            fwhm: config.max_fwhm,
            fwhm_arcsec: None,
            fwhm_x: config.max_fwhm,
            fwhm_y: config.max_fwhm,
            roundness: 0.0,
            star_count: 0,
            stars_used: 0,
            background_level: background as f64,
            background_noise: noise as f64,
            quality_score: f64::MAX,
            has_saturated_stars: has_saturated,
            average_snr: 0.0,
        });
    }

    // Compute aggregate metrics from stars
    let fwhm = compute_median_fwhm(&stars);
    let roundness = compute_average_roundness(&stars);

    let fwhm_x: f64 = stars.iter().map(|s| s.fwhm_x).sum::<f64>() / stars.len() as f64;
    let fwhm_y: f64 = stars.iter().map(|s| s.fwhm_y).sum::<f64>() / stars.len() as f64;
    let average_snr: f64 = stars.iter().map(|s| s.snr).sum::<f64>() / stars.len() as f64;

    // Convert FWHM to arcseconds if pixel scale is known
    let fwhm_arcsec = img.pixel_scale().map(|scale| fwhm * scale);

    let mut metrics = QualityMetrics {
        fwhm,
        fwhm_arcsec,
        fwhm_x,
        fwhm_y,
        roundness,
        star_count: stars.len(),
        stars_used: stars.len(),
        background_level: background as f64,
        background_noise: noise as f64,
        quality_score: 0.0,
        has_saturated_stars: has_saturated,
        average_snr,
    };

    metrics.quality_score = metrics.calculate_score();

    Ok(metrics)
}

/// Select the best reference frame from a set of frames
///
/// Returns the index of the best frame (lowest quality score).
pub fn select_best_reference(frames: &[&FitsImage]) -> usize {
    if frames.is_empty() {
        return 0;
    }

    let config = QualityConfig::default();

    let metrics: Vec<Option<QualityMetrics>> = frames.iter()
        .map(|frame| measure_quality(frame, &config).ok())
        .collect();

    // Find index of lowest quality score
    let mut best_idx = 0;
    let mut best_score = f64::MAX;

    for (i, m) in metrics.iter().enumerate() {
        if let Some(ref metrics) = m {
            if metrics.quality_score < best_score {
                best_score = metrics.quality_score;
                best_idx = i;
            }
        }
    }

    best_idx
}

/// Measure quality metrics for multiple frames
pub fn measure_quality_batch(
    frames: &[&FitsImage],
    config: &QualityConfig,
) -> Vec<StackingResult<QualityMetrics>> {
    frames.iter()
        .map(|frame| measure_quality(frame, config))
        .collect()
}

/// Compute relative quality weights for stacking
///
/// Higher quality frames get higher weights.
pub fn compute_quality_weights(metrics: &[&QualityMetrics]) -> Vec<f32> {
    if metrics.is_empty() {
        return vec![];
    }

    // Use inverse of quality score (lower score = better = higher weight)
    let scores: Vec<f64> = metrics.iter()
        .map(|m| m.quality_score.max(0.1)) // Avoid division by zero
        .collect();

    let inv_scores: Vec<f64> = scores.iter()
        .map(|s| 1.0 / s)
        .collect();

    // Normalize to sum to 1
    let sum: f64 = inv_scores.iter().sum();
    if sum > 1e-10 {
        inv_scores.iter().map(|&w| (w / sum) as f32).collect()
    } else {
        vec![1.0 / metrics.len() as f32; metrics.len()]
    }
}

/// Compute FWHM-based weights for stacking
pub fn compute_fwhm_weights(metrics: &[&QualityMetrics]) -> Vec<f32> {
    if metrics.is_empty() {
        return vec![];
    }

    // Use inverse FWHM^2 (better frames have lower FWHM)
    let inv_fwhm2: Vec<f64> = metrics.iter()
        .map(|m| {
            let fwhm = m.fwhm.max(0.5);
            1.0 / (fwhm * fwhm)
        })
        .collect();

    let sum: f64 = inv_fwhm2.iter().sum();
    if sum > 1e-10 {
        inv_fwhm2.iter().map(|&w| (w / sum) as f32).collect()
    } else {
        vec![1.0 / metrics.len() as f32; metrics.len()]
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_image_with_star() -> FitsImage {
        let mut img = FitsImage::new(100, 100, 1, true).unwrap();

        // Background
        let background = 0.1;
        img.data.iter_mut().for_each(|v| *v = background);

        // Add a Gaussian star
        let cx = 50.0;
        let cy = 50.0;
        let sigma = 2.5;
        let amplitude = 0.4;

        for y in 0..100 {
            for x in 0..100 {
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                let r2 = dx * dx + dy * dy;
                let value = amplitude * (-r2 / (2.0 * sigma * sigma)).exp();
                img.data[y * 100 + x] += value as f32;
            }
        }

        img
    }

    #[test]
    fn test_measure_quality() {
        let img = create_test_image_with_star();
        let config = QualityConfig::default();

        let metrics = measure_quality(&img, &config).unwrap();

        assert!(metrics.star_count >= 1);
        assert!(metrics.fwhm > 0.0);
        assert!(metrics.roundness > 0.0);
        assert!(metrics.quality_score > 0.0);
    }

    #[test]
    fn test_quality_score_calculation() {
        let metrics = QualityMetrics {
            fwhm: 3.0,
            fwhm_arcsec: Some(2.0),
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            roundness: 0.95,
            star_count: 100,
            stars_used: 80,
            background_level: 0.1,
            background_noise: 0.01,
            quality_score: 0.0,
            has_saturated_stars: false,
            average_snr: 50.0,
        };

        let score = metrics.calculate_score();
        assert!(score > 0.0);
        assert!(score < 20.0); // Reasonable range for a good frame
    }

    #[test]
    fn test_select_best_reference() {
        let img1 = create_test_image_with_star();
        let img2 = create_test_image_with_star();

        let frames: Vec<&FitsImage> = vec![&img1, &img2];
        let best = select_best_reference(&frames);

        assert!(best < 2);
    }

    #[test]
    fn test_quality_weights() {
        let m1 = QualityMetrics {
            fwhm: 3.0, fwhm_arcsec: None, fwhm_x: 3.0, fwhm_y: 3.0,
            roundness: 0.95, star_count: 100, stars_used: 80,
            background_level: 0.1, background_noise: 0.01,
            quality_score: 3.0, has_saturated_stars: false, average_snr: 50.0,
        };

        let m2 = QualityMetrics {
            fwhm: 5.0, fwhm_arcsec: None, fwhm_x: 5.0, fwhm_y: 5.0,
            roundness: 0.80, star_count: 50, stars_used: 40,
            background_level: 0.1, background_noise: 0.02,
            quality_score: 6.0, has_saturated_stars: false, average_snr: 30.0,
        };

        let metrics: Vec<&QualityMetrics> = vec![&m1, &m2];
        let weights = compute_quality_weights(&metrics);

        // Better quality (lower score) should have higher weight
        assert!(weights[0] > weights[1]);

        // Weights should sum to 1
        let sum: f32 = weights.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_fwhm_weights() {
        let m1 = QualityMetrics {
            fwhm: 2.0, fwhm_arcsec: None, fwhm_x: 2.0, fwhm_y: 2.0,
            roundness: 1.0, star_count: 100, stars_used: 100,
            background_level: 0.1, background_noise: 0.01,
            quality_score: 1.0, has_saturated_stars: false, average_snr: 50.0,
        };

        let m2 = QualityMetrics {
            fwhm: 4.0, fwhm_arcsec: None, fwhm_x: 4.0, fwhm_y: 4.0,
            roundness: 1.0, star_count: 100, stars_used: 100,
            background_level: 0.1, background_noise: 0.01,
            quality_score: 2.0, has_saturated_stars: false, average_snr: 50.0,
        };

        let metrics: Vec<&QualityMetrics> = vec![&m1, &m2];
        let weights = compute_fwhm_weights(&metrics);

        // Lower FWHM should have higher weight
        // FWHM 2 has 4x the weight of FWHM 4 (inverse square)
        assert!(weights[0] > weights[1] * 3.0);
    }
}
