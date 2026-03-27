use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn default_four() -> u32 { 4 }
fn default_measure_cap() -> u32 { 500 }
fn default_fit_max_iter() -> u32 { 25 }
fn default_fit_tolerance() -> f64 { 1e-4 }
fn default_fit_max_rejects() -> u32 { 5 }
fn default_batch_concurrency() -> u32 { 3 }

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
    /// MRS wavelet noise estimation layers. Default: 4
    #[serde(default = "default_four", alias = "mrs_noise")]
    pub mrs_layers: u32,
    /// Max stars to PSF-fit. 0 = measure all. Default: 2000
    #[serde(default = "default_measure_cap")]
    pub measure_cap: u32,
    /// LM max iterations for measurement pass. Default: 25
    #[serde(default = "default_fit_max_iter")]
    pub fit_max_iter: u32,
    /// LM convergence tolerance for measurement pass. Default: 1e-4
    #[serde(default = "default_fit_tolerance")]
    pub fit_tolerance: f64,
    /// LM consecutive reject bailout. Default: 5
    #[serde(default = "default_fit_max_rejects")]
    pub fit_max_rejects: u32,
    /// Concurrent frames during batch analysis. Default: 3.
    /// Higher values fill more CPU but use more memory (~200MB per concurrent frame).
    #[serde(default = "default_batch_concurrency")]
    pub batch_concurrency: u32,
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
            mrs_layers: 4,
            measure_cap: 500,
            fit_max_iter: 25,
            fit_tolerance: 1e-4,
            fit_max_rejects: 5,
            batch_concurrency: 3,
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
    /// Excludes `batch_concurrency` since it's a runtime performance knob
    /// that doesn't affect analysis results.
    pub fn config_hash(&self) -> String {
        let mut for_hash = self.clone();
        for_hash.batch_concurrency = 0;
        let json = serde_json::to_string(&for_hash).unwrap_or_default();
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
        if self.mrs_layers > 10 {
            return Err("mrs_layers must be between 0 and 10".into());
        }
        if self.measure_cap > 100_000 {
            return Err("measure_cap must be between 0 and 100000".into());
        }
        if self.fit_max_iter < 1 || self.fit_max_iter > 200 {
            return Err("fit_max_iter must be between 1 and 200".into());
        }
        if self.fit_tolerance <= 0.0 || self.fit_tolerance > 1.0 {
            return Err("fit_tolerance must be between 0 (exclusive) and 1.0".into());
        }
        if self.fit_max_rejects < 1 || self.fit_max_rejects > 100 {
            return Err("fit_max_rejects must be between 1 and 100".into());
        }
        if self.batch_concurrency < 1 || self.batch_concurrency > 8 {
            return Err("batch_concurrency must be between 1 and 8".into());
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
