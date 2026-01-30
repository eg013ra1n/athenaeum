//! Calibration implementation
//!
//! This module provides pure Rust implementations for calibrating light frames
//! using master bias, dark, and flat frames.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::fits::FitsImage;
use crate::stacking::ops::{image_subtract_inplace, image_divide_inplace};
use crate::stacking::stats::{mean, median, compute_background_noise};
use rayon::prelude::*;

/// Dark frame optimization method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DarkOptimization {
    /// No optimization - subtract dark as-is
    Off,
    /// Scale dark by exposure time ratio
    ExposureBased,
    /// Optimize dark scaling to minimize noise in result
    NoiseMinimization,
}

impl Default for DarkOptimization {
    fn default() -> Self {
        DarkOptimization::Off
    }
}

/// Flat field normalization method
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlatNormalization {
    /// Automatic normalization from flat center/mean
    Auto,
    /// Use median of flat
    Median,
    /// Manual normalization value
    Manual(f32),
}

impl Default for FlatNormalization {
    fn default() -> Self {
        FlatNormalization::Auto
    }
}

/// Configuration for calibration
#[derive(Debug, Clone)]
pub struct CalibrationConfig {
    /// Dark optimization method
    pub dark_optimization: DarkOptimization,
    /// Flat normalization method
    pub flat_normalization: FlatNormalization,
    /// Minimum pixel value after calibration (to prevent negative values)
    pub pedestal: f32,
    /// Whether to clip negative values
    pub clip_negative: bool,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        CalibrationConfig {
            dark_optimization: DarkOptimization::Off,
            flat_normalization: FlatNormalization::Auto,
            pedestal: 0.0,
            clip_negative: true,
        }
    }
}

// =============================================================================
// Bias Subtraction
// =============================================================================

/// Subtract master bias from a light frame
///
/// light = light - bias
///
/// # Arguments
///
/// * `light` - Light frame to calibrate (modified in place)
/// * `master_bias` - Master bias frame
pub fn subtract_bias(light: &mut FitsImage, master_bias: &FitsImage) -> StackingResult<()> {
    check_dimensions(light, master_bias, "bias")?;

    light.data.par_iter_mut()
        .zip(master_bias.data.par_iter())
        .for_each(|(l, b)| {
            *l -= b;
        });

    Ok(())
}

// =============================================================================
// Dark Subtraction
// =============================================================================

/// Subtract master dark from a light frame
///
/// Supports optional dark optimization based on exposure time.
///
/// # Arguments
///
/// * `light` - Light frame to calibrate (modified in place)
/// * `master_dark` - Master dark frame
/// * `optimization` - Dark optimization method
/// * `light_exp` - Light frame exposure time (for scaling)
/// * `dark_exp` - Dark frame exposure time (for scaling)
pub fn subtract_dark(
    light: &mut FitsImage,
    master_dark: &FitsImage,
    optimization: DarkOptimization,
    light_exp: f64,
    dark_exp: f64,
) -> StackingResult<()> {
    check_dimensions(light, master_dark, "dark")?;

    match optimization {
        DarkOptimization::Off => {
            // Simple subtraction
            light.data.par_iter_mut()
                .zip(master_dark.data.par_iter())
                .for_each(|(l, d)| {
                    *l -= d;
                });
        }
        DarkOptimization::ExposureBased => {
            // Scale dark by exposure ratio
            let scale = if dark_exp > 0.0 {
                (light_exp / dark_exp) as f32
            } else {
                1.0
            };

            light.data.par_iter_mut()
                .zip(master_dark.data.par_iter())
                .for_each(|(l, d)| {
                    *l -= d * scale;
                });
        }
        DarkOptimization::NoiseMinimization => {
            // Compute optimal scaling factor to minimize noise
            let scale = compute_optimal_dark_scale(light, master_dark);

            light.data.par_iter_mut()
                .zip(master_dark.data.par_iter())
                .for_each(|(l, d)| {
                    *l -= d * scale;
                });
        }
    }

    Ok(())
}

