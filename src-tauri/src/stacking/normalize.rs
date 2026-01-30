//! Frame normalization
//!
//! This module provides functions for normalizing frames before stacking.
//! Normalization compensates for differences in sky background and transparency.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::fits::FitsImage;
use crate::stacking::stats::{mean, median, compute_background_noise};
use crate::stacking::ffi::Normalization;
use rayon::prelude::*;

/// Normalization method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormMethod {
    /// Additive normalization: img = img + offset
    /// Matches background levels
    Additive,
    /// Multiplicative normalization: img = img * scale
    /// Matches overall brightness
    Multiplicative,
    /// Additive + scaling: img = (img + offset) * scale
    /// Matches both background and scale
    AdditiveScaling,
    /// Multiplicative + scaling: img = img * scale1 * scale2
    /// Alternative combined method
    MultiplicativeScaling,
}

impl From<Normalization> for NormMethod {
    fn from(n: Normalization) -> Self {
        match n {
            Normalization::NoNorm => NormMethod::Additive, // No-op will be handled separately
            Normalization::Additive => NormMethod::Additive,
            Normalization::Multiplicative => NormMethod::Multiplicative,
            Normalization::AdditiveScaling => NormMethod::AdditiveScaling,
            Normalization::MultiplicativeScaling => NormMethod::MultiplicativeScaling,
        }
    }
}

impl From<NormMethod> for Normalization {
    fn from(m: NormMethod) -> Self {
        match m {
            NormMethod::Additive => Normalization::Additive,
            NormMethod::Multiplicative => Normalization::Multiplicative,
            NormMethod::AdditiveScaling => Normalization::AdditiveScaling,
            NormMethod::MultiplicativeScaling => Normalization::MultiplicativeScaling,
        }
    }
}

/// Normalization coefficients for a single frame
#[derive(Debug, Clone, Copy)]
pub struct NormCoefficients {
    /// Additive offset
    pub offset: f64,
    /// Multiplicative scale
    pub scale: f64,
}

impl Default for NormCoefficients {
    fn default() -> Self {
        NormCoefficients {
            offset: 0.0,
            scale: 1.0,
        }
    }
}

/// Normalize images to a reference frame
///
/// Modifies images in place to match the reference frame's statistics.
///
/// # Arguments
///
/// * `images` - Mutable slice of images to normalize
/// * `reference` - Index of the reference image (this image won't be modified)
/// * `method` - Normalization method to use
pub fn normalize_to_reference(
    images: &mut [FitsImage],
    reference: usize,
    method: NormMethod,
) -> StackingResult<Vec<NormCoefficients>> {
    if images.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    if reference >= images.len() {
        return Err(StackingError::InvalidParameter(
            format!("Reference index {} out of bounds (0..{})", reference, images.len())
        ));
    }

    // Compute reference statistics
    let ref_stats = compute_frame_stats(&images[reference]);

    // Compute coefficients for each image
    let coefficients: Vec<NormCoefficients> = images.iter()
        .enumerate()
        .map(|(i, img)| {
            if i == reference {
                NormCoefficients::default()
            } else {
                compute_normalization_coefficients(img, &ref_stats, method)
            }
        })
        .collect();

    // Apply normalization
    for (i, coeff) in coefficients.iter().enumerate() {
        if i != reference {
            apply_normalization(&mut images[i], coeff);
        }
    }

    Ok(coefficients)
}

/// Compute normalization coefficients between two frames
///
/// Returns (offset, scale) such that img * scale + offset matches ref_img
pub fn compute_normalization_factor(
    img: &FitsImage,
    ref_img: &FitsImage,
) -> (f32, f32) {
    let img_stats = compute_frame_stats(img);
    let ref_stats = compute_frame_stats(ref_img);

    // Simple multiplicative scaling
    let scale = if img_stats.mean > 1e-10 {
        (ref_stats.mean / img_stats.mean) as f32
    } else {
        1.0
    };

    let offset = (ref_stats.background - img_stats.background * scale as f64) as f32;

    (offset, scale)
}

/// Frame statistics used for normalization
#[derive(Debug, Clone)]
struct FrameStats {
    /// Mean value
    mean: f64,
    /// Median value
    median: f64,
    /// Background level (robust estimate)
    background: f64,
    /// Background noise (robust estimate)
    noise: f64,
}

/// Compute statistics for a frame
fn compute_frame_stats(img: &FitsImage) -> FrameStats {
    let mean_val = mean(&img.data) as f64;
    let median_val = median(&img.data) as f64;
    let (background, noise) = compute_background_noise(img);

    FrameStats {
        mean: mean_val,
        median: median_val,
        background: background as f64,
        noise: noise as f64,
    }
}

