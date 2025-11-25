/// Calibration Matching Configuration
///
/// This module provides a fully configurable calibration matching system.
/// Users can define matching rules via UI settings instead of hardcoded logic.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Current configuration schema version
pub const CONFIG_VERSION: i32 = 1;

/// Match mode for parameter comparison
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    /// Must match exactly (with small tolerance for floats)
    Exact,
    /// Match but warn if threshold exceeded (e.g., temperature delta > 2°C)
    Warning,
    /// Don't check this parameter
    Ignore,
}

impl Default for MatchMode {
    fn default() -> Self {
        MatchMode::Ignore
    }
}

/// Configuration for a single parameter matching rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterConfig {
    /// How to match this parameter
    pub mode: MatchMode,
    /// If true and frame's metadata is NULL, skip matching entirely
    pub required: bool,
    /// For Warning mode: threshold value (e.g., temperature delta in °C)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_threshold: Option<f64>,
}

impl Default for ParameterConfig {
    fn default() -> Self {
        Self {
            mode: MatchMode::Ignore,
            required: false,
            warning_threshold: None,
        }
    }
}

impl ParameterConfig {
    pub fn exact(required: bool) -> Self {
        Self {
            mode: MatchMode::Exact,
            required,
            warning_threshold: None,
        }
    }

    pub fn warning(threshold: f64) -> Self {
        Self {
            mode: MatchMode::Warning,
            required: false,
            warning_threshold: Some(threshold),
        }
    }

    pub fn ignore() -> Self {
        Self::default()
    }
}

/// Configuration for matching a specific calibration type
/// Contains rules for all 8 matchable parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationTypeConfig {
    /// Camera/instrument name
    pub instrume: ParameterConfig,
    /// Binning mode (e.g., "1x1", "2x2")
    pub binning: ParameterConfig,
    /// Sensor gain value
    pub gain: ParameterConfig,
    /// Sensor offset value
    pub offset: ParameterConfig,
    /// Exposure time in seconds
    pub exptime: ParameterConfig,
    /// Focal length in mm
    pub focallen: ParameterConfig,
    /// Filter name
    pub filter: ParameterConfig,
    /// CCD temperature in Celsius
    pub ccd_temp: ParameterConfig,
}

impl Default for CalibrationTypeConfig {
    fn default() -> Self {
        Self {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::ignore(),
            focallen: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            ccd_temp: ParameterConfig::ignore(),
        }
    }
}

/// Configuration for a source type (what calibrations it can link to)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceTypeConfig {
    /// Flat calibration rules (for Lights)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flat: Option<CalibrationTypeConfig>,
    /// DarkFlat calibration rules (for Flats)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub darkflat: Option<CalibrationTypeConfig>,
    /// Dark calibration rules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<CalibrationTypeConfig>,
    /// Bias calibration rules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bias: Option<CalibrationTypeConfig>,
}

/// Behavioral options for a source type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehavioralOptions {
    /// Link Bias sets as sub-calibration to Dark sets
    #[serde(default)]
    pub use_bias_for_dark_optimization: bool,
    /// Fallback to Bias if Dark not found (for Flats)
    #[serde(default)]
    pub use_bias_if_no_darks: bool,
    /// Fallback chain order (e.g., ["darkflat", "dark", "bias"])
    #[serde(default)]
    pub fallback_chain: Vec<String>,
}

impl Default for BehavioralOptions {
    fn default() -> Self {
        Self {
            use_bias_for_dark_optimization: false,
            use_bias_if_no_darks: false,
            fallback_chain: Vec::new(),
        }
    }
}

/// Preference for Master frames vs frame sets
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MasterPreference {
    PreferMaster,
    PreferFrameset,
    NoPreference,
}

impl Default for MasterPreference {
    fn default() -> Self {
        MasterPreference::NoPreference
    }
}