/// Compute optimal dark scaling factor to minimize noise
///
/// Uses least-squares fitting in low-signal regions.
fn compute_optimal_dark_scale(light: &FitsImage, dark: &FitsImage) -> f32 {
    // Sample a subset of pixels for efficiency
    let sample_size = (light.data.len() / 100).max(1000).min(light.data.len());
    let step = light.data.len() / sample_size;

    let (light_bg, _) = compute_background_noise(light);
    let (dark_bg, _) = compute_background_noise(dark);

    // Only consider pixels near background level (avoid stars)
    let threshold = light_bg * 1.5;

    let mut sum_ld = 0.0_f64;
    let mut sum_dd = 0.0_f64;

    for i in (0..light.data.len()).step_by(step.max(1)) {
        let l = light.data[i];
        let d = dark.data[i];

        // Only use background pixels
        if l < threshold && d > 0.0 {
            sum_ld += (l * d) as f64;
            sum_dd += (d * d) as f64;
        }
    }

    // Optimal scale minimizes sum((L - scale*D)^2)
    // scale = sum(L*D) / sum(D*D)
    if sum_dd > 1e-10 {
        (sum_ld / sum_dd) as f32
    } else {
        1.0
    }
}

// =============================================================================
// Flat Division
// =============================================================================

/// Apply flat field correction to a light frame
///
/// light = light / flat * normalization
///
/// # Arguments
///
/// * `light` - Light frame to calibrate (modified in place)
/// * `master_flat` - Master flat frame
/// * `normalization` - Flat normalization method
pub fn apply_flat(
    light: &mut FitsImage,
    master_flat: &FitsImage,
    normalization: FlatNormalization,
) -> StackingResult<()> {
    check_dimensions(light, master_flat, "flat")?;

    // Compute normalization value
    let norm_value = match normalization {
        FlatNormalization::Auto => {
            // Use mean of central region
            compute_flat_norm_center(master_flat)
        }
        FlatNormalization::Median => {
            median(&master_flat.data)
        }
        FlatNormalization::Manual(v) => v,
    };

    if norm_value < 1e-10 {
        return Err(StackingError::InvalidParameter(
            "Flat normalization value is too small".to_string()
        ));
    }

    // Divide light by flat, multiply by normalization
    // Avoid division by zero
    let min_flat = 0.001 * norm_value; // 0.1% of norm

    light.data.par_iter_mut()
        .zip(master_flat.data.par_iter())
        .for_each(|(l, &f)| {
            if f > min_flat {
                *l = (*l / f) * norm_value;
            } else {
                // Low flat values likely indicate vignetting or bad pixels
                *l = 0.0;
            }
        });

    Ok(())
}

/// Compute flat normalization from center region
fn compute_flat_norm_center(flat: &FitsImage) -> f32 {
    let width = flat.width as usize;
    let height = flat.height as usize;

    // Use central 25% of the image
    let x_start = width / 4;
    let x_end = 3 * width / 4;
    let y_start = height / 4;
    let y_end = 3 * height / 4;

    let mut center_values: Vec<f32> = Vec::new();

    for y in y_start..y_end {
        for x in x_start..x_end {
            let idx = y * width + x;
            if idx < flat.data.len() {
                center_values.push(flat.data[idx]);
            }
        }
    }

    if center_values.is_empty() {
        mean(&flat.data)
    } else {
        mean(&center_values)
    }
}

// =============================================================================
// Full Calibration Pipeline
// =============================================================================

/// Calibrate a light frame with optional bias, dark, and flat
///
/// Applies calibration in the standard order:
/// 1. Subtract bias
/// 2. Subtract dark (with optional optimization)
/// 3. Divide by flat
///
/// # Arguments
///
/// * `light` - Light frame to calibrate (modified in place)
/// * `bias` - Optional master bias frame
/// * `dark` - Optional master dark frame
/// * `flat` - Optional master flat frame
/// * `config` - Calibration configuration
pub fn calibrate_frame(
    light: &mut FitsImage,
    bias: Option<&FitsImage>,
    dark: Option<&FitsImage>,
    flat: Option<&FitsImage>,
    config: &CalibrationConfig,
) -> StackingResult<()> {
    // Get exposure times for dark optimization
    let light_exp = light.exposure.unwrap_or(1.0);

    // 1. Subtract bias (if provided)
    if let Some(master_bias) = bias {
        subtract_bias(light, master_bias)?;
    }

    // 2. Subtract dark (if provided)
    if let Some(master_dark) = dark {
        let dark_exp = master_dark.exposure.unwrap_or(light_exp);
        subtract_dark(light, master_dark, config.dark_optimization, light_exp, dark_exp)?;
    }

    // 3. Apply flat (if provided)
    if let Some(master_flat) = flat {
        apply_flat(light, master_flat, config.flat_normalization)?;
    }

    // 4. Apply pedestal and clip negative values if configured
    if config.pedestal > 0.0 || config.clip_negative {
        let pedestal = config.pedestal;
        let clip = config.clip_negative;

        light.data.par_iter_mut().for_each(|v| {
            *v += pedestal;
            if clip && *v < 0.0 {
                *v = 0.0;
            }
        });
    }

    Ok(())
}

