//! Data models for the export module

// The tests below name these through `use super::*`.
#[cfg(test)]
use crate::models::{FlatNormMode, LightCalParams};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Camera Type Detection
// ============================================================================

/// Camera type based on Bayer pattern presence
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
    /// How this frame reaches the export: `None` = copy the file at
    /// `file_path` as it is (every mode but one); `Some(debayer)` = the
    /// executor CALIBRATES it into place, debayering it when `true`.
    ///
    /// Set only by the calibrated-lights mode transform
    /// ([`crate::export::apply_export_mode`]), which also renames `filename`
    /// to the matching `c_*` output name. Collectors leave it `None` — a
    /// frame is copied unless something deliberately asks for generation.
    #[serde(default)]
    pub debayer_calibrated: Option<bool>,
}

/// A calibration set with its frames (legacy - kept for compatibility)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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

/// Sanitize a technical name (e.g. instrument) for use in folder paths.
/// Strips spaces and special characters, keeping only lowercase alphanumerics.
/// e.g. "ZWO 2600MM Pro" → "zwo2600mmpro"
pub fn sanitize_folder_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Sanitize a human-readable name for use as a folder name.
/// Preserves spaces, letters, digits, hyphens, underscores, dots, and parentheses.
/// Replaces filesystem-unsafe characters (: / \ * ? " < > |) with underscores,
/// then collapses consecutive underscores.
/// e.g. "Unknown @ RA=21:48:10.0, Dec=+47:17:35.5" → "Unknown @ RA=21_48_10.0, Dec=+47_17_35.5"
pub fn sanitize_display_folder_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            ':' | '/' | '\\' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    // Collapse consecutive underscores
    let mut result = String::with_capacity(sanitized.len());
    let mut prev_underscore = false;
    for c in sanitized.chars() {
        if c == '_' {
            if !prev_underscore {
                result.push(c);
            }
            prev_underscore = true;
        } else {
            result.push(c);
            prev_underscore = false;
        }
    }
    // Frame-set names are free user text and become a directory under the
    // user-chosen output folder: ".." must not climb out of it, "CON"/"NUL"
    // must not abort the export on Windows, and a trailing dot must not make
    // Win32 silently create a differently-named directory.
    crate::archive::path_layout::windows_safe_component(&result, "Unknown")
}

// ============================================================================
// Calibrated-light generation: run options + output naming
// ============================================================================
//
// Both live HERE, beside the folder sanitizers, rather than in
// `export::calibrated_generator` where the generator itself lives: the mode
// transform (`data_collector::apply_calibrated_lights`) names the output files
// and records the debayer decision, and that transform is UNGATED — it must
// compile in a headless build (`--no-default-features`), which has no pixel
// pipeline. Neither item needs one: the options are plain configuration and
// the filename rule is pure string work. The generator re-exports both, so it
// stays the place a reader looks for them.

/// The run options live in `starloom` (`starloom::calibration_library::options`, base layer: plain configuration);
/// this module re-exports the type under its old path. `resolve` and `Default` moved with it.
pub use starloom::calibration_library::options::CalibratedLightOptions;

/// `c_<stem>.fits`, or `c_<stem>_d.fits` for a debayered output — the ONE
/// place that spelling is defined. The export path names files before the
/// pixels are generated and the generator names them at write time; a second
/// implementation of this rule would let those two drift.
///
/// The extension is the chosen container's, never the source's: an XISF
/// source does not decide the output format.
pub fn calibrated_output_filename(
    source_filename: &str,
    debayer: bool,
    format: crate::fits_writer::OutputFormat,
) -> String {
    let stem = std::path::Path::new(source_filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(source_filename);
    let ext = format.extension();
    if debayer {
        format!("c_{stem}_d.{ext}")
    } else {
        format!("c_{stem}.{ext}")
    }
}

// ============================================================================
// Export Progress Events (Tauri event payloads)
// ============================================================================

/// Progress event emitted during export file organization
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgressEvent {
    pub frame_set_id: i64,
    /// Files copied so far
    pub current: usize,
    /// Total files to copy
    pub total: usize,
    pub percent: f64,
    pub current_file: Option<String>,
    /// "collecting" | "copying" | "calibrating" | "complete"
    /// ("calibrating" = this file is being generated from its masters rather
    /// than copied — the calibrated-lights mode.)
    pub phase: String,
}

/// Event emitted when export finishes (success or failure)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportCompleteEvent {
    pub frame_set_id: i64,
    pub success: bool,
    pub files_organized: i32,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub output_dir: String,
    /// Project export only (plan P11): files symlinked / copied as is /
    /// written as a FILTER-patched copy / failed. `0` for a frame-set export.
    #[serde(default)]
    pub linked: i32,
    #[serde(default)]
    pub copied: i32,
    #[serde(default)]
    pub rewritten: i32,
    #[serde(default)]
    pub failed: i32,
    /// See [`ExportResult::zip_copied`].
    #[serde(default)]
    pub zip_copied: usize,
    /// See [`LinkFallbackCounts`].
    #[serde(default)]
    pub link_fallback: LinkFallbackCounts,
}

