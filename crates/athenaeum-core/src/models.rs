use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// Represents a physical file on disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct File {
    pub id: Option<i64>,
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub modified_at: DateTime<Utc>,
    pub format: FileFormat,
    pub created_at: DateTime<Utc>,
    pub metadata_hash: Option<String>,
    pub content_hash: Option<String>,
    // ZIP archive feature — populated when this file's data has been moved
    // into a zip in the archive root. Original `path` is preserved for restore.
    pub archived_in_operation: Option<i64>,
    pub archive_zip_path: Option<String>,
    pub archive_path_in_zip: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FileFormat {
    FITS,
    XISF,
}

/// Effective temperature for a frame: prefer the measured `ccd_temp`, but
/// fall back to `set_temp` (commanded cooler temperature) when the measured
/// reading is missing. Master frames and uncooled emergency captures often
/// arrive with `ccd_temp = NULL` but a valid `set_temp` — without this
/// fallback those frames would cluster into a single "no-temperature" bucket
/// and silently mix cooled and uncooled darks. Auto-link / Manual modal both
/// use this helper.
pub fn effective_temp(ccd_temp: Option<f64>, set_temp: Option<f64>) -> Option<f64> {
    ccd_temp.or(set_temp)
}

/// Heuristic: a frame is "filter-ambiguous mono" when it has no Bayer pattern
/// (so it isn't an OSC sensor) and no FILTER keyword. Filter wheels on mono
/// cameras normally write the slot label to FILTER; a NULL FILTER on a mono
/// camera with a wheel is most often a driver hiccup or hand-edit, not an
/// "I shot without any filter" intention. Auto-link can't tell which physical
/// filter the frame came through, so the matcher logs a warning when it sees
/// this case (used by A7). Pure-mono setups without a filter wheel will also
/// trigger this heuristic — that's intentional, the user must confirm manually
/// that auto-linking by NULL-filter is what they want.
pub fn is_mono_with_ambiguous_filter(bayerpat: &Option<String>, filter: &Option<String>) -> bool {
    bayerpat.is_none() && filter.is_none()
}

/// Represents a FITS/XISF frame with metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Frame {
    pub id: Option<i64>,
    pub file_id: i64,
    pub object: Option<String>,
    pub date_obs: Option<DateTime<Utc>>,
    pub telescop: Option<String>,
    pub instrume: Option<String>,
    pub exptime: Option<f64>,
    pub filter: Option<String>,
    pub imagetyp: Option<ImageType>,
    pub is_master: bool,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub binning: Option<String>,
    pub xbinning: Option<i32>,
    pub ybinning: Option<i32>,
    pub ccd_temp: Option<f64>,
    pub set_temp: Option<f64>,
    pub focallen: Option<f64>,
    pub xpixsz: Option<f64>,
    pub ypixsz: Option<f64>,
    pub naxis1: Option<i32>,
    pub naxis2: Option<i32>,
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    pub sitelat: Option<f64>,
    pub lat_obs: Option<f64>,
    pub sitelong: Option<f64>,
    pub long_obs: Option<f64>,
    pub objctra: Option<String>,
    pub objctdec: Option<String>,
    pub override_: bool,
    pub swcreate: Option<String>,
    /// Bayer pattern for OSC cameras (e.g., "RGGB", "BGGR")
    /// None indicates a mono camera
    pub bayerpat: Option<String>,
    /// Image position angle in degrees (North through East)
    /// Extracted from CROTA2 or computed from CD matrix
    pub rotation: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ImageType {
    Light,
    Dark,
    Flat,
    Bias,
    DarkFlat,
    MasterLight,
    MasterDark,
    MasterFlat,
    MasterBias,
    MasterDarkFlat,
}

