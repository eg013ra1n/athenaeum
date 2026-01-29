//! Light frame stacking
//!
//! This module provides functions for stacking registered light frames
//! into final images. It supports:
//!
//! - Frame grouping by filter and exposure time
//! - Multiple stacking methods (mean, median, sum)
//! - Pixel rejection algorithms (sigma, MAD, winsorized)
//! - Image normalization (additive, multiplicative)
//! - Quality-based weighting (FWHM, star count, noise)
//! - Drizzle upscaling

use crate::stacking::config::{
    LightStackConfig, NormalizationMethod, RejectionConfig, StackingMethod, WeightingMethod,
};
use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi;
use crate::stacking::fits::FitsImage;
use crate::stacking::quality::QualityMetrics;
use crate::stacking::sequence::ImageSequence;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// =============================================================================
// Frame Grouping Types
// =============================================================================

/// Configuration for grouping frames before stacking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupingConfig {
    /// Group frames by filter name
    pub group_by_filter: bool,
    /// Group frames by exposure time
    pub group_by_exposure: bool,
    /// Exposure time tolerance for grouping (percent)
    pub exposure_tolerance_percent: f64,
    /// Minimum frames required per group
    pub min_frames_per_group: usize,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        GroupingConfig {
            group_by_filter: true,
            group_by_exposure: false,
            exposure_tolerance_percent: 5.0,
            min_frames_per_group: 3,
        }
    }
}

/// Metadata for a frame used in grouping
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameMetadata {
    /// Frame ID (for database tracking)
    pub id: Option<i64>,
    /// Path to the frame file
    pub path: PathBuf,
    /// Filter name (e.g., "L", "R", "Ha")
    pub filter: String,
    /// Exposure time in seconds
    pub exposure_time: f64,
    /// Whether this is an OSC (one-shot color) frame
    pub is_osc: bool,
    /// Quality metrics (if measured)
    pub quality: Option<QualityMetrics>,
}

impl FrameMetadata {
    /// Create metadata from a FITS image
    pub fn from_fits(path: &Path, frame: &FitsImage) -> Self {
        FrameMetadata {
            id: None,
            path: path.to_path_buf(),
            filter: frame.filter().unwrap_or_default(),
            exposure_time: frame.exposure_time().unwrap_or(0.0),
            is_osc: frame.is_color(),
            quality: None,
        }
    }

    /// Set frame ID
    pub fn with_id(mut self, id: i64) -> Self {
        self.id = Some(id);
        self
    }

    /// Set quality metrics
    pub fn with_quality(mut self, quality: QualityMetrics) -> Self {
        self.quality = Some(quality);
        self
    }
}

/// A group of frames to be stacked together
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameGroup {
    /// Unique key for this group (e.g., "L_120s" or "Ha_300s")
    pub key: String,
    /// Filter name for this group
    pub filter_name: String,
    /// Representative exposure time for this group
    pub exposure_time: f64,
    /// Whether frames are OSC
    pub is_osc: bool,
    /// Frames in this group
    pub frames: Vec<FrameMetadata>,
}

impl FrameGroup {
    /// Get frame count
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Check if group is empty
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Get total exposure time in seconds
    pub fn total_exposure(&self) -> f64 {
        self.frames.iter().map(|f| f.exposure_time).sum()
    }

    /// Get frame paths
    pub fn paths(&self) -> Vec<PathBuf> {
        self.frames.iter().map(|f| f.path.clone()).collect()
    }

    /// Get frame IDs (if available)
    pub fn ids(&self) -> Vec<i64> {
        self.frames.iter().filter_map(|f| f.id).collect()
    }
}

// =============================================================================
// Stacking Result Types
// =============================================================================

/// Result of stacking a single group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackResult {
    /// Group that was stacked
    pub group_key: String,
    /// Filter name
    pub filter_name: String,
    /// Number of frames stacked
    pub frame_count: usize,
    /// Total exposure time in seconds
    pub total_exposure: f64,
    /// Output file path
    pub output_path: PathBuf,
    /// Processing time in milliseconds
    pub processing_time_ms: u64,
    /// Frames that were rejected/excluded
    pub excluded_frames: Vec<ExcludedFrame>,
}