// ============================================================================
// Legacy Models (Kept for Backwards Compatibility)
// ============================================================================

/// Group of light frames by filter with their calibrations (legacy)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
    /// Project export only (plan P11): files symlinked / copied as is /
    /// written as a FILTER-patched copy / failed. `0` for a frame-set export.
    #[serde(default)]
    pub linked: i32,
    #[serde(default)]
    pub copied: i32,
    #[serde(default)]
    pub rewritten: i32,
    #[serde(default)]
    pub failed: i32,
    /// Files that live inside a zip and were COPIED although the export was asked to link them (a link into the
    /// zip cache would dangle once the cache evicts the entry). `0` for an export with no such file.
    #[serde(default)]
    pub zip_copied: usize,
    /// See [`LinkFallbackCounts`].
    #[serde(default)]
    pub link_fallback: LinkFallbackCounts,
}

/// Link-mode placements Windows did not allow as symbolic links (os error 1314: no Developer Mode, not
/// elevated): made as hard links, or copied where a hard link is impossible too (another volume, FAT/exFAT,
/// a network share). Both `0` when every link was a symbolic link, and in a copy run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LinkFallbackCounts {
    pub hard_links: usize,
    pub copies: usize,
}

// ============================================================================
// Export Summary Models (Enhanced UI)
// ============================================================================

/// Complete export summary for the enhanced UI
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    /// Frame set ID
    pub frame_set_id: i64,
    /// Frame set name
    pub frame_set_name: String,
    /// Object name (target)
    pub object_name: Option<String>,
    /// Unique cameras used
    pub cameras: Vec<String>,
    /// Unique telescopes used
    pub telescopes: Vec<String>,
    /// Date range of sessions (start, end)
    pub date_range: Option<(String, String)>,
    /// Imaging nights the exported lights were shot on — the catalog's night
    /// (`imaging_nights`, noon-to-noon, derived), not the days in the span:
    /// three nights a year apart are 3, and a night that crosses midnight is 1.
    pub night_count: i64,
    /// Filter groups with full breakdown
    pub filter_groups: Vec<FilterGroupSummary>,
    /// Folder structure preview
    pub folder_preview: FolderPreview,
    /// Detailed warnings
    pub warnings: Vec<DetailedWarning>,
    /// Total file count
    pub total_files: i32,
    /// Estimated total size in bytes
    pub estimated_size_bytes: u64,
}

/// Summary for a single filter group (filter + camera type combination)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterGroupSummary {
    /// Filter name (None for unfiltered/luminance)
    pub filter: Option<String>,
    /// Camera type (OSC or Mono)
    pub camera_type: CameraType,
    /// Camera/instrument name
    pub camera: Option<String>,
    /// Telescope name
    pub telescope: Option<String>,
    /// Gain setting
    pub gain: Option<f64>,
    /// Offset setting
    pub offset: Option<f64>,
    /// Binning mode (e.g., "1x1")
    pub binning: Option<String>,
    /// Average CCD temperature
    pub avg_temp: Option<f64>,
    /// Exposure breakdown by exposure time
    pub exposure_groups: Vec<ExposureGroup>,
    /// Total exposure time in seconds
    pub total_exposure: f64,
    /// Total frame count
    pub frame_count: i32,
    /// Linked flat calibration info
    pub flat_info: Option<CalibrationDetail>,
    /// Linked dark calibration info
    pub dark_info: Option<CalibrationDetail>,
    /// Linked bias calibration info
    pub bias_info: Option<CalibrationDetail>,
    /// All frames in this group (for expandable list)
    pub frames: Vec<FrameDetail>,
}