impl ImageType {
    pub fn from_str(s: &str) -> Option<Self> {
        let s_upper = s.to_uppercase();
        match s_upper.as_str() {
            // Light frames: IRAF, SBFITSEXT, INDI FRAME keyword
            "LIGHT" | "LIGHT FRAME" => Some(Self::Light),
            // Dark frames: IRAF, SBFITSEXT
            "DARK" | "DARK FRAME" => Some(Self::Dark),
            // Flat frames: IRAF, SBFITSEXT ("Flat Field"), INDI ("Flat Frame"), observatory variants
            "FLAT" | "FLAT FIELD" | "FLAT FRAME" | "FLATFIELD"
                | "SKY FLAT" | "DOME FLAT" | "TWILIGHT FLAT" => Some(Self::Flat),
            // Bias frames: IRAF, SBFITSEXT, "Offset" alias
            "BIAS" | "BIAS FRAME" | "OFFSET" | "OFFSET FRAME" => Some(Self::Bias),
            // Dark flats: various software
            "DARKFLAT" | "DARK FLAT" | "DARK FLAT FRAME"
                | "FLATDARK" | "FLAT DARK" | "FLAT DARK FRAME" => Some(Self::DarkFlat),
            // Master frames
            "MASTER LIGHT" | "MASTERLIGHT" | "MASTER LIGHT FRAME" => Some(Self::MasterLight),
            "MASTER DARK" | "MASTERDARK" | "MASTER DARK FRAME" => Some(Self::MasterDark),
            "MASTER FLAT" | "MASTERFLAT" | "MASTER FLAT FIELD" | "MASTER FLAT FRAME" => Some(Self::MasterFlat),
            "MASTER BIAS" | "MASTERBIAS" | "MASTER BIAS FRAME"
                | "MASTER OFFSET" | "MASTEROFFSET" => Some(Self::MasterBias),
            "MASTER DARK FLAT" | "MASTERDARKFLAT" | "MASTER DARKFLAT"
                | "MASTER FLAT DARK" | "MASTERFLATDARK" | "MASTER FLATDARK" => Some(Self::MasterDarkFlat),
            _ => None,
        }
    }

    pub fn is_master(&self) -> bool {
        matches!(
            self,
            Self::MasterLight | Self::MasterDark | Self::MasterFlat | Self::MasterBias | Self::MasterDarkFlat
        )
    }
}

/// Monitored scan root path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRoot {
    pub id: Option<i64>,
    pub path: String,
    pub enabled: bool,
    pub find_duplicates: bool,
    pub unique_camera: bool,
    pub last_scan: Option<DateTime<Utc>>,
    pub last_scan_errors: Option<Vec<String>>,
    pub monitor_enabled: bool,
}

/// A single file inside a duplicate group, enriched with the metadata the
/// frontend needs to apply "keep" rules (modified time, assigned scan root)
/// and to display meaningful per-group summaries (filename, frame type,
/// observation date).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    pub file_id: i64,
    pub path: String,
    pub filename: String,
    pub modified_at: DateTime<Utc>,
    /// Longest-prefix match against `scan_roots.path`. `None` when the file
    /// isn't inside any registered scan root (shouldn't happen for scanned
    /// libraries, but we don't want to drop the row).
    pub scan_root_id: Option<i64>,
    pub scan_root_path: Option<String>,
    /// IMAGETYP from the frame row (raw string — e.g. "LIGHT", "FLAT").
    /// `None` if no frame row or the field was missing.
    pub imagetyp: Option<String>,
    /// DATE-OBS from the frame row (RFC3339). `None` if not set.
    pub date_obs: Option<String>,
}

/// Duplicate detection result.
///
/// `file_paths` + `file_ids` are kept as parallel-array convenience fields
/// for existing consumers (folder view, etc.); new code should use `files`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub id: Option<i64>,
    pub size: i64,
    pub content_hash: String,
    pub file_count: i32,
    pub file_paths: Vec<String>,
    pub file_ids: Vec<i64>,
    #[serde(default)]
    pub files: Vec<DuplicateFile>,
}

/// Result of a bulk move-to-black-hole operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkMoveResult {
    pub moved: usize,
    /// Per-file failures — (file_id, error message). Empty on full success.
    pub failed: Vec<(i64, String)>,
}

/// Black hole entry (soft-deleted file)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackHoleEntry {
    pub id: Option<i64>,
    pub file_id: i64,
    pub filename: String,
    pub original_path: String,
    pub from_where: String,
    pub moved_at: DateTime<Utc>,
    pub file_size: i64,
}