/// Calibrate a flat frame (with optional dark/bias)
///
/// Flats typically need dark/bias subtraction but not flat division.
pub fn calibrate_flat_frame(
    flat: &mut FitsImage,
    bias: Option<&FitsImage>,
    dark: Option<&FitsImage>,
    dark_optimization: DarkOptimization,
) -> StackingResult<()> {
    let flat_exp = flat.exposure.unwrap_or(1.0);

    // 1. Subtract bias (if provided)
    if let Some(master_bias) = bias {
        subtract_bias(flat, master_bias)?;
    }

    // 2. Subtract dark (if provided) - often a "dark flat" with same exposure
    if let Some(master_dark) = dark {
        let dark_exp = master_dark.exposure.unwrap_or(flat_exp);
        subtract_dark(flat, master_dark, dark_optimization, flat_exp, dark_exp)?;
    }

    Ok(())
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Check that calibration frame has matching dimensions
fn check_dimensions(light: &FitsImage, cal: &FitsImage, cal_type: &str) -> StackingResult<()> {
    if light.width != cal.width || light.height != cal.height {
        return Err(StackingError::DimensionMismatch {
            expected: format!("{}x{}", light.width, light.height),
            actual: format!("{}x{} ({})", cal.width, cal.height, cal_type),
        });
    }

    // Check channel count (allow mono calibration frames for color images)
    if light.nchannels != cal.nchannels && cal.nchannels != 1 {
        return Err(StackingError::DimensionMismatch {
            expected: format!("{} channels", light.nchannels),
            actual: format!("{} channels ({})", cal.nchannels, cal_type),
        });
    }

    Ok(())
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
        img.exposure = Some(60.0);
        img
    }

    #[test]
    fn test_subtract_bias() {
        let mut light = create_test_image(0.5);
        let bias = create_test_image(0.1);

        subtract_bias(&mut light, &bias).unwrap();

        assert!((light.data[0] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn test_subtract_dark_no_optimization() {
        let mut light = create_test_image(0.5);
        let dark = create_test_image(0.1);

        subtract_dark(&mut light, &dark, DarkOptimization::Off, 60.0, 60.0).unwrap();

        assert!((light.data[0] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn test_subtract_dark_exposure_based() {
        let mut light = create_test_image(0.5);
        let mut dark = create_test_image(0.1);
        dark.exposure = Some(120.0); // Dark is 2x longer

        subtract_dark(&mut light, &dark, DarkOptimization::ExposureBased, 60.0, 120.0).unwrap();

        // Dark should be scaled to 0.05 before subtraction
        assert!((light.data[0] - 0.45).abs() < 1e-6);
    }

    #[test]
    fn test_apply_flat() {
        let mut light = create_test_image(0.5);
        let flat = create_test_image(0.8); // 80% flat (slight vignetting)

        apply_flat(&mut light, &flat, FlatNormalization::Auto).unwrap();

        // Light should be boosted: 0.5 / 0.8 * 0.8 = 0.5
        // (but norm is mean of flat, so it's 0.5 / 0.8 * 0.8 = 0.5)
        assert!((light.data[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_full_calibration() {
        let mut light = create_test_image(0.5);
        let bias = create_test_image(0.05);
        let dark = create_test_image(0.1);
        let flat = create_test_image(0.9);

        let config = CalibrationConfig {
            dark_optimization: DarkOptimization::Off,
            flat_normalization: FlatNormalization::Auto,
            pedestal: 0.0,
            clip_negative: true,
        };

        calibrate_frame(
            &mut light,
            Some(&bias),
            Some(&dark),
            Some(&flat),
            &config,
        ).unwrap();

        // (0.5 - 0.05 - 0.1) / 0.9 * 0.9 = 0.35
        assert!((light.data[0] - 0.35).abs() < 1e-5);
    }

    #[test]
    fn test_dimension_mismatch() {
        let mut light = FitsImage::new(10, 10, 1, true).unwrap();
        let bias = FitsImage::new(20, 20, 1, true).unwrap();

        assert!(subtract_bias(&mut light, &bias).is_err());
    }
}