/// Exposure time group (frames with same exposure)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExposureGroup {
    /// Exposure time in seconds
    pub exptime: f64,
    /// Number of frames with this exposure
    pub count: i32,
    /// Total exposure time for this group (exptime * count)
    pub total_seconds: f64,
}

/// Detailed calibration information
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationDetail {
    /// Calibration set ID
    pub set_id: i64,
    /// Calibration type (Flat, Dark, Bias, DarkFlat)
    pub calibration_type: String,
    /// Number of calibration frames
    pub frame_count: i32,
    /// Average exposure time (for darks/flats)
    pub avg_exptime: Option<f64>,
    /// Average CCD temperature
    pub avg_temp: Option<f64>,
    /// Match quality score (0.0 - 1.0)
    pub match_score: f64,
    /// Date range of calibration frames (start, end)
    pub date_range: Option<(String, String)>,
    /// Specific warnings for this calibration
    pub warnings: Vec<String>,
    /// Sub-calibrations (e.g., Flat -> Dark -> Bias chain)
    pub sub_calibrations: Vec<CalibrationDetail>,
}

/// Individual frame detail for expandable list
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameDetail {
    /// Frame ID
    pub frame_id: i64,
    /// Filename
    pub filename: String,
    /// Full file path
    pub file_path: String,
    /// Date observed
    pub date_obs: Option<String>,
    /// Exposure time in seconds
    pub exptime: Option<f64>,
    /// CCD temperature
    pub temp: Option<f64>,
    /// Gain setting
    pub gain: Option<f64>,
    /// Offset setting
    pub offset: Option<f64>,
    /// Calibration chain description (e.g., "Flat #12 → Dark #8 → Bias #3")
    pub calibration_chain: String,
    /// File size in bytes (if known)
    pub file_size: Option<u64>,
}

/// Folder structure preview for export
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FolderPreview {
    /// Root folder name
    pub root_name: String,
    /// Folder structure tree
    pub structure: Vec<FolderNode>,
    /// Total file count
    pub total_files: i32,
    /// Estimated total size (human readable)
    pub estimated_size: String,
}

/// A node in the folder structure tree
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    /// Node name (folder or file name)
    pub name: String,
    /// Node type
    pub node_type: FolderNodeType,
    /// File count (for folders)
    pub file_count: Option<i32>,
    /// Description (e.g., "← 50 darks, 100 bias")
    pub description: Option<String>,
    /// Child nodes
    pub children: Vec<FolderNode>,
}

/// Type of folder node
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum FolderNodeType {
    /// A folder
    Folder,
    /// A file
    File,
    /// Placeholder for multiple files (e.g., "... 50 more files")
    Ellipsis,
}

/// Detailed warning with full context
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DetailedWarning {
    /// Warning type
    pub warning_type: WarningType,
    /// Warning severity
    pub severity: WarningSeverity,
    /// Short title
    pub title: String,
    /// Detailed description
    pub description: String,
    /// Related calibration set ID (if applicable)
    pub set_id: Option<i64>,
    /// Related filter (if applicable)
    pub filter: Option<String>,
    /// Actual value (for mismatches)
    pub actual_value: Option<String>,
    /// Expected value (for mismatches)
    pub expected_value: Option<String>,
    /// Delta/difference (for numeric comparisons)
    pub delta: Option<String>,
    /// Recommendation text
    pub recommendation: Option<String>,
}

/// Warning type categories
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WarningType {
    /// Temperature mismatch between lights and calibration
    TemperatureMismatch,
    /// Calibration frames are old compared to lights
    CalibrationAge,
    /// Missing calibration (using fallback)
    MissingCalibration,
    /// General warning
    General,
}

/// Warning severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    /// Informational (won't affect results much)
    Info,
    /// Warning (may affect results)
    Warning,
    /// Error (likely to cause issues)
    Error,
}

// ============================================================================
// WBPP Export Configuration
// ============================================================================

