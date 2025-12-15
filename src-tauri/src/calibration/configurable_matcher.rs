/// Configurable Calibration Matcher
///
/// This module implements the configurable calibration matching system
/// that uses user-defined rules from CalibrationMatchingConfig.

use crate::calibration::config::{
    CalibrationMatchingConfig, CalibrationTypeConfig, MatchMode, ParameterConfig,
    MasterPreference,
};
use crate::calibration::finder::CalibrationCandidate;
use crate::models::{Frame, ImageType};
use rusqlite::Connection;
use chrono::{DateTime, Utc};
use anyhow::Result;

/// Result of checking a single parameter
#[derive(Debug, Clone)]
pub struct ParameterCheckResult {
    pub matches: bool,
    #[allow(dead_code)]
    pub warning: bool,
    pub warning_message: Option<String>,
    pub skip_matching: bool, // If true, skip this calibration type entirely
}

impl ParameterCheckResult {
    fn matched() -> Self {
        Self { matches: true, warning: false, warning_message: None, skip_matching: false }
    }

    fn failed() -> Self {
        Self { matches: false, warning: false, warning_message: None, skip_matching: false }
    }

    fn warning(msg: String) -> Self {
        Self { matches: true, warning: true, warning_message: Some(msg), skip_matching: false }
    }

    fn skip() -> Self {
        Self { matches: false, warning: false, warning_message: None, skip_matching: true }
    }
}

/// Check if a string parameter matches based on config
fn check_string_param(
    frame_value: &Option<String>,
    set_value: &Option<String>,
    config: &ParameterConfig,
    param_name: &str,
) -> ParameterCheckResult {
    match config.mode {
        MatchMode::Ignore => ParameterCheckResult::matched(),
        MatchMode::Exact => {
            // If required and frame value is None, skip matching entirely
            if config.required && frame_value.is_none() {
                return ParameterCheckResult::skip();
            }

            match (frame_value, set_value) {
                (Some(f), Some(s)) => {
                    if f == s {
                        ParameterCheckResult::matched()
                    } else {
                        ParameterCheckResult::failed()
                    }
                }
                (None, None) => ParameterCheckResult::matched(),
                _ => {
                    if config.required {
                        ParameterCheckResult::skip()
                    } else {
                        ParameterCheckResult::failed()
                    }
                }
            }
        }
        MatchMode::Warning => {
            // Warning mode for string params just checks if they're different
            match (frame_value, set_value) {
                (Some(f), Some(s)) if f != s => {
                    ParameterCheckResult::warning(format!(
                        "{} mismatch: frame='{}' vs set='{}'", param_name, f, s
                    ))
                }
                _ => ParameterCheckResult::matched(),
            }
        }
    }
}

/// Check if a float parameter matches based on config
fn check_float_param(
    frame_value: Option<f64>,
    set_value: Option<f64>,
    config: &ParameterConfig,
    param_name: &str,
    tolerance: f64,
) -> ParameterCheckResult {
    match config.mode {
        MatchMode::Ignore => ParameterCheckResult::matched(),
        MatchMode::Exact => {
            // If required and frame value is None, skip matching entirely
            if config.required && frame_value.is_none() {
                return ParameterCheckResult::skip();
            }

            match (frame_value, set_value) {
                (Some(f), Some(s)) => {
                    if (f - s).abs() <= tolerance {
                        ParameterCheckResult::matched()
                    } else {
                        ParameterCheckResult::failed()
                    }
                }
                (None, None) => ParameterCheckResult::matched(),
                _ => {
                    if config.required {
                        ParameterCheckResult::skip()
                    } else {
                        ParameterCheckResult::failed()
                    }
                }
            }
        }
        MatchMode::Warning => {
            let threshold = config.warning_threshold.unwrap_or(tolerance);
            match (frame_value, set_value) {
                (Some(f), Some(s)) => {
                    let diff = (f - s).abs();
                    if diff > threshold {
                        ParameterCheckResult::warning(format!(
                            "{} differs by {:.1} (threshold: {:.1})", param_name, diff, threshold
                        ))
                    } else {
                        ParameterCheckResult::matched()
                    }
                }
                _ => ParameterCheckResult::matched(),
            }
        }
    }
}

/// Result of checking all parameters for a calibration match
#[derive(Debug, Clone)]
pub struct ConfigMatchResult {
    pub matches: bool,
    pub skip_matching: bool,
    pub warnings: Vec<String>,
}

