//! Data models for the export module

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ============================================================================
// Camera Type Detection
// ============================================================================

/// Camera type based on Bayer pattern presence
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CameraType {
    /// One-shot color camera (has Bayer pattern like RGGB, BGGR)
    Osc,
    /// Monochrome camera (no Bayer pattern)
    Mono,
}

impl CameraType {
    /// Determine camera type from BAYERPAT FITS keyword
    pub fn from_bayerpat(bayerpat: Option<&str>) -> Self {
        match bayerpat {
            Some(pattern) if !pattern.trim().is_empty() => CameraType::Osc,
            _ => CameraType::Mono,
        }
    }

    /// Get display name for this camera type
    pub fn display_name(&self) -> &'static str {
        match self {
            CameraType::Osc => "OSC",
            CameraType::Mono => "Mono",
        }
    }
}

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

/// Export target application
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportTarget {
    /// Siril - flat structure with generated scripts
    #[default]
    Siril,
    /// PixInsight WBPP - grouped structure for auto-detection
    PixInsightWBPP,
}

/// Reference frame selection mode for registration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceFrameMode {
    /// Use Siril's -2pass auto-selection (recommended)
    #[default]
    SirilAuto,
    /// Pre-select using Athenaeum quality metrics
    AtheneumScoring,
    /// User manually specifies reference frame
    Manual,
}

/// Pixel rejection algorithm for stacking
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RejectionAlgorithm {
    /// Percentile clipping - good for small datasets (<20 frames)
    Percentile,
    /// Sigma clipping - general purpose (default)
    #[default]
    Sigma,
    /// Linear fit clipping - good for large sets with gradients
    LinearFit,
    /// Generalized ESD - best for 50+ images
    Gesd,
    /// MAD clipping - good for drizzled CFA data
    Mad,
}

/// Image weighting method for stacking
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageWeightingMethod {
    /// No weighting
    None,
    /// Weight by number of stars detected
    Stars,
    /// Weight by weighted FWHM (recommended)
    #[default]
    Wfwhm,
    /// Weight by noise level
    Noise,
    /// Weight by integration time
    ExposureTime,
}

/// Drizzle scale factor for super-resolution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DrizzleScale {
    /// No drizzle (1x, disabled)
    #[default]
    None,
    /// 2x super-resolution
    X2,
    /// 3x super-resolution
    X3,
}

/// Exposure time tolerance mode for stacking grouping
///
/// Controls how frames with different exposure times are grouped for stacking.
/// When enabled, frames are only stacked together if their exposure times are
/// within the specified tolerance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExptimeToleranceMode {
    /// Stack all frames with same filter together (ignores exposure time)
    #[default]
    Disabled,
    /// Group frames if within X seconds of each other (e.g., 30 = ±30s)
    Absolute,
    /// Group frames if within X percent of each other (e.g., 10 = ±10%)
    Relative,
}

/// Configuration for an export operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportConfig {
    /// Frame set to export
    pub frame_set_id: i64,
    /// Output directory path
    pub output_dir: PathBuf,
    /// Export target application (Siril or PixInsight WBPP)
    #[serde(default)]
    pub target: ExportTarget,
    /// Export operation mode
    pub mode: ExportMode,
    /// Siril workflow type (only used when target is Siril)
    pub workflow: SirilWorkflow,
    /// Whether to create master calibration frames (Siril only)
    pub create_masters: bool,
    /// Low rejection sigma for stacking (Siril only)
    pub rejection_low: f64,
    /// High rejection sigma for stacking (Siril only)
    pub rejection_high: f64,
    /// Use symbolic links instead of copying files
    pub use_symlinks: bool,

    // === Advanced Siril Options ===

    /// Reference frame selection mode for registration
    #[serde(default)]
    pub reference_frame_mode: ReferenceFrameMode,
    /// Manual reference frame ID (only used when reference_frame_mode is Manual)
    #[serde(default)]
    pub manual_reference_frame_id: Option<i64>,
    /// Pixel rejection algorithm for stacking
    #[serde(default)]
    pub rejection_algorithm: RejectionAlgorithm,
    /// Image weighting method for stacking
    #[serde(default)]
    pub image_weighting: ImageWeightingMethod,
    /// Enable drizzle for super-resolution
    #[serde(default)]
    pub drizzle_enabled: bool,
    /// Drizzle scale factor (only used when drizzle_enabled is true)
    #[serde(default)]
    pub drizzle_scale: DrizzleScale,

    // === Exposure Time Grouping ===

    /// Exposure time tolerance mode for stacking grouping
    #[serde(default)]
    pub exptime_tolerance_mode: ExptimeToleranceMode,
    /// Exposure time tolerance value (seconds for Absolute, percentage for Relative)
    #[serde(default = "default_exptime_tolerance")]
    pub exptime_tolerance_value: f64,
}

