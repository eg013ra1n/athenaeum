/// Calibration Matching Configuration
///
/// This module provides a fully configurable calibration matching system.
/// Users can define matching rules via UI settings instead of hardcoded logic.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Current configuration schema version
/// v2: Added telescop field, dual thresholds (warning + matching), locked/supports_warning flags
/// v3: Unlocked instrume/binning/gain/offset (can now be set to Ignore)
pub const CONFIG_VERSION: i32 = 3;

/// Match mode for parameter comparison
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ParameterConfig {
    /// How to match this parameter
    pub mode: MatchMode,
    /// If true and frame's metadata is NULL, skip matching entirely
    pub required: bool,
    /// For Warning mode: threshold that triggers warning display (must be <= matching_threshold)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub warning_threshold: Option<f64>,
    /// For Warning mode: threshold that rejects match if exceeded
    /// If value difference > matching_threshold, the calibration set is rejected.
    /// If value difference > warning_threshold but <= matching_threshold, match is accepted with warning.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub matching_threshold: Option<f64>,
    /// Whether this parameter can be changed by user (false = locked to Exact mode)
    #[serde(default)]
    pub locked: bool,
    /// Whether Warning mode is available for this parameter
    #[serde(default)]
    pub supports_warning: bool,
}

impl Default for ParameterConfig {
    fn default() -> Self {
        Self {
            mode: MatchMode::Ignore,
            required: false,
            warning_threshold: None,
            matching_threshold: None,
            locked: false,
            supports_warning: false,
        }
    }
}

impl ParameterConfig {
    /// Create an exact match parameter (can be changed to Ignore)
    pub fn exact(required: bool) -> Self {
        Self {
            mode: MatchMode::Exact,
            required,
            warning_threshold: None,
            matching_threshold: None,
            locked: false,
            supports_warning: false,
        }
    }

    /// Create a warning mode parameter with dual thresholds
    /// - warning_threshold: triggers warning display
    /// - matching_threshold: rejects match if exceeded
    pub fn warning(warning_threshold: f64, matching_threshold: f64) -> Self {
        Self {
            mode: MatchMode::Warning,
            required: false,
            warning_threshold: Some(warning_threshold),
            matching_threshold: Some(matching_threshold),
            locked: false,
            supports_warning: true,
        }
    }

    /// Create an ignore parameter
    pub fn ignore() -> Self {
        Self::default()
    }

    /// Create an ignore parameter that supports warning mode
    pub fn ignore_with_warning_support() -> Self {
        Self {
            mode: MatchMode::Ignore,
            required: false,
            warning_threshold: None,
            matching_threshold: None,
            locked: false,
            supports_warning: true,
        }
    }

    /// Validate that warning_threshold <= matching_threshold
    pub fn validate(&self) -> Result<(), String> {
        if self.mode == MatchMode::Warning {
            let warn = self.warning_threshold.unwrap_or(0.0);
            let match_thresh = self.matching_threshold.unwrap_or(f64::MAX);
            if warn > match_thresh {
                return Err(format!(
                    "Warning threshold ({}) cannot be greater than matching threshold ({})",
                    warn, match_thresh
                ));
            }
        }
        Ok(())
    }
}

/// Configuration for matching a specific calibration type
/// Contains rules for all 9 matchable parameters
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct CalibrationTypeConfig {
    /// Camera/instrument name - exact by default, can be set to ignore
    pub instrume: ParameterConfig,
    /// Binning mode (e.g., "1x1", "2x2") - exact by default, can be set to ignore
    pub binning: ParameterConfig,
    /// Sensor gain value - exact by default, can be set to ignore
    pub gain: ParameterConfig,
    /// Sensor offset value - exact by default, can be set to ignore
    pub offset: ParameterConfig,
    /// Telescope name - exact or disabled (no warning mode)
    pub telescop: ParameterConfig,
    /// Exposure time in seconds - supports warning mode with thresholds
    pub exptime: ParameterConfig,
    /// Focal length in mm - supports warning mode with thresholds
    pub focallen: ParameterConfig,
    /// Filter name - exact or disabled (no warning mode)
    pub filter: ParameterConfig,
    /// CCD temperature in Celsius - supports warning mode with thresholds
    pub ccd_temp: ParameterConfig,
}

