//! Master calibration frame creation
//!
//! This module provides functions for creating master calibration frames
//! (bias, dark, flat) by stacking multiple raw frames.

use crate::stacking::config::{FlatConfig, MasterConfig, MasterResult, ProgressCallback};
use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi::{self, Normalization, Rejection, StackingArgs, GFALSE, GTRUE};
use crate::stacking::fits::FitsImage;
use crate::stacking::sequence::ImageSequence;
use std::path::Path;
use std::time::Instant;

/// Create a master bias frame from a sequence of bias frames
///
/// Master bias is created by stacking (typically median) all bias frames.
/// No calibration is applied to bias frames.
///
/// # Arguments
///
/// * `bias_frames` - Paths to the raw bias frames
/// * `output_path` - Path for the output master bias
/// * `config` - Stacking configuration
/// * `progress` - Optional progress callback
///
/// # Returns
///
/// Result containing the master frame info or an error.
///
/// # Example
///
/// ```rust,ignore
/// use crate::stacking::{create_master_bias, MasterConfig};
///
/// let bias_paths = vec!["bias_001.fits", "bias_002.fits", "bias_003.fits"];
/// let result = create_master_bias(
///     &bias_paths,
///     "master_bias.fits",
///     &MasterConfig::median(),
///     None,
/// )?;
/// println!("Created master bias from {} frames", result.frame_count);
/// ```
pub fn create_master_bias<P: AsRef<Path>>(
    bias_frames: &[P],
    output_path: &Path,
    config: &MasterConfig,
    progress: Option<ProgressCallback>,
) -> StackingResult<MasterResult> {
    let start = Instant::now();

    // Create sequence and validate
    let mut sequence = ImageSequence::from_paths(bias_frames)?;

    if let Some(ref cb) = progress {
        cb(0.0, "Validating bias frames...");
    }

    sequence.validate_dimensions()?;

    if let Some(ref cb) = progress {
        cb(0.1, "Stacking bias frames...");
    }

    // Perform stacking
    let result = stack_frames(&sequence, output_path, config, progress.as_ref())?;

    Ok(MasterResult {
        output_path: output_path.display().to_string(),
        frame_count: sequence.len(),
        processing_time_ms: start.elapsed().as_millis() as u64,
        total_exposure_seconds: None, // Bias has no exposure time
    })
}

/// Create a master dark frame from a sequence of dark frames
///
/// Master dark is created by stacking (typically median) all dark frames.
/// Dark frames should all have the same exposure time.
///
/// # Arguments
///
/// * `dark_frames` - Paths to the raw dark frames
/// * `output_path` - Path for the output master dark
/// * `config` - Stacking configuration
/// * `progress` - Optional progress callback
///
/// # Returns
///
/// Result containing the master frame info or an error.
pub fn create_master_dark<P: AsRef<Path>>(
    dark_frames: &[P],
    output_path: &Path,
    config: &MasterConfig,
    progress: Option<ProgressCallback>,
) -> StackingResult<MasterResult> {
    let start = Instant::now();

    // Create sequence and validate
    let mut sequence = ImageSequence::from_paths(dark_frames)?;

    if let Some(ref cb) = progress {
        cb(0.0, "Validating dark frames...");
    }

    sequence.validate_dimensions()?;

    // Get total exposure time
    let exposures = sequence.exposure_times();
    let total_exposure: f64 = exposures.iter().filter_map(|e| *e).sum();

    if let Some(ref cb) = progress {
        cb(0.1, "Stacking dark frames...");
    }

    // Perform stacking
    let result = stack_frames(&sequence, output_path, config, progress.as_ref())?;

    Ok(MasterResult {
        output_path: output_path.display().to_string(),
        frame_count: sequence.len(),
        processing_time_ms: start.elapsed().as_millis() as u64,
        total_exposure_seconds: Some(total_exposure),
    })
}

/// Create a master flat frame from a sequence of flat frames
///
/// Master flat can optionally be calibrated with dark/bias before stacking.
/// The final flat is normalized and can have CFA equalization applied for OSC.
///
/// # Arguments
///
/// * `flat_frames` - Paths to the raw flat frames
/// * `output_path` - Path for the output master flat
/// * `config` - Flat-specific configuration (includes calibration options)
/// * `progress` - Optional progress callback
///
/// # Returns
///
/// Result containing the master frame info or an error.
pub fn create_master_flat<P: AsRef<Path>>(
    flat_frames: &[P],
    output_path: &Path,
    config: &FlatConfig,
    progress: Option<ProgressCallback>,
) -> StackingResult<MasterResult> {
    let start = Instant::now();

    // Create sequence and validate
    let mut sequence = ImageSequence::from_paths(flat_frames)?;

    if let Some(ref cb) = progress {
        cb(0.0, "Validating flat frames...");
    }

    sequence.validate_dimensions()?;

    // Load calibration masters if provided
    let dark = if let Some(ref dark_path) = config.calibrate_with_dark {
        if let Some(ref cb) = progress {
            cb(0.05, "Loading master dark...");
        }
        Some(FitsImage::open(dark_path)?)
    } else {
        None
    };

    let bias = if let Some(ref bias_path) = config.calibrate_with_bias {
        if let Some(ref cb) = progress {
            cb(0.08, "Loading master bias...");
        }
        Some(FitsImage::open(bias_path)?)
    } else {
        None
    };

    // If we have calibration frames, we need to calibrate each flat first
    // For now, we'll proceed with direct stacking
    // TODO: Implement flat calibration before stacking

    if let Some(ref cb) = progress {
        cb(0.1, "Stacking flat frames...");
    }

    // Perform stacking
    let result = stack_frames(&sequence, output_path, &config.stacking, progress.as_ref())?;

    // TODO: Apply CFA equalization if config.equalize_cfa is true

    Ok(MasterResult {
        output_path: output_path.display().to_string(),
        frame_count: sequence.len(),
        processing_time_ms: start.elapsed().as_millis() as u64,
        total_exposure_seconds: None,
    })
}

