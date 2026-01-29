//! FFI-based step executor for export pipeline
//!
//! This module provides an FFI-based executor that can execute export pipeline steps
//! using direct Siril C function calls instead of the CLI subprocess approach.
//!
//! The executor integrates with the existing pipeline infrastructure and can be used
//! as a drop-in replacement for the CLI-based execution.

use crate::stacking::{
    calibration::{
        calibrate_lights_batch, BatchCalibrationConfig, CalibrationConfig, DarkOptimization,
        DebayerConfig, DebayerMethod, FlatNormalization,
    },
    config::{
        LightStackConfig, MasterConfig, NormalizationMethod, ProgressCallback as ConfigProgressCallback,
        RejectionAlgorithm, RejectionConfig, StackingMethod, WeightingMethod,
    },
    error::{StackingError, StackingResult},
    ffi,
    fits::FitsImage,
    master::{create_master_bias, create_master_dark, create_master_flat},
    quality::{measure_quality_batch, QualityConfig, QualityMetrics},
    registration::{
        apply_registration, platesolve_batch, register_astrometric, register_star_alignment,
        Framing, Interpolation, PlateSolveConfig, RegistrationConfig, Transformation,
    },
    stacking::{group_frames, stack_all_groups, FrameMetadata, GroupingConfig},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ============================================================================
// Executor Configuration
// ============================================================================

/// Configuration for the FFI executor
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutorConfig {
    /// Whether to use 32-bit float output
    pub output_32bit: bool,
    /// Master creation settings
    pub master: MasterConfig,
    /// Calibration settings
    pub calibration: CalibrationSettings,
    /// Registration settings
    pub registration: RegistrationSettings,
    /// Stacking settings
    pub stacking: StackingSettings,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            output_32bit: true,
            master: MasterConfig::median(),
            calibration: CalibrationSettings::default(),
            registration: RegistrationSettings::default(),
            stacking: StackingSettings::default(),
        }
    }
}

/// Calibration-specific settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSettings {
    pub use_bias: bool,
    pub use_dark: bool,
    pub use_flat: bool,
    pub dark_optimization: DarkOptimizationMode,
    pub flat_normalization: FlatNormalizationMode,
    pub debayer_method: DebayerMethodOption,
    pub output_32bit: bool,
}

impl Default for CalibrationSettings {
    fn default() -> Self {
        Self {
            use_bias: true,
            use_dark: true,
            use_flat: true,
            dark_optimization: DarkOptimizationMode::Off,
            flat_normalization: FlatNormalizationMode::Auto,
            debayer_method: DebayerMethodOption::Vng,
            output_32bit: true,
        }
    }
}

/// Dark optimization mode for calibration
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DarkOptimizationMode {
    Off,
    ExposureBased,
    NoiseMinimization,
}

/// Flat normalization mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlatNormalizationMode {
    Auto,
    Manual,
}

/// Debayer method option
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DebayerMethodOption {
    Bilinear,
    Vng,
    Auto,
}

/// Registration-specific settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationSettings {
    pub use_plate_solving: bool,
    pub focal_length_mm: f64,
    pub pixel_size_um: f64,
    pub search_radius_deg: f64,
    pub framing: FramingOption,
    pub interpolation: InterpolationOption,
}

impl Default for RegistrationSettings {
    fn default() -> Self {
        Self {
            use_plate_solving: true,
            focal_length_mm: 500.0,
            pixel_size_um: 3.76,
            search_radius_deg: 10.0,
            framing: FramingOption::Max,
            interpolation: InterpolationOption::Lanczos4,
        }
    }
}

/// Framing option for registration
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FramingOption {
    Current,
    Max,
    Min,
    CenterOfGravity,
}

impl From<FramingOption> for Framing {
    fn from(opt: FramingOption) -> Self {
        match opt {
            FramingOption::Current => Framing::Current,
            FramingOption::Max => Framing::Max,
            FramingOption::Min => Framing::Min,
            FramingOption::CenterOfGravity => Framing::CenterOfGravity,
        }
    }
}

