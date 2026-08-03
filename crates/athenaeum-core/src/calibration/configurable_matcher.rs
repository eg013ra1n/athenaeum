/// Configurable Calibration Matcher
///
/// This module implements the configurable calibration matching system
/// that uses user-defined rules from CalibrationMatchingConfig.
use crate::calibration::config::{
    CalibrationMatchingConfig, CalibrationTypeConfig, MasterPreference, MatchMode, ParameterConfig,
    ScoringConfig,
};
use crate::calibration::finder::{
    CalibrationCandidate, CandidateMatchDetails, CandidateMode, ParameterMatch,
};
use crate::models::{Frame, ImageType};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;

/// Result of checking a single parameter, with the per-parameter detail the
/// manual modal needs to render "what was compared, against what threshold".
#[derive(Debug, Clone)]
pub struct ParameterCheckResult {
    pub matches: bool,
    #[allow(dead_code)]
    pub warning: bool,
    pub warning_message: Option<String>,
    pub skip_matching: bool, // If true, skip this calibration type entirely
    pub detail: ParameterMatch,
}

impl ParameterCheckResult {
    fn matched(detail: ParameterMatch) -> Self {
        Self {
            matches: true,
            warning: false,
            warning_message: None,
            skip_matching: false,
            detail: ParameterMatch {
                matched: true,
                ..detail
            },
        }
    }

    fn failed(detail: ParameterMatch) -> Self {
        Self {
            matches: false,
            warning: false,
            warning_message: None,
            skip_matching: false,
            detail: ParameterMatch {
                matched: false,
                ..detail
            },
        }
    }

    fn warning(msg: String, detail: ParameterMatch) -> Self {
        Self {
            matches: true,
            warning: true,
            warning_message: Some(msg),
            skip_matching: false,
            detail: ParameterMatch {
                matched: true,
                warning: true,
                ..detail
            },
        }
    }

    fn skip(detail: ParameterMatch) -> Self {
        Self {
            matches: false,
            warning: false,
            warning_message: None,
            skip_matching: true,
            detail,
        }
    }
}