/// Internal function to perform the actual stacking operation
fn stack_frames(
    sequence: &ImageSequence,
    output_path: &Path,
    config: &MasterConfig,
    progress: Option<&ProgressCallback>,
) -> StackingResult<FitsImage> {
    // Without FFI, we can only return an error or stub
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    #[cfg(feature = "siril-ffi")]
    {
        // This is where we would call the actual Siril stacking functions
        // For now, we document the intended flow

        /*
        // 1. Create internal sequence from paths
        let mut siril_seq = create_siril_sequence(sequence)?;

        // 2. Configure stacking args
        let mut args = StackingArgs::new();
        args.seq = &mut siril_seq;
        args.ref_image = sequence.reference_index() as i32;
        args.nb_images_to_stack = sequence.len() as i32;

        // Set stacking method
        args.method = match config.method {
            StackingMethod::Median => stack_median as *const _,
            StackingMethod::Mean => stack_mean_with_rejection as *const _,
            StackingMethod::Sum => stack_addsum as *const _,
        };

        // Set rejection
        if let Some(ref rejection) = config.rejection {
            args.type_of_rejection = match rejection.algorithm {
                RejectionAlgorithm::Sigma => Rejection::Sigma,
                RejectionAlgorithm::Mad => Rejection::Mad,
                RejectionAlgorithm::Winsorized => Rejection::Winsorized,
                _ => Rejection::NoRejec,
            };
            args.sig[0] = rejection.sigma_low as f32;
            args.sig[1] = rejection.sigma_high as f32;
        }

        // Set output options
        args.use_32bit_output = if config.output_32bit { GTRUE } else { GFALSE };

        // 3. Run stacking
        let result = unsafe { ffi::main_stack(&mut args) };

        if args.retval != 0 {
            return Err(StackingError::from_siril_code(args.retval));
        }

        // 4. Save result
        let output = FitsImage::from_raw(&mut args.result, true);
        output.save(output_path)?;

        Ok(output)
        */

        Err(StackingError::NotInitialized)
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Estimate memory requirements for stacking
///
/// # Arguments
///
/// * `frame_count` - Number of frames to stack
/// * `width` - Image width
/// * `height` - Image height
/// * `channels` - Number of channels
/// * `is_float` - Whether using 32-bit float data
///
/// # Returns
///
/// Estimated memory in bytes
pub fn estimate_memory_requirement(
    frame_count: usize,
    width: u32,
    height: u32,
    channels: u32,
    is_float: bool,
) -> u64 {
    let pixels_per_frame = width as u64 * height as u64 * channels as u64;
    let bytes_per_pixel = if is_float { 4 } else { 2 };

    // Memory for all frames (loaded in memory for median)
    let frame_memory = pixels_per_frame * bytes_per_pixel * frame_count as u64;

    // Additional memory for working buffers (roughly 2x one frame)
    let buffer_memory = pixels_per_frame * bytes_per_pixel * 2;

    frame_memory + buffer_memory
}

/// Check if there is enough memory for the stacking operation
pub fn check_memory_available(required_bytes: u64) -> StackingResult<()> {
    // On most systems, we can't reliably check available memory
    // This is a placeholder for platform-specific implementations

    // Warn if requiring more than 16GB
    if required_bytes > 16 * 1024 * 1024 * 1024 {
        return Err(StackingError::OutOfMemory {
            required_mb: required_bytes / (1024 * 1024),
            available_mb: 0, // Unknown
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_estimation() {
        // 10 frames, 4656x3520, mono, 16-bit
        let mem = estimate_memory_requirement(10, 4656, 3520, 1, false);
        // Should be around 328MB for frames + 65MB for buffers
        assert!(mem > 300_000_000);
        assert!(mem < 500_000_000);
    }

    #[test]
    fn test_memory_estimation_rgb() {
        // 10 frames, 4656x3520, RGB, 32-bit
        let mem = estimate_memory_requirement(10, 4656, 3520, 3, true);
        // Should be around 1.9GB
        assert!(mem > 1_500_000_000);
        assert!(mem < 2_500_000_000);
    }
}
