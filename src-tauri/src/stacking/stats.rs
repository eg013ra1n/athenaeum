//! Statistics and rejection algorithms
//!
//! This module provides statistical functions for image processing,
//! including pixel rejection algorithms for stacking.

use crate::stacking::fits::FitsImage;
use crate::stacking::ffi::ImStats;
use rayon::prelude::*;

// =============================================================================
// Basic Statistics
// =============================================================================

/// Calculate the mean of a slice of values
pub fn mean(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let sum: f64 = values.iter().map(|&v| v as f64).sum();
    (sum / values.len() as f64) as f32
}

/// Calculate the median of a slice of values
///
/// Note: This sorts a copy of the input to avoid modifying the original.
pub fn median(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }

    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

/// Calculate the median of a mutable slice (sorts in place)
pub fn median_inplace(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }

    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// Calculate the standard deviation given the mean
pub fn std_dev(values: &[f32], mean_val: f32) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }

    let variance: f64 = values.iter()
        .map(|&v| {
            let diff = (v - mean_val) as f64;
            diff * diff
        })
        .sum::<f64>() / (values.len() - 1) as f64;

    variance.sqrt() as f32
}

/// Calculate the Median Absolute Deviation (MAD)
///
/// MAD is a robust estimator of scale.
/// MAD = median(|x_i - median(x)|)
pub fn mad(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }

    let med = median(values);
    let deviations: Vec<f32> = values.iter()
        .map(|&v| (v - med).abs())
        .collect();

    median(&deviations)
}

/// Calculate the Biweight Midvariance (robust variance estimator)
pub fn biweight_midvariance(values: &[f32]) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }

    let med = median(values);
    let mad_val = mad(values);

    if mad_val < 1e-10 {
        return 0.0;
    }

    let c = 9.0; // tuning constant
    let scaled_mad = c * mad_val;

    let mut sum_num = 0.0_f64;
    let mut sum_den = 0.0_f64;

    for &v in values {
        let u = (v - med) / scaled_mad;
        let u2 = u * u;

        if u2 < 1.0 {
            let factor = 1.0 - u2 as f64;
            let factor2 = factor * factor;
            let factor4 = factor2 * factor2;

            sum_num += ((v - med) * (v - med)) as f64 * factor4;
            sum_den += factor2 * (1.0 - 5.0 * u2 as f64);
        }
    }

    let n = values.len() as f64;
    if sum_den.abs() < 1e-10 {
        return 0.0;
    }

    (n * sum_num / (sum_den * sum_den)).sqrt() as f32
}

// =============================================================================
// Rejection Algorithms
// =============================================================================

/// Sigma clipping rejection
///
/// Iteratively removes values outside [mean - low*sigma, mean + high*sigma]
/// until no more values are rejected or max iterations reached.
///
/// Returns (mean after clipping, number of rejected values)
pub fn sigma_clip(values: &mut Vec<f32>, low_sigma: f32, high_sigma: f32) -> (f32, usize) {
    let max_iterations = 10;
    let mut total_rejected = 0;

    for _ in 0..max_iterations {
        if values.len() < 3 {
            break;
        }

        let mean_val = mean(values);
        let sigma = std_dev(values, mean_val);

        if sigma < 1e-10 {
            break;
        }

        let low_bound = mean_val - low_sigma * sigma;
        let high_bound = mean_val + high_sigma * sigma;

        let initial_len = values.len();
        values.retain(|&v| v >= low_bound && v <= high_bound);
        let rejected = initial_len - values.len();

        if rejected == 0 {
            break;
        }
        total_rejected += rejected;
    }

    (mean(values), total_rejected)
}

/// Winsorized sigma clipping
///
/// Instead of rejecting outliers, replaces them with boundary values.
/// Returns (mean after winsorization, number of replaced values)
pub fn winsorized_sigma_clip(values: &mut Vec<f32>, sigma: f32) -> (f32, usize) {
    if values.len() < 3 {
        return (mean(values), 0);
    }

    let mean_val = mean(values);
    let std = std_dev(values, mean_val);

    if std < 1e-10 {
        return (mean_val, 0);
    }

    let low_bound = mean_val - sigma * std;
    let high_bound = mean_val + sigma * std;

    let mut replaced = 0;
    for v in values.iter_mut() {
        if *v < low_bound {
            *v = low_bound;
            replaced += 1;
        } else if *v > high_bound {
            *v = high_bound;
            replaced += 1;
        }
    }

    (mean(values), replaced)
}

