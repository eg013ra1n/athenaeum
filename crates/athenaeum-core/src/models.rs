use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// Represents a physical file on disk
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct File {
    pub id: Option<i64>,
    pub path: String,
    pub filename: String,
    pub size: i64,
    pub modified_at: DateTime<Utc>,
    pub format: FileFormat,
    pub created_at: DateTime<Utc>,
    pub content_hash: Option<String>,
    // ZIP archive feature — populated when this file's data has been moved
    // into a zip in the archive root. Original `path` is preserved for restore.
    pub archived_in_operation: Option<i64>,
    pub archive_zip_path: Option<String>,
    pub archive_path_in_zip: Option<String>,
    pub uuid: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
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
    /// CFA phase offset along X (XBAYROFF), as declared by the source header.
    /// `None` means the source did not declare one — never substitute 0: a
    /// fabricated phase silently swaps the color channels of every consumer
    /// that debayers the frame.
    ///
    /// NOTE: index-based readers currently leave this None regardless of
    /// the stored value — consumers needing the real value must query
    /// frames.xbayroff/ybayroff/roworder directly.
    pub xbayroff: Option<i64>,
    /// CFA phase offset along Y (YBAYROFF). Same "absent stays None" rule as
    /// `xbayroff`.
    ///
    /// NOTE: index-based readers currently leave this None regardless of
    /// the stored value — consumers needing the real value must query
    /// frames.xbayroff/ybayroff/roworder directly.
    pub ybayroff: Option<i64>,
    /// Row order as declared by the source header (`ROWORDER`, e.g.
    /// "TOP-DOWN" / "BOTTOM-UP"), stored verbatim. `None` means the header is
    /// silent — the astronomical bottom-up default is a *rendering* decision
    /// (see `orientation.rs`), not something to write back into the catalog.
    ///
    /// NOTE: index-based readers currently leave this None regardless of
    /// the stored value — consumers needing the real value must query
    /// frames.xbayroff/ybayroff/roworder directly.
    pub roworder: Option<String>,
    /// Image position angle in degrees (North through East)
    /// Extracted from CROTA2 or computed from CD matrix
    pub rotation: Option<f64>,
    pub uuid: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ScanRoot {
    pub id: Option<i64>,
    pub path: String,
    pub enabled: bool,
    pub find_duplicates: bool,
    pub unique_camera: bool,
    pub last_scan: Option<DateTime<Utc>>,
    pub last_scan_errors: Option<Vec<String>>,
    pub monitor_enabled: bool,
    /// 'normal' | 'calibration_library' | 'sync_incoming' | 'collaboration'
    pub kind: String,
}

/// A single file inside a duplicate group, enriched with the metadata the
/// frontend needs to apply "keep" rules (modified time, assigned scan root)
/// and to display meaningful per-group summaries (filename, frame type,
/// observation date).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BulkMoveResult {
    pub moved: usize,
    /// Per-file failures — (file_id, error message). Empty on full success.
    pub failed: Vec<(i64, String)>,
}

/// Black hole entry (soft-deleted file)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
    pub uuid: Option<String>,
    pub updated_at: Option<String>,
}

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Imaging night - top-level grouping of frames by observation night
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ImagingNight {
    pub id: Option<i64>,
    pub frames_set_id: i64,
    pub start_time: String,
    pub end_time: String,
    pub created_at: Option<DateTime<Utc>>,
}

/// Session - grouping of frames by instrument within an imaging night
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct Session {
    pub id: Option<i64>,
    pub imaging_night_id: i64,
    pub instrume: String,
    pub frame_count: i32,
    pub total_exp_time: Option<f64>,
    pub created_at: Option<DateTime<Utc>>,
    pub uuid: Option<String>,
    pub updated_at: Option<String>,
}

/// DTO: File with optional frame metadata
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct FileWithFrame {
    pub file: File,
    pub frame: Option<Frame>,
}

/// DTO: a row on the Missing Metadata page. Extends `FileWithFrame` with a
/// duplicate-detection flag so the UI can show which missing-metadata files
/// also have a duplicate somewhere in the catalog. Uses camelCase on the
/// wire to match the frontend convention.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MissingMetadataRow {
    pub file: File,
    pub frame: Frame,
    pub has_duplicate: bool,
}

/// DTO: Session with its frames
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct SessionWithFrames {
    pub session: Session,
    pub frames: Vec<FileWithFrame>,
}

/// DTO: Imaging night with its sessions
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ImagingNightWithSessions {
    pub imaging_night: ImagingNight,
    pub sessions: Vec<SessionWithFrames>,
}

