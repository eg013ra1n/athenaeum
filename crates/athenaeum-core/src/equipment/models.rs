use serde::{Deserialize, Serialize};

/// Pixel pitch is the physical, unbinned detector pitch in micrometres.
/// Focal length is the telescope's native focal length in mm; multiplier is
/// reducer/flattener/barlow magnification (1 for a non-reducing flattener).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct EquipmentProfile {
    pub id: i64,
    pub revision: i64,
    pub name: String,
    pub telescope: String,
    pub camera: String,
    pub focal_length_mm: f64,
    pub optical_multiplier: f64,
    pub pixel_size_um: f64,
    pub binning: i32,
    // Engineering comparison tolerance (%), not a probability or uncertainty.
    pub tolerance_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct EquipmentCandidate {
    pub profile: EquipmentProfile,
    pub expected_scale: f64,
    pub difference_percent: f64,
}

/// Saved solves supply scale in arcsec per catalog-image pixel. Resampling can
/// change that scale without changing the physical camera or telescope.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct EquipmentEvidence {
    pub frame_id: i64,
    pub filename: String,
    pub path: String,
    pub camera: String,
    pub binning_x: Option<i32>,
    pub binning_y: Option<i32>,
    pub solved_scale: f64,
    pub solved_at: String,
    pub candidates: Vec<EquipmentCandidate>,
    pub confirmed_profile_id: Option<i64>,
    pub confirmation_stale: bool,
}
