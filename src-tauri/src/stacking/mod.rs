//! Astrophotography stacking module - Pure Rust implementation
//!
//! This module provides a complete image stacking pipeline for astrophotography,
//! implemented entirely in Rust with no C/FFI dependencies.
//!
//! # Module Organization
//!
//! ## Core Infrastructure
//! - `error` - Error types for stacking operations
//! - `ffi` - Type definitions (originally for FFI, now used as data types)
//! - `fits` - FITS image I/O using the fitsio crate
//! - `config` - Configuration types for stacking operations
//!
//! ## Image Operations
//! - `ops` - Basic image arithmetic (add, subtract, multiply, divide)
//! - `stats` - Statistical functions and rejection algorithms
//!
//! ## Preprocessing
//! - `calibrate_impl` - Core calibration (bias, dark, flat correction)
//! - `debayer` - CFA/Bayer pattern demosaicing
//! - `normalize` - Frame normalization for stacking
//!
//! ## Star Detection & Quality
//! - `stars` - Star detection and measurement
//! - `quality_impl` - Frame quality assessment (FWHM, roundness)
//!
//! ## Registration
//! - `matching` - Star pattern matching for alignment
//! - `transform` - Image transformation (affine, interpolation)
//! - `platesolve` - External plate solver integration
//!
//! ## Stacking
//! - `stack_impl` - Core stacking algorithms (median, mean, rejection)
//!
//! ## High-Level API
//! - `calibration` - High-level calibration workflow
//! - `master` - Master frame creation
//! - `quality` - Quality measurement workflow
//! - `registration` - Registration workflow
//! - `stacking` - Stacking workflow
//! - `sequence` - Batch sequence management
//! - `executor` - Pipeline executor
//!
//! # Example
//!
//! ```rust,ignore
//! use athenaeum::stacking::{
//!     FitsImage, stack_median, calibrate_frame, CalibrateConfig,
//! };
//!
//! // Load images
//! let mut lights: Vec<FitsImage> = paths.iter()
//!     .map(|p| FitsImage::open(p))
//!     .collect::<Result<_, _>>()?;
//!
//! let master_bias = FitsImage::open("master_bias.fits")?;
//! let master_dark = FitsImage::open("master_dark.fits")?;
//! let master_flat = FitsImage::open("master_flat.fits")?;
//!
//! // Calibrate each light frame
//! let config = CalibrateConfig::default();
//! for light in &mut lights {
//!     calibrate_frame(light, Some(&master_bias), Some(&master_dark), Some(&master_flat), &config)?;
//! }
//!
//! // Stack all calibrated frames
//! let refs: Vec<&FitsImage> = lights.iter().collect();
//! let stacked = stack_median(&refs)?;
//! stacked.save("result.fits")?;
//! ```

// Core modules
pub mod error;
pub mod ffi;
pub mod fits;
pub mod config;

// Image operations
pub mod ops;
pub mod stats;

// Preprocessing
pub mod calibrate_impl;
pub mod debayer;
pub mod normalize;

// Star detection and quality
pub mod stars;
pub mod quality_impl;

// Registration
pub mod matching;
pub mod transform;
pub mod platesolve;

// Stacking
pub mod stack_impl;

// High-level API modules
pub mod calibration;
pub mod master;
pub mod quality;
pub mod registration;
pub mod stacking;
pub mod sequence;
pub mod executor;

// Re-export error types
pub use error::{StackingError, StackingResult};

// Re-export FITS image type
pub use fits::FitsImage;

// Re-export configuration types
pub use config::{
    DrizzleConfig, FlatConfig, LightStackConfig, MasterConfig, MasterResult,
    NormalizationMethod, ProgressCallback, RejectionAlgorithm, RejectionConfig,
    StackingMethod, WeightingMethod,
};

// Re-export calibration types from calibration module (high-level API)
pub use calibration::{
    calibrate_light, calibrate_lights_batch, debayer_frame, get_bayer_pattern, is_cfa_image,
    BatchCalibrationConfig, BatchCalibrationResult, CalibrationConfig,
    CalibrationResult, DebayerConfig,
};

// Re-export calibration implementation types
pub use calibrate_impl::{
    DarkOptimization, FlatNormalization,
    CalibrationConfig as CalibrateConfig,
    subtract_bias, subtract_dark, apply_flat, calibrate_frame,
};

// Re-export debayering
pub use debayer::{
    BayerPattern, DebayerMethod,
    detect_bayer_pattern, is_bayer, debayer as debayer_image,
    debayer_bilinear, debayer_vng,
};

// Re-export master frame functions
pub use master::{
    check_memory_available, create_master_bias, create_master_dark, create_master_flat,
    estimate_memory_requirement,
};