/// Information about a frame that was excluded from stacking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedFrame {
    /// Frame path
    pub path: PathBuf,
    /// Reason for exclusion
    pub reason: String,
}

/// Result of stacking multiple groups
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiGroupStackResult {
    /// Results for each successfully stacked group
    pub stacks: Vec<StackResult>,
    /// Groups that failed to stack
    pub failed_groups: Vec<FailedGroup>,
    /// Total frames stacked across all groups
    pub total_frames_stacked: usize,
    /// Total exposure time across all groups
    pub total_exposure: f64,
}

/// Information about a group that failed to stack
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedGroup {
    /// Group key
    pub group_key: String,
    /// Error message
    pub error: String,
}

// =============================================================================
// Frame Grouping Functions
// =============================================================================

/// Group frames by filter and/or exposure time
///
/// # Arguments
///
/// * `frames` - Frame metadata to group
/// * `config` - Grouping configuration
///
/// # Returns
///
/// Vector of frame groups
pub fn group_frames(frames: &[FrameMetadata], config: &GroupingConfig) -> Vec<FrameGroup> {
    let mut groups: HashMap<String, FrameGroup> = HashMap::new();

    for frame in frames {
        let key = compute_group_key(frame, config);

        groups
            .entry(key.clone())
            .or_insert_with(|| FrameGroup {
                key: key.clone(),
                filter_name: frame.filter.clone(),
                exposure_time: frame.exposure_time,
                is_osc: frame.is_osc,
                frames: vec![],
            })
            .frames
            .push(frame.clone());
    }

    // Filter out groups with too few frames
    groups
        .into_values()
        .filter(|g| g.len() >= config.min_frames_per_group)
        .collect()
}

/// Compute a group key for a frame based on configuration
fn compute_group_key(frame: &FrameMetadata, config: &GroupingConfig) -> String {
    let mut parts = vec![];

    if config.group_by_filter && !frame.filter.is_empty() {
        parts.push(frame.filter.clone());
    }

    if config.group_by_exposure {
        // Round exposure to a canonical value for grouping
        let rounded = round_exposure(frame.exposure_time, config.exposure_tolerance_percent);
        parts.push(format!("{}s", rounded));
    }

    if frame.is_osc {
        parts.push("OSC".to_string());
    }

    if parts.is_empty() {
        "default".to_string()
    } else {
        parts.join("_")
    }
}

/// Round exposure time to a canonical value for grouping
fn round_exposure(exposure: f64, tolerance_percent: f64) -> f64 {
    // Common exposure times to snap to
    let common_exposures = [
        1.0, 2.0, 5.0, 10.0, 15.0, 20.0, 30.0, 45.0, 60.0, 90.0, 120.0, 180.0, 240.0, 300.0, 600.0,
        900.0, 1200.0, 1800.0,
    ];

    let tolerance_factor = tolerance_percent / 100.0;

    for &common in &common_exposures {
        let diff = (exposure - common).abs() / common;
        if diff <= tolerance_factor {
            return common;
        }
    }

    // No common exposure matched, round to nearest integer
    exposure.round()
}

// =============================================================================
// Weight Calculation
// =============================================================================

/// Calculate weights for frames based on quality metrics
///
/// # Arguments
///
/// * `metrics` - Quality metrics for each frame
/// * `method` - Weighting method to use
///
/// # Returns
///
/// Normalized weights (mean = 1.0)
pub fn calculate_weights(metrics: &[Option<QualityMetrics>], method: WeightingMethod) -> Vec<f64> {
    let count = metrics.len();

    if count == 0 {
        return vec![];
    }

    match method {
        WeightingMethod::None => vec![1.0; count],

        WeightingMethod::Fwhm => {
            // Weight inversely proportional to FWHM squared
            // Missing metrics get weight 1.0
            let weights: Vec<f64> = metrics
                .iter()
                .map(|m| {
                    m.as_ref()
                        .map(|q| {
                            if q.fwhm > 0.0 {
                                1.0 / (q.fwhm * q.fwhm)
                            } else {
                                1.0
                            }
                        })
                        .unwrap_or(1.0)
                })
                .collect();
            normalize_weights(&weights)
        }

        WeightingMethod::StarCount => {
            let weights: Vec<f64> = metrics
                .iter()
                .map(|m| m.as_ref().map(|q| q.stars_used as f64).unwrap_or(1.0))
                .collect();
            normalize_weights(&weights)
        }

        WeightingMethod::Noise => {
            // Weight inversely proportional to noise squared
            let weights: Vec<f64> = metrics
                .iter()
                .map(|m| {
                    m.as_ref()
                        .map(|q| {
                            if q.background_noise > 0.0 {
                                1.0 / (q.background_noise * q.background_noise)
                            } else {
                                1.0
                            }
                        })
                        .unwrap_or(1.0)
                })
                .collect();
            normalize_weights(&weights)
        }

        WeightingMethod::StackCount => {
            // All frames get weight 1.0 for stack count method
            // (this is handled differently in actual stacking)
            vec![1.0; count]
        }
    }
}

