//! Data models for the export module

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Pixel size in micrometers (from XPIXSZ or PIXSIZE1 FITS header)
    pub xpixsz: Option<f64>,
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

// ============================================================================
// Calibration Route (UI Display)
// ============================================================================

/// Calibration route for UI display - shows complete hierarchy and script preview
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRoute {
    /// Export groups and their calibration trees
    pub groups: Vec<CalibrationRouteGroup>,
    /// Generated Siril script previews (kept for type compatibility)
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

/// Preview of a Siril script (kept for type compatibility)
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