/// Folder similarity result for duplicate detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderSimilarity {
    pub folder_a: String,
    pub folder_b: String,
    pub similarity_percent: f64,
    pub shared_files: i32,
    pub shared_size: i64,
    pub unique_a: i32,
    pub unique_b: i32,
    pub shared_file_ids: Vec<i64>,
}

/// Frames set (collection of related frames)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FramesSet {
    pub id: Option<i64>,
    pub name: Option<String>,
    pub is_custom: bool,
    pub is_archived: bool,
    pub date_obs_start: Option<String>,
    pub date_obs_end: Option<String>,
    pub objctra: Option<String>,
    pub objctdec: Option<String>,
    pub total_exp_time: Option<f64>,
    pub flat_pattern: Option<String>,
    pub avg_rotation: Option<f64>,
    pub min_rotation: Option<f64>,
    pub max_rotation: Option<f64>,
    // ZIP archive feature — populated when this frame set has been ZIP-archived
    pub archived_at: Option<String>,          // ISO 8601 timestamp string
    pub archive_operation_id: Option<i64>,
}

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Imaging night - top-level grouping of frames by observation night
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagingNight {
    pub id: Option<i64>,
    pub frames_set_id: i64,
    pub start_time: String,
    pub end_time: String,
    pub created_at: Option<DateTime<Utc>>,
}

/// Session - grouping of frames by instrument within an imaging night
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Option<i64>,
    pub imaging_night_id: i64,
    pub instrume: String,
    pub frame_count: i32,
    pub total_exp_time: Option<f64>,
    pub created_at: Option<DateTime<Utc>>,
}

/// DTO: File with optional frame metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWithFrame {
    pub file: File,
    pub frame: Option<Frame>,
}

/// DTO: a row on the Missing Metadata page. Extends `FileWithFrame` with a
/// duplicate-detection flag so the UI can show which missing-metadata files
/// also have a duplicate somewhere in the catalog. Uses camelCase on the
/// wire to match the frontend convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingMetadataRow {
    pub file: File,
    pub frame: Frame,
    pub has_duplicate: bool,
}

/// DTO: Session with its frames
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionWithFrames {
    pub session: Session,
    pub frames: Vec<FileWithFrame>,
}

/// DTO: Imaging night with its sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagingNightWithSessions {
    pub imaging_night: ImagingNight,
    pub sessions: Vec<SessionWithFrames>,
}

/// DTO: Complete frame set detail with nights and sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameSetDetail {
    pub frames_set: FramesSet,
    pub nights: Vec<ImagingNightWithSessions>,
}

/// Equipment/Camera statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraStats {
    pub instrume: String,
    pub frame_count: i64,
    pub total_hours: f64,
    pub first_use: Option<String>,  // ISO 8601
    pub last_use: Option<String>,   // ISO 8601
}

/// Calibration set with extended metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSetDetail {
    pub id: Option<i64>,
    pub imagetyp: ImageType,
    pub exptime: Option<f64>,
    pub ccd_temp: f64,           // Average temperature
    pub temp_min: f64,
    pub temp_max: f64,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub binning: Option<String>,
    pub instrume: Option<String>,
    pub filter: Option<String>,  // Filter (for flats)
    pub date_start: String,      // ISO 8601
    pub date_end: String,        // ISO 8601
    pub date_display: String,    // e.g., "2025-10"
    pub frame_count: i64,
    pub is_master: bool,         // True if this is a master calibration set
    // Extended fields from frame metadata
    pub naxis1: Option<i32>,     // Image width
    pub naxis2: Option<i32>,     // Image height
    pub bayerpat: Option<String>, // Bayer pattern (e.g., "RGGB") or None for mono
    pub swcreate: Option<String>, // Software that created the file
    pub xpixsz: Option<f64>,     // Pixel size in microns
    pub format: Option<String>,  // File format (FITS, XISF)
    pub focallen: Option<f64>,   // Focal length in mm
}

/// Result of dark library creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DarkLibraryResult {
    pub sets_created: i64,
    pub frames_grouped: i64,
    pub frames_excluded: i64,
}

/// Result of file relinking operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelinkResult {
    pub files_matched: usize,
    pub files_new: usize,
    pub files_orphaned: usize,
    pub orphaned_file_ids: Vec<i64>,
}