/// Normalize weights so mean = 1.0
fn normalize_weights(weights: &[f64]) -> Vec<f64> {
    if weights.is_empty() {
        return vec![];
    }

    let sum: f64 = weights.iter().sum();
    let mean = sum / weights.len() as f64;

    if mean > 0.0 {
        weights.iter().map(|w| w / mean).collect()
    } else {
        vec![1.0; weights.len()]
    }
}

// =============================================================================
// Stacking Functions
// =============================================================================

/// Stack a group of registered frames
///
/// # Arguments
///
/// * `group` - Frame group to stack
/// * `output_path` - Path for output stacked image
/// * `config` - Stacking configuration
/// * `progress` - Optional progress callback (progress 0.0-1.0, message)
///
/// # Returns
///
/// Stack result with statistics
pub fn stack_group(
    group: &FrameGroup,
    output_path: &Path,
    config: &LightStackConfig,
    progress: Option<&Box<dyn Fn(f32, &str) + Send>>,
) -> StackingResult<StackResult> {
    if group.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    let start_time = std::time::Instant::now();

    if let Some(cb) = progress {
        cb(0.0, &format!("Loading {} frames...", group.len()));
    }

    // Create sequence from frame paths
    let paths = group.paths();
    let sequence = ImageSequence::from_paths(&paths)?;

    // Calculate weights from quality metrics
    let metrics: Vec<Option<QualityMetrics>> =
        group.frames.iter().map(|f| f.quality.clone()).collect();
    let weights = calculate_weights(&metrics, config.weighting);

    if let Some(cb) = progress {
        cb(0.1, "Computing normalization...");
    }

    // Perform the actual stacking
    let stacked = stack_sequence(&sequence, config, Some(&weights), |p| {
        if let Some(cb) = progress {
            cb(0.1 + p * 0.8, "Stacking...");
        }
    })?;

    if let Some(cb) = progress {
        cb(0.9, "Saving result...");
    }

    // Save the stacked result
    stacked.save(output_path)?;

    let elapsed = start_time.elapsed();

    if let Some(cb) = progress {
        cb(1.0, "Complete");
    }

    Ok(StackResult {
        group_key: group.key.clone(),
        filter_name: group.filter_name.clone(),
        frame_count: group.len(),
        total_exposure: group.total_exposure(),
        output_path: output_path.to_path_buf(),
        processing_time_ms: elapsed.as_millis() as u64,
        excluded_frames: vec![],
    })
}

/// Stack an image sequence with the given configuration
fn stack_sequence(
    _sequence: &ImageSequence,
    _config: &LightStackConfig,
    _weights: Option<&[f64]>,
    progress: impl Fn(f32),
) -> StackingResult<FitsImage> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    // Note: Full implementation requires creating a Siril sequence structure
    // from the ImageSequence paths. This is a placeholder that shows the
    // structure of the FFI calls that would be made.

    #[cfg(feature = "siril-ffi")]
    {
        progress(0.1);

        // In a full implementation:
        // 1. Create a Siril sequence from paths
        // 2. Configure stacking args
        // 3. Call the appropriate stacking function
        // 4. Extract result

        // For now, this requires the siril-ffi feature and proper
        // sequence initialization which isn't fully implemented yet.

        progress(1.0);

        Err(StackingError::StackingFailed(
            "Full stacking implementation requires siril-ffi sequence integration".to_string(),
        ))
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        progress(1.0);
        // Without FFI, return a stub error
        Err(StackingError::NotInitialized)
    }
}

