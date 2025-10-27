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
    pub content_hash: String, // Kept for backwards compatibility, no longer computed
    pub duplicate_group_id: Option<i64>,
    pub created_at: DateTime<Utc>,
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
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub binning: Option<String>,
    pub xbinning: Option<i32>,
    pub ybinning: Option<i32>,
    pub ccd_temp: Option<f64>,
    pub set_temp: Option<f64>,
    pub focal_length: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ImageType {
    Light,
    Dark,
    Flat,
    Bias,
    DarkFlat,
}

impl ImageType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "LIGHT" => Some(Self::Light),
            "DARK" => Some(Self::Dark),
            "FLAT" => Some(Self::Flat),
            "BIAS" => Some(Self::Bias),
            "DARKFLAT" | "DARK FLAT" => Some(Self::DarkFlat),
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
}
