use serde::{Deserialize, Serialize};

/// Explicit integration target and catalog-measurement policy for one field/filter.
/// Durations are seconds; FWHM is catalog-image pixels, eccentricity dimensionless.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ObservingGoal {
    pub frame_set_id: i64,
    pub filter: String,
    pub revision: i64,
    pub target_seconds: f64,
    pub require_analysis: bool,
    pub max_fwhm_px: Option<f64>,
    pub max_eccentricity: Option<f64>,
    pub reject_trailed: bool,
}

/// Counts are mutually exclusive per representative exposure, within one field.
/// These describe catalog integration progress, not geometric sky-area coverage.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ObservingProgress {
    pub filter: String,
    pub goal: Option<ObservingGoal>,
    pub accepted: i64,
    pub rejected: i64,
    pub unknown: i64,
    pub accepted_seconds: f64,
}