impl Default for CalibrationTypeConfig {
    fn default() -> Self {
        Self {
            // Equipment parameters - exact by default, can be set to ignore
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            // Exact or disabled (no warning support)
            telescop: ParameterConfig::ignore(), // Can be set to exact or ignore
            filter: ParameterConfig::ignore(),   // Can be set to exact or ignore
            // Warning-capable parameters (with dual thresholds)
            exptime: ParameterConfig::ignore_with_warning_support(),
            focallen: ParameterConfig::ignore_with_warning_support(),
            ccd_temp: ParameterConfig::ignore_with_warning_support(),
        }
    }
}

impl CalibrationTypeConfig {
    /// Validate all parameter configs in this type config
    pub fn validate(&self) -> Result<(), String> {
        self.instrume
            .validate()
            .map_err(|e| format!("instrume: {}", e))?;
        self.binning
            .validate()
            .map_err(|e| format!("binning: {}", e))?;
        self.gain.validate().map_err(|e| format!("gain: {}", e))?;
        self.offset
            .validate()
            .map_err(|e| format!("offset: {}", e))?;
        self.telescop
            .validate()
            .map_err(|e| format!("telescop: {}", e))?;
        self.exptime
            .validate()
            .map_err(|e| format!("exptime: {}", e))?;
        self.focallen
            .validate()
            .map_err(|e| format!("focallen: {}", e))?;
        self.filter
            .validate()
            .map_err(|e| format!("filter: {}", e))?;
        self.ccd_temp
            .validate()
            .map_err(|e| format!("ccd_temp: {}", e))?;
        Ok(())
    }
}

/// Configuration for a source type (what calibrations it can link to)
#[derive(Debug, Clone, Serialize, Deserialize, Default, ts_rs::TS)]
pub struct SourceTypeConfig {
    /// Flat calibration rules (for Lights)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub flat: Option<CalibrationTypeConfig>,
    /// DarkFlat calibration rules (for Flats)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub darkflat: Option<CalibrationTypeConfig>,
    /// Dark calibration rules
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dark: Option<CalibrationTypeConfig>,
    /// Bias calibration rules
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bias: Option<CalibrationTypeConfig>,
}

/// Behavioral options for a source type
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MasterPreference {
    PreferMaster,
    PreferFrameset,
    NoPreference,
}

impl Default for MasterPreference {
    fn default() -> Self {
        MasterPreference::PreferMaster
    }
}

/// Clustering configuration for a calibration type
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ClusteringConfig {
    /// Maximum age of frames to consider valid (in days)
    pub max_age_days: i64,
    /// Time threshold for clustering frames (in minutes)
    pub time_cluster_minutes: i64,
    /// Temperature threshold for clustering frames (in degrees Celsius)
    #[serde(default = "default_temp_threshold")]
    pub temp_threshold_celsius: f64,
}

fn default_temp_threshold() -> f64 {
    2.0
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            max_age_days: 30,
            time_cluster_minutes: 30,
            temp_threshold_celsius: 2.0,
        }
    }
}

/// Scoring configuration for calibration matching
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ScoringConfig {
    /// Weight for temperature proximity in scoring (0.0-1.0)
    pub temperature_match_weight: f64,
    /// Temperature scaling factor for scoring formula (default 2.0)
    pub temperature_scale: f64,
    /// Weight for exposure time proximity in scoring (0.0-1.0)
    pub exposure_match_weight: f64,
    /// Exposure time scaling factor for scoring formula (default 1.0s)
    pub exposure_scale: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            temperature_match_weight: 0.3,
            temperature_scale: 2.0,
            exposure_match_weight: 0.4,
            exposure_scale: 1.0,
        }
    }
}

/// Warning thresholds for calibration matching
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct WarningConfig {
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
            flat_date_warning_days: 30,
            dark_date_warning_days: 365,
            darkflat_date_warning_days: 365,
        }
    }
}

