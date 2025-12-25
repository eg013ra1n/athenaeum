// Calibration finder - matches calibration sets to light frames
use crate::models::ImageType;

/// Represents a candidate calibration set with its match score
#[derive(Debug, Clone)]
pub struct CalibrationCandidate {
    pub set_id: i64,
    pub imagetyp: ImageType,
    pub match_score: f64,  // 0.0-1.0
    pub date_diff_days: i64,
    pub temp_diff: Option<f64>,
    pub date_warning: bool,
    pub temp_warning: bool,
    pub is_master: bool,  // Whether this is a master calibration set (is_master_library = 1)
}