/// Clustering configuration for a calibration type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusteringConfig {
    /// Maximum age of frames to consider valid (in days)
    pub max_age_days: i64,
    /// Time threshold for clustering frames (in minutes)
    pub time_cluster_minutes: i64,
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            max_age_days: 30,
            time_cluster_minutes: 30,
        }
    }
}

/// Scoring configuration for calibration matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringConfig {
    /// Weight for temperature proximity in scoring (0.0-1.0)
    pub temperature_match_weight: f64,
    /// Temperature scaling factor for scoring formula (default 2.0)
    pub temperature_scale: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            temperature_match_weight: 0.3,
            temperature_scale: 2.0,
        }
    }
}

/// Warning thresholds for calibration matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarningConfig {
    /// Temperature delta tolerance in Celsius
    pub temp_delta_celsius: f64,
    /// Flat calibration date warning threshold in days
    pub flat_date_warning_days: i64,
    /// Dark calibration date warning threshold in days
    pub dark_date_warning_days: i64,
    /// DarkFlat calibration date warning threshold in days
    pub darkflat_date_warning_days: i64,
}

impl Default for WarningConfig {
    fn default() -> Self {
        Self {
            temp_delta_celsius: 2.0,
            flat_date_warning_days: 30,
            dark_date_warning_days: 365,
            darkflat_date_warning_days: 365,
        }
    }
}

/// Complete calibration matching configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationMatchingConfig {
    /// Schema version for migration support
    pub version: i32,
    /// Configuration for Light frames (→ Flat, Dark, Bias)
    pub lights: SourceTypeConfig,
    /// Configuration for Flat frames (→ DarkFlat, Dark, Bias with fallback chain)
    pub flats: SourceTypeConfig,
    /// Configuration for Dark frames (→ Bias)
    pub darks: SourceTypeConfig,
    /// Behavioral options per source type
    pub behavioral_options: HashMap<String, BehavioralOptions>,
    /// Master preference per calibration type
    pub master_preferences: HashMap<String, MasterPreference>,
    /// Clustering configuration per calibration type
    pub clustering: HashMap<String, ClusteringConfig>,
    /// Scoring configuration
    pub scoring: ScoringConfig,
    /// Warning thresholds
    pub warnings: WarningConfig,
}

impl Default for CalibrationMatchingConfig {
    fn default() -> Self {
        // Create default config matching current hardcoded behavior
        let mut config = Self {
            version: CONFIG_VERSION,
            lights: SourceTypeConfig::default(),
            flats: SourceTypeConfig::default(),
            darks: SourceTypeConfig::default(),
            behavioral_options: HashMap::new(),
            master_preferences: HashMap::new(),
            clustering: HashMap::new(),
            scoring: ScoringConfig::default(),
            warnings: WarningConfig::default(),
        };

        // Configure Lights → Flat (with filter matching)
        config.lights.flat = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::ignore(),
            focallen: ParameterConfig::exact(true),
            filter: ParameterConfig::exact(true),
            ccd_temp: ParameterConfig::ignore(),
        });

        // Configure Lights → Dark (no filter, with temp warning)
        config.lights.dark = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::exact(true),
            focallen: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            ccd_temp: ParameterConfig::warning(2.0),
        });

        // Configure Lights → Bias (no filter, no exptime, with temp warning)
        config.lights.bias = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::ignore(),
            focallen: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            ccd_temp: ParameterConfig::warning(2.0),
        });

        // Configure Flats → DarkFlat (no filter, with temp warning)
        config.flats.darkflat = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::exact(true),
            focallen: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            ccd_temp: ParameterConfig::warning(2.0),
        });

        // Configure Flats → Dark (same as DarkFlat)
        config.flats.dark = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::exact(true),
            focallen: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            ccd_temp: ParameterConfig::warning(2.0),
        });

        // Configure Flats → Bias
        config.flats.bias = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::ignore(),
            focallen: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            ccd_temp: ParameterConfig::warning(2.0),
        });

        // Configure Darks → Bias
        config.darks.bias = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            exptime: ParameterConfig::ignore(),
            focallen: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            ccd_temp: ParameterConfig::warning(2.0),
        });

        // Behavioral options
        let mut lights_opts = BehavioralOptions::default();
        lights_opts.use_bias_for_dark_optimization = true;
        config.behavioral_options.insert("lights".to_string(), lights_opts);

        let mut flats_opts = BehavioralOptions::default();
        flats_opts.use_bias_for_dark_optimization = true;
        flats_opts.fallback_chain = vec!["darkflat".to_string(), "dark".to_string(), "bias".to_string()];
        config.behavioral_options.insert("flats".to_string(), flats_opts);

        let mut darks_opts = BehavioralOptions::default();
        darks_opts.use_bias_for_dark_optimization = true;
        config.behavioral_options.insert("darks".to_string(), darks_opts);

        // Master preferences (default to NoPreference)
        config.master_preferences.insert("flat".to_string(), MasterPreference::NoPreference);
        config.master_preferences.insert("dark".to_string(), MasterPreference::NoPreference);
        config.master_preferences.insert("bias".to_string(), MasterPreference::NoPreference);
        config.master_preferences.insert("darkflat".to_string(), MasterPreference::NoPreference);

        // Clustering defaults
        config.clustering.insert("flat".to_string(), ClusteringConfig::default());
        config.clustering.insert("dark".to_string(), ClusteringConfig::default());
        config.clustering.insert("bias".to_string(), ClusteringConfig::default());
        config.clustering.insert("darkflat".to_string(), ClusteringConfig::default());

        config
    }
}

