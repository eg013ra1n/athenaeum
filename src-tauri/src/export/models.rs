//! Data models for the export module

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Export operation mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExportMode {
    /// Only generate Siril scripts without copying files
    GenerateScripts,
    /// Only organize files into folder structure
    OrganizeFiles,
    /// Organize files and generate scripts
    OrganizeAndScript,
    /// Full execution: organize, generate scripts, and run Siril
    DirectExecution,
}

/// Siril processing workflow type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SirilWorkflow {
    /// Process each filter separately (mono cameras or narrowband)
    MonoPreprocessing,
    /// One-shot color camera processing
    OscPreprocessing,
    /// LRGB combination workflow
    LrgbProcessing,
}

/// Configuration for an export operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportConfig {
    /// Frame set to export
    pub frame_set_id: i64,
    /// Output directory path
    pub output_dir: PathBuf,
    /// Export operation mode
    pub mode: ExportMode,
    /// Siril workflow type
    pub workflow: SirilWorkflow,
    /// Whether to create master calibration frames
    pub create_masters: bool,
    /// Low rejection sigma for stacking
    pub rejection_low: f64,
    /// High rejection sigma for stacking
    pub rejection_high: f64,
    /// Use symbolic links instead of copying files
    pub use_symlinks: bool,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            frame_set_id: 0,
            output_dir: PathBuf::new(),
            mode: ExportMode::OrganizeAndScript,
            workflow: SirilWorkflow::MonoPreprocessing,
            create_masters: true,
            rejection_low: 3.0,
            rejection_high: 3.0,
            use_symlinks: false,
        }
    }
}

/// A single frame for export
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFrame {
    /// Frame ID in database
    pub frame_id: i64,
    /// File ID in database
    pub file_id: i64,
    /// Full file path
    pub file_path: String,
    /// Filename only
    pub filename: String,
    /// Exposure time in seconds
    pub exptime: Option<f64>,
    /// Filter name
    pub filter: Option<String>,
    /// CCD temperature
    pub ccd_temp: Option<f64>,
    /// Gain setting
    pub gain: Option<i32>,
    /// Offset setting
    pub offset: Option<i32>,
    /// Binning (e.g., "1x1")
    pub binning: Option<String>,
    /// Date observed
    pub date_obs: Option<String>,
}

/// A calibration set with its frames
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCalibrationSet {
    /// Calibration set ID
    pub set_id: i64,
    /// Image type (FLAT, DARK, BIAS, DARKFLAT)
    pub imagetyp: String,
    /// Frames in this calibration set
    pub frames: Vec<ExportFrame>,
    /// Sub-calibrations (e.g., Flat -> Dark, Dark -> Bias)
    pub sub_calibrations: Vec<ExportCalibrationSet>,
    /// Match quality score (0.0 - 1.0)
    pub match_score: Option<f64>,
    /// Warnings about this calibration match
    pub warnings: Vec<String>,
}

/// Group of light frames by filter with their calibrations
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterExportGroup {
    /// Filter name (None for unfiltered/OSC)
    pub filter: Option<String>,
    /// Light frames for this filter
    pub light_frames: Vec<ExportFrame>,
    /// Matched flat calibration sets
    pub flat_sets: Vec<ExportCalibrationSet>,
    /// Matched dark calibration sets
    pub dark_sets: Vec<ExportCalibrationSet>,
    /// Matched bias calibration sets
    pub bias_sets: Vec<ExportCalibrationSet>,
}

/// Summary of calibration availability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSummary {
    /// Total flat frames available
    pub flat_count: i32,
    /// Total dark frames available
    pub dark_count: i32,
    /// Total bias frames available
    pub bias_count: i32,
    /// Total dark flat frames available
    pub dark_flat_count: i32,
    /// Whether all lights have matched flats
    pub flats_complete: bool,
    /// Whether all lights have matched darks
    pub darks_complete: bool,
    /// Whether all lights have matched bias
    pub bias_complete: bool,
    /// Warnings about calibration matching
    pub warnings: Vec<String>,
}

/// Complete export data for a frame set
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportData {
    /// Frame set ID
    pub frame_set_id: i64,
    /// Frame set name
    pub frame_set_name: String,
    /// Object name
    pub object_name: Option<String>,
    /// Filter groups with their calibrations
    pub filters: Vec<FilterExportGroup>,
    /// Overall calibration summary
    pub calibration_summary: CalibrationSummary,
    /// Total light frame count
    pub total_light_frames: i32,
    /// Total exposure time in seconds
    pub total_exposure_seconds: f64,
}

/// Result of an export operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    /// Whether the export was successful
    pub success: bool,
    /// Output directory path
    pub output_dir: String,
    /// Number of files copied/linked
    pub files_organized: i32,
    /// Generated script paths
    pub scripts_generated: Vec<String>,
    /// Any warnings during export
    pub warnings: Vec<String>,
    /// Error message if failed
    pub error: Option<String>,
}

/// Progress update during export or Siril execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgress {
    /// Current stage of the export
    pub stage: ExportStage,
    /// Progress percentage (0-100)
    pub progress: f64,
    /// Current status message
    pub message: String,
    /// Current file being processed (if applicable)
    pub current_file: Option<String>,
}

/// Export operation stages
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExportStage {
    /// Collecting frame data
    Collecting,
    /// Organizing files into folders
    Organizing,
    /// Generating scripts
    GeneratingScripts,
    /// Running Siril calibration
    SirilCalibrating,
    /// Running Siril registration
    SirilRegistering,
    /// Running Siril stacking
    SirilStacking,
    /// Export complete
    Complete,
    /// Export failed
    Failed,
}
