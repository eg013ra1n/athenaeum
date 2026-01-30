//! Core stacking algorithms
//!
//! This module provides pure Rust implementations of image stacking algorithms.
//! All functions take references to FitsImage arrays and produce a stacked result.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::fits::FitsImage;
use crate::stacking::stats::{mean, median_inplace, sigma_clip, winsorized_sigma_clip, mad_clip};
use crate::stacking::ffi::DataType;
use rayon::prelude::*;

// =============================================================================
// Basic Stacking Methods
// =============================================================================

/// Stack images using median combination
///
/// Takes the median value at each pixel position across all images.
/// This is the most robust method against outliers (hot pixels, cosmic rays, satellites).
pub fn stack_median(images: &[&FitsImage]) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();
    let total_pixels = (width as usize) * (height as usize) * (channels as usize);

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        let mut values: Vec<f32> = images.iter()
            .map(|img| img.data[idx])
            .collect();
        *pixel = median_inplace(&mut values);
    });

    // Copy metadata from first image
    copy_metadata(&mut result, images[0]);

    Ok(result)
}

/// Stack images using mean (average) combination
///
/// Takes the arithmetic mean at each pixel position.
/// Fast but sensitive to outliers.
pub fn stack_mean(images: &[&FitsImage]) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();
    let n = images.len() as f32;

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        let sum: f32 = images.iter().map(|img| img.data[idx]).sum();
        *pixel = sum / n;
    });

    copy_metadata(&mut result, images[0]);

    Ok(result)
}

/// Stack images using sum (addition) combination
///
/// Adds all pixel values. Useful for creating integration maps.
pub fn stack_sum(images: &[&FitsImage]) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        *pixel = images.iter().map(|img| img.data[idx]).sum();
    });

    copy_metadata(&mut result, images[0]);

    // Sum total exposure time
    result.exposure = images.iter()
        .filter_map(|img| img.exposure)
        .sum::<f64>()
        .into();

    Ok(result)
}

/// Stack images using minimum combination
///
/// Takes the minimum value at each pixel position.
/// Useful for creating dark frames or finding consistent features.
pub fn stack_min(images: &[&FitsImage]) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        *pixel = images.iter()
            .map(|img| img.data[idx])
            .fold(f32::MAX, |a, b| a.min(b));
    });

    copy_metadata(&mut result, images[0]);

    Ok(result)
}

/// Stack images using maximum combination
///
/// Takes the maximum value at each pixel position.
/// Useful for creating star trails or detecting transients.
pub fn stack_max(images: &[&FitsImage]) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        *pixel = images.iter()
            .map(|img| img.data[idx])
            .fold(f32::MIN, |a, b| a.max(b));
    });

    copy_metadata(&mut result, images[0]);

    Ok(result)
}

// =============================================================================
// Stacking with Rejection
// =============================================================================

/// Stack images using mean with sigma rejection
///
/// Iteratively rejects pixels outside [mean - low_sigma, mean + high_sigma]
/// before computing the final mean.
pub fn stack_mean_sigma(
    images: &[&FitsImage],
    low_sigma: f32,
    high_sigma: f32,
) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        let mut values: Vec<f32> = images.iter()
            .map(|img| img.data[idx])
            .collect();

        let (clipped_mean, _) = sigma_clip(&mut values, low_sigma, high_sigma);
        *pixel = clipped_mean;
    });

    copy_metadata(&mut result, images[0]);

    Ok(result)
}

/// Stack images using mean with winsorized sigma rejection
///
/// Replaces outliers with boundary values instead of rejecting them.
pub fn stack_mean_winsorized(
    images: &[&FitsImage],
    sigma: f32,
) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        let mut values: Vec<f32> = images.iter()
            .map(|img| img.data[idx])
            .collect();

        let (winsorized_mean, _) = winsorized_sigma_clip(&mut values, sigma);
        *pixel = winsorized_mean;
    });

    copy_metadata(&mut result, images[0]);

    Ok(result)
}