/// Stack multiple groups
///
/// # Arguments
///
/// * `groups` - Frame groups to stack
/// * `output_dir` - Directory for output files
/// * `config` - Stacking configuration
/// * `progress` - Optional progress callback (group_key, progress, message)
///
/// # Returns
///
/// Multi-group stack result
pub fn stack_all_groups(
    groups: &[FrameGroup],
    output_dir: &Path,
    config: &LightStackConfig,
    progress: Option<Box<dyn Fn(&str, f32, &str) + Send>>,
) -> StackingResult<MultiGroupStackResult> {
    if groups.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    // Ensure output directory exists
    if !output_dir.exists() {
        std::fs::create_dir_all(output_dir)?;
    }

    let mut stacks = Vec::with_capacity(groups.len());
    let mut failed_groups = Vec::new();
    let mut total_frames = 0;
    let mut total_exposure = 0.0;

    for (i, group) in groups.iter().enumerate() {
        let group_progress = (i as f32) / (groups.len() as f32);

        if let Some(ref cb) = progress {
            cb(&group.key, group_progress, &format!("Stacking {}", group.key));
        }

        // Create output path
        let output_filename = format!("{}_stacked.fits", sanitize_filename(&group.key));
        let output_path = output_dir.join(output_filename);

        // Stack without per-group progress for simplicity
        // (the main progress callback already handles group-level progress)
        match stack_group(group, &output_path, config, None) {
            Ok(result) => {
                total_frames += result.frame_count;
                total_exposure += result.total_exposure;
                stacks.push(result);
            }
            Err(e) => {
                failed_groups.push(FailedGroup {
                    group_key: group.key.clone(),
                    error: e.to_string(),
                });
            }
        }
    }

    if let Some(ref cb) = progress {
        cb("", 1.0, "Complete");
    }

    Ok(MultiGroupStackResult {
        stacks,
        failed_groups,
        total_frames_stacked: total_frames,
        total_exposure,
    })
}

/// Sanitize a string for use as a filename
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Get recommended stacking configuration based on frame count
///
/// Returns configuration optimized for the number of frames available.
pub fn recommended_config(frame_count: usize) -> LightStackConfig {
    if frame_count < 5 {
        // Few frames: use median, no rejection
        LightStackConfig {
            method: StackingMethod::Median,
            rejection: None,
            normalization: NormalizationMethod::Additive,
            weighting: WeightingMethod::None,
            output_32bit: true,
            drizzle: None,
        }
    } else if frame_count < 15 {
        // Moderate frames: use mean with gentle sigma rejection
        LightStackConfig {
            method: StackingMethod::Mean,
            rejection: Some(RejectionConfig::sigma(3.0, 3.0)),
            normalization: NormalizationMethod::Additive,
            weighting: WeightingMethod::Fwhm,
            output_32bit: true,
            drizzle: None,
        }
    } else {
        // Many frames: use mean with tighter rejection
        LightStackConfig {
            method: StackingMethod::Mean,
            rejection: Some(RejectionConfig::sigma(2.5, 2.5)),
            normalization: NormalizationMethod::Additive,
            weighting: WeightingMethod::Fwhm,
            output_32bit: true,
            drizzle: None,
        }
    }
}