/// Compute normalization coefficients based on method
fn compute_normalization_coefficients(
    img: &FitsImage,
    ref_stats: &FrameStats,
    method: NormMethod,
) -> NormCoefficients {
    let img_stats = compute_frame_stats(img);

    match method {
        NormMethod::Additive => {
            // Only adjust background level
            let offset = ref_stats.background - img_stats.background;
            NormCoefficients {
                offset,
                scale: 1.0,
            }
        }
        NormMethod::Multiplicative => {
            // Only adjust scale
            let scale = if img_stats.mean > 1e-10 {
                ref_stats.mean / img_stats.mean
            } else {
                1.0
            };
            NormCoefficients {
                offset: 0.0,
                scale,
            }
        }
        NormMethod::AdditiveScaling => {
            // Adjust both background and scale
            // First scale to match signal level, then offset for background
            let scale = if img_stats.median > 1e-10 {
                ref_stats.median / img_stats.median
            } else {
                1.0
            };
            let offset = ref_stats.background - (img_stats.background * scale);
            NormCoefficients {
                offset,
                scale,
            }
        }
        NormMethod::MultiplicativeScaling => {
            // Alternative: scale background and signal separately
            let bg_scale = if img_stats.background > 1e-10 {
                ref_stats.background / img_stats.background
            } else {
                1.0
            };
            let signal_scale = if (img_stats.mean - img_stats.background) > 1e-10 {
                (ref_stats.mean - ref_stats.background) / (img_stats.mean - img_stats.background)
            } else {
                1.0
            };
            // Use geometric mean of scales
            let scale = (bg_scale * signal_scale).sqrt();
            NormCoefficients {
                offset: 0.0,
                scale,
            }
        }
    }
}

/// Apply normalization coefficients to an image
fn apply_normalization(img: &mut FitsImage, coeff: &NormCoefficients) {
    let scale = coeff.scale as f32;
    let offset = coeff.offset as f32;

    img.data.par_iter_mut().for_each(|v| {
        *v = *v * scale + offset;
    });
}

/// Normalize a single frame to match target statistics
pub fn normalize_frame(
    img: &mut FitsImage,
    target_mean: f64,
    target_background: f64,
) -> NormCoefficients {
    let img_stats = compute_frame_stats(img);

    // Compute scale to match mean (excluding background)
    let scale = if (img_stats.mean - img_stats.background) > 1e-10 {
        (target_mean - target_background) / (img_stats.mean - img_stats.background)
    } else {
        1.0
    };

    // Compute offset to match background
    let offset = target_background - (img_stats.background * scale);

    let coeff = NormCoefficients { offset, scale };
    apply_normalization(img, &coeff);

    coeff
}

/// Compute optimal weights for weighted stacking based on noise
pub fn compute_stacking_weights(images: &[&FitsImage]) -> Vec<f32> {
    // Weight by inverse variance (noise^2)
    let noise_values: Vec<f32> = images.iter()
        .map(|img| {
            let (_, noise) = compute_background_noise(img);
            if noise > 1e-10 { noise } else { 1.0 }
        })
        .collect();

    // Inverse variance weighting
    let inv_var: Vec<f32> = noise_values.iter()
        .map(|n| 1.0 / (n * n))
        .collect();

    // Normalize weights to sum to 1
    let sum: f32 = inv_var.iter().sum();
    if sum > 1e-10 {
        inv_var.iter().map(|&w| w / sum).collect()
    } else {
        vec![1.0 / images.len() as f32; images.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_additive_normalization() {
        let mut img1 = FitsImage::new(10, 10, 1, true).unwrap();
        let mut img2 = FitsImage::new(10, 10, 1, true).unwrap();

        // img1: background 0.1
        img1.data.iter_mut().for_each(|v| *v = 0.1);
        // img2: background 0.2
        img2.data.iter_mut().for_each(|v| *v = 0.2);

        let mut images = vec![img1, img2];
        let coeffs = normalize_to_reference(&mut images, 0, NormMethod::Additive).unwrap();

        // img2 should be shifted down by ~0.1
        assert!(coeffs[1].offset < -0.05);
        assert!((coeffs[1].scale - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_multiplicative_normalization() {
        let mut img1 = FitsImage::new(10, 10, 1, true).unwrap();
        let mut img2 = FitsImage::new(10, 10, 1, true).unwrap();

        // img1: mean 0.5
        img1.data.iter_mut().for_each(|v| *v = 0.5);
        // img2: mean 0.25 (half as bright)
        img2.data.iter_mut().for_each(|v| *v = 0.25);

        let mut images = vec![img1, img2];
        let coeffs = normalize_to_reference(&mut images, 0, NormMethod::Multiplicative).unwrap();

        // img2 should be scaled up by ~2x
        assert!((coeffs[1].scale - 2.0).abs() < 0.1);
        assert!(coeffs[1].offset.abs() < 0.01);
    }

    #[test]
    fn test_normalization_factor() {
        let mut img = FitsImage::new(10, 10, 1, true).unwrap();
        let mut ref_img = FitsImage::new(10, 10, 1, true).unwrap();

        img.data.iter_mut().for_each(|v| *v = 0.25);
        ref_img.data.iter_mut().for_each(|v| *v = 0.5);

        let (offset, scale) = compute_normalization_factor(&img, &ref_img);

        // Scale should be ~2x
        assert!((scale - 2.0).abs() < 0.1);
    }

    #[test]
    fn test_stacking_weights() {
        let img1 = FitsImage::new(10, 10, 1, true).unwrap();
        let img2 = FitsImage::new(10, 10, 1, true).unwrap();

        let images: Vec<&FitsImage> = vec![&img1, &img2];
        let weights = compute_stacking_weights(&images);

        // Weights should sum to 1
        let sum: f32 = weights.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
    }
}