/// MAD-based clipping
///
/// Uses Median Absolute Deviation for robust outlier rejection.
/// Returns (median after clipping, number of rejected values)
pub fn mad_clip(values: &mut Vec<f32>, threshold: f32) -> (f32, usize) {
    if values.len() < 3 {
        return (median(values), 0);
    }

    let med = median(values);
    let mad_val = mad(values);

    if mad_val < 1e-10 {
        return (med, 0);
    }

    // Scale MAD to approximate standard deviation (for normal distribution)
    let scaled_mad = 1.4826 * mad_val * threshold;

    let low_bound = med - scaled_mad;
    let high_bound = med + scaled_mad;

    let initial_len = values.len();
    values.retain(|&v| v >= low_bound && v <= high_bound);

    (median(values), initial_len - values.len())
}

/// Linear fit clipping (simplified version)
///
/// Fits a line to sorted values and rejects outliers.
/// This is a simplified implementation that just uses percentile-based rejection.
/// Returns (mean after clipping, number of rejected values)
pub fn linear_fit_clip(values: &mut Vec<f32>, sigma: f32) -> (f32, usize) {
    if values.len() < 5 {
        return (mean(values), 0);
    }

    // Sort values and compute expected linear trend
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = values.len();
    let mut residuals: Vec<f32> = Vec::with_capacity(n);

    // Compute deviation from linear fit (simplified: just deviation from median)
    let med = values[n / 2];

    for v in values.iter() {
        residuals.push((v - med).abs());
    }

    let residual_median = median(&residuals);
    let threshold = residual_median * sigma * 1.4826;

    let initial_len = values.len();
    let med = values[n / 2]; // Recalculate since we'll filter

    values.retain(|&v| (v - med).abs() <= threshold);

    (mean(values), initial_len - values.len())
}

/// Percentile clipping
///
/// Rejects the lowest and highest percentiles.
/// Returns (mean after clipping, number of rejected values)
pub fn percentile_clip(values: &mut Vec<f32>, low_percentile: f32, high_percentile: f32) -> (f32, usize) {
    if values.len() < 3 {
        return (mean(values), 0);
    }

    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = values.len();
    let low_idx = ((low_percentile / 100.0) * n as f32).floor() as usize;
    let high_idx = ((high_percentile / 100.0) * n as f32).ceil() as usize;

    let low_idx = low_idx.min(n - 1);
    let high_idx = high_idx.min(n);

    let retained: Vec<f32> = values[low_idx..high_idx].to_vec();
    let rejected = values.len() - retained.len();

    *values = retained;

    (mean(values), rejected)
}

// =============================================================================
// Image Statistics
// =============================================================================

/// Compute comprehensive statistics for an image
pub fn compute_image_stats(img: &FitsImage) -> ImStats {
    let data = &img.data;

    if data.is_empty() {
        return ImStats::default();
    }

    // Parallel min/max computation
    let (min_val, max_val) = data.par_iter()
        .fold(|| (f32::MAX, f32::MIN), |(min, max), &v| {
            (min.min(v), max.max(v))
        })
        .reduce(|| (f32::MAX, f32::MIN), |(min1, max1), (min2, max2)| {
            (min1.min(min2), max1.max(max2))
        });

    let mean_val = mean(data);
    let median_val = median(data);
    let sigma = std_dev(data, mean_val);
    let mad_val = mad(data);

    // Average deviation
    let avg_dev: f64 = data.iter()
        .map(|&v| (v - mean_val).abs() as f64)
        .sum::<f64>() / data.len() as f64;

    ImStats {
        total: data.len() as i64,
        ngoodpix: data.len() as i64,
        mean: mean_val as f64,
        median: median_val as f64,
        sigma: sigma as f64,
        avg_dev,
        mad: mad_val as f64,
        sqrtbwmv: biweight_midvariance(data) as f64,
        location: median_val as f64,
        scale: mad_val as f64 * 1.4826, // Scale to approximate sigma
        min: min_val as f64,
        max: max_val as f64,
        norm_value: mean_val as f64,
        bgnoise: 0.0, // Computed separately
    }
}