impl CalibrationMatchingConfig {
    /// Get the matching config for a source→calibration pair
    pub fn get_type_config(&self, source: &str, calibration: &str) -> Option<&CalibrationTypeConfig> {
        let source_config = match source {
            "lights" | "light" => &self.lights,
            "flats" | "flat" => &self.flats,
            "darks" | "dark" => &self.darks,
            _ => return None,
        };

        match calibration {
            "flat" => source_config.flat.as_ref(),
            "darkflat" => source_config.darkflat.as_ref(),
            "dark" => source_config.dark.as_ref(),
            "bias" => source_config.bias.as_ref(),
            _ => None,
        }
    }

    /// Get behavioral options for a source type
    pub fn get_behavioral_options(&self, source: &str) -> Option<&BehavioralOptions> {
        self.behavioral_options.get(source)
    }

    /// Get master preference for a calibration type
    pub fn get_master_preference(&self, calibration: &str) -> MasterPreference {
        self.master_preferences
            .get(calibration)
            .cloned()
            .unwrap_or_default()
    }

    /// Get clustering config for a calibration type
    pub fn get_clustering(&self, calibration: &str) -> ClusteringConfig {
        self.clustering
            .get(calibration)
            .cloned()
            .unwrap_or_default()
    }

    /// Serialize to JSON string
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON string
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CalibrationMatchingConfig::default();
        assert_eq!(config.version, CONFIG_VERSION);

        // Check Lights→Flat has filter matching
        let flat_config = config.get_type_config("lights", "flat").unwrap();
        assert_eq!(flat_config.filter.mode, MatchMode::Exact);

        // Check Lights→Dark has temp warning
        let dark_config = config.get_type_config("lights", "dark").unwrap();
        assert_eq!(dark_config.ccd_temp.mode, MatchMode::Warning);
        assert_eq!(dark_config.ccd_temp.warning_threshold, Some(2.0));

        // Check Flats has fallback chain
        let flats_opts = config.get_behavioral_options("flats").unwrap();
        assert_eq!(flats_opts.fallback_chain, vec!["darkflat", "dark", "bias"]);
    }

    #[test]
    fn test_json_serialization() {
        let config = CalibrationMatchingConfig::default();
        let json = config.to_json().unwrap();
        let parsed = CalibrationMatchingConfig::from_json(&json).unwrap();

        assert_eq!(config.version, parsed.version);
    }
}
