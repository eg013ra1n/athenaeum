//! Frame quality measurement
//!
//! This module provides functions for measuring image quality metrics
//! such as FWHM, roundness, star count, and background noise.
//! These metrics are used for frame selection and weighting during stacking.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi::{self, PsfStar, Rectangle, StarFinderParams, StarProfile, GFALSE};
use crate::stacking::fits::FitsImage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// =============================================================================
// Configuration Types
// =============================================================================

/// Configuration for star detection and quality measurement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityConfig {
    /// Detection sigma threshold (higher = fewer stars detected)
    pub detection_sigma: f64,
    /// Maximum stars to analyze
    pub max_stars: u32,
    /// Channel to analyze (None = luminance/first channel)
    pub channel: Option<u32>,
    /// Minimum FWHM filter (pixels) - stars below this are likely hot pixels
    pub fwhm_min: Option<f64>,
    /// Maximum FWHM filter (pixels) - stars above this are likely extended objects
    pub fwhm_max: Option<f64>,
    /// Maximum roundness deviation (0 = perfect circle, 1 = 2:1 ellipse)
    pub roundness_limit: f64,
    /// PSF profile type for fitting
    pub profile: PsfProfile,
}

impl Default for QualityConfig {
    fn default() -> Self {
        QualityConfig {
            detection_sigma: 3.0,
            max_stars: 500,
            channel: None, // Use first channel
            fwhm_min: Some(1.0),
            fwhm_max: Some(20.0),
            roundness_limit: 0.5,
            profile: PsfProfile::Gaussian,
        }
    }
}

/// PSF profile type for star fitting
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PsfProfile {
    /// Gaussian PSF model
    Gaussian,
    /// Moffat PSF model (better for atmospheric seeing)
    Moffat,
}

impl Default for PsfProfile {
    fn default() -> Self {
        PsfProfile::Gaussian
    }
}

impl From<PsfProfile> for StarProfile {
    fn from(profile: PsfProfile) -> Self {
        match profile {
            PsfProfile::Gaussian => StarProfile::Gaussian,
            PsfProfile::Moffat => StarProfile::Moffat,
        }
    }
}

// =============================================================================
// Result Types
// =============================================================================

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
    pub star_count: u32,
    /// Number of stars used for quality calculation (after filtering)
    pub stars_used: u32,
    /// Mean background level (normalized 0-1)
    pub background_level: f64,
    /// Background noise (standard deviation, normalized)
    pub background_noise: f64,
    /// Combined quality score (lower is better)
    pub quality_score: f64,
    /// Whether frame has saturated stars
    pub has_saturated_stars: bool,
}

impl QualityMetrics {
    /// Calculate quality score from metrics
    ///
    /// Score prioritizes:
    /// 1. Lower FWHM (sharper images)
    /// 2. More stars (better signal)
    /// 3. Lower noise
    fn calculate_score(&self) -> f64 {
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

/// Individual star measurement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarMeasurement {
    /// Star X position
    pub x: f64,
    /// Star Y position
    pub y: f64,
    /// FWHM in X
    pub fwhm_x: f64,
    /// FWHM in Y
    pub fwhm_y: f64,
    /// Average FWHM
    pub fwhm: f64,
    /// Roundness
    pub roundness: f64,
    /// Amplitude (brightness)
    pub amplitude: f64,
    /// Background level at star
    pub background: f64,
    /// Signal to noise ratio
    pub snr: f64,
    /// Whether star is saturated
    pub is_saturated: bool,
}

/// Result of batch quality measurement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchQualityResult {
    /// Metrics for each frame (path, metrics)
    pub metrics: Vec<(String, QualityMetrics)>,
    /// Index of best frame (lowest quality_score)
    pub best_frame_index: usize,
    /// Path to best frame
    pub best_frame_path: String,
    /// Average FWHM across all frames
    pub average_fwhm: f64,
    /// Median FWHM across all frames
    pub median_fwhm: f64,
    /// Number of frames that failed measurement
    pub failed_count: usize,
}

// =============================================================================
// Quality Measurement Functions
// =============================================================================