// Re-export quality measurement (high-level API)
pub use quality::{
    measure_frame_quality, measure_fwhm_quick, measure_quality_batch,
    select_best_reference as select_best_reference_old,
    BatchQualityResult, PsfProfile, QualityConfig, QualityMetrics, StarMeasurement,
};

// Re-export quality implementation
pub use quality_impl::{
    QualityMetrics as QualityMetricsImpl, QualityConfig as QualityMeasureConfig,
    measure_quality, select_best_reference,
    compute_quality_weights, compute_fwhm_weights,
};

// Re-export registration (high-level API)
pub use registration::{
    apply_registration as apply_registration_old, calculate_image_scale,
    platesolve_batch, platesolve_frame,
    register_astrometric, register_star_alignment,
    BatchPlateSolveResult, Catalogue, Framing,
    HomographyData, Interpolation as InterpolationOld, MagnitudeLimit,
    PlateSolveConfig as PlateSolveConfigOld, PlateSolveResult as PlateSolveResultOld,
    RegistrationConfig, RegistrationResult, Solver, Transformation,
    WcsSolution as WcsSolutionOld,
};

// Re-export registration implementation
pub use matching::{Triangle, StarMatch, build_triangles, match_stars};
pub use transform::{
    AffineTransform, Interpolation,
    estimate_affine, apply_transform,
};
pub use platesolve::{
    PlateSolveConfig, PlateSolveResult,
    solve_with_astrometry_net, read_wcs_solution,
};

// Re-export sequence management
pub use sequence::{ImageSequence, SequenceSummary};

// Re-export stacking (high-level API)
pub use stacking::{
    calculate_weights, estimate_output_size, group_frames, recommended_config,
    stack_all_groups, stack_group,
    ExcludedFrame, FailedGroup, FrameGroup, FrameMetadata, GroupingConfig,
    MultiGroupStackResult, StackResult,
};

// Re-export stacking implementation
pub use stack_impl::{
    stack_median, stack_mean, stack_sum, stack_min, stack_max,
    stack_mean_sigma, stack_mean_winsorized, stack_mean_mad,
    stack_weighted_mean,
};

// Re-export normalization
pub use normalize::{
    NormMethod, NormCoefficients,
    normalize_to_reference, compute_normalization_factor,
};

// Re-export image operations
pub use ops::{
    image_add, image_subtract, image_multiply, image_divide,
    image_scale, image_clip, image_normalize, image_invert,
};

// Re-export statistics
pub use stats::{
    mean, median, std_dev, mad,
    sigma_clip, winsorized_sigma_clip, mad_clip,
    compute_image_stats, compute_background_noise,
};

// Re-export star detection
pub use stars::{
    Star, StarFinderConfig,
    find_stars, measure_fwhm, compute_roundness,
    compute_average_fwhm, compute_median_fwhm,
};

// Re-export executor
pub use executor::{
    CalibrationSettings, DarkOptimizationMode, DebayerMethodOption, ExecutorConfig,
    FfiExecutor, FfiStepResult, FlatNormalizationMode, FramingOption, InterpolationOption,
    NormalizationOption, ProgressCallback as ExecutorProgressCallback, RegistrationSettings,
    RejectionOption, StackingMethodOption, StackingSettings, StepStats, WeightingOption,
};

// Re-export FFI types (used as data structures)
pub use ffi::{
    Normalization, Rejection, StackMethod, WeightingType,
    SensorPattern, InterpolationMethod, StarProfile,
    Homography, ImStats, WcsSolution,
    GTRUE, GFALSE, gboolean,
};

use std::sync::atomic::{AtomicBool, Ordering};

/// Global initialization state
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the stacking library.
///
/// This function initializes any global state needed for stacking operations.
/// It is safe to call multiple times - subsequent calls are no-ops.
///
/// With the pure Rust implementation, this primarily ensures the module
/// is properly loaded.
///
/// # Returns
///
/// `Ok(())` if initialization succeeded, or `Err` with details on failure.
pub fn initialize() -> Result<(), StackingError> {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return Ok(()); // Already initialized
    }

    // Pure Rust implementation - no external initialization needed
    Ok(())
}

/// Check if the stacking library has been initialized.
pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::SeqCst)
}

/// Check if stacking is available (always true with pure Rust implementation)
pub fn is_ffi_available() -> bool {
    ffi::is_ffi_available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize() {
        assert!(initialize().is_ok());
        assert!(is_initialized());
        // Second call should also succeed
        assert!(initialize().is_ok());
    }

    #[test]
    fn test_ffi_available() {
        // With pure Rust implementation, FFI is always "available"
        assert!(is_ffi_available());
    }
}