/// Stack images using mean with MAD-based rejection
///
/// Uses Median Absolute Deviation for robust outlier rejection.
pub fn stack_mean_mad(
    images: &[&FitsImage],
    threshold: f32,
) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        let mut values: Vec<f32> = images.iter()
            .map(|img| img.data[idx])
            .collect();

        let (clipped_median, _) = mad_clip(&mut values, threshold);
        // After MAD clipping, compute mean of remaining values
        *pixel = if values.is_empty() {
            clipped_median
        } else {
            mean(&values)
        };
    });

    copy_metadata(&mut result, images[0]);

    Ok(result)
}

// =============================================================================
// Weighted Stacking
// =============================================================================

/// Stack images using weighted mean
///
/// Each image contributes proportionally to its weight.
/// Weights should sum to 1.0, or they will be normalized.
pub fn stack_weighted_mean(
    images: &[&FitsImage],
    weights: &[f32],
) -> StackingResult<FitsImage> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    if images.len() != weights.len() {
        return Err(StackingError::InvalidParameter(
            format!("Number of images ({}) doesn't match number of weights ({})",
                    images.len(), weights.len())
        ));
    }

    validate_same_dimensions(images)?;

    let (width, height, channels) = images[0].dimensions();

    // Normalize weights
    let weight_sum: f32 = weights.iter().sum();
    let normalized_weights: Vec<f32> = if weight_sum > 1e-10 {
        weights.iter().map(|w| w / weight_sum).collect()
    } else {
        vec![1.0 / images.len() as f32; images.len()]
    };

    let mut result = FitsImage::new(width, height, channels, true)?;

    // Process pixels in parallel
    result.data.par_iter_mut().enumerate().for_each(|(idx, pixel)| {
        *pixel = images.iter()
            .zip(normalized_weights.iter())
            .map(|(img, &w)| img.data[idx] * w)
            .sum();
    });

    copy_metadata(&mut result, images[0]);

    // Weighted sum of exposure times
    if let Some(total_exp) = images.iter()
        .zip(normalized_weights.iter())
        .filter_map(|(img, &w)| img.exposure.map(|e| e * w as f64))
        .sum::<f64>()
        .into()
    {
        result.exposure = Some(total_exp);
    }

    Ok(result)
}

