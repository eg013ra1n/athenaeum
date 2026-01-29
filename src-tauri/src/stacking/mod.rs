//! Stacking module - Direct FFI bindings to Siril C stacking algorithms
//!
//! This module provides Rust wrappers around the Siril astrophotography processing
//! library for direct integration without CLI subprocess execution.
//!
//! # Module Organization
//!
//! - `ffi` - Raw C FFI bindings (types and functions)
//! - `fits` - Safe Rust wrapper for FITS image handling
//! - `error` - Error types for stacking operations
//! - `config` - Configuration types for stacking operations
//! - `sequence` - Sequence management for batch operations
//! - `master` - Master calibration frame creation
//! - `calibration` - Light frame calibration (bias, dark, flat subtraction)
//! - `quality` - Frame quality measurement (FWHM, roundness, star count)
//! - `registration` - Plate-solving and image alignment
//!
//! # Example
//!
//! ```rust,ignore
//! use athenaeum::stacking::{
//!     initialize, FitsImage, ImageSequence,
//!     create_master_bias, create_master_dark, create_master_flat,
//!     MasterConfig, FlatConfig,
//! };
//!
//! // Initialize the library
//! initialize()?;
//!
//! // Create master bias from bias frames
//! let bias_paths = vec!["bias_001.fits", "bias_002.fits", "bias_003.fits"];
//! let bias_result = create_master_bias(
//!     &bias_paths,
//!     Path::new("master_bias.fits"),
//!     &MasterConfig::median(),
//!     None,
//! )?;
//!
//! // Create master dark from dark frames
//! let dark_paths = vec!["dark_001.fits", "dark_002.fits", "dark_003.fits"];
//! let dark_result = create_master_dark(
//!     &dark_paths,
//!     Path::new("master_dark.fits"),
//!     &MasterConfig::median(),
//!     None,
//! )?;
//!
//! // Create master flat (calibrated with bias)
//! let flat_paths = vec!["flat_001.fits", "flat_002.fits", "flat_003.fits"];
//! let flat_config = FlatConfig::default()
//!     .with_bias("master_bias.fits");
//! let flat_result = create_master_flat(
//!     &flat_paths,
//!     Path::new("master_flat.fits"),
//!     &flat_config,
//!     None,
//! )?;
//! ```

pub mod calibration;
pub mod config;
pub mod error;
pub mod executor;
pub mod ffi;
pub mod fits;
pub mod master;
pub mod quality;
pub mod registration;
pub mod sequence;
pub mod stacking;

// Re-export commonly used types
pub use calibration::{
    calibrate_light, calibrate_lights_batch, debayer_frame, get_bayer_pattern, is_cfa_image,
    BatchCalibrationConfig, BatchCalibrationResult, BayerPattern, CalibrationConfig,
    CalibrationResult, DarkOptimization, DebayerConfig, DebayerMethod, FlatNormalization,
};
pub use config::{
    DrizzleConfig, FlatConfig, LightStackConfig, MasterConfig, MasterResult,
    NormalizationMethod, ProgressCallback, RejectionAlgorithm, RejectionConfig,
    StackingMethod, WeightingMethod,
};
pub use error::{StackingError, StackingResult};
pub use fits::FitsImage;
pub use master::{
    check_memory_available, create_master_bias, create_master_dark, create_master_flat,
    estimate_memory_requirement,
};
pub use quality::{
    measure_frame_quality, measure_fwhm_quick, measure_quality_batch, select_best_reference,
    BatchQualityResult, PsfProfile, QualityConfig, QualityMetrics, StarMeasurement,
};
pub use registration::{
    apply_registration, calculate_image_scale, platesolve_batch, platesolve_frame,
    register_astrometric, register_star_alignment, BatchPlateSolveResult, Catalogue, Framing,
    HomographyData, Interpolation, MagnitudeLimit, PlateSolveConfig, PlateSolveResult,
    RegistrationConfig, RegistrationResult, Solver, Transformation, WcsSolution,
};
pub use sequence::{ImageSequence, SequenceSummary};
pub use stacking::{
    calculate_weights, estimate_output_size, group_frames, recommended_config, stack_all_groups,
    stack_group, ExcludedFrame, FailedGroup, FrameGroup, FrameMetadata, GroupingConfig,
    MultiGroupStackResult, StackResult,
};
pub use executor::{
    CalibrationSettings, DarkOptimizationMode, DebayerMethodOption, ExecutorConfig,
    FfiExecutor, FfiStepResult, FlatNormalizationMode, FramingOption, InterpolationOption,
    NormalizationOption, ProgressCallback as ExecutorProgressCallback, RegistrationSettings,
    RejectionOption, StackingMethodOption, StackingSettings, StepStats, WeightingOption,
};

use std::sync::atomic::{AtomicBool, Ordering};

/// Global initialization state
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize the stacking library.
///
/// This must be called before any stacking operations. It is safe to call
/// multiple times - subsequent calls are no-ops.
///
/// # Returns
///
/// `Ok(())` if initialization succeeded, or `Err` with details on failure.
pub fn initialize() -> Result<(), StackingError> {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return Ok(()); // Already initialized
    }

    // TODO: Initialize Siril global state when FFI is enabled
    // - Set up memory allocators
    // - Configure thread pools
    // - Initialize FITS library

    Ok(())
}

/// Check if the stacking library has been initialized.
pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::SeqCst)
}

/// Check if FFI is available (Siril library linked)
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
        // Without the feature flag, FFI should not be available
        #[cfg(not(feature = "siril-ffi"))]
        assert!(!is_ffi_available());
    }
}