/// DTO: Complete frame set detail with nights and sessions
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct FrameSetDetail {
    pub frames_set: FramesSet,
    pub nights: Vec<ImagingNightWithSessions>,
}

/// Equipment/Camera statistics
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CameraStats {
    pub instrume: String,
    pub frame_count: i64,
    pub total_hours: f64,
    pub first_use: Option<String>,  // ISO 8601
    pub last_use: Option<String>,   // ISO 8601
}

/// Calibration set with extended metadata
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
    pub uuid: Option<String>,
    pub updated_at: Option<String>,
    /// Set when this raw set has been superseded by a built master.
    pub superseded_by_set_id: Option<i64>,
}

/// Result of file relinking operation
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct RelinkResult {
    pub files_matched: usize,
    pub files_new: usize,
    pub files_orphaned: usize,
    pub orphaned_file_ids: Vec<i64>,
}

/// Represents an orphaned file that couldn't be relinked
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationHierarchy {
    pub light_frame_id: i64,
    pub flat_sets: Vec<CalibrationSetWithLinks>,
    pub dark_sets: Vec<CalibrationSetWithLinks>,
    pub missing_calibration: Vec<String>,  // List of missing calibration types
    pub warnings: Vec<CalibrationWarning>,
}

/// Calibration set with its sub-calibration links
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationSetWithLinks {
    pub set: CalibrationSetDetail,
    pub sub_calibration: Vec<CalibrationLink>,  // Links to Dark/Bias sets for this set
}

/// Warning about calibration quality
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationWarning {
    pub warning_type: String,  // 'date' or 'temperature'
    pub message: String,
    pub calibration_type: String,  // 'Dark', 'Flat', 'Bias', 'DarkFlat'
    pub set_id: i64,
}

/// Statistics about calibration for a frame set
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct FrameSetCalibrationGroups {
    pub groups: Vec<CalibrationGroup>,
    pub uncalibrated_frame_count: usize,
    pub uncalibrated_frame_ids: Vec<i64>,
    pub total_frames: usize,
}

/// Tolerance configuration for calibration matching
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationHierarchyView {
    pub date_groups: Vec<CalibrationDateGroup>,
    pub total_frames: usize,
    pub calibrated_frames: usize,
    pub uncalibrated_frames: usize,
}

/// Group of frames for a single session date
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationDateGroup {
    pub date: String,                    // e.g., "2024-01-15"
    pub date_display: String,            // e.g., "January 15, 2024"
    pub camera_groups: Vec<CalibrationCameraGroup>,
    pub frame_count: usize,
    pub has_warnings: bool,
}

/// Group of frames for a single camera within a date
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationCameraGroup {
    pub instrume: String,                // Camera name
    pub filter_groups: Vec<CalibrationFilterGroup>,
    pub frame_count: usize,
    pub has_warnings: bool,
}

/// Sub-calibration linked to a calibration set with full details
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct SubCalibrationDetail {
    pub calibration_type: String,        // "Dark", "DarkFlat", "Bias"
    pub set: CalibrationSetDetail,       // Full set details
    pub date_warning: bool,
    pub temp_warning: bool,
}

/// A calibration set with the count of frames that use it
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationSetWithFrameCount {
    pub set: CalibrationSetDetail,
    pub frame_count: i64,              // How many frames in this group use this set
    pub frame_ids: Vec<i64>,           // Which frames use this set
    pub warnings: Vec<CalibrationWarning>,
    pub sub_calibration: Vec<SubCalibrationDetail>,  // Linked sub-calibrations (e.g., Flat→Dark, Dark→Bias)
}

/// A calibration set with match score for manual selection
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationSetWithScore {
    pub set: CalibrationSetDetail,
    /// CLOSENESS only — date / temperature / exposure proximity, 0.0-1.0.
    /// Whether the user's config accepts this set is `compatible`, never this
    /// number (2026-09-05 design §3): folding the two together left every
    /// rejected candidate at exactly 0.0 and the list unsortable.
    pub match_score: f64,
    /// Passed every Exact/Warning parameter check at the configured
    /// thresholds — what auto-link would accept.
    pub compatible: bool,
    /// Every parameter the engine compared, with both values, the difference
    /// and the thresholds, so a card can say WHY a set was refused. The
    /// blockers are the entries whose status is `Mismatch` or `Unknown` and
    /// whose mode is not `Ignore` — derived, not carried separately.
    pub parameters: Vec<ParameterVerdict>,
    /// Legacy six-boolean summary the current modals render. Superseded by
    /// `parameters`; removed with those modals, not before.
    pub match_details: MatchDetails,
}