fn default_exptime_tolerance() -> f64 {
    30.0 // 30 seconds or 30% depending on mode
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            frame_set_id: 0,
            output_dir: PathBuf::new(),
            target: ExportTarget::default(),
            mode: ExportMode::OrganizeAndScript,
            workflow: SirilWorkflow::MonoPreprocessing,
            create_masters: true,
            rejection_low: 2.5,
            rejection_high: 2.5,
            use_symlinks: false,
            // Advanced Siril options with sensible defaults
            reference_frame_mode: ReferenceFrameMode::default(),
            manual_reference_frame_id: None,
            rejection_algorithm: RejectionAlgorithm::default(),
            image_weighting: ImageWeightingMethod::default(),
            drizzle_enabled: false,
            drizzle_scale: DrizzleScale::default(),
            // Exposure time grouping
            exptime_tolerance_mode: ExptimeToleranceMode::default(),
            exptime_tolerance_value: default_exptime_tolerance(),
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
    pub gain: Option<f64>,
    /// Offset setting
    pub offset: Option<f64>,
    /// Binning (e.g., "1x1")
    pub binning: Option<String>,
    /// Date observed
    pub date_obs: Option<String>,
    /// Focal length in mm
    pub focallen: Option<f64>,
    /// Bayer pattern for OSC detection (e.g., "RGGB")
    pub bayerpat: Option<String>,
    /// Camera/instrument name
    pub instrume: Option<String>,
}

/// A calibration set with its frames (legacy - kept for compatibility)
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

// ============================================================================
// New Export Models (Phase 2 Refactoring)
// ============================================================================

/// Information about a calibration set and its sub-calibrations (recursive)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSetInfo {
    /// Calibration set ID
    pub set_id: i64,
    /// Image type (FLAT, DARK, BIAS, DARKFLAT)
    pub imagetyp: String,
    /// Frames in this calibration set
    pub frames: Vec<ExportFrame>,
    /// Frame count
    pub frame_count: i32,
    /// Sub-calibration: DarkFlat set (for Flats)
    pub dark_flat: Option<Box<CalibrationSetInfo>>,
    /// Sub-calibration: Dark set (for Flats or Lights)
    pub dark: Option<Box<CalibrationSetInfo>>,
    /// Sub-calibration: Bias set (for Flats, Darks, or Lights)
    pub bias: Option<Box<CalibrationSetInfo>>,
    /// Match quality score (0.0 - 1.0)
    pub match_score: Option<f64>,
    /// Warnings (date, temperature mismatch, etc.)
    pub warnings: Vec<String>,
}

/// A subgroup of frames that share the same calibration set links
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSubgroup {
    /// Unique subgroup key (hash of calibration set IDs)
    pub subgroup_key: String,
    /// Display name (e.g., "Night 1 - Camera X" or auto-generated)
    pub display_name: String,
    /// Light frames in this subgroup
    pub frames: Vec<ExportFrame>,
    /// Linked Flat calibration set (with its own sub-calibrations)
    pub flat: Option<CalibrationSetInfo>,
    /// Linked Dark calibration set (with its own sub-calibrations)
    pub dark: Option<CalibrationSetInfo>,
    /// Linked Bias calibration set
    pub bias: Option<CalibrationSetInfo>,
    /// Warnings for this subgroup
    pub warnings: Vec<String>,
}

