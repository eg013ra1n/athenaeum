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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FileFormat {
    FITS,
    XISF,
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
}

/// Duplicate detection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub id: Option<i64>,
    pub size: i64,
    pub content_hash: String,
    pub file_count: i32,
    pub file_paths: Vec<String>,
    pub file_ids: Vec<i64>,
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

/// Excluded frame entry (frame excluded during auto-generation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedFrameEntry {
    pub file_id: i64,
    pub path: String,
    pub filename: String,
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