/// Complete calibration matching configuration
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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

        // Configure Lights → Flat (with filter matching, focal length with threshold)
        config.lights.flat = Some(CalibrationTypeConfig {
            // Locked exact parameters
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            // Exact or disabled (no warning)
            telescop: ParameterConfig::ignore(),
            filter: ParameterConfig::exact(true),
            // Warning-capable (with dual thresholds: warn at 5mm, reject at 10mm)
            // Note: focallen warning_threshold is also used as the tolerance for flat set
            // clustering during scan. Exact → no tolerance, Warning → threshold tolerance,
            // Ignore → focallen not used for grouping.
            exptime: ParameterConfig::ignore_with_warning_support(),
            focallen: ParameterConfig::warning(5.0, 10.0),
            ccd_temp: ParameterConfig::ignore_with_warning_support(),
        });

        // Configure Lights → Dark (exposure must match, temp with thresholds)
        config.lights.dark = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            telescop: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            // Exposure: warn at 1s diff, reject at 5s diff
            exptime: ParameterConfig::warning(1.0, 5.0),
            focallen: ParameterConfig::ignore_with_warning_support(),
            // Temperature: warn at 2°C, reject at 5°C
            ccd_temp: ParameterConfig::warning(2.0, 5.0),
        });

        // Configure Lights → Bias (no filter, no exptime, temp with thresholds)
        config.lights.bias = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            telescop: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            exptime: ParameterConfig::ignore_with_warning_support(),
            focallen: ParameterConfig::ignore_with_warning_support(),
            ccd_temp: ParameterConfig::warning(2.0, 5.0),
        });

        // Configure Flats → DarkFlat (exposure match, temp with thresholds)
        config.flats.darkflat = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            telescop: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            exptime: ParameterConfig::warning(1.0, 5.0),
            focallen: ParameterConfig::ignore_with_warning_support(),
            ccd_temp: ParameterConfig::warning(2.0, 5.0),
        });

        // Configure Flats → Dark (same as DarkFlat)
        config.flats.dark = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            telescop: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            exptime: ParameterConfig::warning(1.0, 5.0),
            focallen: ParameterConfig::ignore_with_warning_support(),
            ccd_temp: ParameterConfig::warning(2.0, 5.0),
        });

        // Configure Flats → Bias (temp with thresholds)
        config.flats.bias = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            telescop: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            exptime: ParameterConfig::ignore_with_warning_support(),
            focallen: ParameterConfig::ignore_with_warning_support(),
            ccd_temp: ParameterConfig::warning(2.0, 5.0),
        });

        // Configure Darks → Bias (temp with thresholds)
        config.darks.bias = Some(CalibrationTypeConfig {
            instrume: ParameterConfig::exact(true),
            binning: ParameterConfig::exact(true),
            gain: ParameterConfig::exact(true),
            offset: ParameterConfig::exact(true),
            telescop: ParameterConfig::ignore(),
            filter: ParameterConfig::ignore(),
            exptime: ParameterConfig::ignore_with_warning_support(),
            focallen: ParameterConfig::ignore_with_warning_support(),
            ccd_temp: ParameterConfig::warning(2.0, 5.0),
        });

        // Behavioral options
        let mut lights_opts = BehavioralOptions::default();
        lights_opts.use_bias_for_dark_optimization = true;
        config
            .behavioral_options
            .insert("lights".to_string(), lights_opts);

        let mut flats_opts = BehavioralOptions::default();
        flats_opts.use_bias_for_dark_optimization = true;
        flats_opts.use_bias_if_no_darks = true; // Default to true for backwards compatibility
        flats_opts.fallback_chain = vec![
            "darkflat".to_string(),
            "dark".to_string(),
            "bias".to_string(),
        ];
        config
            .behavioral_options
            .insert("flats".to_string(), flats_opts);

        let mut darks_opts = BehavioralOptions::default();
        darks_opts.use_bias_for_dark_optimization = true;
        config
            .behavioral_options
            .insert("darks".to_string(), darks_opts);

        // Master preferences (default to PreferMaster: masters are always
        // candidates, the preference only orders them ahead of raw sets)
        config
            .master_preferences
            .insert("flat".to_string(), MasterPreference::PreferMaster);
        config
            .master_preferences
            .insert("dark".to_string(), MasterPreference::PreferMaster);
        config
            .master_preferences
            .insert("bias".to_string(), MasterPreference::PreferMaster);
        config
            .master_preferences
            .insert("darkflat".to_string(), MasterPreference::PreferMaster);

        // Clustering defaults
        // Flat: 30 days max age, 30 minutes time cluster (session-based)
        config
            .clustering
            .insert("flat".to_string(), ClusteringConfig::default());
        // Dark/Bias/DarkFlat: 365 days max age, 30 days time cluster (1440 min/day * 30 = 43200)
        config.clustering.insert(
            "dark".to_string(),
            ClusteringConfig {
                max_age_days: 365,
                time_cluster_minutes: 43200, // 30 days
                ..ClusteringConfig::default()
            },
        );
        config.clustering.insert(
            "bias".to_string(),
            ClusteringConfig {
                max_age_days: 365,
                time_cluster_minutes: 43200, // 30 days
                ..ClusteringConfig::default()
            },
        );
        config.clustering.insert(
            "darkflat".to_string(),
            ClusteringConfig {
                max_age_days: 365,
                time_cluster_minutes: 43200, // 30 days
                ..ClusteringConfig::default()
            },
        );

        config
    }
}

