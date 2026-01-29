//! Light frame calibration
//!
//! This module provides functions for calibrating light frames using
//! master bias, dark, and flat frames.

use crate::stacking::config::ProgressCallback;
use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi::{
    self, gboolean, InterpolationMethod, PreprocessingData, SensorPattern, GFALSE, GTRUE,
};
use crate::stacking::fits::FitsImage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// =============================================================================
// Configuration Types
// =============================================================================

/// Dark optimization method
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DarkOptimization {
    /// No optimization - subtract dark as-is
    Off,
    /// Scale dark by exposure time ratio
    ExposureBased,
    /// Minimize noise in calibrated result
    NoiseMinimization,
}

impl Default for DarkOptimization {
    fn default() -> Self {
        DarkOptimization::Off
    }
}

/// Flat normalization method
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlatNormalization {
    /// Automatic normalization from flat center
    Auto,
    /// Manual normalization value
    Manual(f32),
}

impl Default for FlatNormalization {
    fn default() -> Self {
        FlatNormalization::Auto
    }
}

/// Debayer method configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebayerMethod {
    /// Bilinear interpolation (fast)
    Bilinear,
    /// VNG interpolation (good balance)
    Vng,
    /// AHD interpolation (high quality)
    Ahd,
    /// AMaZE interpolation (very high quality)
    Amaze,
    /// RCD interpolation (high quality, fast)
    Rcd,
    /// Auto-detect from sensor (X-Trans aware)
    Auto,
}

impl Default for DebayerMethod {
    fn default() -> Self {
        DebayerMethod::Vng
    }
}

impl From<DebayerMethod> for InterpolationMethod {
    fn from(method: DebayerMethod) -> Self {
        match method {
            DebayerMethod::Bilinear => InterpolationMethod::BayerBilinear,
            DebayerMethod::Vng => InterpolationMethod::BayerVng,
            DebayerMethod::Ahd => InterpolationMethod::BayerAhd,
            DebayerMethod::Amaze => InterpolationMethod::BayerAmaze,
            DebayerMethod::Rcd => InterpolationMethod::BayerRcd,
            DebayerMethod::Auto => InterpolationMethod::BayerVng, // Default for auto
        }
    }
}

/// Bayer pattern configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BayerPattern {
    /// RGGB pattern
    Rggb,
    /// BGGR pattern
    Bggr,
    /// GBRG pattern
    Gbrg,
    /// GRBG pattern
    Grbg,
    /// Auto-detect from FITS header
    Auto,
}

impl Default for BayerPattern {
    fn default() -> Self {
        BayerPattern::Auto
    }
}

impl From<BayerPattern> for SensorPattern {
    fn from(pattern: BayerPattern) -> Self {
        match pattern {
            BayerPattern::Rggb => SensorPattern::BayerRggb,
            BayerPattern::Bggr => SensorPattern::BayerBggr,
            BayerPattern::Gbrg => SensorPattern::BayerGbrg,
            BayerPattern::Grbg => SensorPattern::BayerGrbg,
            BayerPattern::Auto => SensorPattern::BayerNone, // Will be auto-detected
        }
    }
}

/// Configuration for light frame calibration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationConfig {
    /// Whether to apply bias subtraction
    pub use_bias: bool,
    /// Whether to apply dark subtraction
    pub use_dark: bool,
    /// Whether to apply flat division
    pub use_flat: bool,
    /// Dark optimization method
    pub dark_optimization: DarkOptimization,
    /// Flat normalization method
    pub flat_normalization: FlatNormalization,
    /// Whether to output 32-bit float
    pub output_32bit: bool,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        CalibrationConfig {
            use_bias: true,
            use_dark: true,
            use_flat: true,
            dark_optimization: DarkOptimization::Off,
            flat_normalization: FlatNormalization::Auto,
            output_32bit: true,
        }
    }
}

/// Configuration for debayering (demosaicing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebayerConfig {
    /// Interpolation method
    pub method: DebayerMethod,
    /// Bayer pattern (or auto-detect)
    pub pattern: BayerPattern,
}

impl Default for DebayerConfig {
    fn default() -> Self {
        DebayerConfig {
            method: DebayerMethod::Vng,
            pattern: BayerPattern::Auto,
        }
    }
}

/// Configuration for batch calibration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCalibrationConfig {
    /// Calibration settings
    pub calibration: CalibrationConfig,
    /// Debayer settings (None = no debayering)
    pub debayer: Option<DebayerConfig>,
    /// Output directory
    pub output_dir: PathBuf,
    /// Output filename prefix
    pub output_prefix: String,
}