/// How one parameter compared for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ParameterStatus {
    /// Compared and equal (or inside tolerance).
    Match,
    /// Compared, outside the warning threshold but inside the matching one —
    /// accepted, with a warning.
    Warning,
    /// Compared and disagreed, or beyond the matching threshold.
    Mismatch,
    /// Could not be compared: one side does not declare the value. Different
    /// from `Mismatch` — "we don't know" is not "these are wrong for each
    /// other", and an imported master with no GAIN is the common case.
    Unknown,
}

/// One parameter's verdict for one candidate, as the modal renders it.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ParameterVerdict {
    /// Canonical parameter name: `instrume`, `binning`, `gain`, `offset`,
    /// `telescop`, `exptime`, `focallen`, `filter`, `ccd_temp`.
    pub name: String,
    /// The user's config takes this parameter into account (`MatchMode` is not
    /// `Ignore`). A boolean rather than the mode itself: the mode's TS type
    /// lives in a different generated file, and this is the only question the
    /// cards ask of it.
    pub enforced: bool,
    pub status: ParameterStatus,
    pub frame_value: Option<String>,
    pub set_value: Option<String>,
    pub diff: Option<f64>,
    pub warning_threshold: Option<f64>,
    pub matching_threshold: Option<f64>,
}

/// Details about how well a calibration set matches light frame parameters
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
    /// Distinct INSTRUME values of the day's frames, sorted.
    pub cameras: Vec<String>,
    /// Distinct TELESCOP values of the day's frames, sorted (NULLs skipped).
    pub telescopes: Vec<String>,
    /// Earliest / latest DATE-OBS of the day's frames.
    pub first_obs: Option<String>,
    pub last_obs: Option<String>,
}

/// Group of unorganized frames for calendar display
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
    /// Distinct INSTRUME values of the group's frames, sorted.
    pub cameras: Vec<String>,
    /// Distinct TELESCOP values of the group's frames, sorted (NULLs skipped).
    pub telescopes: Vec<String>,
    /// Earliest / latest DATE-OBS of the group's frames.
    pub first_obs: Option<String>,
    pub last_obs: Option<String>,
}

/// Summary of imaging activity for a single calendar day
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CalendarDayEvent {
    pub date: String, // YYYY-MM-DD
    pub frame_sets: Vec<CalendarFrameSetSummary>,
    pub unorganized_groups: Vec<CalendarUnorganizedGroup>,
    pub total_frame_count: i32,
    pub total_exposure_seconds: f64,
}

/// Full calendar data for a month
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
///
/// ts(optional = nullable): the old hand-written TS declared every field
/// `field?: T | null` (optional AND nullable) so callers can send a sparse
/// edit object with only the changed keys — without the override, ts-rs's
/// default `Option<T>` mapping would render `field: T | null` (present,
/// nullable, but never omittable), which breaks call sites that build a
/// partial edits object (e.g. `{}`). Unlike `FrameMetadataEdits`, these
/// fields are plain `Option<T>` (no `deserialize_double_option`) — Rust
/// itself doesn't distinguish "absent" from "null" here, so this is a
/// TS-side-only ergonomics fix, not a wire-semantics change.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationMetadataEdits {
    #[ts(optional = nullable)]
    pub ccd_temp: Option<f64>,
    #[ts(optional = nullable)]
    pub gain: Option<f64>,
    #[ts(optional = nullable)]
    pub offset: Option<f64>,
    #[ts(optional = nullable)]
    pub binning: Option<String>,
    #[ts(optional = nullable)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameMetadataEdits {
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub instrume: Option<Option<String>>,
    /// RFC 3339 / ISO 8601 datetime string (frontend converts from user input).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub date_obs: Option<Option<String>>,
    /// Raw IMAGETYP value — "LIGHT", "DARK", "FLAT", "BIAS", "DARKFLAT".
    /// For master variants, pair with `is_master = Some(true)` and a base
    /// type (e.g. `imagetyp = "DARK"`, `is_master = true` → master dark).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub imagetyp: Option<Option<String>>,
    #[ts(optional = nullable)]
    pub is_master: Option<bool>,
    /// Target name (FITS OBJECT). Common cleanup case for misnamed targets.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub object: Option<Option<String>>,
    /// Filter name (FITS FILTER). Common cleanup case for ASIAir mis-records.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub filter: Option<Option<String>>,
    /// Telescope name (FITS TELESCOP).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub telescop: Option<Option<String>>,
    /// Focal length in mm (FITS FOCALLEN).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub focallen: Option<Option<f64>>,
    /// Pixel size in µm (FITS XPIXSZ). Required alongside FOCALLEN for the
    /// plate-solve scale hint; user-editable on any frame for sparse headers
    /// (some surveys ship without XPIXSZ — e.g. SkyMapper).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub xpixsz: Option<Option<f64>>,
    /// Sensor gain (FITS GAIN). Typically 0-1000.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub gain: Option<Option<f64>>,
    /// Sensor offset (FITS OFFSET). Typically 0+.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub offset: Option<Option<f64>>,
    /// Binning string (e.g. "1x1", "2x2"). When set, xbinning/ybinning are
    /// also updated to match the parsed components.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub binning: Option<Option<String>>,
    /// Exposure time in seconds (FITS EXPTIME). Must be > 0.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub exptime: Option<Option<f64>>,
    /// CCD temperature in °C (FITS CCD-TEMP). Typically -50..50.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub ccd_temp: Option<Option<f64>>,
    /// Right Ascension in decimal degrees [0, 360). Only set by the metadata
    /// pane's RA/DEC revert path — there is no text input for these. The
    /// "manual update" path for coordinates is plate-solving. When set,
    /// `bulk_update_frame_metadata` also writes the parallel sexagesimal
    /// `objctra` so derived consumers stay consistent with what the scanner
    /// and plate solver produce.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub ra: Option<Option<f64>>,
    /// Declination in decimal degrees [-90, 90]. See `ra`.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub dec: Option<Option<f64>>,
    /// Image position angle in degrees (north through east). Plate solving
    /// derives this from the CD matrix. The metadata pane lets the user
    /// override or revert it just like any other numeric field. Range is
    /// `[-360, 360]` to accept either sign convention.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(optional)]
    pub rotation: Option<Option<f64>>,
}