/// Check all parameters for a calibration match using the config
pub fn check_calibration_match(
    frame: &Frame,
    set_instrume: &Option<String>,
    set_binning: &Option<String>,
    set_gain: Option<f64>,
    set_offset: Option<f64>,
    set_exptime: Option<f64>,
    set_focallen: Option<f64>,
    set_filter: &Option<String>,
    set_ccd_temp: Option<f64>,
    config: &CalibrationTypeConfig,
) -> ConfigMatchResult {
    let mut warnings = Vec::new();
    let mut all_match = true;

    // Check instrume
    let result = check_string_param(&frame.instrume, set_instrume, &config.instrume, "instrume");
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check binning
    let result = check_string_param(&frame.binning, set_binning, &config.binning, "binning");
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check gain (tolerance: 0.01)
    let result = check_float_param(frame.gain, set_gain, &config.gain, "gain", 0.01);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check offset (tolerance: 0.01)
    let result = check_float_param(frame.offset, set_offset, &config.offset, "offset", 0.01);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check exptime (tolerance: 0.1s)
    let result = check_float_param(frame.exptime, set_exptime, &config.exptime, "exptime", 0.1);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check focallen (tolerance: 1.0mm)
    let result = check_float_param(frame.focallen, set_focallen, &config.focallen, "focallen", 1.0);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check filter
    let result = check_string_param(&frame.filter, set_filter, &config.filter, "filter");
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check ccd_temp (tolerance: default 2.0°C)
    let result = check_float_param(frame.ccd_temp, set_ccd_temp, &config.ccd_temp, "ccd_temp", 2.0);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    ConfigMatchResult { matches: all_match, skip_matching: false, warnings }
}