/// Measure quality metrics for a single frame
///
/// Detects stars and calculates FWHM, roundness, and other quality metrics.
///
/// # Arguments
///
/// * `frame` - The FITS image to analyze
/// * `config` - Quality measurement configuration
///
/// # Returns
///
/// Quality metrics for the frame
pub fn measure_frame_quality(
    frame: &FitsImage,
    config: &QualityConfig,
) -> StackingResult<QualityMetrics> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    let (width, height, channels) = frame.dimensions();
    let channel = config.channel.unwrap_or(0).min(channels.saturating_sub(1));

    #[cfg(feature = "siril-ffi")]
    {
        // Set up star finder parameters
        let mut sf_params = StarFinderParams {
            sigma: config.detection_sigma,
            roundness_limit: config.roundness_limit,
            max_stars: config.max_stars as i32,
            min_beta: 1.5,
            max_beta: 10.0,
            min_a: 0.0,
            max_a: 1.0,
            profile: config.profile.into(),
        };

        // Detect stars
        let mut nb_stars: i32 = 0;
        let stars = unsafe {
            // Create image struct for peaker
            // Note: In real implementation, this would properly wrap the fits
            ffi::peaker(
                frame.as_ptr() as *mut std::ffi::c_void,
                channel as i32,
                &mut sf_params,
                &mut nb_stars,
                std::ptr::null_mut(), // whole image
                GFALSE,               // no timing
                ffi::GTRUE,           // limit stars
                config.max_stars as i32,
                config.profile.into(),
                0, // multi-threaded
            )
        };

        if stars.is_null() || nb_stars <= 0 {
            return Err(StackingError::NoStarsDetected);
        }

        // Process stars and calculate metrics
        let metrics = unsafe {
            calculate_metrics_from_stars(stars, nb_stars as usize, frame, config)?
        };

        // Clean up
        unsafe {
            ffi::free_fitted_stars(stars);
        }

        Ok(metrics)
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        // Return stub metrics for testing without FFI
        Err(StackingError::NotInitialized)
    }
}