/// Interpolation option
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InterpolationOption {
    Nearest,
    Bilinear,
    Bicubic,
    Lanczos4,
}

impl From<InterpolationOption> for Interpolation {
    fn from(opt: InterpolationOption) -> Self {
        match opt {
            InterpolationOption::Nearest => Interpolation::Nearest,
            InterpolationOption::Bilinear => Interpolation::Bilinear,
            InterpolationOption::Bicubic => Interpolation::Bicubic,
            InterpolationOption::Lanczos4 => Interpolation::Lanczos4,
        }
    }
}

/// Stacking-specific settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackingSettings {
    pub method: StackingMethodOption,
    pub rejection: RejectionOption,
    pub sigma_low: f64,
    pub sigma_high: f64,
    pub normalization: NormalizationOption,
    pub weighting: WeightingOption,
    pub output_32bit: bool,
}

impl Default for StackingSettings {
    fn default() -> Self {
        Self {
            method: StackingMethodOption::Mean,
            rejection: RejectionOption::Sigma,
            sigma_low: 2.5,
            sigma_high: 2.5,
            normalization: NormalizationOption::AdditiveScaling,
            weighting: WeightingOption::Fwhm,
            output_32bit: true,
        }
    }
}

/// Stacking method option
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StackingMethodOption {
    Mean,
    Median,
    Sum,
}

impl From<StackingMethodOption> for StackingMethod {
    fn from(opt: StackingMethodOption) -> Self {
        match opt {
            StackingMethodOption::Mean => StackingMethod::Mean,
            StackingMethodOption::Median => StackingMethod::Median,
            StackingMethodOption::Sum => StackingMethod::Sum,
        }
    }
}

/// Rejection option
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RejectionOption {
    None,
    Sigma,
    Mad,
    Winsorized,
    Percentile,
    LinearFit,
    Gesdt,
}

impl From<RejectionOption> for RejectionAlgorithm {
    fn from(opt: RejectionOption) -> Self {
        match opt {
            RejectionOption::None => RejectionAlgorithm::None,
            RejectionOption::Sigma => RejectionAlgorithm::Sigma,
            RejectionOption::Mad => RejectionAlgorithm::Mad,
            RejectionOption::Winsorized => RejectionAlgorithm::Winsorized,
            RejectionOption::Percentile => RejectionAlgorithm::Percentile,
            RejectionOption::LinearFit => RejectionAlgorithm::LinearFit,
            RejectionOption::Gesdt => RejectionAlgorithm::Gesdt,
        }
    }
}

/// Normalization option
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationOption {
    None,
    Additive,
    Multiplicative,
    AdditiveScaling,
}

/// Weighting option
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WeightingOption {
    None,
    Fwhm,
    StarCount,
    Noise,
}

// ============================================================================
// Step Result
// ============================================================================

/// Result of executing a step via FFI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FfiStepResult {
    /// Whether the step succeeded
    pub success: bool,
    /// Output files created by this step
    pub output_files: Vec<String>,
    /// Error message if failed
    pub error_message: Option<String>,
    /// Whether this step was skipped (output already exists)
    pub skipped: bool,
    /// Skip reason if skipped
    pub skip_reason: Option<String>,
    /// Processing statistics
    pub stats: StepStats,
}

impl FfiStepResult {
    pub fn success(output_files: Vec<String>) -> Self {
        Self {
            success: true,
            output_files,
            error_message: None,
            skipped: false,
            skip_reason: None,
            stats: StepStats::default(),
        }
    }

    pub fn success_with_stats(output_files: Vec<String>, stats: StepStats) -> Self {
        Self {
            success: true,
            output_files,
            error_message: None,
            skipped: false,
            skip_reason: None,
            stats,
        }
    }

    pub fn failure(error: String) -> Self {
        Self {
            success: false,
            output_files: vec![],
            error_message: Some(error),
            skipped: false,
            skip_reason: None,
            stats: StepStats::default(),
        }
    }