/// Represents an orphaned file that couldn't be relinked
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanedFile {
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub modified_at: String,
    pub has_frame: bool,
    pub object: Option<String>,
    pub date_obs: Option<String>,
}

/// Sky atlas imaging location (for visualization)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagingLocation {
    pub id: i64,
    pub ra: f64,
    pub dec: f64,
    pub object_name: Option<String>,
    pub frame_count: i32,
    pub total_exposure: f64,  // in seconds
    pub filters: Vec<String>,
    pub date_range: (String, String),  // ISO date strings
    pub frame_set_id: Option<i64>,
    pub fov_width: Option<f64>,   // Field of view in degrees
    pub fov_height: Option<f64>,  // Field of view in degrees
    pub location_type: String,  // "frameset" or "cluster" for unorganized frames
    pub cameras: Option<String>,  // Comma-separated list of camera/instrument names
    pub focal_lengths: Option<String>,  // Comma-separated list of focal lengths in mm
    pub is_custom: bool,  // true for custom frame sets, false for auto-generated or clusters
    pub rotation: Option<f64>,  // Average position angle in degrees (N through E)
}

/// Bounding box for rectangular region selection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionBounds {
    pub ra_min: f64,
    pub ra_max: f64,
    pub dec_min: f64,
    pub dec_max: f64,
    #[serde(default)]
    pub crosses_meridian: Option<bool>,
}

/// A candidate frame from a spatial query with its coordinates
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionCandidate {
    pub id: i64,
    pub ra: f64,
    pub dec: f64,
    pub exposure: f64,
}

/// Result of a spatial selection query (raw candidates with coordinates)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionCandidates {
    pub candidates: Vec<SelectionCandidate>,
    pub count: usize,
    pub total_exposure_seconds: f64,
}



/// Selection criteria for splitting frame sets
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SplitSelection {
    Nights { ids: Vec<i64> },
    Sessions { ids: Vec<i64> },
    Frames { ids: Vec<i64> },
}

/// Link between a frame/calibration set and its required calibration set
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationLink {
    pub id: Option<i64>,
    pub source_id: i64,
    pub source_type: String,  // 'frame' or 'calibration_set'
    pub calibration_set_id: i64,
    pub calibration_type: String,  // 'Dark', 'Flat', 'Bias', 'DarkFlat'
    pub matched_at: String,  // ISO 8601
    pub match_score: Option<f64>,  // 0.0-1.0 confidence
    pub date_warning: bool,
    pub temp_warning: bool,
    pub is_manual_override: bool,  // true if manually assigned by user
}

/// Calibration status for a single frame
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameCalibrationStatus {
    pub frame_id: i64,
    pub has_flats: bool,
    pub has_darks: bool,
    pub has_bias: bool,
    pub has_darkflats: bool,
    pub flats_warning: bool,
    pub darks_warning: bool,
    pub bias_warning: bool,
    pub flat_set_id: Option<i64>,
    pub dark_set_id: Option<i64>,
    pub bias_set_id: Option<i64>,
    pub darkflat_set_id: Option<i64>,
}

/// Complete calibration hierarchy for a frame
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationHierarchy {
    pub light_frame_id: i64,
    pub flat_sets: Vec<CalibrationSetWithLinks>,
    pub dark_sets: Vec<CalibrationSetWithLinks>,
    pub missing_calibration: Vec<String>,  // List of missing calibration types
    pub warnings: Vec<CalibrationWarning>,
}

/// Calibration set with its sub-calibration links
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSetWithLinks {
    pub set: CalibrationSetDetail,
    pub sub_calibration: Vec<CalibrationLink>,  // Links to Dark/Bias sets for this set
}

/// Warning about calibration quality
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationWarning {
    pub warning_type: String,  // 'date' or 'temperature'
    pub message: String,
    pub calibration_type: String,  // 'Dark', 'Flat', 'Bias', 'DarkFlat'
    pub set_id: i64,
}

/// Statistics about calibration for a frame set
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationStats {
    pub total_frames: usize,
    pub frames_with_flats: usize,
    pub frames_with_darks: usize,
    pub frames_with_bias: usize,
    pub frames_complete: usize,  // All required calibration found
    pub frames_partial: usize,    // Some calibration found
    pub frames_none: usize,       // No calibration found
    pub total_warnings: usize,
}