/// Quick FWHM measurement (faster than full quality analysis)
///
/// Uses a simplified algorithm to quickly estimate average FWHM.
///
/// # Arguments
///
/// * `frame` - The FITS image to analyze
/// * `channel` - Channel to analyze (0 for mono)
///
/// # Returns
///
/// Average FWHM in pixels
pub fn measure_fwhm_quick(frame: &FitsImage, channel: u32) -> StackingResult<f64> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    #[cfg(feature = "siril-ffi")]
    {
        let fwhm = unsafe { ffi::measure_image_FWHM(frame.as_ptr(), channel as i32) };

        if fwhm < 0.0 {
            return Err(StackingError::NoStarsDetected);
        }

        Ok(fwhm as f64)
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Measure quality for multiple frames and find the best one
///
/// # Arguments
///
/// * `frame_paths` - Paths to frames to analyze
/// * `config` - Quality measurement configuration
/// * `progress` - Optional progress callback (current, total)
///
/// # Returns
///
/// Batch result with metrics for all frames and best frame selection
pub fn measure_quality_batch<P: AsRef<Path>>(
    frame_paths: &[P],
    config: &QualityConfig,
    progress: Option<Box<dyn Fn(usize, usize, &str)>>,
) -> StackingResult<BatchQualityResult> {
    if frame_paths.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    let mut metrics = Vec::with_capacity(frame_paths.len());
    let mut failed_count = 0;

    for (i, path) in frame_paths.iter().enumerate() {
        let path_ref = path.as_ref();

        if let Some(ref cb) = progress {
            cb(
                i + 1,
                frame_paths.len(),
                &format!(
                    "Measuring {}",
                    path_ref.file_name().unwrap_or_default().to_string_lossy()
                ),
            );
        }

        match FitsImage::open(path_ref) {
            Ok(frame) => match measure_frame_quality(&frame, config) {
                Ok(m) => {
                    metrics.push((path_ref.display().to_string(), m));
                }
                Err(e) => {
                    eprintln!("Quality measurement failed for {}: {}", path_ref.display(), e);
                    failed_count += 1;
                }
            },
            Err(e) => {
                eprintln!("Failed to open {}: {}", path_ref.display(), e);
                failed_count += 1;
            }
        }
    }

    if metrics.is_empty() {
        return Err(StackingError::NoStarsDetected);
    }

    // Find best frame (lowest quality score)
    let (best_index, _) = metrics
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            a.1.quality_score
                .partial_cmp(&b.1.quality_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();

    // Calculate average and median FWHM
    let mut fwhms: Vec<f64> = metrics.iter().map(|(_, m)| m.fwhm).collect();
    fwhms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let average_fwhm = fwhms.iter().sum::<f64>() / fwhms.len() as f64;
    let median_fwhm = if fwhms.len() % 2 == 0 {
        (fwhms[fwhms.len() / 2 - 1] + fwhms[fwhms.len() / 2]) / 2.0
    } else {
        fwhms[fwhms.len() / 2]
    };

    Ok(BatchQualityResult {
        best_frame_index: best_index,
        best_frame_path: metrics[best_index].0.clone(),
        average_fwhm,
        median_fwhm,
        failed_count,
        metrics,
    })
}

/// Select best reference frame from a set of frames
///
/// # Arguments
///
/// * `frame_paths` - Paths to candidate frames
/// * `config` - Quality measurement configuration
///
/// # Returns
///
/// Index and path of the best frame
pub fn select_best_reference<P: AsRef<Path>>(
    frame_paths: &[P],
    config: &QualityConfig,
) -> StackingResult<(usize, PathBuf)> {
    let result = measure_quality_batch(frame_paths, config, None)?;

    Ok((
        result.best_frame_index,
        PathBuf::from(&result.best_frame_path),
    ))
}

// =============================================================================
// Internal Helper Functions
// =============================================================================

#[cfg(feature = "siril-ffi")]
unsafe fn calculate_metrics_from_stars(
    stars: *mut *mut PsfStar,
    nb_stars: usize,
    frame: &FitsImage,
    config: &QualityConfig,
) -> StackingResult<QualityMetrics> {
    let mut total_fwhm_x = 0.0;
    let mut total_fwhm_y = 0.0;
    let mut total_roundness = 0.0;
    let mut total_background = 0.0;
    let mut stars_used = 0u32;
    let mut has_saturated = false;

    for i in 0..nb_stars {
        let star_ptr = *stars.add(i);
        if star_ptr.is_null() {
            continue;
        }

        let star = &*star_ptr;

        // Filter by FWHM bounds
        let fwhm = star.fwhm();
        if let Some(min) = config.fwhm_min {
            if fwhm < min {
                continue;
            }
        }
        if let Some(max) = config.fwhm_max {
            if fwhm > max {
                continue;
            }
        }

        // Filter by roundness
        let roundness = star.roundness();
        if (1.0 - roundness).abs() > config.roundness_limit {
            continue;
        }

        // Accumulate statistics
        total_fwhm_x += star.fwhmx;
        total_fwhm_y += star.fwhmy;
        total_roundness += roundness;
        total_background += star.b;
        stars_used += 1;

        if star.has_saturated != 0 {
            has_saturated = true;
        }
    }

    if stars_used == 0 {
        return Err(StackingError::NoStarsDetected);
    }

    let avg_fwhm_x = total_fwhm_x / stars_used as f64;
    let avg_fwhm_y = total_fwhm_y / stars_used as f64;
    let avg_fwhm = (avg_fwhm_x * avg_fwhm_y).sqrt();
    let avg_roundness = total_roundness / stars_used as f64;
    let avg_background = total_background / stars_used as f64;

    // Calculate FWHM in arcseconds if possible
    let fwhm_arcsec = frame.pixel_scale().map(|scale| avg_fwhm * scale);

    // Estimate background noise (simplified - would use image statistics in real impl)
    let background_noise = 0.01; // Placeholder

    let mut metrics = QualityMetrics {
        fwhm: avg_fwhm,
        fwhm_arcsec,
        fwhm_x: avg_fwhm_x,
        fwhm_y: avg_fwhm_y,
        roundness: avg_roundness,
        star_count: nb_stars as u32,
        stars_used,
        background_level: avg_background,
        background_noise,
        quality_score: 0.0, // Will be calculated
        has_saturated_stars: has_saturated,
    };

    metrics.quality_score = metrics.calculate_score();

    Ok(metrics)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quality_config_default() {
        let config = QualityConfig::default();
        assert_eq!(config.detection_sigma, 3.0);
        assert_eq!(config.max_stars, 500);
        assert!(config.fwhm_min.is_some());
    }

    #[test]
    fn test_quality_metrics_score() {
        let metrics = QualityMetrics {
            fwhm: 3.0,
            fwhm_arcsec: Some(2.0),
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            roundness: 1.0,
            star_count: 100,
            stars_used: 80,
            background_level: 0.1,
            background_noise: 0.01,
            quality_score: 0.0,
            has_saturated_stars: false,
        };

        let score = metrics.calculate_score();
        assert!(score > 0.0);
        assert!(score < 10.0); // Reasonable range for good frame
    }

    #[test]
    fn test_psf_profile_conversion() {
        assert_eq!(
            StarProfile::from(PsfProfile::Gaussian),
            StarProfile::Gaussian
        );
        assert_eq!(StarProfile::from(PsfProfile::Moffat), StarProfile::Moffat);
    }
}