impl Default for BatchCalibrationConfig {
    fn default() -> Self {
        BatchCalibrationConfig {
            calibration: CalibrationConfig::default(),
            debayer: None,
            output_dir: PathBuf::new(),
            output_prefix: "cal_".to_string(),
        }
    }
}

/// Result of calibrating a single frame
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationResult {
    /// Path to the calibrated output file
    pub output_path: String,
    /// Whether debayering was applied
    pub debayered: bool,
    /// Processing time in milliseconds
    pub processing_time_ms: u64,
}

/// Result of batch calibration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCalibrationResult {
    /// Number of frames successfully calibrated
    pub calibrated_count: usize,
    /// Number of frames that failed
    pub failed_count: usize,
    /// Output directory used
    pub output_dir: String,
    /// List of output files
    pub output_files: Vec<String>,
    /// List of failed files with error messages
    pub failed_files: Vec<(String, String)>,
    /// Total processing time in milliseconds
    pub total_processing_time_ms: u64,
}

// =============================================================================
// Calibration Functions
// =============================================================================

/// Calibrate a single light frame
///
/// Applies bias, dark, and flat calibration as configured.
///
/// # Arguments
///
/// * `light` - The light frame to calibrate (modified in place)
/// * `bias` - Optional master bias frame
/// * `dark` - Optional master dark frame
/// * `flat` - Optional master flat frame
/// * `config` - Calibration configuration
///
/// # Returns
///
/// Ok(()) on success, or error details
pub fn calibrate_light(
    light: &mut FitsImage,
    bias: Option<&FitsImage>,
    dark: Option<&FitsImage>,
    flat: Option<&FitsImage>,
    config: &CalibrationConfig,
) -> StackingResult<()> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    #[cfg(feature = "siril-ffi")]
    {
        // Set up preprocessing data
        let mut prepro = PreprocessingData::new();

        // Configure bias
        if config.use_bias {
            if let Some(bias_img) = bias {
                prepro.use_bias = GTRUE;
                prepro.bias = bias_img.as_ptr();
            }
        }

        // Configure dark
        if config.use_dark {
            if let Some(dark_img) = dark {
                prepro.use_dark = GTRUE;
                prepro.dark = dark_img.as_ptr();

                // Dark optimization
                match config.dark_optimization {
                    DarkOptimization::Off => {
                        prepro.use_dark_optim = GFALSE;
                        prepro.use_exposure = GFALSE;
                    }
                    DarkOptimization::ExposureBased => {
                        prepro.use_dark_optim = GTRUE;
                        prepro.use_exposure = GTRUE;
                    }
                    DarkOptimization::NoiseMinimization => {
                        prepro.use_dark_optim = GTRUE;
                        prepro.use_exposure = GFALSE;
                    }
                }
            }
        }

        // Configure flat
        if config.use_flat {
            if let Some(flat_img) = flat {
                prepro.use_flat = GTRUE;
                prepro.flat = flat_img.as_ptr();

                // Flat normalization
                match config.flat_normalization {
                    FlatNormalization::Auto => {
                        prepro.autolevel = GTRUE;
                    }
                    FlatNormalization::Manual(value) => {
                        prepro.autolevel = GFALSE;
                        prepro.normalisation = value;
                    }
                }
            }
        }

        // Configure output
        prepro.allow_32bit_output = if config.output_32bit { GTRUE } else { GFALSE };
        prepro.is_sequence = GFALSE;
        prepro.debayer = GFALSE; // Debayering handled separately

        // Apply calibration via FFI
        // Note: The actual FFI call would modify light in place
        // For now, return NotInitialized since FFI is not fully implemented
        return Err(StackingError::NotInitialized);
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Debayer (demosaic) a CFA image
///
/// Converts a Bayer-pattern CFA image to RGB.
///
/// # Arguments
///
/// * `frame` - The frame to debayer (modified in place)
/// * `config` - Debayer configuration
///
/// # Returns
///
/// Ok(()) on success, or error details
pub fn debayer_frame(frame: &mut FitsImage, config: &DebayerConfig) -> StackingResult<()> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    // Check if frame is CFA
    if !frame.is_cfa() {
        // Not a CFA image, nothing to do
        return Ok(());
    }

    #[cfg(feature = "siril-ffi")]
    {
        // Determine pattern
        let pattern = if config.pattern == BayerPattern::Auto {
            // Auto-detect from FITS header
            frame
                .bayer_pattern()
                .map(|s| SensorPattern::from_string(&s))
                .unwrap_or(SensorPattern::BayerRggb)
        } else {
            config.pattern.into()
        };

        // Determine interpolation method
        let method: InterpolationMethod = config.method.into();

        // Call FFI debayer
        unsafe {
            let result = ffi::debayer(frame.as_mut_ptr(), method, pattern);
            if result != 0 {
                return Err(StackingError::DebayerError(format!(
                    "Debayer returned error code {}",
                    result
                )));
            }
        }

        Ok(())
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Calibrate light frames in batch
///
/// Processes multiple light frames with the same calibration masters.
///
/// # Arguments
///
/// * `light_paths` - Paths to light frames
/// * `bias` - Optional master bias frame
/// * `dark` - Optional master dark frame
/// * `flat` - Optional master flat frame
/// * `config` - Batch calibration configuration
/// * `progress` - Optional progress callback
///
/// # Returns
///
/// Result containing batch processing statistics
pub fn calibrate_lights_batch<P: AsRef<Path>>(
    light_paths: &[P],
    bias: Option<&FitsImage>,
    dark: Option<&FitsImage>,
    flat: Option<&FitsImage>,
    config: &BatchCalibrationConfig,
    progress: Option<ProgressCallback>,
) -> StackingResult<BatchCalibrationResult> {
    use std::time::Instant;

    let start = Instant::now();
    let mut output_files = Vec::with_capacity(light_paths.len());
    let mut failed_files = Vec::new();

    // Ensure output directory exists
    std::fs::create_dir_all(&config.output_dir).map_err(|e| StackingError::WriteError {
        path: config.output_dir.display().to_string(),
        details: e.to_string(),
    })?;

    for (i, path) in light_paths.iter().enumerate() {
        let path_ref = path.as_ref();

        if let Some(ref cb) = progress {
            let percent = (i as f32) / (light_paths.len() as f32);
            cb(
                percent,
                &format!("Calibrating {}", path_ref.file_name().unwrap_or_default().to_string_lossy()),
            );
        }

        match calibrate_single_frame(path_ref, bias, dark, flat, config) {
            Ok(output_path) => {
                output_files.push(output_path);
            }
            Err(e) => {
                failed_files.push((path_ref.display().to_string(), e.to_string()));
            }
        }
    }

    if let Some(ref cb) = progress {
        cb(1.0, "Calibration complete");
    }

    Ok(BatchCalibrationResult {
        calibrated_count: output_files.len(),
        failed_count: failed_files.len(),
        output_dir: config.output_dir.display().to_string(),
        output_files,
        failed_files,
        total_processing_time_ms: start.elapsed().as_millis() as u64,
    })
}

/// Calibrate a single frame and save to output directory
fn calibrate_single_frame(
    light_path: &Path,
    bias: Option<&FitsImage>,
    dark: Option<&FitsImage>,
    flat: Option<&FitsImage>,
    config: &BatchCalibrationConfig,
) -> StackingResult<String> {
    // Load light frame
    let mut light = FitsImage::open(light_path)?;

    // Apply calibration
    calibrate_light(&mut light, bias, dark, flat, &config.calibration)?;

    // Apply debayering if configured
    if let Some(ref debayer_config) = config.debayer {
        debayer_frame(&mut light, debayer_config)?;
    }

    // Generate output filename
    let input_stem = light_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("frame");
    let output_name = format!("{}{}.fits", config.output_prefix, input_stem);
    let output_path = config.output_dir.join(&output_name);

    // Save calibrated frame
    light.save(&output_path)?;

    Ok(output_path.display().to_string())
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if a FITS image is a CFA (color filter array) image
pub fn is_cfa_image(image: &FitsImage) -> bool {
    image.is_cfa()
}

/// Get the Bayer pattern from a FITS image
pub fn get_bayer_pattern(image: &FitsImage) -> Option<BayerPattern> {
    image.bayer_pattern().map(|s| match s.to_uppercase().as_str() {
        "RGGB" => BayerPattern::Rggb,
        "BGGR" => BayerPattern::Bggr,
        "GBRG" => BayerPattern::Gbrg,
        "GRBG" => BayerPattern::Grbg,
        _ => BayerPattern::Auto,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calibration_config_default() {
        let config = CalibrationConfig::default();
        assert!(config.use_bias);
        assert!(config.use_dark);
        assert!(config.use_flat);
        assert!(config.output_32bit);
    }

    #[test]
    fn test_debayer_method_conversion() {
        assert_eq!(
            InterpolationMethod::from(DebayerMethod::Vng),
            InterpolationMethod::BayerVng
        );
        assert_eq!(
            InterpolationMethod::from(DebayerMethod::Amaze),
            InterpolationMethod::BayerAmaze
        );
    }

    #[test]
    fn test_bayer_pattern_conversion() {
        assert_eq!(
            SensorPattern::from(BayerPattern::Rggb),
            SensorPattern::BayerRggb
        );
        assert_eq!(
            SensorPattern::from(BayerPattern::Auto),
            SensorPattern::BayerNone
        );
    }
}