/// Group of frames sharing the same calibration set combination
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationGroup {
    pub flat_set_id: Option<i64>,
    pub dark_set_id: Option<i64>,
    pub bias_set_id: Option<i64>,
    pub flat_set_detail: Option<CalibrationSetDetail>,
    pub dark_set_detail: Option<CalibrationSetDetail>,
    pub bias_set_detail: Option<CalibrationSetDetail>,
    pub frame_count: usize,
    pub frame_ids: Vec<i64>,
    pub has_warnings: bool,
    // Per-calibration warnings with contextual messages
    pub flat_warnings: Vec<CalibrationWarning>,
    pub dark_warnings: Vec<CalibrationWarning>,
    pub bias_warnings: Vec<CalibrationWarning>,
}

/// Complete calibration grouping for a frame set
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameSetCalibrationGroups {
    pub groups: Vec<CalibrationGroup>,
    pub uncalibrated_frame_count: usize,
    pub uncalibrated_frame_ids: Vec<i64>,
    pub total_frames: usize,
}

/// Tolerance configuration for calibration matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationTolerance {
    pub flat_date_warning_days: i64,
    pub dark_date_warning_days: i64,
}

impl Default for CalibrationTolerance {
    fn default() -> Self {
        Self {
            flat_date_warning_days: 30,
            dark_date_warning_days: 365,
        }
    }
}

// ========== Calibration Hierarchy View Structures ==========

/// Hierarchical calibration view organized by Date → Camera → Filter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationHierarchyView {
    pub date_groups: Vec<CalibrationDateGroup>,
    pub total_frames: usize,
    pub calibrated_frames: usize,
    pub uncalibrated_frames: usize,
}

/// Group of frames for a single session date
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationDateGroup {
    pub date: String,                    // e.g., "2024-01-15"
    pub date_display: String,            // e.g., "January 15, 2024"
    pub camera_groups: Vec<CalibrationCameraGroup>,
    pub frame_count: usize,
    pub has_warnings: bool,
}

/// Group of frames for a single camera within a date
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationCameraGroup {
    pub instrume: String,                // Camera name
    pub filter_groups: Vec<CalibrationFilterGroup>,
    pub frame_count: usize,
    pub has_warnings: bool,
}

/// Sub-calibration linked to a calibration set with full details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubCalibrationDetail {
    pub calibration_type: String,        // "Dark", "DarkFlat", "Bias"
    pub set: CalibrationSetDetail,       // Full set details
    pub date_warning: bool,
    pub temp_warning: bool,
}

/// A calibration set with the count of frames that use it
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSetWithFrameCount {
    pub set: CalibrationSetDetail,
    pub frame_count: i64,              // How many frames in this group use this set
    pub frame_ids: Vec<i64>,           // Which frames use this set
    pub warnings: Vec<CalibrationWarning>,
    pub sub_calibration: Vec<SubCalibrationDetail>,  // Linked sub-calibrations (e.g., Flat→Dark, Dark→Bias)
}

/// A calibration set with match score for manual selection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSetWithScore {
    pub set: CalibrationSetDetail,
    pub match_score: f64,              // 0.0-1.0, higher is better match
    pub match_details: MatchDetails,
}

/// Details about how well a calibration set matches light frame parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchDetails {
    pub instrume_match: bool,          // Camera matches
    pub binning_match: bool,           // Binning matches
    pub gain_match: bool,              // Gain matches (or both null)
    pub offset_match: bool,            // Offset matches (or both null)
    pub exptime_match: bool,           // Exposure time matches (CRITICAL for darks!)
    pub filter_match: bool,            // Filter matches (only relevant for flats)
    pub temp_diff: Option<f64>,        // Temperature difference in Celsius
    pub date_diff_days: i64,           // Days between calibration and light frames
}

/// Average parameters of light frames for manual selection display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightFrameParameters {
    pub instrume: Option<String>,
    pub binning: Option<String>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub filter: Option<String>,
    pub avg_ccd_temp: Option<f64>,
    pub avg_exptime: Option<f64>,
    pub exptime_range: Option<(f64, f64)>,  // min, max
    pub frame_count: usize,
    pub date_range: Option<(String, String)>,  // start, end
    pub current_flat_set_id: Option<i64>,
    pub current_dark_set_id: Option<i64>,
    pub current_bias_set_id: Option<i64>,
}