/// Compute background level and noise using robust estimators
///
/// Uses an iterative sigma-clipping approach to estimate background.
/// Returns (background level, noise estimate)
pub fn compute_background_noise(img: &FitsImage) -> (f32, f32) {
    let data = &img.data;

    if data.is_empty() {
        return (0.0, 0.0);
    }

    // Use MAD-based estimator for robust background
    let background = median(data);
    let mad_val = mad(data);

    // Estimate noise from pixels within 2 MAD of median (to exclude stars)
    let threshold = 2.0 * mad_val * 1.4826;
    let background_pixels: Vec<f32> = data.iter()
        .copied()
        .filter(|&v| (v - background).abs() < threshold)
        .collect();

    let noise = if background_pixels.len() > 10 {
        std_dev(&background_pixels, mean(&background_pixels))
    } else {
        mad_val * 1.4826
    };

    (background, noise)
}

/// Compute statistics for a single channel
pub fn compute_channel_stats(img: &FitsImage, channel: u32) -> ImStats {
    let data = img.channel_data(channel);

    if data.is_empty() {
        return ImStats::default();
    }

    let mean_val = mean(data);
    let median_val = median(data);
    let sigma = std_dev(data, mean_val);
    let mad_val = mad(data);

    let (min_val, max_val) = data.iter()
        .fold((f32::MAX, f32::MIN), |(min, max), &v| {
            (min.min(v), max.max(v))
        });

    let avg_dev: f64 = data.iter()
        .map(|&v| (v - mean_val).abs() as f64)
        .sum::<f64>() / data.len() as f64;

    ImStats {
        total: data.len() as i64,
        ngoodpix: data.len() as i64,
        mean: mean_val as f64,
        median: median_val as f64,
        sigma: sigma as f64,
        avg_dev,
        mad: mad_val as f64,
        sqrtbwmv: biweight_midvariance(data) as f64,
        location: median_val as f64,
        scale: mad_val as f64 * 1.4826,
        min: min_val as f64,
        max: max_val as f64,
        norm_value: mean_val as f64,
        bgnoise: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mean() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((mean(&values) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_median_odd() {
        let values = vec![1.0, 3.0, 2.0, 5.0, 4.0];
        assert!((median(&values) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_median_even() {
        let values = vec![1.0, 2.0, 3.0, 4.0];
        assert!((median(&values) - 2.5).abs() < 1e-6);
    }

    #[test]
    fn test_std_dev() {
        let values = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let m = mean(&values);
        let s = std_dev(&values, m);
        assert!((s - 2.138).abs() < 0.01);
    }

    #[test]
    fn test_mad() {
        let values = vec![1.0, 1.0, 2.0, 2.0, 4.0, 6.0, 9.0];
        let m = mad(&values);
        assert!((m - 1.0).abs() < 1e-6); // median = 2, deviations = [1,1,0,0,2,4,7], median(dev) = 1
    }

    #[test]
    fn test_sigma_clip() {
        let mut values: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 100.0]; // 100 is outlier
        let (clipped_mean, rejected) = sigma_clip(&mut values, 2.0, 2.0);
        assert_eq!(rejected, 1);
        assert!((clipped_mean - 3.0).abs() < 0.1);
    }

    #[test]
    fn test_mad_clip() {
        let mut values: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 100.0];
        let (clipped_median, rejected) = mad_clip(&mut values, 3.0);
        assert!(rejected >= 1);
        assert!(clipped_median < 10.0);
    }

    #[test]
    fn test_compute_background_noise() {
        let mut img = FitsImage::new(100, 100, 1, true).unwrap();
        // Fill with mostly background (0.1) with some noise
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = 0.1 + (i % 10) as f32 * 0.001;
        }

        let (bg, noise) = compute_background_noise(&img);
        assert!((bg - 0.1).abs() < 0.01);
        assert!(noise < 0.01);
    }
}