    pub fn skipped(reason: String) -> Self {
        Self {
            success: true,
            output_files: vec![],
            error_message: None,
            skipped: true,
            skip_reason: Some(reason),
            stats: StepStats::default(),
        }
    }
}

/// Processing statistics for a step
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepStats {
    pub frames_processed: u32,
    pub frames_rejected: u32,
    pub total_exposure_time: f64,
    pub average_fwhm: Option<f64>,
    pub processing_time_ms: u64,
}

// ============================================================================
// Progress Callback
// ============================================================================

/// Progress callback type for step execution
pub type ProgressCallback = Box<dyn Fn(f32, &str) + Send + Sync>;

/// Create a no-op progress callback
pub fn no_progress() -> ProgressCallback {
    Box::new(|_, _| {})
}

// ============================================================================
// FFI Step Executor
// ============================================================================

/// FFI-based executor for export pipeline steps
pub struct FfiExecutor {
    /// Configuration for this executor
    config: ExecutorConfig,
    /// Cancellation flag
    cancelled: Arc<AtomicBool>,
}

impl FfiExecutor {
    /// Create a new FFI executor with the given configuration
    pub fn new(config: ExecutorConfig) -> Self {
        Self {
            config,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get a cancellation token for this executor
    pub fn cancellation_token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Cancel any running operation
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Check if cancelled
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Check if FFI is available
    pub fn is_available() -> bool {
        ffi::is_ffi_available()
    }

    // ========================================================================
    // Master Creation Steps
    // ========================================================================

    /// Create a master bias frame
    pub fn create_master_bias(
        &self,
        bias_paths: &[PathBuf],
        output_path: &Path,
        progress: Option<ProgressCallback>,
    ) -> StackingResult<FfiStepResult> {
        self.check_cancelled()?;

        // Check if output already exists
        if output_path.exists() {
            return Ok(FfiStepResult::skipped(format!(
                "Master bias already exists: {}",
                output_path.display()
            )));
        }

        let progress = progress.unwrap_or_else(no_progress);
        progress(0.0, "Creating master bias...");

        let start = std::time::Instant::now();

        // Create progress callback compatible with master functions
        let master_progress: ConfigProgressCallback =
            Box::new(move |p, msg| progress(p, msg));

        let result =
            create_master_bias(bias_paths, output_path, &self.config.master, Some(master_progress))?;

        let elapsed = start.elapsed().as_millis() as u64;

        Ok(FfiStepResult::success_with_stats(
            vec![output_path.to_string_lossy().to_string()],
            StepStats {
                frames_processed: result.frame_count as u32,
                frames_rejected: 0,
                total_exposure_time: 0.0,
                average_fwhm: None,
                processing_time_ms: elapsed,
            },
        ))
    }

    /// Create a master dark frame
    pub fn create_master_dark(
        &self,
        dark_paths: &[PathBuf],
        output_path: &Path,
        progress: Option<ProgressCallback>,
    ) -> StackingResult<FfiStepResult> {
        self.check_cancelled()?;

        if output_path.exists() {
            return Ok(FfiStepResult::skipped(format!(
                "Master dark already exists: {}",
                output_path.display()
            )));
        }

        let progress = progress.unwrap_or_else(no_progress);
        progress(0.0, "Creating master dark...");

        let start = std::time::Instant::now();

        let master_progress: ConfigProgressCallback =
            Box::new(move |p, msg| progress(p, msg));

        let result =
            create_master_dark(dark_paths, output_path, &self.config.master, Some(master_progress))?;

        let elapsed = start.elapsed().as_millis() as u64;

        Ok(FfiStepResult::success_with_stats(
            vec![output_path.to_string_lossy().to_string()],
            StepStats {
                frames_processed: result.frame_count as u32,
                frames_rejected: 0,
                total_exposure_time: result.total_exposure_seconds.unwrap_or(0.0),
                average_fwhm: None,
                processing_time_ms: elapsed,
            },
        ))
    }

    /// Create a master flat frame
    pub fn create_master_flat(
        &self,
        flat_paths: &[PathBuf],
        output_path: &Path,
        bias_path: Option<&Path>,
        dark_path: Option<&Path>,
        progress: Option<ProgressCallback>,
    ) -> StackingResult<FfiStepResult> {
        self.check_cancelled()?;

        if output_path.exists() {
            return Ok(FfiStepResult::skipped(format!(
                "Master flat already exists: {}",
                output_path.display()
            )));
        }

        let progress = progress.unwrap_or_else(no_progress);
        progress(0.0, "Creating master flat...");

        let start = std::time::Instant::now();

        // Build flat config with optional calibration
        let mut flat_config = crate::stacking::config::FlatConfig::default();
        if let Some(bias) = bias_path {
            flat_config = flat_config.with_bias(bias.to_string_lossy().to_string());
        }
        if let Some(dark) = dark_path {
            flat_config = flat_config.with_dark(dark.to_string_lossy().to_string());
        }

        let master_progress: ConfigProgressCallback =
            Box::new(move |p, msg| progress(p, msg));

        let result =
            create_master_flat(flat_paths, output_path, &flat_config, Some(master_progress))?;

        let elapsed = start.elapsed().as_millis() as u64;

        Ok(FfiStepResult::success_with_stats(
            vec![output_path.to_string_lossy().to_string()],
            StepStats {
                frames_processed: result.frame_count as u32,
                frames_rejected: 0,
                total_exposure_time: 0.0,
                average_fwhm: None,
                processing_time_ms: elapsed,
            },
        ))
    }

    // ========================================================================
    // Calibration Steps
    // ========================================================================

    /// Calibrate a batch of light frames
    pub fn calibrate_lights(
        &self,
        light_paths: &[PathBuf],
        output_dir: &Path,
        bias_path: Option<&Path>,
        dark_path: Option<&Path>,
        flat_path: Option<&Path>,
        progress: Option<ProgressCallback>,
    ) -> StackingResult<FfiStepResult> {
        self.check_cancelled()?;

        let progress = progress.unwrap_or_else(no_progress);
        progress(0.0, "Calibrating light frames...");

        let start = std::time::Instant::now();

        // Load calibration masters
        let bias = if let Some(path) = bias_path {
            Some(FitsImage::open(path)?)
        } else {
            None
        };

        let dark = if let Some(path) = dark_path {
            Some(FitsImage::open(path)?)
        } else {
            None
        };

        let flat = if let Some(path) = flat_path {
            Some(FitsImage::open(path)?)
        } else {
            None
        };

        // Build calibration config
        let cal_config = CalibrationConfig {
            use_bias: self.config.calibration.use_bias && bias.is_some(),
            use_dark: self.config.calibration.use_dark && dark.is_some(),
            use_flat: self.config.calibration.use_flat && flat.is_some(),
            dark_optimization: match self.config.calibration.dark_optimization {
                DarkOptimizationMode::Off => DarkOptimization::Off,
                DarkOptimizationMode::ExposureBased => DarkOptimization::ExposureBased,
                DarkOptimizationMode::NoiseMinimization => DarkOptimization::NoiseMinimization,
            },
            flat_normalization: FlatNormalization::Auto,
            output_32bit: self.config.calibration.output_32bit,
        };

        // Build debayer config if needed
        let debayer_config = Some(DebayerConfig {
            method: match self.config.calibration.debayer_method {
                DebayerMethodOption::Bilinear => DebayerMethod::Bilinear,
                DebayerMethodOption::Vng => DebayerMethod::Vng,
                DebayerMethodOption::Auto => DebayerMethod::Auto,
            },
            pattern: crate::stacking::calibration::BayerPattern::Auto,
        });

        // Build batch config
        let batch_config = BatchCalibrationConfig {
            calibration: cal_config,
            debayer: debayer_config,
            output_dir: output_dir.to_path_buf(),
            output_prefix: "pp_lights".to_string(),
        };

        // Create progress callback compatible with calibration functions
        let total = light_paths.len();
        let cal_progress: ConfigProgressCallback = Box::new(move |p, msg| {
            let current = (p * total as f32) as usize;
            progress(p, &format!("Calibrating frame {}/{}... {}", current + 1, total, msg));
        });

        // Run batch calibration
        let result = calibrate_lights_batch(
            light_paths,
            bias.as_ref(),
            dark.as_ref(),
            flat.as_ref(),
            &batch_config,
            Some(cal_progress),
        )?;

        let elapsed = start.elapsed().as_millis() as u64;

        Ok(FfiStepResult::success_with_stats(
            result.output_files,
            StepStats {
                frames_processed: result.calibrated_count as u32,
                frames_rejected: result.failed_count as u32,
                total_exposure_time: 0.0,
                average_fwhm: None,
                processing_time_ms: elapsed,
            },
        ))
    }

    // ========================================================================
    // Quality Measurement Steps
    // ========================================================================

    /// Measure quality of calibrated frames
    pub fn measure_quality(
        &self,
        frame_paths: &[PathBuf],
        progress: Option<ProgressCallback>,
    ) -> StackingResult<(FfiStepResult, HashMap<PathBuf, QualityMetrics>)> {
        self.check_cancelled()?;

        let progress = progress.unwrap_or_else(no_progress);
        progress(0.0, "Measuring frame quality...");

        let start = std::time::Instant::now();

        let quality_config = QualityConfig::default();

        // Create progress callback compatible with quality functions
        let total = frame_paths.len();
        let quality_progress: Box<dyn Fn(usize, usize, &str)> =
            Box::new(move |current, _total, msg| {
                let p = current as f32 / total as f32;
                progress(p, &format!("Measuring quality {}/{}... {}", current, total, msg));
            });

        let result = measure_quality_batch(frame_paths, &quality_config, Some(quality_progress))?;

        let elapsed = start.elapsed().as_millis() as u64;

        // Build metrics map - convert String paths to PathBuf
        let metrics_map: HashMap<PathBuf, QualityMetrics> = result
            .metrics
            .iter()
            .map(|(path, metrics)| (PathBuf::from(path), metrics.clone()))
            .collect();

        Ok((
            FfiStepResult::success_with_stats(
                vec![],
                StepStats {
                    frames_processed: result.metrics.len() as u32,
                    frames_rejected: result.failed_count as u32,
                    total_exposure_time: 0.0,
                    average_fwhm: Some(result.average_fwhm),
                    processing_time_ms: elapsed,
                },
            ),
            metrics_map,
        ))
    }

    // ========================================================================
    // Registration Steps
    // ========================================================================

    /// Plate-solve a batch of frames
    pub fn platesolve_frames(
        &self,
        frame_paths: &[PathBuf],
        progress: Option<ProgressCallback>,
    ) -> StackingResult<FfiStepResult> {
        self.check_cancelled()?;

        let progress = progress.unwrap_or_else(no_progress);
        progress(0.0, "Plate-solving frames...");

        let start = std::time::Instant::now();

        let config = PlateSolveConfig {
            focal_length_mm: self.config.registration.focal_length_mm,
            pixel_size_um: self.config.registration.pixel_size_um,
            search_radius_deg: self.config.registration.search_radius_deg,
            ..Default::default()
        };

        // Create progress callback compatible with platesolve functions
        let total = frame_paths.len();
        let solve_progress: Box<dyn Fn(usize, usize, &str)> =
            Box::new(move |current, _total, status| {
                let p = current as f32 / total as f32;
                progress(p, &format!("Plate-solving {}/{}: {}", current, total, status));
            });

        let result = platesolve_batch(frame_paths, &config, Some(solve_progress))?;

        let elapsed = start.elapsed().as_millis() as u64;

        Ok(FfiStepResult::success_with_stats(
            vec![],
            StepStats {
                frames_processed: result.success_count as u32,
                frames_rejected: result.failed_count as u32,
                total_exposure_time: 0.0,
                average_fwhm: None,
                processing_time_ms: elapsed,
            },
        ))
    }

    /// Register frames using astrometric or star-based alignment
    pub fn register_frames(
        &self,
        frame_paths: &[PathBuf],
        reference_index: usize,
        output_dir: &Path,
        progress: Option<ProgressCallback>,
    ) -> StackingResult<FfiStepResult> {
        self.check_cancelled()?;

        let progress = Arc::new(progress.unwrap_or_else(no_progress));
        progress(0.0, "Registering frames...");

        let start = std::time::Instant::now();

        let reg_config = RegistrationConfig {
            framing: self.config.registration.framing.into(),
            interpolation: self.config.registration.interpolation.into(),
            transformation: Transformation::Homography,
            output_scale: 1.0,
            clamp: true,
            clamping_factor: 3.0,
            min_pairs: 10,
            max_stars: 500,
            two_pass: false,
        };

        // Create progress callback compatible with registration functions
        let total = frame_paths.len();
        let progress_clone = Arc::clone(&progress);
        let reg_progress: Box<dyn Fn(usize, usize, &str)> =
            Box::new(move |current, _total, msg| {
                let p = current as f32 / total as f32;
                progress_clone(p * 0.5, &format!("Computing registration {}/{}... {}", current, total, msg));
            });

        // Try astrometric registration first, fall back to star-based
        let reg_result = if self.config.registration.use_plate_solving {
            register_astrometric(frame_paths, reference_index, &reg_config, Some(reg_progress))
        } else {
            register_star_alignment(frame_paths, reference_index, &reg_config, Some(reg_progress))
        }?;

        progress(0.5, "Applying registration transforms...");

        // Load frames and apply registration
        let mut output_files = Vec::new();
        std::fs::create_dir_all(output_dir)?;

        for (i, (path, homography)) in frame_paths
            .iter()
            .zip(reg_result.homographies.iter())
            .enumerate()
        {
            self.check_cancelled()?;

            let frame = FitsImage::open(path)?;
            let output_path = output_dir.join(format!("r_pp_lights_{:05}.fit", i + 1));
            let registered = apply_registration(
                &frame,
                homography,
                (reg_result.output_width, reg_result.output_height),
                &reg_config,
            )?;
            registered.save(&output_path)?;
            output_files.push(output_path.to_string_lossy().to_string());

            let p = 0.5 + (i as f32 / frame_paths.len() as f32) * 0.5;
            progress(p, &format!("Registered frame {}/{}", i + 1, frame_paths.len()));
        }

        let elapsed = start.elapsed().as_millis() as u64;

        Ok(FfiStepResult::success_with_stats(
            output_files,
            StepStats {
                frames_processed: frame_paths.len() as u32,
                frames_rejected: 0,
                total_exposure_time: 0.0,
                average_fwhm: None,
                processing_time_ms: elapsed,
            },
        ))
    }

    // ========================================================================
    // Stacking Steps
    // ========================================================================

    /// Group and stack frames
    pub fn stack_frames(
        &self,
        frame_paths: &[PathBuf],
        quality_metrics: Option<&HashMap<PathBuf, QualityMetrics>>,
        output_dir: &Path,
        progress: Option<ProgressCallback>,
    ) -> StackingResult<FfiStepResult> {
        self.check_cancelled()?;

        let progress = progress.unwrap_or_else(no_progress);
        progress(0.0, "Grouping and stacking frames...");

        let start = std::time::Instant::now();

        // Build frame metadata
        let mut frame_metadata = Vec::new();
        for (i, path) in frame_paths.iter().enumerate() {
            let fits = FitsImage::open(path)?;
            let quality = quality_metrics.and_then(|m| m.get(path).cloned());

            frame_metadata.push(FrameMetadata {
                id: Some(i as i64),
                path: path.clone(),
                filter: fits.filter().unwrap_or_else(|| "Unfiltered".to_string()),
                exposure_time: fits.exposure_time().unwrap_or(0.0),
                is_osc: fits.is_color(),
                quality,
            });
        }

        // Group frames
        let grouping_config = GroupingConfig::default();
        let groups = group_frames(&frame_metadata, &grouping_config);

        progress(0.1, &format!("Created {} groups", groups.len()));

        // Build stack config
        let stack_config = LightStackConfig {
            method: self.config.stacking.method.into(),
            rejection: Some(RejectionConfig {
                algorithm: self.config.stacking.rejection.into(),
                sigma_low: self.config.stacking.sigma_low,
                sigma_high: self.config.stacking.sigma_high,
            }),
            normalization: match self.config.stacking.normalization {
                NormalizationOption::None => NormalizationMethod::None,
                NormalizationOption::Additive => NormalizationMethod::Additive,
                NormalizationOption::Multiplicative => NormalizationMethod::Multiplicative,
                NormalizationOption::AdditiveScaling => NormalizationMethod::AdditiveScaling,
            },
            weighting: match self.config.stacking.weighting {
                WeightingOption::None => WeightingMethod::None,
                WeightingOption::Fwhm => WeightingMethod::Fwhm,
                WeightingOption::StarCount => WeightingMethod::StarCount,
                WeightingOption::Noise => WeightingMethod::Noise,
            },
            output_32bit: self.config.stacking.output_32bit,
            ..Default::default()
        };

        // Create progress callback compatible with stacking functions
        let stack_progress: Box<dyn Fn(&str, f32, &str) + Send> =
            Box::new(move |group_key, p, msg| {
                progress(0.1 + p * 0.9, &format!("Stacking {}... {}", group_key, msg));
            });

        // Stack all groups (note: order is groups, output_dir, config, progress)
        let result = stack_all_groups(&groups, output_dir, &stack_config, Some(stack_progress))?;

        let elapsed = start.elapsed().as_millis() as u64;

        // Collect output files
        let mut output_files = Vec::new();
        let mut total_frames = 0u32;
        let mut total_exposure = 0.0f64;

        for stack_result in &result.stacks {
            output_files.push(stack_result.output_path.to_string_lossy().to_string());
            total_frames += stack_result.frame_count as u32;
            total_exposure += stack_result.total_exposure;
        }

        Ok(FfiStepResult::success_with_stats(
            output_files,
            StepStats {
                frames_processed: total_frames,
                frames_rejected: result.failed_groups.len() as u32,
                total_exposure_time: total_exposure,
                average_fwhm: None,
                processing_time_ms: elapsed,
            },
        ))
    }

    // ========================================================================
    // Helper Methods
    // ========================================================================

    /// Check if the executor has been cancelled
    fn check_cancelled(&self) -> StackingResult<()> {
        if self.is_cancelled() {
            Err(StackingError::Cancelled)
        } else {
            Ok(())
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_config_default() {
        let config = ExecutorConfig::default();
        assert!(config.output_32bit);
        assert!(config.calibration.use_bias);
        assert!(config.calibration.use_dark);
        assert!(config.calibration.use_flat);
    }

    #[test]
    fn test_ffi_step_result_success() {
        let result = FfiStepResult::success(vec!["output.fit".to_string()]);
        assert!(result.success);
        assert!(!result.skipped);
        assert_eq!(result.output_files.len(), 1);
    }

    #[test]
    fn test_ffi_step_result_skipped() {
        let result = FfiStepResult::skipped("Already exists".to_string());
        assert!(result.success);
        assert!(result.skipped);
        assert!(result.skip_reason.is_some());
    }

    #[test]
    fn test_ffi_step_result_failure() {
        let result = FfiStepResult::failure("Something went wrong".to_string());
        assert!(!result.success);
        assert!(result.error_message.is_some());
    }
}