/// Find calibration sets matching a source frame using configurable rules
pub fn find_calibration_sets(
    conn: &Connection,
    frame: &Frame,
    source_type: &str,
    calibration_type: &str,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    // Get the type config for this source→calibration pair
    let type_config = match config.get_type_config(source_type, calibration_type) {
        Some(tc) => tc,
        None => {
            // No config means this calibration type is not configured for this source
            return Ok(Vec::new());
        }
    };

    // Get image type for the calibration
    let imagetyp = match calibration_type {
        "flat" => ImageType::Flat,
        "dark" => ImageType::Dark,
        "bias" => ImageType::Bias,
        "darkflat" => ImageType::DarkFlat,
        _ => return Ok(Vec::new()),
    };

    // Query calibration sets
    let imagetyp_str = match calibration_type {
        "flat" => "Flat",
        "dark" => "Dark",
        "bias" => "Bias",
        "darkflat" => "DarkFlat",
        _ => return Ok(Vec::new()),
    };

    let mut stmt = conn.prepare(
        "SELECT id, gain, offset, binning, instrume, exptime, focallen, filter,
                ccd_temp, temp_min, temp_max, date_start, date_end
         FROM calibration_set
         WHERE imagetyp = ?1
         ORDER BY date_start DESC"
    )?;

    let mut candidates = Vec::new();

    let rows = stmt.query_map([imagetyp_str], |row| {
        Ok((
            row.get::<_, i64>(0)?,           // id
            row.get::<_, Option<f64>>(1)?,   // gain
            row.get::<_, Option<f64>>(2)?,   // offset
            row.get::<_, Option<String>>(3)?, // binning
            row.get::<_, Option<String>>(4)?, // instrume
            row.get::<_, Option<f64>>(5)?,   // exptime
            row.get::<_, Option<f64>>(6)?,   // focallen
            row.get::<_, Option<String>>(7)?, // filter
            row.get::<_, Option<f64>>(8)?,   // ccd_temp
            row.get::<_, Option<f64>>(9)?,   // temp_min
            row.get::<_, Option<f64>>(10)?,  // temp_max
            row.get::<_, String>(11)?,       // date_start
            row.get::<_, String>(12)?,       // date_end
        ))
    })?;

    for row_result in rows {
        let (set_id, gain, offset, binning, instrume, exptime, focallen, filter,
             ccd_temp, temp_min, temp_max, date_start, date_end) = row_result?;

        // Use the average temp if available
        let set_temp = match (temp_min, temp_max) {
            (Some(min), Some(max)) => Some((min + max) / 2.0),
            _ => ccd_temp,
        };

        // Check all parameters using configurable rules
        let match_result = check_calibration_match(
            frame,
            &instrume,
            &binning,
            gain,
            offset,
            exptime,
            focallen,
            &filter,
            set_temp,
            type_config,
        );

        // Skip entirely if required field is missing
        if match_result.skip_matching {
            continue;
        }

        // Skip if parameters don't match
        if !match_result.matches {
            continue;
        }

        // Calculate date difference for scoring
        let date_diff = calculate_date_diff(frame.date_obs, &date_start, &date_end);

        // Calculate temperature difference for scoring
        let temp_diff = match (frame.ccd_temp, set_temp) {
            (Some(f_temp), Some(s_temp)) => Some((f_temp - s_temp).abs()),
            _ => None,
        };

        // Score the match using temperature weight and scale from config
        let score = score_match(date_diff, temp_diff, config.scoring.temperature_match_weight, config.scoring.temperature_scale);

        // Determine warnings
        let temp_warning = !match_result.warnings.is_empty() &&
            match_result.warnings.iter().any(|w| w.contains("ccd_temp"));
        let date_warning = check_date_warning_days(date_diff, calibration_type, config);

        candidates.push(CalibrationCandidate {
            set_id,
            imagetyp: imagetyp.clone(),
            match_score: score,
            date_diff_days: date_diff.unwrap_or(0),
            temp_diff,
            date_warning,
            temp_warning,
        });
    }

    // Sort by score (best first)
    candidates.sort_by(|a, b| {
        b.match_score.partial_cmp(&a.match_score).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Apply master preference if configured
    let master_pref = config.get_master_preference(calibration_type);
    candidates = apply_master_preference(conn, candidates, master_pref)?;

    Ok(candidates)
}

/// Calculate date difference in days
fn calculate_date_diff(
    frame_date: Option<DateTime<Utc>>,
    set_date_start: &str,
    set_date_end: &str,
) -> Option<i64> {
    let frame_dt = frame_date?;

    let set_start = DateTime::parse_from_rfc3339(set_date_start).ok()?;
    let set_end = DateTime::parse_from_rfc3339(set_date_end).ok()?;

    let diff_from_start = (frame_dt - set_start.with_timezone(&Utc)).num_days().abs();
    let diff_from_end = (frame_dt - set_end.with_timezone(&Utc)).num_days().abs();

    Some(diff_from_start.min(diff_from_end))
}

/// Check if date difference should trigger a warning using config thresholds
fn check_date_warning_days(date_diff: Option<i64>, calibration_type: &str, config: &CalibrationMatchingConfig) -> bool {
    match date_diff {
        Some(days) => {
            let threshold = match calibration_type {
                "flat" => config.warnings.flat_date_warning_days,
                "dark" => config.warnings.dark_date_warning_days,
                "darkflat" => config.warnings.darkflat_date_warning_days,
                "bias" => return false, // No date warning for bias
                _ => return false,
            };
            // Only trigger warning if threshold is enabled (>0 and reasonable)
            threshold > 0 && threshold < 10000 && days > threshold
        }
        None => false,
    }
}

/// Score a calibration match
fn score_match(
    date_diff_days: Option<i64>,
    temp_diff: Option<f64>,
    temp_weight: f64,
    temp_scale: f64,
) -> f64 {
    let mut score = 1.0;

    // Date scoring: exponential decay
    if let Some(days) = date_diff_days {
        let date_score = 1.0 / (1.0 + (days as f64 / 30.0));
        score *= date_score;
    }

    // Temperature scoring with configurable weight and scale
    if let Some(temp) = temp_diff {
        let temp_score = 1.0 / (1.0 + (temp.abs() / temp_scale));
        // Apply weight: weighted average between 1.0 and temp_score
        let weighted_temp = 1.0 * (1.0 - temp_weight) + temp_score * temp_weight;
        score *= weighted_temp;
    }

    score.max(0.0).min(1.0)
}

/// Apply master preference to sort candidates
fn apply_master_preference(
    conn: &Connection,
    mut candidates: Vec<CalibrationCandidate>,
    preference: MasterPreference,
) -> Result<Vec<CalibrationCandidate>> {
    match preference {
        MasterPreference::NoPreference => Ok(candidates),
        MasterPreference::PreferMaster | MasterPreference::PreferFrameset => {
            // Separate into masters and non-masters
            let mut masters = Vec::new();
            let mut framesets = Vec::new();

            for candidate in candidates.drain(..) {
                // Check if this set contains any master frames
                let is_master: bool = conn.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM calibration_set_frames csf
                        JOIN frames f ON csf.frame_id = f.id
                        WHERE csf.set_id = ?1 AND f.is_master = 1
                    )",
                    [candidate.set_id],
                    |row| row.get(0),
                ).unwrap_or(false);

                if is_master {
                    masters.push(candidate);
                } else {
                    framesets.push(candidate);
                }
            }

            // Concatenate in preference order
            match preference {
                MasterPreference::PreferMaster => {
                    masters.extend(framesets);
                    Ok(masters)
                }
                MasterPreference::PreferFrameset => {
                    framesets.extend(masters);
                    Ok(framesets)
                }
                _ => unreachable!(),
            }
        }
    }
}