/// Stack images using weighted mean based on SNR (signal-to-noise ratio)
///
/// Automatically computes weights from image statistics.
pub fn stack_weighted_snr(
    images: &[&FitsImage],
) -> StackingResult<FitsImage> {
    use crate::stacking::stats::compute_background_noise;

    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    validate_same_dimensions(images)?;

    // Compute weights based on inverse noise^2 (optimal SNR weighting)
    let weights: Vec<f32> = images.iter()
        .map(|img| {
            let (_, noise) = compute_background_noise(img);
            if noise > 1e-10 {
                1.0 / (noise * noise)
            } else {
                1.0
            }
        })
        .collect();

    stack_weighted_mean(images, &weights)
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Validate that all images have the same dimensions
fn validate_same_dimensions(images: &[&FitsImage]) -> StackingResult<()> {
    if images.len() < 2 {
        return Ok(());
    }

    let (ref_width, ref_height, ref_channels) = images[0].dimensions();

    for (i, img) in images.iter().enumerate().skip(1) {
        let (w, h, c) = img.dimensions();
        if w != ref_width || h != ref_height || c != ref_channels {
            return Err(StackingError::DimensionMismatch {
                expected: format!("{}x{}x{}", ref_width, ref_height, ref_channels),
                actual: format!("{}x{}x{} (image {})", w, h, c, i),
            });
        }
    }

    Ok(())
}

/// Copy metadata from source to destination image
fn copy_metadata(dest: &mut FitsImage, src: &FitsImage) {
    dest.exposure = src.exposure;
    dest.filter = src.filter.clone();
    dest.object = src.object.clone();
    dest.instrument = src.instrument.clone();
    dest.bayer_pattern = src.bayer_pattern.clone();
    dest.pixel_size_x = src.pixel_size_x;
    dest.pixel_size_y = src.pixel_size_y;
    dest.focal_length = src.focal_length;
    dest.ccd_temp = src.ccd_temp;
    dest.gain = src.gain;
    dest.offset = src.offset;
    dest.binning_x = src.binning_x;
    dest.binning_y = src.binning_y;
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_image(value: f32) -> FitsImage {
        let mut img = FitsImage::new(10, 10, 1, true).unwrap();
        img.data.iter_mut().for_each(|v| *v = value);
        img
    }

    #[test]
    fn test_stack_median() {
        let img1 = create_test_image(1.0);
        let img2 = create_test_image(2.0);
        let img3 = create_test_image(3.0);

        let images: Vec<&FitsImage> = vec![&img1, &img2, &img3];
        let result = stack_median(&images).unwrap();

        assert!((result.data[0] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_stack_mean() {
        let img1 = create_test_image(1.0);
        let img2 = create_test_image(2.0);
        let img3 = create_test_image(3.0);

        let images: Vec<&FitsImage> = vec![&img1, &img2, &img3];
        let result = stack_mean(&images).unwrap();

        assert!((result.data[0] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_stack_sum() {
        let img1 = create_test_image(1.0);
        let img2 = create_test_image(2.0);
        let img3 = create_test_image(3.0);

        let images: Vec<&FitsImage> = vec![&img1, &img2, &img3];
        let result = stack_sum(&images).unwrap();

        assert!((result.data[0] - 6.0).abs() < 1e-6);
    }

    #[test]
    fn test_stack_min() {
        let img1 = create_test_image(3.0);
        let img2 = create_test_image(1.0);
        let img3 = create_test_image(2.0);

        let images: Vec<&FitsImage> = vec![&img1, &img2, &img3];
        let result = stack_min(&images).unwrap();

        assert!((result.data[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_stack_max() {
        let img1 = create_test_image(1.0);
        let img2 = create_test_image(3.0);
        let img3 = create_test_image(2.0);

        let images: Vec<&FitsImage> = vec![&img1, &img2, &img3];
        let result = stack_max(&images).unwrap();

        assert!((result.data[0] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_stack_mean_sigma_outlier() {
        // With more images, sigma clipping works better
        // Values: 1, 1.5, 2, 2.5, 3, 100 - the 100 should be rejected
        let img1 = create_test_image(1.0);
        let img2 = create_test_image(1.5);
        let img3 = create_test_image(2.0);
        let img4 = create_test_image(2.5);
        let img5 = create_test_image(3.0);
        let img6 = create_test_image(100.0); // outlier

        let images: Vec<&FitsImage> = vec![&img1, &img2, &img3, &img4, &img5, &img6];
        let result = stack_mean_sigma(&images, 2.0, 2.0).unwrap();

        // After outlier rejection, result should be close to mean of [1, 1.5, 2, 2.5, 3] = 2.0
        // With iterative sigma clipping, 100 should be rejected
        assert!((result.data[0] - 2.0).abs() < 1.0);
    }

    #[test]
    fn test_stack_weighted() {
        let img1 = create_test_image(1.0);
        let img2 = create_test_image(2.0);

        let images: Vec<&FitsImage> = vec![&img1, &img2];
        let weights = vec![0.75, 0.25];

        let result = stack_weighted_mean(&images, &weights).unwrap();

        // Weighted mean: 0.75 * 1.0 + 0.25 * 2.0 = 1.25
        assert!((result.data[0] - 1.25).abs() < 1e-6);
    }

    #[test]
    fn test_empty_sequence() {
        let images: Vec<&FitsImage> = vec![];
        assert!(stack_median(&images).is_err());
    }

    #[test]
    fn test_dimension_mismatch() {
        let img1 = FitsImage::new(10, 10, 1, true).unwrap();
        let img2 = FitsImage::new(20, 20, 1, true).unwrap();

        let images: Vec<&FitsImage> = vec![&img1, &img2];
        assert!(stack_median(&images).is_err());
    }
}
