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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub pixsz: Option<f64>,
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    pub sitelat: Option<f64>,
    pub lat_obs: Option<f64>,
    pub sitelong: Option<f64>,
    pub long_obs: Option<f64>,
    pub objctra: Option<String>,
    pub objctdec: Option<String>,
    pub override_: bool,
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
            "LIGHT" => Some(Self::Light),
            "DARK" => Some(Self::Dark),
            "FLAT" => Some(Self::Flat),
            "BIAS" => Some(Self::Bias),
            "DARKFLAT" | "DARK FLAT" => Some(Self::DarkFlat),
            "MASTER LIGHT" | "MASTERLIGHT" => Some(Self::MasterLight),
            "MASTER DARK" | "MASTERDARK" => Some(Self::MasterDark),
            "MASTER FLAT" | "MASTERFLAT" => Some(Self::MasterFlat),
            "MASTER BIAS" | "MASTERBIAS" => Some(Self::MasterBias),
            "MASTER DARK FLAT" | "MASTERDARKFLAT" | "MASTER DARKFLAT" => Some(Self::MasterDarkFlat),
            _ => None,
        }
    }

    pub fn to_frame_folder(&self) -> String {
        match self {
            Self::Light => "Lights".to_string(),
            Self::Dark => "Calibration/Darks".to_string(),
            Self::Flat => "Calibration/Flats".to_string(),
            Self::Bias => "Calibration/Bias".to_string(),
            Self::DarkFlat => "Calibration/DarkFlats".to_string(),
            Self::MasterLight => "Masters/Lights".to_string(),
            Self::MasterDark => "Masters/Darks".to_string(),
            Self::MasterFlat => "Masters/Flats".to_string(),
            Self::MasterBias => "Masters/Bias".to_string(),
            Self::MasterDarkFlat => "Masters/DarkFlats".to_string(),
        }
    }

    pub fn is_master(&self) -> bool {
        matches!(
            self,
            Self::MasterLight | Self::MasterDark | Self::MasterFlat | Self::MasterBias | Self::MasterDarkFlat
        )
    }

    pub fn base_type(&self) -> Self {
        match self {
            Self::MasterLight => Self::Light,
            Self::MasterDark => Self::Dark,
            Self::MasterFlat => Self::Flat,
            Self::MasterBias => Self::Bias,
            Self::MasterDarkFlat => Self::DarkFlat,
            _ => self.clone(),
        }
    }
}

/// Represents a day of captures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Day {
    pub id: Option<i64>,
    pub date: String, // ISO 8601 date (YYYY-MM-DD)
    pub frame_count: i32,
}

/// Represents a capture setup (telescope + camera + settings)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Setup {
    pub id: Option<i64>,
    pub telescop: Option<String>,
    pub instrume: Option<String>,
    pub filter: Option<String>,
    pub binning: Option<String>,
    pub gain: Option<f64>,
}

/// Represents a calibration set
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSet {
    pub id: Option<i64>,
    pub imagetyp: ImageType,
    pub exptime: Option<f64>,
    pub filter: Option<String>,
    pub ccd_temp: Option<f64>,
    pub gain: Option<f64>,
    pub binning: Option<String>,
    pub instrume: Option<String>,
    pub date: String,
    pub frame_ids: Vec<i64>,
}

/// User-defined tag
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
}

/// Tag assignment to frames
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameTag {
    pub frame_id: i64,
    pub tag_id: i64,
}

/// Monitored scan root path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRoot {
    pub id: Option<i64>,
    pub path: String,
    pub enabled: bool,
    pub find_duplicates: bool,
    pub last_scan: Option<DateTime<Utc>>,
}

/// Export template configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTemplate {
    pub id: Option<i64>,
    pub name: String,
    pub template: String,
    pub description: Option<String>,
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

/// Project for organizing imaging sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Option<i64>,
    pub name: String,
}

/// Frames set (imaging session within a project)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FramesSet {
    pub id: Option<i64>,
    pub name: Option<String>,
    pub is_custom: bool,
    pub date_obs: Option<String>,
    pub objctra: Option<String>,
    pub objctdec: Option<String>,
    pub total_exp_time: Option<f64>,
    pub project_id: Option<i64>,
}

/// FITS header storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitsHeader {
    pub id: Option<i64>,
    pub file_id: i64,
    pub header: String,
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

/// Session with aggregated metadata (for filtering in custom set creation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionWithMetadata {
    pub id: Option<i64>,
    pub imaging_night_id: i64,
    pub instrume: String,
    pub frame_count: i32,
    pub total_exp_time: Option<f64>,
    pub created_at: Option<DateTime<Utc>>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub avg_ra: Option<String>,
    pub avg_dec: Option<String>,
}

/// Junction table member for session_members
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMember {
    pub session_id: i64,
    pub frame_id: i64,
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
    pub date_start: String,      // ISO 8601
    pub date_end: String,        // ISO 8601
    pub date_display: String,    // e.g., "2025-10"
    pub frame_count: i64,
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
}

/// Bounding box for rectangular region selection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionBounds {
    pub ra_min: f64,
    pub ra_max: f64,
    pub dec_min: f64,
    pub dec_max: f64,
}

/// Result of a spatial selection query
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionResult {
    pub frame_ids: Vec<i64>,
    pub count: usize,
    pub total_exposure_seconds: f64,
}
