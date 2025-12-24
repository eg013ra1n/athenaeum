/// Configurable Calibration Matcher
///
/// This module implements the configurable calibration matching system
/// that uses user-defined rules from CalibrationMatchingConfig.

use crate::calibration::config::{
    CalibrationMatchingConfig, CalibrationTypeConfig, MatchMode, ParameterConfig,
    MasterPreference, ScoringConfig,
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
///
/// For Warning mode with dual thresholds:
/// - If diff > matching_threshold: REJECT the match
/// - If diff > warning_threshold but <= matching_threshold: Accept with WARNING
/// - If diff <= warning_threshold: Accept without warning
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
            // Dual threshold logic:
            // - matching_threshold: maximum allowed difference (rejects if exceeded)
            // - warning_threshold: triggers warning display (must be <= matching_threshold)
            let warning_thresh = config.warning_threshold.unwrap_or(tolerance);
            let matching_thresh = config.matching_threshold.unwrap_or(f64::MAX);

            match (frame_value, set_value) {
                (Some(f), Some(s)) => {
                    let diff = (f - s).abs();

                    // First check: if outside matching threshold, REJECT
                    if diff > matching_thresh {
                        return ParameterCheckResult::failed();
                    }

                    // Second check: if outside warning threshold, accept with WARNING
                    if diff > warning_thresh {
                        ParameterCheckResult::warning(format!(
                            "{} differs by {:.1} (warning threshold: {:.1}, max: {:.1})",
                            param_name, diff, warning_thresh, matching_thresh
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
    set_telescop: &Option<String>,
    set_exptime: Option<f64>,
    set_focallen: Option<f64>,
    set_filter: &Option<String>,
    set_ccd_temp: Option<f64>,
    config: &CalibrationTypeConfig,
) -> ConfigMatchResult {
    let mut warnings = Vec::new();
    let mut all_match = true;

    // Check instrume (always exact, locked)
    let result = check_string_param(&frame.instrume, set_instrume, &config.instrume, "instrume");
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check binning (always exact, locked)
    let result = check_string_param(&frame.binning, set_binning, &config.binning, "binning");
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check gain (always exact, locked, tolerance: 0.01)
    let result = check_float_param(frame.gain, set_gain, &config.gain, "gain", 0.01);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check offset (always exact, locked, tolerance: 0.01)
    let result = check_float_param(frame.offset, set_offset, &config.offset, "offset", 0.01);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check telescop (exact or disabled, no warning mode)
    let result = check_string_param(&frame.telescop, set_telescop, &config.telescop, "telescop");
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check exptime (supports warning mode with dual thresholds, tolerance: 0.1s)
    let result = check_float_param(frame.exptime, set_exptime, &config.exptime, "exptime", 0.1);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check focallen (supports warning mode with dual thresholds, tolerance: 1.0mm)
    let result = check_float_param(frame.focallen, set_focallen, &config.focallen, "focallen", 1.0);
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check filter (exact or disabled, no warning mode)
    let result = check_string_param(&frame.filter, set_filter, &config.filter, "filter");
    if result.skip_matching { return ConfigMatchResult { matches: false, skip_matching: true, warnings }; }
    if !result.matches { all_match = false; }
    if let Some(msg) = result.warning_message { warnings.push(msg); }

    // Check ccd_temp (supports warning mode with dual thresholds, tolerance: 2.0°C)
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
                ccd_temp, temp_min, temp_max, date_start, date_end, telescop
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
            row.get::<_, Option<String>>(13)?, // telescop
        ))
    })?;

    for row_result in rows {
        let (set_id, gain, offset, binning, instrume, exptime, focallen, filter,
             ccd_temp, temp_min, temp_max, date_start, date_end, telescop) = row_result?;

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
            &telescop,
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

        // Calculate exposure time difference for scoring
        let exptime_diff = match (frame.exptime, exptime) {
            (Some(f_exp), Some(s_exp)) => Some((f_exp - s_exp).abs()),
            _ => None,
        };

        // Score the match using configurable weights and scales
        let score = score_match(date_diff, temp_diff, exptime_diff, &config.scoring);

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

/// Score a calibration match based on date proximity, temperature, and exposure time.
/// Returns a score from 0.0 to 1.0 where 1.0 is a perfect match.
pub fn score_match(
    date_diff_days: Option<i64>,
    temp_diff: Option<f64>,
    exptime_diff: Option<f64>,
    config: &ScoringConfig,
) -> f64 {
    let mut score = 1.0;

    // Date scoring: exponential decay
    if let Some(days) = date_diff_days {
        let date_score = 1.0 / (1.0 + (days as f64 / 30.0));
        score *= date_score;
    }

    // Temperature scoring with configurable weight and scale
    if let Some(temp) = temp_diff {
        let temp_score = 1.0 / (1.0 + (temp.abs() / config.temperature_scale));
        // Apply weight: weighted average between 1.0 and temp_score
        let weighted_temp = 1.0 * (1.0 - config.temperature_match_weight) + temp_score * config.temperature_match_weight;
        score *= weighted_temp;
    }

    // Exposure time scoring with configurable weight and scale
    if let Some(exp) = exptime_diff {
        let exp_score = 1.0 / (1.0 + (exp.abs() / config.exposure_scale));
        // Apply weight: weighted average between 1.0 and exp_score
        let weighted_exp = 1.0 * (1.0 - config.exposure_match_weight) + exp_score * config.exposure_match_weight;
        score *= weighted_exp;
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

/// Get default fallback chain for a source type
fn get_default_fallback_chain(source_type: &str, include_bias: bool) -> Vec<String> {
    match source_type {
        "flats" => {
            if include_bias {
                vec!["darkflat".to_string(), "dark".to_string(), "bias".to_string()]
            } else {
                vec!["darkflat".to_string(), "dark".to_string()]
            }
        }
        "lights" => vec!["dark".to_string()], // Lights don't have fallback to bias
        "darks" => vec!["bias".to_string()],
        _ => Vec::new(),
    }
}

/// Find calibration with fallback chain (for Flats: DarkFlat → Dark → Bias)
/// Respects the use_bias_if_no_darks setting for flats
pub fn find_calibration_with_fallback(
    conn: &Connection,
    frame: &Frame,
    source_type: &str,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    // Get behavioral options for the source type
    let fallback_chain = match config.get_behavioral_options(source_type) {
        Some(opts) => {
            // For flats: respect use_bias_if_no_darks setting
            if source_type == "flats" {
                if opts.use_bias_if_no_darks {
                    // Include bias in fallback chain
                    if !opts.fallback_chain.is_empty() {
                        opts.fallback_chain.clone()
                    } else {
                        get_default_fallback_chain(source_type, true)
                    }
                } else {
                    // Exclude bias from fallback chain
                    get_default_fallback_chain(source_type, false)
                }
            } else if !opts.fallback_chain.is_empty() {
                opts.fallback_chain.clone()
            } else {
                get_default_fallback_chain(source_type, true)
            }
        }
        _ => get_default_fallback_chain(source_type, true),
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
    fn test_check_float_param_warning_dual_threshold() {
        // Dual threshold: warn at 2.0, reject at 5.0
        let config = ParameterConfig::warning(2.0, 5.0);

        // Within warning threshold - no warning
        let result = check_float_param(
            Some(-10.0),
            Some(-11.0),
            &config,
            "ccd_temp",
            2.0,
        );
        assert!(result.matches, "Should match when within warning threshold");
        assert!(!result.warning, "Should not warn when within warning threshold");

        // Outside warning threshold but within matching threshold - warning but match
        let result = check_float_param(
            Some(-10.0),
            Some(-14.0), // 4 degree difference: > 2.0 warn, < 5.0 reject
            &config,
            "ccd_temp",
            2.0,
        );
        assert!(result.matches, "Should match when within matching threshold");
        assert!(result.warning, "Should warn when outside warning threshold");
        assert!(result.warning_message.is_some());

        // Outside matching threshold - should REJECT (not match)
        let result = check_float_param(
            Some(-10.0),
            Some(-16.0), // 6 degree difference: > 5.0 reject threshold
            &config,
            "ccd_temp",
            2.0,
        );
        assert!(!result.matches, "Should NOT match when outside matching threshold");
    }

    #[test]
    fn test_score_match() {
        let config = ScoringConfig {
            temperature_match_weight: 0.3,
            temperature_scale: 2.0,
            exposure_match_weight: 0.4,
            exposure_scale: 1.0,
        };

        // Perfect match
        let score = score_match(Some(0), Some(0.0), Some(0.0), &config);
        assert!(score > 0.99);

        // With temperature difference only
        let score_temp = score_match(Some(10), Some(5.0), None, &config);
        assert!(score_temp > 0.0 && score_temp <= 1.0);

        // With exposure difference only
        let score_exp = score_match(Some(10), None, Some(2.0), &config);
        assert!(score_exp > 0.0 && score_exp <= 1.0);

        // With both temperature and exposure differences
        let score_both = score_match(Some(10), Some(3.0), Some(1.5), &config);
        assert!(score_both > 0.0 && score_both <= 1.0);

        // Higher weight should amplify the effect
        let config_low_weight = ScoringConfig {
            temperature_match_weight: 0.1,
            temperature_scale: 2.0,
            exposure_match_weight: 0.1,
            exposure_scale: 1.0,
        };
        let config_high_weight = ScoringConfig {
            temperature_match_weight: 0.5,
            temperature_scale: 2.0,
            exposure_match_weight: 0.5,
            exposure_scale: 1.0,
        };

        let score_low_weight = score_match(Some(10), Some(5.0), Some(3.0), &config_low_weight);
        let score_high_weight = score_match(Some(10), Some(5.0), Some(3.0), &config_high_weight);

        // Higher weight should amplify the differences (lower score)
        assert!(score_high_weight < score_low_weight);
    }

    #[test]
    fn test_score_match_exposure_preference() {
        // Test that closer exposure time is preferred
        let config = ScoringConfig::default();

        // Simulating Flat 1.18s vs Dark 1s (diff = 0.18) and Dark 5s (diff = 3.82)
        let score_1s = score_match(Some(3), Some(0.1), Some(0.18), &config);
        let score_5s = score_match(Some(3), Some(0.1), Some(3.82), &config);

        // 1s dark should score higher (closer exposure match)
        assert!(score_1s > score_5s, "Closer exposure should score higher: {} vs {}", score_1s, score_5s);
    }
}