/// What a WBPP export puts on disk for the lights + calibration side (spec §12.2).
///
/// The default ([`ExportMode::RawWithCalibrationSets`]) reproduces the historical
/// behavior bit-for-bit, so an existing config (which never carried this field)
/// deserializes via `#[serde(default)]` into zero behavioral change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ExportMode {
    /// Raw light frames only — every calibration node dropped, light paths
    /// untouched. The frame-set send's "just the lights"; for a folder export
    /// the lights land under `camera_<x>/lights/`.
    LightsOnly,
    /// Calibrate every in-scope light from its linked masters as the export
    /// places it — `c_*.fits` generated at export/send time, with NO
    /// calibration frames at all (WBPP runs with its own calibration
    /// disabled). Nothing is cached or reused: there is no artifact store to
    /// be fresh or stale against. Gated instead on the INPUTS — the v2 §4
    /// masters-built gate (`api::lights::check_mode_ready`): every linked
    /// calibration set must already be a built master, and a light with no
    /// calibration links at all blocks.
    CalibratedLights,
    /// Raw lights as today, with the calibration side exporting ONLY built
    /// master files (`calibration_set.is_master_library = 1`). Strict (spec
    /// 2026-08-28 D2): a linked set that still has raw frames is an error, not
    /// a silent omission — what the summary shows is what lands.
    RawWithMasters,
    /// Default: raw lights plus the raw calibration sets behind their links.
    /// Where a link names an Athenaeum-built master, the raw set that master
    /// superseded is exported instead — the build repointed the link, this
    /// mode promises the raw frames (`data_collector::resolve_raw_calibration_sets`).
    /// An imported master, with nothing raw behind it, stays and is reported;
    /// raw originals that are not on disk (archived after the build) block the
    /// mode up front (`ExportReadiness.missing_raw_calibration_files`).
    RawWithCalibrationSets,
}

impl Default for ExportMode {
    fn default() -> Self {
        ExportMode::RawWithCalibrationSets
    }
}

/// Configuration for WBPP export folder hierarchy
///
/// Controls the keyword nesting order used to build the folder structure.
/// WBPP's "Grouping Keywords with Pre" reads folder nesting to determine
/// calibration chains — parent calibrates child.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct WbppExportConfig {
    /// Keyword nesting order (outermost first).
    /// Default: ["CAMERA", "BIAS", "DARKS", "FLAT"]
    pub keyword_order: Vec<String>,
    /// What the export writes for lights + calibration (spec §12.2). Defaults
    /// to [`ExportMode::RawWithCalibrationSets`] (today's behavior) so a config
    /// persisted before this field existed loads unchanged.
    #[serde(default)]
    pub export_mode: ExportMode,
}

impl Default for WbppExportConfig {
    fn default() -> Self {
        Self {
            keyword_order: vec![
                "CAMERA".to_string(),
                "BIAS".to_string(),
                "DARKS".to_string(),
                "FLAT".to_string(),
            ],
            export_mode: ExportMode::default(),
        }
    }
}

/// How many files each export mode would place for one frame set — the
/// informational half of `ExportReadiness` (spec 2026-08-28 §5). Computed by a
/// count-only walk that never bails, so a not-ready mode still shows a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportFileCounts {
    pub lights_only: i64,
    pub raw_with_calibration_sets: i64,
    pub raw_with_masters: i64,
    pub calibrated_lights: i64,
}

/// Setup instructions for configuring WBPP grouping keywords
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct WbppSetupInstructions {
    /// Ordered list of keywords to configure in WBPP
    pub keywords: Vec<WbppKeywordInstruction>,
    /// Example folder structure matching the current config
    pub example_structure: String,
}