/// An export group - frames that will be stacked into one master light
/// Groups frames by filter AND camera type (OSC vs Mono)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportGroup {
    /// Unique group key for identification (e.g., "Ha_Mono")
    pub group_key: String,
    /// Filter name (None for unfiltered/OSC luminance)
    pub filter: Option<String>,
    /// Camera type (OSC or Mono)
    pub camera_type: CameraType,
    /// Display name for UI (e.g., "Ha (Mono)", "Luminance (OSC)")
    pub display_name: String,
    /// Calibration subgroups - frames grouped by their linked calibration sets
    pub subgroups: Vec<CalibrationSubgroup>,
    /// Total light frame count across all subgroups
    pub total_frames: i32,
    /// Total exposure time across all subgroups (seconds)
    pub total_exposure: f64,
    /// Warnings specific to this group
    pub warnings: Vec<String>,
}

impl ExportGroup {
    /// Generate a group key from filter and camera type
    pub fn make_group_key(filter: Option<&str>, camera_type: &CameraType) -> String {
        let filter_part = filter.unwrap_or("Unfiltered");
        format!("{}_{}", filter_part, camera_type.display_name())
    }

    /// Generate display name from filter and camera type
    pub fn make_display_name(filter: Option<&str>, camera_type: &CameraType) -> String {
        let filter_part = filter.unwrap_or("Luminance");
        format!("{} ({})", filter_part, camera_type.display_name())
    }
}

// ============================================================================
// Master Creation Plan
// ============================================================================

/// Plan for creating all required master calibration files
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterCreationPlan {
    /// Ordered list of masters to create (respects dependencies)
    pub masters: Vec<MasterInfo>,
    /// Map of set_id → master file path for reference
    pub master_paths: HashMap<i64, String>,
}

impl Default for MasterCreationPlan {
    fn default() -> Self {
        Self {
            masters: Vec::new(),
            master_paths: HashMap::new(),
        }
    }
}

/// Information about a master calibration file to create
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterInfo {
    /// Calibration set ID
    pub set_id: i64,
    /// Master type (Bias, Dark, DarkFlat, Flat)
    pub master_type: String,
    /// Output filename (e.g., "master_bias_3.fit")
    pub output_name: String,
    /// Source frames for this master
    pub source_frames: Vec<ExportFrame>,
    /// Dependencies - set IDs of masters needed before this one
    pub depends_on: Vec<i64>,
    /// Calibration master to apply: Bias set ID
    pub apply_bias: Option<i64>,
    /// Calibration master to apply: Dark set ID (for lights and darks that need dark calibration)
    /// For flats, this is only set if the dark exposure time matches the flat exposure (±30%)
    pub apply_dark: Option<i64>,
    /// Calibration master to apply: DarkFlat set ID (for flats - short exposure dark matching flat exposure)
    pub apply_darkflat: Option<i64>,
    /// Source frame exposure time (for exposure-time matching in flat calibration)
    #[serde(default)]
    pub source_exptime: Option<f64>,
}

// ============================================================================
// V3 Export Models - Nested Folder Hierarchy
// ============================================================================

/// Sanitize a name for use in folder paths
/// Replaces spaces and special characters with underscores
pub fn sanitize_folder_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