/// Estimate output file size for stacked image
///
/// # Arguments
///
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
/// * `channels` - Number of channels (1 for mono, 3 for color)
/// * `is_32bit` - Whether output is 32-bit float
///
/// # Returns
///
/// Estimated file size in bytes
pub fn estimate_output_size(width: u32, height: u32, channels: u32, is_32bit: bool) -> u64 {
    let bytes_per_pixel = if is_32bit { 4 } else { 2 };
    let data_size = width as u64 * height as u64 * channels as u64 * bytes_per_pixel;

    // Add estimated FITS header overhead (2880 bytes per header block)
    let header_overhead = 2880 * 3; // Primary + extension headers

    data_size + header_overhead
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grouping_config_default() {
        let config = GroupingConfig::default();
        assert!(config.group_by_filter);
        assert!(!config.group_by_exposure);
        assert_eq!(config.min_frames_per_group, 3);
    }

    #[test]
    fn test_group_frames_by_filter() {
        let frames = vec![
            FrameMetadata {
                id: None,
                path: PathBuf::from("l1.fits"),
                filter: "L".to_string(),
                exposure_time: 120.0,
                is_osc: false,
                quality: None,
            },
            FrameMetadata {
                id: None,
                path: PathBuf::from("l2.fits"),
                filter: "L".to_string(),
                exposure_time: 120.0,
                is_osc: false,
                quality: None,
            },
            FrameMetadata {
                id: None,
                path: PathBuf::from("l3.fits"),
                filter: "L".to_string(),
                exposure_time: 120.0,
                is_osc: false,
                quality: None,
            },
            FrameMetadata {
                id: None,
                path: PathBuf::from("r1.fits"),
                filter: "R".to_string(),
                exposure_time: 120.0,
                is_osc: false,
                quality: None,
            },
        ];

        let groups = group_frames(&frames, &GroupingConfig::default());

        // Should have 1 group for L (3 frames), R doesn't meet minimum (1 frame)
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].filter_name, "L");
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn test_round_exposure() {
        assert_eq!(round_exposure(118.0, 5.0), 120.0);
        assert_eq!(round_exposure(122.0, 5.0), 120.0);
        assert_eq!(round_exposure(299.0, 5.0), 300.0);
        assert_eq!(round_exposure(45.5, 5.0), 45.0);
    }

    #[test]
    fn test_calculate_weights_fwhm() {
        let metrics = vec![
            Some(QualityMetrics {
                fwhm: 2.0,
                fwhm_arcsec: None,
                fwhm_x: 2.0,
                fwhm_y: 2.0,
                roundness: 1.0,
                star_count: 100,
                stars_used: 80,
                background_level: 0.1,
                background_noise: 0.01,
                quality_score: 2.0,
                has_saturated_stars: false,
            }),
            Some(QualityMetrics {
                fwhm: 4.0, // 2x worse FWHM
                fwhm_arcsec: None,
                fwhm_x: 4.0,
                fwhm_y: 4.0,
                roundness: 1.0,
                star_count: 100,
                stars_used: 80,
                background_level: 0.1,
                background_noise: 0.01,
                quality_score: 4.0,
                has_saturated_stars: false,
            }),
        ];

        let weights = calculate_weights(&metrics, WeightingMethod::Fwhm);

        // Frame with FWHM 2.0 should have 4x more weight than frame with FWHM 4.0
        assert!(weights[0] > weights[1]);
        assert!((weights[0] / weights[1] - 4.0).abs() < 0.1);
    }

    #[test]
    fn test_normalize_weights() {
        let weights = vec![1.0, 2.0, 3.0];
        let normalized = normalize_weights(&weights);

        // Mean should be 1.0
        let mean: f64 = normalized.iter().sum::<f64>() / normalized.len() as f64;
        assert!((mean - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("L_120s"), "L_120s");
        assert_eq!(sanitize_filename("Ha 300s"), "Ha_300s");
        assert_eq!(sanitize_filename("R/G/B"), "R_G_B");
    }

    #[test]
    fn test_recommended_config() {
        let config_few = recommended_config(3);
        assert_eq!(config_few.method, StackingMethod::Median);
        assert!(config_few.rejection.is_none());

        let config_many = recommended_config(20);
        assert_eq!(config_many.method, StackingMethod::Mean);
        assert!(config_many.rejection.is_some());
    }

    #[test]
    fn test_estimate_output_size() {
        // 4656 x 3520 mono 16-bit
        let size_16 = estimate_output_size(4656, 3520, 1, false);
        let expected_16 = 4656 * 3520 * 2 + 2880 * 3;
        assert_eq!(size_16, expected_16 as u64);

        // Same image but 32-bit
        let size_32 = estimate_output_size(4656, 3520, 1, true);
        let expected_32 = 4656 * 3520 * 4 + 2880 * 3;
        assert_eq!(size_32, expected_32 as u64);
    }
}