/// A single WBPP keyword instruction
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct WbppKeywordInstruction {
    /// The WBPP grouping keyword name
    pub keyword: String,
    /// Whether "Pre" should be checked for this keyword
    pub pre_checked: bool,
    /// Description of what this keyword controls
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_folder_name_is_windows_safe() {
        assert_eq!(sanitize_display_folder_name(".."), "Unknown");
        assert_eq!(sanitize_display_folder_name("CON"), "CON_");
        assert_eq!(sanitize_display_folder_name("M31."), "M31");
        assert_eq!(sanitize_display_folder_name("M31 Panel 1"), "M31 Panel 1");
        assert_eq!(sanitize_display_folder_name("a\u{7}b"), "ab");
    }

    /// Every field is optional on the wire: a host that knows only some of
    /// these — or a payload written before a field existed — must decode, not
    /// fail. `{}` is the extreme case and has to equal the documented default.
    #[test]
    fn options_decode_from_a_partial_payload() {
        let empty: CalibratedLightOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, CalibratedLightOptions::default());

        let partial: CalibratedLightOptions =
            serde_json::from_str(r#"{"debayerOsc": false}"#).unwrap();
        assert!(!partial.debayer_osc, "the sent field wins");
        assert!(partial.flat_norm, "omitted flat_norm defaults ON");
        assert!(
            partial.hot_pixel_correction,
            "omitted hot_pixel_correction defaults ON"
        );
        assert_eq!(partial.flat_norm_mode, FlatNormMode::CentralThird);
        // An omitted `params` defaults wholesale, its own fields included.
        assert_eq!(partial.params, LightCalParams::default());
        assert_eq!(
            partial,
            CalibratedLightOptions {
                debayer_osc: false,
                ..CalibratedLightOptions::default()
            }
        );

        // The camelCase spelling is the wire contract; round-tripping our own
        // serialization must land back on the same value.
        let round: CalibratedLightOptions =
            serde_json::from_str(&serde_json::to_string(&partial).unwrap()).unwrap();
        assert_eq!(round, partial);
    }

    /// The whole-frame trimmed-mean mode was renamed in code; the persisted /
    /// over-the-wire string is deliberately unchanged, so a stacking config,
    /// a `flat_norm_mode` column value, a saved frontend preference or a
    /// request from an older build still reads — and what this build writes is
    /// still readable by an older one. The new spelling is accepted as an alias.
    #[test]
    fn flat_norm_mode_old_wire_string_still_reads_and_is_still_written() {
        let old: FlatNormMode = serde_json::from_str(r#""pixinsightTrimmed""#).unwrap();
        assert_eq!(old, FlatNormMode::TrimmedMeanWhole);
        let alias: FlatNormMode = serde_json::from_str(r#""trimmedMeanWhole""#).unwrap();
        assert_eq!(alias, FlatNormMode::TrimmedMeanWhole);
        let central: FlatNormMode = serde_json::from_str(r#""centralThird""#).unwrap();
        assert_eq!(central, FlatNormMode::CentralThird);

        // Written form == the stored/wire string, for both modes.
        for mode in [FlatNormMode::CentralThird, FlatNormMode::TrimmedMeanWhole] {
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode.as_wire_str())
            );
        }
        assert_eq!(FlatNormMode::TrimmedMeanWhole.as_wire_str(), "pixinsightTrimmed");

        // Inside the real option payload an old client sends.
        let opts: CalibratedLightOptions =
            serde_json::from_str(r#"{"flatNormMode": "pixinsightTrimmed"}"#).unwrap();
        assert_eq!(opts.flat_norm_mode, FlatNormMode::TrimmedMeanWhole);
        let round: CalibratedLightOptions =
            serde_json::from_str(&serde_json::to_string(&opts).unwrap()).unwrap();
        assert_eq!(round, opts);
    }

    #[test]
    fn calibrated_output_filename_takes_the_container() {
        use crate::fits_writer::OutputFormat;
        assert_eq!(
            calibrated_output_filename("L_1.fits", false, OutputFormat::Fits),
            "c_L_1.fits"
        );
        assert_eq!(
            calibrated_output_filename("L_1.fits", true, OutputFormat::Xisf),
            "c_L_1_d.xisf"
        );
        assert_eq!(
            calibrated_output_filename("L_1.xisf", false, OutputFormat::Fits),
            "c_L_1.fits",
            "an XISF source still yields the chosen container, not its own"
        );
    }

    #[test]
    fn options_format_defaults_to_fits_and_decodes_old_documents() {
        use crate::fits_writer::OutputFormat;
        let old: CalibratedLightOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(old.format, OutputFormat::Fits);
        let r =
            CalibratedLightOptions::resolve(None, None, None, None, None, Some(OutputFormat::Xisf));
        assert_eq!(r.format, OutputFormat::Xisf);
    }
}