/// Parameters of a calibration set for sub-calibration selection display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSetParameters {
    pub set_id: i64,
    pub imagetyp: String,
    pub instrume: Option<String>,
    pub binning: Option<String>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub exptime: Option<f64>,
    pub filter: Option<String>,
    pub ccd_temp: Option<f64>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub frame_count: i64,
    // Current sub-calibration links
    pub current_dark_set_id: Option<i64>,
    pub current_darkflat_set_id: Option<i64>,
    pub current_bias_set_id: Option<i64>,
}

/// Group of frames for a single filter within a camera
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationFilterGroup {
    pub filter: Option<String>,          // None = "No Filter"
    pub filter_display: String,          // "Ha", "OIII", "No Filter", or "Ha (60s)" when split by exptime
    pub exptime: Option<f64>,            // When split by exposure time, this is the exptime for this sub-group
    pub light_frames: Vec<LightFrameWithCalibration>,
    pub flat_sets: Vec<CalibrationSetWithFrameCount>,   // All unique flat sets used by frames in this group
    pub dark_sets: Vec<CalibrationSetWithFrameCount>,   // All unique dark sets used by frames in this group
    pub bias_sets: Vec<CalibrationSetWithFrameCount>,   // All unique bias sets used by frames in this group
    pub has_warnings: bool,
    pub frame_count: usize,
}

/// A light frame with its calibration status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightFrameWithCalibration {
    pub frame_id: i64,
    pub file_id: i64,
    pub filename: String,
    pub file_path: String,
    pub date_obs: Option<String>,
    pub exptime: Option<f64>,
    pub telescop: Option<String>,
    pub focallen: Option<f64>,
    pub xpixsz: Option<f64>,
    pub binning: Option<String>,
    pub ccd_temp: Option<f64>,
    pub swcreate: Option<String>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub rotation: Option<f64>,
    pub objctra: Option<String>,
    pub objctdec: Option<String>,
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    /// True when a `plate_solves` row exists for this frame, i.e. the WCS
    /// was computed by Athenaeum's plate solver (vs. read from the FITS
    /// header). Drives the Original/Athenaeum WCS badge.
    pub plate_solved: bool,
    pub calibration_status: FrameCalibrationStatus,
}

// ========== Calendar View Structures ==========

/// Summary of a frame set for calendar display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarFrameSetSummary {
    pub id: i64,
    pub name: Option<String>,
    pub object_name: Option<String>,
    pub frame_count: i32,
    pub total_exposure_seconds: f64,
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    pub filters: Vec<String>,
}

/// Group of unorganized frames for calendar display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarUnorganizedGroup {
    pub id: String,
    pub object_name: Option<String>,
    pub frame_count: i32,
    pub total_exposure_seconds: f64,
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    pub filters: Vec<String>,
    pub frame_ids: Vec<i64>,
}

/// Summary of imaging activity for a single calendar day
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarDayEvent {
    pub date: String, // YYYY-MM-DD
    pub frame_sets: Vec<CalendarFrameSetSummary>,
    pub unorganized_groups: Vec<CalendarUnorganizedGroup>,
    pub total_frame_count: i32,
    pub total_exposure_seconds: f64,
}

/// Full calendar data for a month
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarMonthData {
    pub year: i32,
    pub month: i32, // 1-12
    pub days: Vec<CalendarDayEvent>,
    pub total_frame_count: i32,
    pub total_exposure_seconds: f64,
}

// ========== Calibration Set Metadata Editing ==========

/// Edits to apply to calibration set metadata (selective fields)
/// None means don't change that field
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationMetadataEdits {
    pub ccd_temp: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub binning: Option<String>,
    pub exptime: Option<f64>,
}

