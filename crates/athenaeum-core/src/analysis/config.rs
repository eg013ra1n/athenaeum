use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn default_true() -> bool { true }
fn default_one() -> u32 { 1 }
fn default_mesh_64() -> Option<u32> { Some(64) }
fn default_mrs_0() -> u32 { 0 }

/// Star detection and analysis configuration.
/// Stored as JSON in the settings table under key "analysis.config".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// Detection threshold in sigma above background. Default: 5.0
    pub detection_sigma: f64,
    /// Minimum connected-component area in pixels. Default: 5
    pub min_star_area: u32,
    /// Maximum connected-component area in pixels. Default: 2000
    pub max_star_area: u32,
    /// Reject stars with peak > this fraction of saturation. Default: 0.95
    pub saturation_fraction: f64,
    /// Keep only the brightest N stars. Default: 500
    pub max_stars: u32,
    /// R² threshold for trail detection. Default: 0.5 (range 0.0-1.0)
    pub trail_threshold: f64,
    /// Use 2D Gaussian fit (accurate, slower) vs windowed moments (fast). Default: true
    pub use_gaussian_fit: bool,
    /// Mesh-grid background estimation cell size. None = global. Default: 64
    #[serde(default = "default_mesh_64")]
    pub background_mesh_size: Option<u32>,
    /// Use Moffat PSF fitting instead of Gaussian. Reports per-star β values. Default: true
    #[serde(default = "default_true")]
    pub use_moffat_fit: bool,
    /// Iterative source-masked background passes (0 = disabled, 1-5).
    /// Requires background_mesh_size to be set. Default: 1
    #[serde(default = "default_one")]
    pub iterative_background: u32,
    /// MRS wavelet noise estimation layers (0 = legacy MAD, 1+ = MRS wavelet). Default: 0
    #[serde(default = "default_mrs_0")]
    pub mrs_noise: u32,
    /// Fixed Moffat beta parameter. None = auto-fit (default).
    #[serde(default)]
    pub moffat_beta: Option<f32>,
    /// Reject stars with distortion above this threshold. None = disabled (default).
    #[serde(default)]
    pub max_distortion: Option<f32>,
    /// Quality scoring weights
    pub scoring_weights: ScoringWeights,
}

/// Weights for composite quality score calculation.
/// Normalized to sum = 1.0 before use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    /// Weight for FWHM (lower is better). Default: 0.35
    pub fwhm: f64,
    /// Weight for eccentricity (lower is better). Default: 0.15
    pub eccentricity: f64,
    /// Weight for SNR weight (higher is better). Default: 0.40
    pub snr_weight: f64,
    /// Weight for star count (higher is better). Default: 0.10
    pub star_count: f64,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            detection_sigma: 5.0,
            min_star_area: 5,
            max_star_area: 2000,
            saturation_fraction: 0.95,
            max_stars: 500,
            trail_threshold: 0.5,
            use_gaussian_fit: true,
            background_mesh_size: Some(64),
            use_moffat_fit: true,
            iterative_background: 1,
            mrs_noise: 0,
            moffat_beta: None,
            max_distortion: None,
            scoring_weights: ScoringWeights::default(),
        }
    }
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            fwhm: 0.35,
            eccentricity: 0.15,
            snr_weight: 0.40,
            star_count: 0.10,
        }
    }
}

impl ScoringWeights {
    /// Return normalized weights that sum to 1.0
    pub fn normalized(&self) -> ScoringWeights {
        let sum = self.fwhm + self.eccentricity + self.snr_weight + self.star_count;
        if sum <= 0.0 {
            return ScoringWeights::default();
        }
        ScoringWeights {
            fwhm: self.fwhm / sum,
            eccentricity: self.eccentricity / sum,
            snr_weight: self.snr_weight / sum,
            star_count: self.star_count / sum,
        }
    }
}

impl AnalysisConfig {
    /// Deserialize from JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Compute SHA256 hash of the config for staleness detection.
    pub fn config_hash(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Validate config values are within acceptable ranges.
    pub fn validate(&self) -> Result<(), String> {
        if self.detection_sigma < 1.0 || self.detection_sigma > 20.0 {
            return Err("detection_sigma must be between 1.0 and 20.0".into());
        }
        if self.min_star_area < 1 {
            return Err("min_star_area must be at least 1".into());
        }
        if self.max_star_area < self.min_star_area {
            return Err("max_star_area must be >= min_star_area".into());
        }
        if self.saturation_fraction < 0.5 || self.saturation_fraction > 1.0 {
            return Err("saturation_fraction must be between 0.5 and 1.0".into());
        }
        if self.max_stars < 10 || self.max_stars > 2000 {
            return Err("max_stars must be between 10 and 2000".into());
        }
        if self.trail_threshold < 0.0 || self.trail_threshold > 1.0 {
            return Err("trail_threshold must be between 0.0 and 1.0".into());
        }
        if let Some(mesh) = self.background_mesh_size {
            if mesh < 16 {
                return Err("background_mesh_size must be at least 16".into());
            }
        }
        if self.iterative_background > 5 {
            return Err("iterative_background must be between 0 and 5".into());
        }
        if self.iterative_background > 0 && self.background_mesh_size.is_none() {
            return Err("iterative_background requires background_mesh_size to be set".into());
        }
        if self.mrs_noise > 10 {
            return Err("mrs_noise must be between 0 and 10".into());
        }
        if let Some(beta) = self.moffat_beta {
            if beta < 1.0 || beta > 10.0 {
                return Err("moffat_beta must be between 1.0 and 10.0".into());
            }
        }
        if let Some(dist) = self.max_distortion {
            if dist < 0.0 || dist > 1.0 {
                return Err("max_distortion must be between 0.0 and 1.0".into());
            }
        }
        let w = &self.scoring_weights;
        if w.fwhm < 0.0 || w.eccentricity < 0.0 || w.snr_weight < 0.0 || w.star_count < 0.0 {
            return Err("Scoring weights must be non-negative".into());
        }
        Ok(())
    }
}

/// Load analysis config from the database settings table.
/// Returns default config if not found.
pub fn load_config(conn: &rusqlite::Connection) -> AnalysisConfig {
    match crate::db::get_setting(conn, "analysis.config") {
        Ok(Some(json)) => AnalysisConfig::from_json(&json).unwrap_or_default(),
        _ => AnalysisConfig::default(),
    }
}

/// Save analysis config to the database settings table.
pub fn save_config(conn: &rusqlite::Connection, config: &AnalysisConfig) -> Result<(), String> {
    config.validate()?;
    let json = config.to_json().map_err(|e| e.to_string())?;
    crate::db::set_setting(conn, "analysis.config", &json).map_err(|e| e.to_string())
}