/// Excluded frame entry (frame excluded during auto-generation)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedFrameRow {
    pub file: File,
    pub frame: Frame,
    pub has_duplicate: bool,
    pub reason: String,
    pub excluded_at: String,
}

/// Frame analysis results from star detection and image quality metrics
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSetConsumer {
    pub frame_set_id: i64,
    pub name: Option<String>,
    pub date_obs_start: Option<String>,
}

/// Original calibration set metadata values (backed up before editing)
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NoCoords,
}

/// A candidate light frame eligible for merging into a frame set.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct SkippedFrame {
    pub frame_id: i64,
    pub filename: String,
    pub reason: SkipReason,
}

/// Result of `find_new_frames_for_set` command.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct FindNewFramesResult {
    pub candidates: Vec<CandidateFrame>,
    pub scan_was_awaited: bool,
}

/// Report produced by merging candidates into a frame set.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogMeta {
    pub catalog_uuid: String,
    pub schema_version: i64,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Light-calibration shared types.
//
// These plain data types + consts are used by BOTH the render-gated
// light-calibration engine (`calibration_library::light_cal`) and ungated
// consumers (`scanner`, `export`). They live here in
// `models` so the ungated side compiles with `--no-default-features` while the
// engine (gated behind the `render` feature) re-exports them for its callers.
// ---------------------------------------------------------------------------

/// Bump when the calibration math or the engine's output surface changes — a
/// stamped `ATH_CVER` then tells a consumer which engine produced the file, and
/// an older stamp reads as out of date. Nothing consults it automatically since
/// calibrated-export v2 retired the tracking table — every export regenerates —
/// so it is now provenance a human (or a future consumer) reads off the file.
///
/// The single definition lives HERE, not next to the engine: the `ATH_CVER`
/// stamp itself sits in `calibration_library::light_headers`, which is ungated,
/// while the engine (`calibration_library::light_cal`) is behind the `render`
/// feature — a definition inside the engine would not compile headless. The
/// engine re-exports this constant so `light_cal::LIGHT_CAL_ENGINE_VERSION`
/// stays a valid path for its render-gated callers.
///
/// - `2`: flat denominator floored at `FLAT_DENOM_FLOOR`, and the output scale
///   divisor follows the source's BITPIX instead of a hardcoded 65535.
/// - `3`: the engine's output surface grew — cosmetic hot-pixel correction and
///   the OSC debayer stage, with the cards that record them.
pub const LIGHT_CAL_ENGINE_VERSION: i64 = 3;

/// Fraction of pixels discarded from EACH tail by the PixInsight-compatible
/// trimmed mean (`FlatNormMode::PixinsightTrimmed`). Matches PixInsight
/// ImageCalibration's `flatScaleClippingFactor = 0.05`, identified empirically
/// against PI's own arithmetic to 1.7e-6 relative (spec §2). The trim indices
/// are `lo = floor(n * PI_TRIM_FRACTION)`, `hi = n − floor(n * PI_TRIM_FRACTION)`
/// and the statistic is the f64 mean of `sorted[lo..hi]` — exactness is the
/// point, so this lives in exactly one place.
pub const PI_TRIM_FRACTION: f64 = 0.05;