impl CalibrationMatchingConfig {
    /// Migrate config from older versions to current version.
    /// v2→v3: Unlock instrume/binning/gain/offset (set locked=false).
    pub fn migrate(mut self) -> Self {
        if self.version < 3 {
            Self::unlock_core_params(&mut self.lights);
            Self::unlock_core_params(&mut self.flats);
            Self::unlock_core_params(&mut self.darks);
            self.version = CONFIG_VERSION;
        }
        self
    }

    fn unlock_core_params(source: &mut SourceTypeConfig) {
        for type_config in [
            source.flat.as_mut(),
            source.darkflat.as_mut(),
            source.dark.as_mut(),
            source.bias.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            type_config.instrume.locked = false;
            type_config.binning.locked = false;
            type_config.gain.locked = false;
            type_config.offset.locked = false;
        }
    }

    /// Get the matching config for a source→calibration pair
    pub fn get_type_config(
        &self,
        source: &str,
        calibration: &str,
    ) -> Option<&CalibrationTypeConfig> {
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

    /// Serialize to JSON string
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON string
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Validate all parameter configs across all source→calibration pairs
    /// Returns an error if any parameter has invalid threshold configuration
    pub fn validate(&self) -> Result<(), String> {
        // Validate lights configs
        if let Some(flat) = &self.lights.flat {
            flat.validate().map_err(|e| format!("lights→flat: {}", e))?;
        }
        if let Some(dark) = &self.lights.dark {
            dark.validate().map_err(|e| format!("lights→dark: {}", e))?;
        }
        if let Some(bias) = &self.lights.bias {
            bias.validate().map_err(|e| format!("lights→bias: {}", e))?;
        }

        // Validate flats configs
        if let Some(darkflat) = &self.flats.darkflat {
            darkflat
                .validate()
                .map_err(|e| format!("flats→darkflat: {}", e))?;
        }
        if let Some(dark) = &self.flats.dark {
            dark.validate().map_err(|e| format!("flats→dark: {}", e))?;
        }
        if let Some(bias) = &self.flats.bias {
            bias.validate().map_err(|e| format!("flats→bias: {}", e))?;
        }

        // Validate darks configs
        if let Some(bias) = &self.darks.bias {
            bias.validate().map_err(|e| format!("darks→bias: {}", e))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CalibrationMatchingConfig::default();
        assert_eq!(config.version, CONFIG_VERSION);

        // Check Lights→Flat has filter matching and unlocked equipment params
        let flat_config = config.get_type_config("lights", "flat").unwrap();
        assert_eq!(flat_config.filter.mode, MatchMode::Exact);
        assert!(!flat_config.instrume.locked, "instrume should be unlocked");
        assert!(!flat_config.gain.locked, "gain should be unlocked");
        assert!(!flat_config.binning.locked, "binning should be unlocked");
        assert!(!flat_config.offset.locked, "offset should be unlocked");
        // Defaults are still Exact mode
        assert_eq!(flat_config.instrume.mode, MatchMode::Exact);
        assert_eq!(flat_config.gain.mode, MatchMode::Exact);
        assert_eq!(flat_config.binning.mode, MatchMode::Exact);
        assert_eq!(flat_config.offset.mode, MatchMode::Exact);

        // Check Lights→Dark has temp warning with dual thresholds
        let dark_config = config.get_type_config("lights", "dark").unwrap();
        assert_eq!(dark_config.ccd_temp.mode, MatchMode::Warning);
        assert_eq!(dark_config.ccd_temp.warning_threshold, Some(2.0));
        assert_eq!(dark_config.ccd_temp.matching_threshold, Some(5.0));

        // Check warning-capable params have supports_warning flag
        assert!(dark_config.ccd_temp.supports_warning);
        assert!(dark_config.exptime.supports_warning);

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