/// Serde helper: distinguish "field absent" from "field present with null".
/// Used by `FrameMetadataEdits` so the metadata-pane revert path can clear
/// a value back to `NULL` — a plain `Option<T>` collapses both states into
/// `None` and silently drops the clear.
///   - JSON key absent       → outer `None`        (no edit)
///   - JSON `{ key: null }`  → `Some(None)`        (clear to NULL)
///   - JSON `{ key: value }` → `Some(Some(value))` (set)
fn deserialize_double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Bulk edits for light/calibration frame metadata in the `frames` table.
/// Used by the Missing Metadata page's Set Camera / Set Date / Set Frame Type actions.
///
/// Every nullable field uses `Option<Option<T>>` with a custom serde
/// deserializer so the wire format can distinguish three states cleanly:
///   - absent in JSON       → outer `None`        → no edit
///   - `null` in JSON       → `Some(None)`        → clear to `NULL`
///   - value in JSON        → `Some(Some(v))`     → set to `v`
/// A plain `Option<T>` collapses the first two into `None`, and the SET-clause
/// loop only fires on `Some(v)` — so revert-to-NULL would silently no-op. The
/// metadata pane's revert button depends on this distinction.
///
/// `is_master` is the one exception: it's a derived tri-state paired with
/// `imagetyp`, not a user-typed value, so plain `Option<bool>` is fine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMetadataEdits {
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub instrume: Option<Option<String>>,
    /// RFC 3339 / ISO 8601 datetime string (frontend converts from user input).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub date_obs: Option<Option<String>>,
    /// Raw IMAGETYP value — "LIGHT", "DARK", "FLAT", "BIAS", "DARKFLAT".
    /// For master variants, pair with `is_master = Some(true)` and a base
    /// type (e.g. `imagetyp = "DARK"`, `is_master = true` → master dark).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub imagetyp: Option<Option<String>>,
    pub is_master: Option<bool>,
    /// Target name (FITS OBJECT). Common cleanup case for misnamed targets.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub object: Option<Option<String>>,
    /// Filter name (FITS FILTER). Common cleanup case for ASIAir mis-records.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub filter: Option<Option<String>>,
    /// Telescope name (FITS TELESCOP).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub telescop: Option<Option<String>>,
    /// Focal length in mm (FITS FOCALLEN).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub focallen: Option<Option<f64>>,
    /// Pixel size in µm (FITS XPIXSZ). Required alongside FOCALLEN for the
    /// plate-solve scale hint; user-editable on any frame for sparse headers
    /// (some surveys ship without XPIXSZ — e.g. SkyMapper).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub xpixsz: Option<Option<f64>>,
    /// Sensor gain (FITS GAIN). Typically 0-1000.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub gain: Option<Option<f64>>,
    /// Sensor offset (FITS OFFSET). Typically 0+.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub offset: Option<Option<f64>>,
    /// Binning string (e.g. "1x1", "2x2"). When set, xbinning/ybinning are
    /// also updated to match the parsed components.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub binning: Option<Option<String>>,
    /// Exposure time in seconds (FITS EXPTIME). Must be > 0.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub exptime: Option<Option<f64>>,
    /// CCD temperature in °C (FITS CCD-TEMP). Typically -50..50.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub ccd_temp: Option<Option<f64>>,
    /// Right Ascension in decimal degrees [0, 360). Only set by the metadata
    /// pane's RA/DEC revert path — there is no text input for these. The
    /// "manual update" path for coordinates is plate-solving. When set,
    /// `bulk_update_frame_metadata` also writes the parallel sexagesimal
    /// `objctra` so derived consumers stay consistent with what the scanner
    /// and plate solver produce.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub ra: Option<Option<f64>>,
    /// Declination in decimal degrees [-90, 90]. See `ra`.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub dec: Option<Option<f64>>,
    /// Image position angle in degrees (north through east). Plate solving
    /// derives this from the CD matrix. The metadata pane lets the user
    /// override or revert it just like any other numeric field. Range is
    /// `[-360, 360]` to accept either sign convention.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub rotation: Option<Option<f64>>,
}

/// Excluded frame entry (frame excluded during auto-generation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedFrameEntry {
    pub file_id: i64,
    pub path: String,
    pub filename: String,
    pub reason: String,
    pub excluded_at: String,
}