/// Find calibration with fallback chain (for Flats: DarkFlat → Dark → Bias)
pub fn find_calibration_with_fallback(
    conn: &Connection,
    frame: &Frame,
    source_type: &str,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    // Get behavioral options for the source type
    let fallback_chain = match config.get_behavioral_options(source_type) {
        Some(opts) if !opts.fallback_chain.is_empty() => opts.fallback_chain.clone(),
        _ => {
            // Default fallback chains
            match source_type {
                "flats" => vec!["darkflat".to_string(), "dark".to_string(), "bias".to_string()],
                "lights" => vec!["dark".to_string()], // Lights don't have fallback to bias
                "darks" => vec!["bias".to_string()],
                _ => Vec::new(),
            }
        }
    };

    // Try each calibration type in the fallback chain
    for calibration_type in fallback_chain {
        let candidates = find_calibration_sets(conn, frame, source_type, &calibration_type, config)?;
        if !candidates.is_empty() {
            return Ok(candidates);
        }
    }

    Ok(Vec::new())
}

/// Setting key for calibration matching config
const CALIBRATION_CONFIG_KEY: &str = "calibration.matching_config";

/// Load calibration matching config from the database
/// Falls back to default config if not found or invalid
pub fn load_config(conn: &Connection) -> CalibrationMatchingConfig {
    // Try to load from database
    let result: Result<String, _> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [CALIBRATION_CONFIG_KEY],
        |row| row.get(0),
    );

    match result {
        Ok(json) => {
            match CalibrationMatchingConfig::from_json(&json) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("Failed to parse calibration config, using defaults: {}", e);
                    CalibrationMatchingConfig::default()
                }
            }
        }
        Err(_) => {
            // No config in database, use defaults
            CalibrationMatchingConfig::default()
        }
    }
}

/// Find Dark calibration sets for a light frame using configurable rules
pub fn find_dark_for_light(
    conn: &Connection,
    frame: &Frame,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    find_calibration_sets(conn, frame, "lights", "dark", config)
}

/// Find Bias calibration sets for a frame using configurable rules
#[allow(dead_code)]
pub fn find_bias_for_frame(
    conn: &Connection,
    frame: &Frame,
    source_type: &str,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    find_calibration_sets(conn, frame, source_type, "bias", config)
}

/// Find calibration for Flat sets (DarkFlat → Dark → Bias fallback)
pub fn find_calibration_for_flat(
    conn: &Connection,
    frame: &Frame,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    find_calibration_with_fallback(conn, frame, "flats", config)
}

/// Find calibration for Dark sets (Bias only, if enabled)
pub fn find_calibration_for_dark(
    conn: &Connection,
    frame: &Frame,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    // Check if bias for dark optimization is enabled
    if let Some(opts) = config.get_behavioral_options("darks") {
        if opts.use_bias_for_dark_optimization {
            return find_calibration_sets(conn, frame, "darks", "bias", config);
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::config::ParameterConfig;

    #[test]
    fn test_check_string_param_exact() {
        let config = ParameterConfig::exact(true);

        // Matching values
        let result = check_string_param(
            &Some("ASI294MC".to_string()),
            &Some("ASI294MC".to_string()),
            &config,
            "instrume",
        );
        assert!(result.matches);
        assert!(!result.skip_matching);

        // Non-matching values
        let result = check_string_param(
            &Some("ASI294MC".to_string()),
            &Some("ASI1600MM".to_string()),
            &config,
            "instrume",
        );
        assert!(!result.matches);

        // Missing required value
        let result = check_string_param(
            &None,
            &Some("ASI294MC".to_string()),
            &config,
            "instrume",
        );
        assert!(result.skip_matching);
    }

    #[test]
    fn test_check_string_param_ignore() {
        let config = ParameterConfig::ignore();

        // Should always match when ignored
        let result = check_string_param(
            &Some("ASI294MC".to_string()),
            &Some("ASI1600MM".to_string()),
            &config,
            "instrume",
        );
        assert!(result.matches);
    }

    #[test]
    fn test_check_float_param_warning() {
        let config = ParameterConfig::warning(2.0);

        // Within threshold
        let result = check_float_param(
            Some(-10.0),
            Some(-11.0),
            &config,
            "ccd_temp",
            2.0,
        );
        assert!(result.matches);
        assert!(!result.warning);

        // Outside threshold - should match but with warning
        let result = check_float_param(
            Some(-10.0),
            Some(-15.0),
            &config,
            "ccd_temp",
            2.0,
        );
        assert!(result.matches);
        assert!(result.warning);
        assert!(result.warning_message.is_some());
    }

    #[test]
    fn test_score_match() {
        // Perfect match
        let score = score_match(Some(0), Some(0.0), 0.3, 2.0);
        assert!(score > 0.99);

        // With temperature weight
        let score_low_weight = score_match(Some(10), Some(5.0), 0.1, 2.0);
        let score_high_weight = score_match(Some(10), Some(5.0), 0.5, 2.0);

        // Higher weight should amplify the temperature effect
        // Both should be valid scores
        assert!(score_low_weight > 0.0 && score_low_weight <= 1.0);
        assert!(score_high_weight > 0.0 && score_high_weight <= 1.0);
    }
}