/// Check if a string parameter matches based on config
fn check_string_param(
    frame_value: &Option<String>,
    set_value: &Option<String>,
    config: &ParameterConfig,
    param_name: &str,
) -> ParameterCheckResult {
    let base_detail = ParameterMatch {
        mode: config.mode,
        frame_value: frame_value.clone(),
        set_value: set_value.clone(),
        ..Default::default()
    };
    match config.mode {
        MatchMode::Ignore => ParameterCheckResult::matched(base_detail),
        MatchMode::Exact => {
            // If required and frame value is None, skip matching entirely
            if config.required && frame_value.is_none() {
                return ParameterCheckResult::skip(base_detail);
            }

            match (frame_value, set_value) {
                (Some(f), Some(s)) => {
                    if f == s {
                        ParameterCheckResult::matched(base_detail)
                    } else {
                        ParameterCheckResult::failed(base_detail)
                    }
                }
                (None, None) => ParameterCheckResult::matched(base_detail),
                _ => {
                    if config.required {
                        ParameterCheckResult::skip(base_detail)
                    } else {
                        ParameterCheckResult::failed(base_detail)
                    }
                }
            }
        }
        MatchMode::Warning => {
            // Warning mode for string params just checks if they're different
            match (frame_value, set_value) {
                (Some(f), Some(s)) if f != s => ParameterCheckResult::warning(
                    format!("{} mismatch: frame='{}' vs set='{}'", param_name, f, s),
                    base_detail,
                ),
                _ => ParameterCheckResult::matched(base_detail),
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
    let diff = match (frame_value, set_value) {
        (Some(f), Some(s)) => Some((f - s).abs()),
        _ => None,
    };
    let base_detail = ParameterMatch {
        mode: config.mode,
        frame_value: frame_value.map(|v| v.to_string()),
        set_value: set_value.map(|v| v.to_string()),
        diff,
        warning_threshold: config.warning_threshold,
        matching_threshold: config.matching_threshold,
        ..Default::default()
    };
    match config.mode {
        MatchMode::Ignore => ParameterCheckResult::matched(base_detail),
        MatchMode::Exact => {
            // If required and frame value is None, skip matching entirely
            if config.required && frame_value.is_none() {
                return ParameterCheckResult::skip(base_detail);
            }

            match (frame_value, set_value) {
                (Some(f), Some(s)) => {
                    if (f - s).abs() <= tolerance {
                        ParameterCheckResult::matched(base_detail)
                    } else {
                        ParameterCheckResult::failed(base_detail)
                    }
                }
                (None, None) => ParameterCheckResult::matched(base_detail),
                _ => {
                    if config.required {
                        ParameterCheckResult::skip(base_detail)
                    } else {
                        ParameterCheckResult::failed(base_detail)
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
                    let abs_diff = (f - s).abs();

                    // First check: if outside matching threshold, REJECT
                    if abs_diff > matching_thresh {
                        return ParameterCheckResult::failed(base_detail);
                    }

                    // Second check: if outside warning threshold, accept with WARNING
                    if abs_diff > warning_thresh {
                        ParameterCheckResult::warning(
                            format!(
                                "{} differs by {:.1} (warning threshold: {:.1}, max: {:.1})",
                                param_name, abs_diff, warning_thresh, matching_thresh
                            ),
                            base_detail,
                        )
                    } else {
                        ParameterCheckResult::matched(base_detail)
                    }
                }
                _ => ParameterCheckResult::matched(base_detail),
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

/// Check all parameters for a calibration match using the config.
///
/// Returns `(ConfigMatchResult, CandidateMatchDetails)`. The first describes
/// pass/fail/skip + accumulated warning strings (used by both auto-link and
/// manual paths to gate the candidate). The second is the per-parameter
/// breakdown used by the manual modal to render exactly what was compared.
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
) -> (ConfigMatchResult, CandidateMatchDetails) {
    let mut warnings = Vec::new();
    let mut all_match = true;
    let mut details = CandidateMatchDetails::default();

    macro_rules! apply {
        ($field:ident, $result:expr) => {{
            let r = $result;
            details.$field = r.detail.clone();
            if r.skip_matching {
                return (
                    ConfigMatchResult {
                        matches: false,
                        skip_matching: true,
                        warnings: warnings.clone(),
                    },
                    details,
                );
            }
            if !r.matches {
                all_match = false;
            }
            if let Some(msg) = r.warning_message {
                warnings.push(msg);
            }
        }};
    }

    apply!(
        instrume,
        check_string_param(&frame.instrume, set_instrume, &config.instrume, "instrume")
    );
    apply!(
        binning,
        check_string_param(&frame.binning, set_binning, &config.binning, "binning")
    );
    apply!(
        gain,
        check_float_param(frame.gain, set_gain, &config.gain, "gain", 0.01)
    );
    apply!(
        offset,
        check_float_param(frame.offset, set_offset, &config.offset, "offset", 0.01)
    );
    apply!(
        telescop,
        check_string_param(&frame.telescop, set_telescop, &config.telescop, "telescop")
    );
    apply!(
        exptime,
        check_float_param(frame.exptime, set_exptime, &config.exptime, "exptime", 0.1)
    );
    apply!(
        focallen,
        check_float_param(
            frame.focallen,
            set_focallen,
            &config.focallen,
            "focallen",
            1.0
        )
    );
    apply!(
        filter,
        check_string_param(&frame.filter, set_filter, &config.filter, "filter")
    );
    // A4: fall back to set_temp when measured ccd_temp is missing so master
    // frames / uncooled emergency captures still get a temperature comparison.
    let frame_temp_effective = crate::models::effective_temp(frame.ccd_temp, frame.set_temp);
    apply!(
        ccd_temp,
        check_float_param(
            frame_temp_effective,
            set_ccd_temp,
            &config.ccd_temp,
            "ccd_temp",
            2.0
        )
    );

    (
        ConfigMatchResult {
            matches: all_match,
            skip_matching: false,
            warnings,
        },
        details,
    )
}

/// Find calibration sets matching a source frame using configurable rules.
///
/// Auto-link path: returns only candidates that pass the config-driven hard
/// filter, sorted by score (and master preference). This is now a thin wrapper
/// around `find_calibration_candidates(..., OnlyCompatible)` so auto-link and
/// the manual modal share one engine, one scorer, one set of filter rules.
pub fn find_calibration_sets(
    conn: &Connection,
    frame: &Frame,
    source_type: &str,
    calibration_type: &str,
    config: &CalibrationMatchingConfig,
) -> Result<Vec<CalibrationCandidate>> {
    find_calibration_candidates(
        conn,
        frame,
        source_type,
        calibration_type,
        config,
        CandidateMode::OnlyCompatible,
    )
}

/// Unified candidate engine used by both auto-link and the manual modal.
///
/// Always queries both regular and master sets so the manual modal can show
/// masters when the user toggles "Show All". Filtering and master-preference
/// ordering happen as a post-step on the scored list.
///
/// Behavior in `OnlyCompatible` mode:
/// - Drops candidates that fail any Exact / Warning parameter check.
/// - Masters are always candidates; `master_preferences` orders, never filters.
/// - Honors `master_preferences`:
///   - `PreferMaster`  → masters first, then framesets.
///   - `PreferFrameset` → framesets first, then masters.
///   - `NoPreference`  → no grouping; both sorted together by score.
///
/// Behavior in `IncludeIncompatible` mode:
/// - Returns every set of the requested type. Compatible candidates first
///   (descending by score), then incompatible (descending by score). Master
///   preference is applied within the compatible group only — incompatible
///   sets keep their score-only order.
pub fn find_calibration_candidates(
    conn: &Connection,
    frame: &Frame,
    source_type: &str,
    calibration_type: &str,
    config: &CalibrationMatchingConfig,
    mode: CandidateMode,
) -> Result<Vec<CalibrationCandidate>> {
    let type_config = match config.get_type_config(source_type, calibration_type) {
        Some(tc) => tc,
        None => return Ok(Vec::new()),
    };

    let imagetyp = match calibration_type {
        "flat" => ImageType::Flat,
        "dark" => ImageType::Dark,
        "bias" => ImageType::Bias,
        "darkflat" => ImageType::DarkFlat,
        _ => return Ok(Vec::new()),
    };

    let (imagetyp_str, master_imagetyp_str) = match calibration_type {
        "flat" => ("Flat", "MasterFlat"),
        "dark" => ("Dark", "MasterDark"),
        "bias" => ("Bias", "MasterBias"),
        "darkflat" => ("DarkFlat", "MasterDarkFlat"),
        _ => return Ok(Vec::new()),
    };

    let master_pref = config.get_master_preference(calibration_type);

    // Always query both regular + master rows. Master preference is applied
    // post-query so the manual modal can show masters in "Show All" even when
    // the user has master preference set to NoPreference.
    // superseded_by_set_id IS NULL: a raw set that's been replaced by a
    // registered master (calibration_library::register::register_master)
    // must never surface as a candidate — the master is already relinked in
    // its place. This is the ONE place both auto-link (OnlyCompatible) and
    // the manual modal (IncludeIncompatible) enumerate candidate sets, so
    // gating it here covers both call sites in api/calibration.rs.
    let query = format!(
        "SELECT id, gain, offset, binning, instrume, exptime, focallen, filter,
                ccd_temp, temp_min, temp_max, date_start, date_end, telescop, is_master_library
         FROM calibration_set
         WHERE imagetyp IN ('{}', '{}')
           AND superseded_by_set_id IS NULL
         ORDER BY date_start DESC",
        imagetyp_str, master_imagetyp_str
    );

    let mut stmt = conn.prepare(&query)?;
    let mut compatible: Vec<CalibrationCandidate> = Vec::new();
    let mut incompatible: Vec<CalibrationCandidate> = Vec::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,              // id
            row.get::<_, Option<f64>>(1)?,      // gain
            row.get::<_, Option<f64>>(2)?,      // offset
            row.get::<_, Option<String>>(3)?,   // binning
            row.get::<_, Option<String>>(4)?,   // instrume
            row.get::<_, Option<f64>>(5)?,      // exptime
            row.get::<_, Option<f64>>(6)?,      // focallen
            row.get::<_, Option<String>>(7)?,   // filter
            row.get::<_, Option<f64>>(8)?,      // ccd_temp
            row.get::<_, Option<f64>>(9)?,      // temp_min
            row.get::<_, Option<f64>>(10)?,     // temp_max
            row.get::<_, String>(11)?,          // date_start
            row.get::<_, String>(12)?,          // date_end
            row.get::<_, Option<String>>(13)?,  // telescop
            row.get::<_, i32>(14).unwrap_or(0), // is_master_library
        ))
    })?;

    for row_result in rows {
        let (
            set_id,
            gain,
            offset,
            binning,
            instrume,
            exptime,
            focallen,
            filter,
            ccd_temp,
            temp_min,
            temp_max,
            date_start,
            date_end,
            telescop,
            is_master_library,
        ) = row_result?;

        let set_temp = match (temp_min, temp_max) {
            (Some(min), Some(max)) => Some((min + max) / 2.0),
            _ => ccd_temp,
        };

        let (match_result, mut details) = check_calibration_match(
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

        // Diffs for the engine score. These are also surfaced via details so
        // both UIs can display them consistently.
        let date_diff = calculate_date_diff(frame.date_obs, &date_start, &date_end);
        // A4: same set_temp fallback as in check_calibration_match so the
        // displayed temp_diff agrees with the hard-filter decision.
        let frame_temp_for_score = crate::models::effective_temp(frame.ccd_temp, frame.set_temp);
        let temp_diff = match (frame_temp_for_score, set_temp) {
            (Some(f), Some(s)) => Some((f - s).abs()),
            _ => None,
        };
        let exptime_diff = match (frame.exptime, exptime) {
            (Some(f), Some(s)) => Some((f - s).abs()),
            _ => None,
        };
        details.date_diff_days = date_diff.unwrap_or(0);
        details.temp_diff = temp_diff;
        details.exptime_diff = exptime_diff;

        let date_warning = check_date_warning_days(date_diff, calibration_type, config);
        let temp_warning = match_result.warnings.iter().any(|w| w.contains("ccd_temp"));

        // `skip_matching` fires per-candidate when an Exact+required parameter
        // can't be COMPARED at all (frame missing the value, or set missing it —
        // e.g. a CCD set with no GAIN header). That is a hard-filter failure,
        // not an invisibility cloak: auto-link must not offer it, but the manual
        // modal's "Show All" must still list it so the user can pick it by hand.
        // The `OnlyCompatible` gate below is the one and only place it drops out.
        let passed_hard_filter = match_result.matches && !match_result.skip_matching;

        if mode == CandidateMode::OnlyCompatible && !passed_hard_filter {
            continue;
        }

        // Score reflects "is this a real match per the user's config" — not
        // just date/temp/exptime closeness. A wrong-camera (or wrong-filter,
        // wrong-binning, wrong-gain, wrong-offset) candidate is honestly a
        // zero-confidence match even if the date / temp / exptime line up,
        // and so are warning-mode params that exceed `matching_threshold`
        // (focal length, exptime, temp). All of those flip
        // `passed_hard_filter = false`, so this single override zeroes the
        // score for every kind of hard-rejection consistently.
        let score = if passed_hard_filter {
            score_match(date_diff, temp_diff, exptime_diff, &config.scoring)
        } else {
            0.0
        };

        let candidate = CalibrationCandidate {
            set_id,
            imagetyp: imagetyp.clone(),
            match_score: score,
            date_diff_days: date_diff.unwrap_or(0),
            temp_diff,
            date_warning,
            temp_warning,
            is_master: is_master_library == 1,
            passed_hard_filter,
            details,
            warnings: match_result.warnings,
        };

        if passed_hard_filter {
            compatible.push(candidate);
        } else {
            incompatible.push(candidate);
        }
    }

    // Score-sort each group, descending.
    let by_score_desc = |a: &CalibrationCandidate, b: &CalibrationCandidate| {
        b.match_score
            .partial_cmp(&a.match_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    compatible.sort_by(by_score_desc);
    incompatible.sort_by(by_score_desc);

    // Master preference applies inside the compatible group.
    let mut compatible = apply_master_preference(compatible, master_pref);

    match mode {
        CandidateMode::OnlyCompatible => {
            // Masters are ALWAYS auto-link candidates (2026-08-02 audit C1): a
            // superseded raw set is excluded by the candidate query, so hiding its
            // master too left whole lineages unmatchable and re-matching minted
            // duplicate raw sets. `master_preferences` only orders the list now.
            Ok(compatible)
        }
        CandidateMode::IncludeIncompatible => {
            // Compatible block first (already master-preference-sorted),
            // incompatible block second (score-only).
            compatible.extend(incompatible);
            Ok(compatible)
        }
    }
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
fn check_date_warning_days(
    date_diff: Option<i64>,
    calibration_type: &str,
    config: &CalibrationMatchingConfig,
) -> bool {
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
        let weighted_temp = 1.0 * (1.0 - config.temperature_match_weight)
            + temp_score * config.temperature_match_weight;
        score *= weighted_temp;
    }

    // Exposure time scoring with configurable weight and scale
    if let Some(exp) = exptime_diff {
        let exp_score = 1.0 / (1.0 + (exp.abs() / config.exposure_scale));
        // Apply weight: weighted average between 1.0 and exp_score
        let weighted_exp =
            1.0 * (1.0 - config.exposure_match_weight) + exp_score * config.exposure_match_weight;
        score *= weighted_exp;
    }

    score.max(0.0).min(1.0)
}

/// Apply master preference to sort candidates
fn apply_master_preference(
    mut candidates: Vec<CalibrationCandidate>,
    preference: MasterPreference,
) -> Vec<CalibrationCandidate> {
    match preference {
        MasterPreference::NoPreference => candidates,
        MasterPreference::PreferMaster | MasterPreference::PreferFrameset => {
            // Separate into masters and non-masters using the is_master field
            let mut masters = Vec::new();
            let mut framesets = Vec::new();

            for candidate in candidates.drain(..) {
                if candidate.is_master {
                    masters.push(candidate);
                } else {
                    framesets.push(candidate);
                }
            }

            // Concatenate in preference order
            match preference {
                MasterPreference::PreferMaster => {
                    masters.extend(framesets);
                    masters
                }
                MasterPreference::PreferFrameset => {
                    framesets.extend(masters);
                    framesets
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
                vec![
                    "darkflat".to_string(),
                    "dark".to_string(),
                    "bias".to_string(),
                ]
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
        let candidates =
            find_calibration_sets(conn, frame, source_type, &calibration_type, config)?;
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
        Ok(json) => match CalibrationMatchingConfig::from_json(&json) {
            Ok(config) => config.migrate(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse calibration matching config, using defaults");
                CalibrationMatchingConfig::default()
            }
        },
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
        let result = check_string_param(&None, &Some("ASI294MC".to_string()), &config, "instrume");
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
        let result = check_float_param(Some(-10.0), Some(-11.0), &config, "ccd_temp", 2.0);
        assert!(result.matches, "Should match when within warning threshold");
        assert!(
            !result.warning,
            "Should not warn when within warning threshold"
        );

        // Outside warning threshold but within matching threshold - warning but match
        let result = check_float_param(
            Some(-10.0),
            Some(-14.0), // 4 degree difference: > 2.0 warn, < 5.0 reject
            &config,
            "ccd_temp",
            2.0,
        );
        assert!(
            result.matches,
            "Should match when within matching threshold"
        );
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
        assert!(
            !result.matches,
            "Should NOT match when outside matching threshold"
        );
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
        assert!(
            score_1s > score_5s,
            "Closer exposure should score higher: {} vs {}",
            score_1s,
            score_5s
        );
    }

    // ── Engine-level tests ──────────────────────────────────────────────
    // These exercise `find_calibration_candidates` with an in-memory SQLite
    // DB to make sure the auto-link and manual-suggestion paths see identical
    // scores and that the `OnlyCompatible` / `IncludeIncompatible` modes do
    // exactly what the unification plan promises.

    use crate::db::schema::init_db;
    use rusqlite::params;
    use rusqlite::Connection;

    fn engine_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn insert_dark_set(
        conn: &Connection,
        id: i64,
        instrume: &str,
        binning: &str,
        gain: f64,
        offset: f64,
        exptime: f64,
        ccd_temp: f64,
        date_start: &str,
        is_master: i32,
        imagetyp: &str,
    ) {
        conn.execute(
            "INSERT INTO calibration_set
             (id, imagetyp, exptime, ccd_temp, temp_min, temp_max, gain, offset,
              binning, instrume, date, date_start, date_end, frame_count, is_master_library)
             VALUES (?1, ?2, ?3, ?4, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9, 10, ?10)",
            params![
                id, imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, date_start,
                is_master,
            ],
        )
        .unwrap();
    }

    fn light_frame_for_dark() -> Frame {
        Frame {
            id: Some(1),
            file_id: 1,
            object: None,
            date_obs: Some(
                chrono::DateTime::parse_from_rfc3339("2025-10-01T00:00:00+00:00")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            telescop: None,
            instrume: Some("ASI2600MM".to_string()),
            exptime: Some(300.0),
            filter: Some("L".to_string()),
            imagetyp: Some(ImageType::Light),
            is_master: false,
            gain: Some(56.0),
            offset: Some(50.0),
            binning: Some("1x1".to_string()),
            xbinning: Some(1),
            ybinning: Some(1),
            ccd_temp: Some(-10.0),
            set_temp: None,
            focallen: Some(448.0),
            xpixsz: None,
            ypixsz: None,
            naxis1: None,
            naxis2: None,
            ra: None,
            dec: None,
            sitelat: None,
            lat_obs: None,
            sitelong: None,
            long_obs: None,
            objctra: None,
            objctdec: None,
            override_: false,
            swcreate: None,
            bayerpat: None,
            xbayroff: None,
            ybayroff: None,
            roworder: None,
            rotation: None,
            uuid: None,
            updated_at: None,
        }
    }

    #[test]
    fn engine_only_compatible_drops_failing_candidates() {
        let conn = engine_test_db();
        // Match: same camera/binning/gain/offset/exptime, ~10°C
        insert_dark_set(
            &conn,
            1,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        // Mismatch: different binning
        insert_dark_set(
            &conn,
            2,
            "ASI2600MM",
            "2x2",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        // Mismatch: different exptime (rejected by Exact in default config)
        insert_dark_set(
            &conn,
            3,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            60.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );

        let frame = light_frame_for_dark();
        let config = CalibrationMatchingConfig::default();

        let compatible = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::OnlyCompatible,
        )
        .unwrap();

        let ids: Vec<i64> = compatible.iter().map(|c| c.set_id).collect();
        assert_eq!(
            ids,
            vec![1],
            "OnlyCompatible should keep just the matching set"
        );
        assert!(compatible[0].passed_hard_filter);
    }

    #[test]
    fn engine_include_incompatible_returns_all_with_flag() {
        let conn = engine_test_db();
        insert_dark_set(
            &conn,
            1,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        insert_dark_set(
            &conn,
            2,
            "ASI2600MM",
            "2x2",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        insert_dark_set(
            &conn,
            3,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            60.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );

        let frame = light_frame_for_dark();
        let config = CalibrationMatchingConfig::default();

        let all = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::IncludeIncompatible,
        )
        .unwrap();

        // All three rows present.
        assert_eq!(all.len(), 3, "IncludeIncompatible should keep every row");

        // Compatible (set 1) appears first; incompatible follows.
        assert_eq!(all[0].set_id, 1);
        assert!(all[0].passed_hard_filter);
        assert!(!all[1].passed_hard_filter);
        assert!(!all[2].passed_hard_filter);
    }

    #[test]
    fn engine_same_set_scores_identical_in_both_modes() {
        // The auto-link path's score and the manual-modal path's score must
        // agree for any compatible candidate, otherwise the modal lies about
        // what auto-link will pick.
        let conn = engine_test_db();
        insert_dark_set(
            &conn,
            1,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -8.5,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        insert_dark_set(
            &conn,
            2,
            "ASI2600MM",
            "2x2",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );

        let frame = light_frame_for_dark();
        let config = CalibrationMatchingConfig::default();

        let compatible = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::OnlyCompatible,
        )
        .unwrap();
        let all = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::IncludeIncompatible,
        )
        .unwrap();

        let auto_score = compatible
            .iter()
            .find(|c| c.set_id == 1)
            .map(|c| c.match_score)
            .expect("set 1 should be compatible");
        let manual_score = all
            .iter()
            .find(|c| c.set_id == 1)
            .map(|c| c.match_score)
            .expect("set 1 should also appear in IncludeIncompatible");

        assert!(
            (auto_score - manual_score).abs() < 1e-12,
            "auto vs manual score divergence: {} vs {}",
            auto_score,
            manual_score
        );
    }

    #[test]
    fn engine_include_incompatible_surfaces_cross_camera_dark() {
        // Manual modal use case: the user's lights are for camera A, but the
        // only Dark sets in their DB are for camera B. The manual modal needs
        // those B-camera Darks to appear (so the user can pick or confirm) —
        // OnlyCompatible would drop them on the instrume Exact filter.
        let conn = engine_test_db();
        // Wrong-camera dark: same exptime/gain/binning, different instrume.
        insert_dark_set(
            &conn,
            5,
            "OtherCamera",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );

        let frame = light_frame_for_dark();
        let config = CalibrationMatchingConfig::default();

        // Auto-link path: must reject the cross-camera dark.
        let auto = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::OnlyCompatible,
        )
        .unwrap();
        assert!(auto.is_empty(), "auto-link must NOT pick wrong-camera dark");

        // Manual modal path: must surface it so the user can override.
        let manual = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::IncludeIncompatible,
        )
        .unwrap();
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].set_id, 5);
        assert!(!manual[0].passed_hard_filter);
        assert!(!manual[0].details.instrume.matched);
        // Score must be zero for any hard-filter reject (camera mismatch here).
        // High date/temp/exptime closeness must not produce misleading scores.
        assert_eq!(
            manual[0].match_score, 0.0,
            "wrong-camera dark must score 0 even with perfect date/temp/exptime"
        );
    }

    #[test]
    fn engine_score_zero_for_hard_filter_rejects() {
        // Verify the score-zero rule fires for every kind of hard-filter
        // reject: instrume / binning / gain / offset (Exact mismatch),
        // exptime / focallen / temp (Warning mode beyond matching_threshold).
        let conn = engine_test_db();

        // Wrong binning, otherwise perfect.
        insert_dark_set(
            &conn,
            1,
            "ASI2600MM",
            "2x2",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        // Wrong gain.
        insert_dark_set(
            &conn,
            2,
            "ASI2600MM",
            "1x1",
            999.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        // exptime way off (>5s matching_threshold).
        insert_dark_set(
            &conn,
            3,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            60.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        // temp way off (>5°C matching_threshold).
        insert_dark_set(
            &conn,
            4,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -25.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        // Real match.
        insert_dark_set(
            &conn,
            5,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );

        let frame = light_frame_for_dark();
        let config = CalibrationMatchingConfig::default();

        let manual = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::IncludeIncompatible,
        )
        .unwrap();

        let by_id: std::collections::HashMap<i64, &CalibrationCandidate> =
            manual.iter().map(|c| (c.set_id, c)).collect();

        for &id in &[1i64, 2, 3, 4] {
            let c = by_id
                .get(&id)
                .expect("incompatible candidate must still be returned");
            assert!(!c.passed_hard_filter, "set {} should fail hard filter", id);
            assert_eq!(
                c.match_score, 0.0,
                "set {} score must be 0 (hard-filter reject)",
                id
            );
        }
        // Compatible match keeps a real score.
        let good = by_id.get(&5).unwrap();
        assert!(good.passed_hard_filter);
        assert!(
            good.match_score > 0.5,
            "compatible match should score well, got {}",
            good.match_score
        );
    }

    #[test]
    fn engine_skip_matching_does_not_abort_search() {
        // Regression: a calibration set with NULL on a required-Exact param
        // (e.g., gain) used to make the engine return Vec::new() and hide every
        // valid candidate behind it. Make sure such a row only costs itself an
        // auto-link slot — it must never suppress the rest of the search. (Its
        // own visibility in "Show All" is pinned by
        // `engine_skip_matching_set_is_incompatible_not_invisible`.)
        let conn = engine_test_db();

        // Set 1: malformed — NULL gain. With config.gain Exact+required, the
        // per-param check returns skip_matching for THIS row.
        conn.execute(
            "INSERT INTO calibration_set
             (id, imagetyp, exptime, ccd_temp, temp_min, temp_max, gain, offset,
              binning, instrume, date, date_start, date_end, frame_count, is_master_library)
             VALUES (1, 'Dark', 300.0, -10.0, -10.0, -10.0, NULL, 50.0, '1x1', 'ASI2600MM',
                     '2025-09-25T00:00:00+00:00', '2025-09-25T00:00:00+00:00',
                     '2025-09-25T00:00:00+00:00', 10, 0)",
            params![],
        )
        .unwrap();
        // Set 2: valid match
        insert_dark_set(
            &conn,
            2,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );

        let frame = light_frame_for_dark();
        let config = CalibrationMatchingConfig::default();

        let compatible = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::OnlyCompatible,
        )
        .unwrap();

        let ids: Vec<i64> = compatible.iter().map(|c| c.set_id).collect();
        assert_eq!(
            ids,
            vec![2],
            "skip_matching on one row must not hide other valid candidates"
        );
    }

    #[test]
    fn engine_skip_matching_set_is_incompatible_not_invisible() {
        // A set that can't be COMPARED on an Exact+required parameter (here:
        // NULL gain, the classic CCD-without-GAIN case) is not a match — but it
        // is still a set the user may legitimately want to pick by hand. It
        // must be absent from auto-link (`OnlyCompatible`) and present in the
        // manual modal's "Show All" (`IncludeIncompatible`) flagged as
        // incompatible, instead of vanishing from both.
        let conn = engine_test_db();

        const NULL_GAIN_SET_ID: i64 = 1;
        const VALID_SET_ID: i64 = 2;

        conn.execute(
            "INSERT INTO calibration_set
             (id, imagetyp, exptime, ccd_temp, temp_min, temp_max, gain, offset,
              binning, instrume, date, date_start, date_end, frame_count, is_master_library)
             VALUES (?1, 'Dark', 300.0, -10.0, -10.0, -10.0, NULL, 50.0, '1x1', 'ASI2600MM',
                     '2025-09-25T00:00:00+00:00', '2025-09-25T00:00:00+00:00',
                     '2025-09-25T00:00:00+00:00', 10, 0)",
            params![NULL_GAIN_SET_ID],
        )
        .unwrap();
        insert_dark_set(
            &conn,
            VALID_SET_ID,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );

        let frame = light_frame_for_dark();
        let config = CalibrationMatchingConfig::default();
        // Pin the premise: gain is Exact + required, so the NULL-gain set is
        // genuinely incomparable rather than merely mismatched.
        let gain_cfg = &config.get_type_config("lights", "dark").unwrap().gain;
        assert_eq!(gain_cfg.mode, MatchMode::Exact);
        assert!(
            gain_cfg.required,
            "test premise: gain must be Exact + required"
        );

        // Auto-link: incomparable set must NOT be offered.
        let auto = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::OnlyCompatible,
        )
        .unwrap();
        let auto_ids: Vec<i64> = auto.iter().map(|c| c.set_id).collect();
        assert!(
            !auto_ids.contains(&NULL_GAIN_SET_ID),
            "auto-link must not offer an incomparable set: got {:?}",
            auto_ids
        );
        assert!(
            auto_ids.contains(&VALID_SET_ID),
            "valid set still auto-links: got {:?}",
            auto_ids
        );

        // Show All: incomparable set must be visible, flagged incompatible.
        let manual = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::IncludeIncompatible,
        )
        .unwrap();
        let incomparable = manual
            .iter()
            .find(|c| c.set_id == NULL_GAIN_SET_ID)
            .expect("Show All must surface the incomparable set so the user can pick it by hand");
        assert!(
            !incomparable.passed_hard_filter,
            "incomparable set must be flagged incompatible, not presented as a match"
        );
        assert_eq!(
            incomparable.match_score, 0.0,
            "incomparable set must score 0 like every other hard-filter reject"
        );
        // The un-comparable parameter is rendered honestly: frame has a value,
        // the set does not, and it did not match.
        assert!(!incomparable.details.gain.matched);
        assert!(incomparable.details.gain.set_value.is_none());
    }

    #[test]
    fn engine_no_preference_includes_masters_for_auto_link() {
        // One raw Dark set + one MasterDark set, both parameter-compatible with
        // the frame. Under NoPreference the master must APPEAR in
        // OnlyCompatible results — `master_preferences` orders, never filters.
        use crate::calibration::config::{BehavioralOptions, ParameterConfig};

        const RAW_SET_ID: i64 = 1;
        const MASTER_SET_ID: i64 = 2;

        let conn = engine_test_db();
        insert_dark_set(
            &conn,
            RAW_SET_ID,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        insert_dark_set(
            &conn,
            MASTER_SET_ID,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            1,
            "MasterDark",
        );

        let frame = light_frame_for_dark();
        let mut config = CalibrationMatchingConfig::default();
        // Force NoPreference for darks
        config
            .master_preferences
            .insert("dark".to_string(), MasterPreference::NoPreference);
        // Make sure the darks→bias optional knob doesn't interfere with the test.
        config.behavioral_options.insert(
            "darks".to_string(),
            BehavioralOptions {
                use_bias_for_dark_optimization: false,
                use_bias_if_no_darks: false,
                fallback_chain: Vec::new(),
            },
        );
        // Avoid letting filter Exact-required reject the test setup.
        if let Some(tc) = config.lights.dark.as_mut() {
            tc.filter = ParameterConfig::ignore();
        }

        let ids: Vec<i64> = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::OnlyCompatible,
        )
        .unwrap()
        .into_iter()
        .map(|c| c.set_id)
        .collect();
        assert!(
            ids.contains(&MASTER_SET_ID),
            "master must be an auto-link candidate: got {:?}",
            ids
        );
        assert!(
            ids.contains(&RAW_SET_ID),
            "raw set still competes on score: got {:?}",
            ids
        );

        let all = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::IncludeIncompatible,
        )
        .unwrap();
        let all_ids: Vec<i64> = all.iter().map(|c| c.set_id).collect();
        assert!(
            all_ids.contains(&MASTER_SET_ID),
            "IncludeIncompatible should still surface masters: got {:?}",
            all_ids
        );
    }

    #[test]
    fn engine_prefer_master_orders_master_first_for_auto_link() {
        // The shipped default is PreferMaster: with a raw and a master set of
        // equal score, auto-link must see the master at the head of the list
        // (and the raw set must still be there as a fallback).
        use crate::calibration::config::{BehavioralOptions, ParameterConfig};

        const RAW_SET_ID: i64 = 1;
        const MASTER_SET_ID: i64 = 2;

        let conn = engine_test_db();
        insert_dark_set(
            &conn,
            RAW_SET_ID,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            0,
            "Dark",
        );
        insert_dark_set(
            &conn,
            MASTER_SET_ID,
            "ASI2600MM",
            "1x1",
            56.0,
            50.0,
            300.0,
            -10.0,
            "2025-09-25T00:00:00+00:00",
            1,
            "MasterDark",
        );

        let frame = light_frame_for_dark();
        // No master_preferences override — this pins the SHIPPED default.
        let mut config = CalibrationMatchingConfig::default();
        config.behavioral_options.insert(
            "darks".to_string(),
            BehavioralOptions {
                use_bias_for_dark_optimization: false,
                use_bias_if_no_darks: false,
                fallback_chain: Vec::new(),
            },
        );
        if let Some(tc) = config.lights.dark.as_mut() {
            tc.filter = ParameterConfig::ignore();
        }

        let ids: Vec<i64> = find_calibration_candidates(
            &conn,
            &frame,
            "lights",
            "dark",
            &config,
            CandidateMode::OnlyCompatible,
        )
        .unwrap()
        .into_iter()
        .map(|c| c.set_id)
        .collect();
        assert_eq!(
            ids,
            vec![MASTER_SET_ID, RAW_SET_ID],
            "default preference must order the master first without dropping the raw set"
        );
    }
}