/// Excluded frame with full file + frame metadata. Mirrors `MissingMetadataRow`
/// so the same view shell can render both — adds the exclusion `reason` and
/// `excluded_at` so the Excluded Frames page can surface why a row landed there.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedFrameRow {
    pub file: File,
    pub frame: Frame,
    pub has_duplicate: bool,
    pub reason: String,
    pub excluded_at: String,
}

/// Frame analysis results from star detection and image quality metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameAnalysis {
    pub id: Option<i64>,
    pub frame_id: i64,
    pub file_id: i64,
    pub stars_detected: i64,
    pub median_fwhm: f64,
    pub median_eccentricity: f64,
    pub median_snr: f64,
    pub median_hfr: f64,
    pub frame_snr: f64,
    pub snr_weight: f64,
    pub psf_signal: f64,
    pub background: f64,
    pub noise: f64,
    pub detection_threshold: f64,
    pub width: i64,
    pub height: i64,
    pub source_channels: i64,
    pub trail_r_squared: f64,
    pub possibly_trailed: bool,
    pub median_beta: Option<f64>,
    pub quality_score: Option<f64>,
    pub config_hash: Option<String>,
    pub analyzed_at: String,
}

/// Individual star detection result with position, shape, and quality metrics.
/// Used for client-side annotation overlay rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarMetric {
    pub id: Option<i64>,
    pub frame_analysis_id: i64,
    pub x: f64,
    pub y: f64,
    pub peak: f64,
    pub flux: f64,
    pub fwhm: f64,
    pub fwhm_x: f64,
    pub fwhm_y: f64,
    pub eccentricity: f64,
    pub snr: f64,
    pub hfr: f64,
    pub theta: f64,
    pub beta: Option<f64>,
    pub fit_method: String,
    pub fit_residual: f64,
}

/// Response for the get_frame_star_metrics command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarMetricsResponse {
    pub stars: Vec<StarMetric>,
    pub metrics: FrameAnalysis,
    pub image_width: i64,
    pub image_height: i64,
    /// Whether the displayed image was vertically flipped during processing.
    /// When true, star Y coordinates must be flipped: y_display = image_height - 1 - y
    /// and theta must be negated. Mirrors rustafits annotate.rs compute_annotations().
    pub flip_vertical: bool,
}

/// One frame set that ultimately consumes a calibration set, either directly
/// (a Light using the set) or transitively via the sub-cal chain
/// (e.g. Light → Dark → Bias).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSetConsumer {
    pub frame_set_id: i64,
    pub name: Option<String>,
    pub date_obs_start: Option<String>,
}

/// Original calibration set metadata values (backed up before editing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSetOriginals {
    pub set_id: i64,
    pub ccd_temp: Option<f64>,
    pub temp_min: Option<f64>,
    pub temp_max: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub binning: Option<String>,
    pub exptime: Option<f64>,
    pub saved_at: String,  // ISO 8601
}

/// Reason a frame was skipped during auto-merge candidate evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NoCoords,
    OutsideThreshold,
    AlreadyInSet,
}

/// A candidate light frame eligible for merging into a frame set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateFrame {
    pub frame_id: i64,
    pub file_id: i64,
    pub filename: String,
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    pub date_obs: Option<DateTime<Utc>>,
    pub filter: Option<String>,
    pub instrume: Option<String>,
}

/// A frame skipped during auto-merge, with reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedFrame {
    pub frame_id: i64,
    pub filename: String,
    pub reason: SkipReason,
}

/// Result of `find_new_frames_for_set` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindNewFramesResult {
    pub candidates: Vec<CandidateFrame>,
    pub scan_was_awaited: bool,
}

/// Report produced by merging candidates into a frame set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    pub frames_set_id: i64,
    pub added_count: i64,
    pub skipped_count: i64,
    pub frames_added: Vec<CandidateFrame>,
    pub frames_skipped: Vec<SkippedFrame>,
    pub threshold_arcmin: f64,
    pub source: String,
}

/// One row of the frame-set merge audit log, as returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeLogEntry {
    pub id: i64,
    pub frames_set_id: i64,
    pub occurred_at: DateTime<Utc>,
    pub source: String,
    pub threshold_arcmin: f64,
    pub frames_added: Vec<CandidateFrame>,
    pub frames_skipped: Vec<SkippedFrame>,
    pub added_count: i64,
    pub skipped_count: i64,
}