/// A calibration branch represents a unique path through the calibration hierarchy
/// Each branch has: Camera → Bias → Dark → Flat → (DarkFlat) → Lights
/// Branch ID includes filter for per-filter stacking
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationBranch {
    /// Unique branch identifier (e.g., "qhy268m_b23_d56_f38_L")
    pub branch_id: String,
    /// Camera name (from instrume field)
    pub camera_name: String,
    /// Sanitized camera name for folder paths
    pub camera_folder_name: String,
    /// Bias set ID (0 if missing)
    pub bias_id: i64,
    /// Dark set ID (0 if missing)
    pub dark_id: i64,
    /// Flat set ID (0 if missing)
    pub flat_id: i64,
    /// DarkFlat set ID (0 if missing)
    pub darkflat_id: i64,
    /// Filter name (from flat or lights)
    pub filter: Option<String>,
    /// Light frames in this branch
    pub light_frames: Vec<ExportFrame>,
    /// Calibration set info for bias level
    pub bias_info: Option<CalibrationSetInfo>,
    /// Calibration set info for dark level
    pub dark_info: Option<CalibrationSetInfo>,
    /// Calibration set info for flat level
    pub flat_info: Option<CalibrationSetInfo>,
    /// Calibration set info for darkflat level
    pub darkflat_info: Option<CalibrationSetInfo>,
}

impl CalibrationBranch {
    /// Generate branch ID from calibration set IDs and filter
    /// Format: "{camera}_b{bias}_d{dark}_f{flat}_{filter}"
    pub fn make_branch_id(
        camera: &str,
        bias_id: i64,
        dark_id: i64,
        flat_id: i64,
        filter: Option<&str>,
    ) -> String {
        let cam_safe = sanitize_folder_name(camera);
        let filter_safe = filter
            .map(|f| sanitize_folder_name(f))
            .unwrap_or_else(|| "nofilter".to_string());
        format!("{}_b{}_d{}_f{}_{}", cam_safe, bias_id, dark_id, flat_id, filter_safe)
    }

    /// Get total exposure time for this branch
    pub fn total_exposure(&self) -> f64 {
        self.light_frames
            .iter()
            .filter_map(|f| f.exptime)
            .sum()
    }

    /// Check if this branch uses OSC camera (has Bayer pattern)
    pub fn is_osc(&self) -> bool {
        self.light_frames
            .first()
            .and_then(|f| f.bayerpat.as_ref())
            .map(|p| !p.trim().is_empty())
            .unwrap_or(false)
    }
}

/// Complete export data using V3 nested hierarchy structure
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDataV3 {
    /// Frame set ID
    pub frame_set_id: i64,
    /// Frame set name
    pub frame_set_name: String,
    /// Object name (target name)
    pub object_name: Option<String>,
    /// All calibration branches (one per unique calibration path + filter)
    pub branches: Vec<CalibrationBranch>,
    /// Master creation plan (topologically sorted)
    pub master_plan: MasterCreationPlan,
    /// Total light frame count
    pub total_light_frames: i32,
    /// Total exposure time in seconds
    pub total_exposure_seconds: f64,
    /// All unique camera names
    pub cameras: Vec<String>,
    /// All unique filter names
    pub filters: Vec<String>,
}

impl ExportDataV3 {}

// ============================================================================
// Calibration Route (UI Display)
// ============================================================================

/// Calibration route for UI display - shows complete hierarchy and script preview
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRoute {
    /// Export groups and their calibration trees
    pub groups: Vec<CalibrationRouteGroup>,
    /// Generated Siril script previews
    pub script_preview: Vec<SirilScriptPreview>,
    /// Overall summary
    pub summary: CalibrationRouteSummary,
}

/// A group in the calibration route display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRouteGroup {
    /// Group display name (e.g., "Ha (Mono)")
    pub name: String,
    /// Number of light frames
    pub light_count: i32,
    /// Total exposure time (seconds)
    pub total_exposure: f64,
    /// Number of subgroups
    pub subgroup_count: i32,
    /// Calibration tree nodes
    pub calibration_tree: Vec<CalibrationTreeNode>,
}

/// A node in the calibration tree for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationTreeNode {
    /// Node type: "Light", "Flat", "Dark", "Bias", "DarkFlat"
    pub node_type: String,
    /// Display label (e.g., "Flat Set 5 (30 frames)")
    pub label: String,
    /// Calibration set ID (None for Light nodes)
    pub set_id: Option<i64>,
    /// Frame count
    pub count: i32,
    /// Child nodes (sub-calibrations)
    pub children: Vec<CalibrationTreeNode>,
    /// Warnings for this node
    pub warnings: Vec<String>,
    /// Whether this node is missing/incomplete
    pub is_missing: bool,
    /// Whether this set is shared with other subgroups/groups
    pub is_shared: bool,
}