/// Which statistic normalizes the master flat when normalization is ON
/// (spec §2, "Normalization statistic is selectable"). Applies only when
/// `flat_norm` is on; recorded in the tracking row so a mode change makes a
/// flat-applied frame stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum FlatNormMode {
    /// Athenaeum convention (default): the flat's central-third mean, read from
    /// the master's `ATH_FNRM` card and recomputed on the fly when absent.
    #[default]
    CentralThird,
    /// PixInsight-compatible: a two-sided trimmed mean over the WHOLE frame,
    /// discarding [`PI_TRIM_FRACTION`] of the pixels from each tail. Always
    /// computed from the flat file — the `ATH_FNRM` card is ignored.
    PixinsightTrimmed,
}

impl FlatNormMode {
    /// The over-the-wire / stored string for this mode — identical to the serde
    /// camelCase representation and to the `flat_norm_mode` DB column values.
    /// Kept as a `&'static str` so a consumer can compare a stored/stamped mode
    /// against a wanted mode without a serde round-trip.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            FlatNormMode::CentralThird => "centralThird",
            FlatNormMode::PixinsightTrimmed => "pixinsightTrimmed",
        }
    }
}

/// What to do for a LIGHT frame that has NO dark master (spec §2 "Advanced
/// parameters"). Default = current behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum BiasFallback {
    /// Current behavior: subtract the linked master bias (`(L − B)`).
    #[default]
    SubtractBias,
    /// Refuse bias-only calibration — a light with no dark master is a per-frame
    /// failure ("no dark master (bias fallback disabled)"), no output written.
    SkipFrame,
}

/// serde default for [`LightCalParams::trim_fraction`] — the current per-tail
/// discard fraction ([`PI_TRIM_FRACTION`] = 0.05), so an omitted wire field or a
/// stored `'{}'` decodes to today's behavior.
fn default_trim_fraction() -> f64 {
    PI_TRIM_FRACTION
}

/// serde default for [`LightCalParams::cfa_flat_scaling`] — ON, so an omitted
/// wire field decodes to the recommended behavior for colour sensors. NOTE the
/// asymmetry with the other defaults: a stored/stamped `'{}'` written before
/// this field existed also decodes to `true`, which is NOT what it was
/// calibrated with. `cal_params` therefore records what was REQUESTED; what was
/// APPLIED is reported separately (`ATH_CCFA` on the output).
fn default_true() -> bool {
    true
}

/// Advanced per-run light-calibration parameters (spec §2 "Advanced
/// parameters"). Every field is optional on the wire (`#[serde(default)]`) with
/// a default equal to the current behavior, so an omitted field — or a
/// pre-feature `'{}'` — decodes to [`LightCalParams::default`].
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LightCalParams {
    /// Per-tail discard fraction for the `pixinsightTrimmed` statistic
    /// (default [`PI_TRIM_FRACTION`] = 0.05). Only meaningful when the flat is
    /// normalized in `pixinsightTrimmed` mode; stamped as `ATH_CTRM` then.
    #[serde(default = "default_trim_fraction")]
    pub trim_fraction: f64,
    /// DN added to the output AFTER the scale divide (`out += pedestal_dn /
    /// OUTPUT_SCALE_DIVISOR`, default 0 = off), for consumers that clip
    /// negatives. Stamped as `ATH_CPED` (the DN value) always; `CALSTAT`
    /// unchanged — a pedestal is not a calibration step.
    #[serde(default)]
    pub pedestal_dn: f64,
    /// What to do for a light with no dark master (default `subtractBias`).
    #[serde(default)]
    pub bias_fallback: BiasFallback,
    /// Normalize a master flat per CFA channel instead of by one whole-frame
    /// constant (default ON). Only ever acts on a light that declares a CFA
    /// pattern: a mono light has no channels to separate, so the flag is
    /// inert there and the frame is calibrated exactly as before.
    ///
    /// Ignored in [`FlatNormMode::PixinsightTrimmed`], which is a
    /// tool-parity statistic over the whole frame — splitting it per channel
    /// would no longer be the statistic it promises. That combination logs a
    /// `debug!` and normalizes globally.
    #[serde(default = "default_true")]
    pub cfa_flat_scaling: bool,
}

impl Default for LightCalParams {
    fn default() -> Self {
        Self {
            trim_fraction: PI_TRIM_FRACTION,
            pedestal_dn: 0.0,
            bias_fallback: BiasFallback::SubtractBias,
            cfa_flat_scaling: true,
        }
    }
}