/// Preview of a Siril script
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SirilScriptPreview {
    /// Script name (e.g., "00_create_masters.ssf")
    pub name: String,
    /// Script purpose description
    pub description: String,
    /// Full script content
    pub content: String,
}

/// Summary of the calibration route
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRouteSummary {
    /// Total export groups
    pub group_count: i32,
    /// Total light frames
    pub total_lights: i32,
    /// Total exposure time (seconds)
    pub total_exposure: f64,
    /// Number of unique calibration sets
    pub unique_calibration_sets: i32,
    /// Number of masters to create
    pub masters_to_create: i32,
    /// Calibration completeness flags
    pub flats_complete: bool,
    pub darks_complete: bool,
    pub bias_complete: bool,
    /// Overall warnings
    pub warnings: Vec<String>,
}

// ============================================================================
// Legacy Models (Kept for Backwards Compatibility)
// ============================================================================

/// Group of light frames by filter with their calibrations (legacy)
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
    /// Export groups (new structure with subgroups)
    pub groups: Vec<ExportGroup>,
    /// Master creation plan (ordered list of masters to create)
    pub master_plan: MasterCreationPlan,
    /// Filter groups with their calibrations (legacy - kept for compatibility)
    #[serde(default)]
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
    /// Creating calibration masters
    SirilCreatingMasters,
    /// Running Siril calibration
    SirilCalibrating,
    /// Collecting calibrated frames to unified directory
    CollectingCalibratedFrames,
    /// Running Siril registration
    SirilRegistering,
    /// Running Siril stacking
    SirilStacking,
    /// Export complete
    Complete,
    /// Export failed
    Failed,
}

// ============================================================================
// Global Registration Plan
// ============================================================================

/// Information about a branch's position in the global merged sequence
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchMergeInfo {
    /// Branch index in the data.branches array
    pub branch_idx: usize,
    /// Branch ID
    pub branch_id: String,
    /// Filter name
    pub filter: Option<String>,
    /// Camera type (Mono or OSC)
    pub camera_type: CameraType,
    /// Representative exposure time for this branch (median of frames)
    pub exptime: Option<f64>,
    /// Number of frames in this branch
    pub frame_count: usize,
    /// Starting frame index in the global sequence (1-based, as Siril uses)
    pub start_frame: usize,
    /// Ending frame index in the global sequence (inclusive)
    pub end_frame: usize,
}

/// Plan for global registration across all lights
///
/// When all calibrated lights are merged into a single sequence and registered
/// with a global reference frame, this structure tracks which frame numbers
/// in the merged sequence belong to which filter/camera for later stacking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalRegistrationPlan {
    /// Ordered list of branches in the merge order
    /// This determines frame numbering in the merged sequence
    pub merge_order: Vec<BranchMergeInfo>,
    /// Total number of frames in the merged sequence
    pub total_frames: usize,
    /// Unique filters present
    pub filters: Vec<Option<String>>,
    /// Unique camera types present
    pub camera_types: Vec<CameraType>,
}

/// Calculate median exposure time from a list of frames
fn calculate_median_exptime(frames: &[ExportFrame]) -> Option<f64> {
    let mut exptimes: Vec<f64> = frames
        .iter()
        .filter_map(|f| f.exptime)
        .collect();

    if exptimes.is_empty() {
        return None;
    }

    exptimes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = exptimes.len() / 2;

    if exptimes.len() % 2 == 0 && exptimes.len() >= 2 {
        Some((exptimes[mid - 1] + exptimes[mid]) / 2.0)
    } else {
        Some(exptimes[mid])
    }
}

/// Format exposure time for display in filenames
fn format_exptime_display(exptime: f64) -> String {
    if exptime < 1.0 {
        format!("{:.0}ms", exptime * 1000.0)
    } else if (exptime - exptime.round()).abs() < 0.01 {
        format!("{:.0}s", exptime)
    } else {
        format!("{:.1}s", exptime)
    }
}

/// A stacking group for frames with similar exposure times
///
/// Groups frames by filter, camera type, AND exposure time to ensure only
/// compatible frames are stacked together. OSC (3-channel) and Mono (1-channel)
/// frames cannot be stacked together.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExptimeStackGroup {
    /// Filter name for this group
    pub filter: Option<String>,
    /// Camera type for this group (OSC or Mono) - frames must match
    pub camera_type: CameraType,
    /// Representative exposure time for this group
    pub exptime: Option<f64>,
    /// Display string for output filename (e.g., "60s", "300s")
    pub exptime_display: String,
    /// Frame indices in the registered sequence (1-based)
    pub frame_indices: Vec<usize>,
}

impl GlobalRegistrationPlan {
    /// Create a global registration plan from export data
    pub fn from_export_data(data: &ExportDataV3) -> Self {
        let mut merge_order = Vec::new();
        let mut current_frame = 1; // Siril uses 1-based frame indices

        // Only include branches with >= 2 light frames (Siril requirement)
        for (idx, branch) in data.branches.iter().enumerate() {
            if branch.light_frames.len() >= 2 {
                let frame_count = branch.light_frames.len();
                let camera_type = if branch.is_osc() {
                    CameraType::Osc
                } else {
                    CameraType::Mono
                };

                // Calculate median exposure time for the branch
                let exptime = calculate_median_exptime(&branch.light_frames);

                merge_order.push(BranchMergeInfo {
                    branch_idx: idx,
                    branch_id: branch.branch_id.clone(),
                    filter: branch.filter.clone(),
                    camera_type,
                    exptime,
                    frame_count,
                    start_frame: current_frame,
                    end_frame: current_frame + frame_count - 1,
                });

                current_frame += frame_count;
            }
        }

        let total_frames = current_frame - 1;

        // Collect unique filters
        let mut filters: Vec<Option<String>> = merge_order
            .iter()
            .map(|b| b.filter.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        filters.sort();

        // Collect unique camera types
        let mut camera_types: Vec<CameraType> = merge_order
            .iter()
            .map(|b| b.camera_type.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        camera_types.sort_by_key(|c| c.display_name().to_string());

        Self {
            merge_order,
            total_frames,
            filters,
            camera_types,
        }
    }

    /// Get branches for a specific filter and camera type
    pub fn branches_for_filter_and_camera(
        &self,
        filter: &Option<String>,
        camera_type: &CameraType,
    ) -> Vec<&BranchMergeInfo> {
        self.merge_order
            .iter()
            .filter(|b| &b.filter == filter && &b.camera_type == camera_type)
            .collect()
    }

    /// Group branches into stacking groups by filter, camera type, and exposure time
    ///
    /// Returns a list of `ExptimeStackGroup` that can be iterated over for stacking.
    /// Each group contains frames that share the same filter, camera type, and have
    /// exposure times within the specified tolerance.
    ///
    /// **Critical**: OSC (3-channel) and Mono (1-channel) frames cannot be stacked
    /// together, so camera_type is always used as a grouping dimension.
    pub fn stacking_groups(
        &self,
        mode: &ExptimeToleranceMode,
        tolerance: f64,
    ) -> Vec<ExptimeStackGroup> {
        let mut groups = Vec::new();

        // Group by filter AND camera type (required - can't mix OSC and Mono)
        for filter in &self.filters {
            for camera_type in &self.camera_types {
                let branches = self.branches_for_filter_and_camera(filter, camera_type);

                if branches.is_empty() {
                    continue;
                }

                match mode {
                    ExptimeToleranceMode::Disabled => {
                        // All branches with same filter + camera_type go in one group
                        let frame_indices: Vec<usize> = branches
                            .iter()
                            .flat_map(|b| b.start_frame..=b.end_frame)
                            .collect();

                        groups.push(ExptimeStackGroup {
                            filter: filter.clone(),
                            camera_type: camera_type.clone(),
                            exptime: None,
                            exptime_display: String::new(),
                            frame_indices,
                        });
                    }
                    ExptimeToleranceMode::Absolute | ExptimeToleranceMode::Relative => {
                        let clustered =
                            self.cluster_by_exptime(&branches, filter, camera_type, mode, tolerance);
                        groups.extend(clustered);
                    }
                }
            }
        }

        groups
    }

    /// Cluster branches by exposure time within tolerance
    fn cluster_by_exptime(
        &self,
        branches: &[&BranchMergeInfo],
        filter: &Option<String>,
        camera_type: &CameraType,
        mode: &ExptimeToleranceMode,
        tolerance: f64,
    ) -> Vec<ExptimeStackGroup> {
        // Collect branches with valid exposure times
        let mut with_exptime: Vec<(&BranchMergeInfo, f64)> = branches
            .iter()
            .filter_map(|b| b.exptime.map(|e| (*b, e)))
            .collect();

        // Sort by exposure time
        with_exptime.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        if with_exptime.is_empty() {
            // No exposure times available, create single group with all frames
            let frame_indices: Vec<usize> = branches
                .iter()
                .flat_map(|b| b.start_frame..=b.end_frame)
                .collect();

            return vec![ExptimeStackGroup {
                filter: filter.clone(),
                camera_type: camera_type.clone(),
                exptime: None,
                exptime_display: String::new(),
                frame_indices,
            }];
        }

        // Cluster using tolerance
        let mut clusters: Vec<Vec<&BranchMergeInfo>> = Vec::new();
        let mut current_cluster: Vec<&BranchMergeInfo> = Vec::new();
        let mut cluster_first_exptime: Option<f64> = None;

        for (branch, exptime) in &with_exptime {
            let should_join = if let Some(first_exp) = cluster_first_exptime {
                match mode {
                    ExptimeToleranceMode::Absolute => {
                        (exptime - first_exp).abs() <= tolerance
                    }
                    ExptimeToleranceMode::Relative => {
                        let max_exp = exptime.max(first_exp);
                        if max_exp > 0.0 {
                            (exptime - first_exp).abs() / max_exp * 100.0 <= tolerance
                        } else {
                            true
                        }
                    }
                    ExptimeToleranceMode::Disabled => true,
                }
            } else {
                true // First item always joins
            };

            if should_join {
                current_cluster.push(branch);
                if cluster_first_exptime.is_none() {
                    cluster_first_exptime = Some(*exptime);
                }
            } else {
                // Start new cluster
                if !current_cluster.is_empty() {
                    clusters.push(current_cluster);
                }
                current_cluster = vec![branch];
                cluster_first_exptime = Some(*exptime);
            }
        }

        // Don't forget last cluster
        if !current_cluster.is_empty() {
            clusters.push(current_cluster);
        }

        // Convert clusters to ExptimeStackGroups
        let camera_type = camera_type.clone();
        clusters
            .into_iter()
            .map(|cluster| {
                let frame_indices: Vec<usize> = cluster
                    .iter()
                    .flat_map(|b| b.start_frame..=b.end_frame)
                    .collect();

                // Calculate representative exposure time (median of cluster)
                let mut exptimes: Vec<f64> = cluster.iter().filter_map(|b| b.exptime).collect();
                exptimes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let exptime = if exptimes.is_empty() {
                    None
                } else {
                    let mid = exptimes.len() / 2;
                    Some(exptimes[mid])
                };

                let exptime_display = exptime
                    .map(format_exptime_display)
                    .unwrap_or_default();

                ExptimeStackGroup {
                    filter: filter.clone(),
                    camera_type: camera_type.clone(),
                    exptime,
                    exptime_display,
                    frame_indices,
                }
            })
            .collect()
    }
}
