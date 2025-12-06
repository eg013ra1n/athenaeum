// Calibration finder - matches calibration sets to light frames
use crate::models::{CalibrationTolerance, Frame, ImageType};
use rusqlite::Connection;
use chrono::{DateTime, Utc};
use anyhow::Result;

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
}

/// Check if two optional values match exactly
#[allow(dead_code)]
fn matches_exact_optional<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
    match (a, b) {
        (Some(val_a), Some(val_b)) => val_a == val_b,
        (None, None) => true,
        _ => false,
    }
}

/// Check if gain matches exactly (within 0.01 tolerance for floating point)
pub fn matches_gain(frame_gain: Option<f64>, set_gain: Option<f64>) -> bool {
    match (frame_gain, set_gain) {
        (Some(a), Some(b)) => (a - b).abs() < 0.01,
        (None, None) => true,
        _ => false,
    }
}

/// Check if offset matches exactly (within 0.01 tolerance for floating point)
pub fn matches_offset(frame_offset: Option<f64>, set_offset: Option<f64>) -> bool {
    match (frame_offset, set_offset) {
        (Some(a), Some(b)) => (a - b).abs() < 0.01,
        (None, None) => true,
        _ => false,
    }
}

/// Check if exposure time matches exactly (within 0.1s tolerance)
pub fn matches_exptime(frame_exptime: Option<f64>, set_exptime: Option<f64>) -> bool {
    match (frame_exptime, set_exptime) {
        (Some(a), Some(b)) => (a - b).abs() < 0.1,
        (None, None) => true,
        _ => false,
    }
}

/// Check if focal length matches exactly (within 1mm tolerance)
pub fn matches_focallen(frame_focallen: Option<f64>, set_focallen: Option<f64>) -> bool {
    match (frame_focallen, set_focallen) {
        (Some(a), Some(b)) => (a - b).abs() < 1.0,
        (None, None) => true,
        _ => false,
    }
}

/// Check if parameters match for Flat calibration
/// Required exact matches: gain, offset, binning, filter, focallen, instrume
pub fn matches_flat_parameters(
    frame: &Frame,
    set_gain: Option<f64>,
    set_offset: Option<f64>,
    set_binning: &Option<String>,
    set_filter: &Option<String>,
    set_focallen: Option<f64>,
    set_instrume: &Option<String>,
) -> bool {
    matches_gain(frame.gain, set_gain)
        && matches_offset(frame.offset, set_offset)
        && matches_exact_optional(&frame.binning, set_binning)
        && matches_exact_optional(&frame.filter, set_filter)
        && matches_focallen(frame.focallen, set_focallen)
        && matches_exact_optional(&frame.instrume, set_instrume)
}

/// Check if parameters match for Dark calibration
/// Required exact matches: gain, offset, binning, instrume, exptime
pub fn matches_dark_parameters(
    frame: &Frame,
    set_gain: Option<f64>,
    set_offset: Option<f64>,
    set_binning: &Option<String>,
    set_instrume: &Option<String>,
    set_exptime: Option<f64>,
) -> bool {
    matches_gain(frame.gain, set_gain)
        && matches_offset(frame.offset, set_offset)
        && matches_exact_optional(&frame.binning, set_binning)
        && matches_exact_optional(&frame.instrume, set_instrume)
        && matches_exptime(frame.exptime, set_exptime)
}

/// Check if parameters match for Bias calibration
/// Required exact matches: gain, offset, binning, instrume
pub fn matches_bias_parameters(
    frame: &Frame,
    set_gain: Option<f64>,
    set_offset: Option<f64>,
    set_binning: &Option<String>,
    set_instrume: &Option<String>,
) -> bool {
    matches_gain(frame.gain, set_gain)
        && matches_offset(frame.offset, set_offset)
        && matches_exact_optional(&frame.binning, set_binning)
        && matches_exact_optional(&frame.instrume, set_instrume)
}

/// Check if temperature is within tolerance
/// Note: temp_threshold is now passed explicitly instead of from CalibrationTolerance
pub fn check_temperature_tolerance(
    frame_temp: Option<f64>,
    set_temp_min: Option<f64>,
    set_temp_max: Option<f64>,
    temp_threshold: f64,
) -> (bool, bool) {
    match (frame_temp, set_temp_min, set_temp_max) {
        (Some(f_temp), Some(min_temp), Some(max_temp)) => {
            let avg_temp = (min_temp + max_temp) / 2.0;
            let diff = (f_temp - avg_temp).abs();
            let within_tolerance = diff <= temp_threshold;
            (within_tolerance, !within_tolerance)  // (matches, warning)
        }
        _ => (true, false),  // No temperature data, allow match without warning
    }
}

/// Calculate date difference in days
pub fn calculate_date_diff_days(
    frame_date: Option<DateTime<Utc>>,
    set_date_start: &str,
    set_date_end: &str,
) -> Option<i64> {
    let frame_dt = frame_date?;

    // Parse set date range
    let set_start = DateTime::parse_from_rfc3339(set_date_start).ok()?;
    let set_end = DateTime::parse_from_rfc3339(set_date_end).ok()?;

    // Calculate difference from the nearest edge of the range
    let diff_from_start = (frame_dt - set_start.with_timezone(&Utc)).num_days().abs();
    let diff_from_end = (frame_dt - set_end.with_timezone(&Utc)).num_days().abs();

    Some(diff_from_start.min(diff_from_end))
}

/// Check if date difference triggers a warning
pub fn check_date_warning(
    date_diff_days: Option<i64>,
    calibration_type: &str,
    tolerance: &CalibrationTolerance,
) -> bool {
    match date_diff_days {
        Some(days) => {
            match calibration_type {
                "Flat" => days > tolerance.flat_date_warning_days,
                "Dark" | "DarkFlat" => days > tolerance.dark_date_warning_days,
                "Bias" => false,  // No date warning for bias
                _ => false,
            }
        }
        None => false,  // No date data, no warning
    }
}

/// Score a calibration match
/// Priority: 1. Nearest date/time, 2. Nearest temperature
/// Returns score between 0.0 (worst) and 1.0 (best)
pub fn score_calibration_match(
    date_diff_days: Option<i64>,
    temp_diff: Option<f64>,
) -> f64 {
    let mut score = 1.0;

    // Date scoring: exponential decay
    // 0 days = 1.0, 30 days = 0.5, 365 days = 0.1
    if let Some(days) = date_diff_days {
        let date_score = 1.0 / (1.0 + (days as f64 / 30.0));
        score *= date_score;
    }

    // Temperature scoring: exponential decay
    // 0°C diff = 1.0, 2°C diff = 0.5, 10°C diff = 0.1
    if let Some(temp) = temp_diff {
        let temp_score = 1.0 / (1.0 + (temp.abs() / 2.0));
        score *= temp_score;
    }

    score.max(0.0).min(1.0)
}

/// Find flat calibration sets for a light frame
pub fn find_flat_sets_for_light_frame(
    conn: &Connection,
    frame: &Frame,
    tolerance: &CalibrationTolerance,
) -> Result<Vec<CalibrationCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, gain, offset, binning, filter, focallen, instrume,
                ccd_temp, temp_min, temp_max, date_start, date_end
         FROM calibration_set
         WHERE imagetyp = 'Flat'
         ORDER BY date_start DESC"
    )?;

    let mut candidates = Vec::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,          // id
            row.get::<_, Option<f64>>(1)?,  // gain
            row.get::<_, Option<f64>>(2)?,  // offset
            row.get::<_, Option<String>>(3)?,  // binning
            row.get::<_, Option<String>>(4)?,  // filter
            row.get::<_, Option<f64>>(5)?,  // focallen
            row.get::<_, Option<String>>(6)?,  // instrume
            row.get::<_, Option<f64>>(7)?,  // ccd_temp
            row.get::<_, Option<f64>>(8)?,  // temp_min
            row.get::<_, Option<f64>>(9)?,  // temp_max
            row.get::<_, String>(10)?,      // date_start
            row.get::<_, String>(11)?,      // date_end
        ))
    })?;

    for row_result in rows {
        let (set_id, gain, offset, binning, filter, focallen, instrume,
             _ccd_temp, temp_min, temp_max, date_start, date_end) = row_result?;

        // Check exact parameter matches
        if !matches_flat_parameters(frame, gain, offset, &binning, &filter, focallen, &instrume) {
            continue;
        }

        // Check temperature tolerance
        let (temp_match, temp_warning) = check_temperature_tolerance(
            frame.ccd_temp,
            temp_min,
            temp_max,
            2.0,  // Default threshold (this code path is deprecated)
        );

        if !temp_match {
            continue;  // Skip if temperature doesn't match
        }

        // Calculate date difference
        let date_diff = calculate_date_diff_days(frame.date_obs, &date_start, &date_end);
        let date_warning = check_date_warning(date_diff, "Flat", tolerance);

        // Calculate temperature difference for scoring
        let temp_diff = match (frame.ccd_temp, temp_min, temp_max) {
            (Some(f_temp), Some(min), Some(max)) => {
                let avg = (min + max) / 2.0;
                Some((f_temp - avg).abs())
            }
            _ => None,
        };

        // Score the match
        let score = score_calibration_match(date_diff, temp_diff);

        candidates.push(CalibrationCandidate {
            set_id,
            imagetyp: ImageType::Flat,
            match_score: score,
            date_diff_days: date_diff.unwrap_or(0),
            temp_diff,
            date_warning,
            temp_warning,
        });
    }

    Ok(candidates)
}

/// Find dark calibration sets for a light frame
pub fn find_dark_sets_for_light_frame(
    conn: &Connection,
    frame: &Frame,
    tolerance: &CalibrationTolerance,
) -> Result<Vec<CalibrationCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, gain, offset, binning, instrume, exptime,
                ccd_temp, temp_min, temp_max, date_start, date_end
         FROM calibration_set
         WHERE imagetyp = 'Dark'
         ORDER BY date_start DESC"
    )?;

    let mut candidates = Vec::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,          // id
            row.get::<_, Option<f64>>(1)?,  // gain
            row.get::<_, Option<f64>>(2)?,  // offset
            row.get::<_, Option<String>>(3)?,  // binning
            row.get::<_, Option<String>>(4)?,  // instrume
            row.get::<_, Option<f64>>(5)?,  // exptime
            row.get::<_, Option<f64>>(6)?,  // ccd_temp
            row.get::<_, Option<f64>>(7)?,  // temp_min
            row.get::<_, Option<f64>>(8)?,  // temp_max
            row.get::<_, String>(9)?,       // date_start
            row.get::<_, String>(10)?,      // date_end
        ))
    })?;

    for row_result in rows {
        let (set_id, gain, offset, binning, instrume, exptime,
             _ccd_temp, temp_min, temp_max, date_start, date_end) = row_result?;

        // Check exact parameter matches
        if !matches_dark_parameters(frame, gain, offset, &binning, &instrume, exptime) {
            continue;
        }

        // Check temperature tolerance
        let (temp_match, temp_warning) = check_temperature_tolerance(
            frame.ccd_temp,
            temp_min,
            temp_max,
            2.0,  // Default threshold (this code path is deprecated)
        );

        if !temp_match {
            continue;
        }

        // Calculate date difference
        let date_diff = calculate_date_diff_days(frame.date_obs, &date_start, &date_end);
        let date_warning = check_date_warning(date_diff, "Dark", tolerance);

        // Calculate temperature difference for scoring
        let temp_diff = match (frame.ccd_temp, temp_min, temp_max) {
            (Some(f_temp), Some(min), Some(max)) => {
                let avg = (min + max) / 2.0;
                Some((f_temp - avg).abs())
            }
            _ => None,
        };

        // Score the match
        let score = score_calibration_match(date_diff, temp_diff);

        candidates.push(CalibrationCandidate {
            set_id,
            imagetyp: ImageType::Dark,
            match_score: score,
            date_diff_days: date_diff.unwrap_or(0),
            temp_diff,
            date_warning,
            temp_warning,
        });
    }

    Ok(candidates)
}

/// Find bias calibration sets for a frame
pub fn find_bias_sets_for_frame(
    conn: &Connection,
    frame: &Frame,
    tolerance: &CalibrationTolerance,
) -> Result<Vec<CalibrationCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, gain, offset, binning, instrume,
                ccd_temp, temp_min, temp_max, date_start, date_end
         FROM calibration_set
         WHERE imagetyp = 'Bias'
         ORDER BY date_start DESC"
    )?;

    let mut candidates = Vec::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,          // id
            row.get::<_, Option<f64>>(1)?,  // gain
            row.get::<_, Option<f64>>(2)?,  // offset
            row.get::<_, Option<String>>(3)?,  // binning
            row.get::<_, Option<String>>(4)?,  // instrume
            row.get::<_, Option<f64>>(5)?,  // ccd_temp
            row.get::<_, Option<f64>>(6)?,  // temp_min
            row.get::<_, Option<f64>>(7)?,  // temp_max
            row.get::<_, String>(8)?,       // date_start
            row.get::<_, String>(9)?,       // date_end
        ))
    })?;

    for row_result in rows {
        let (set_id, gain, offset, binning, instrume,
             _ccd_temp, temp_min, temp_max, date_start, date_end) = row_result?;

        // Check exact parameter matches
        if !matches_bias_parameters(frame, gain, offset, &binning, &instrume) {
            continue;
        }

        // Check temperature tolerance
        let (temp_match, temp_warning) = check_temperature_tolerance(
            frame.ccd_temp,
            temp_min,
            temp_max,
            2.0,  // Default threshold (this code path is deprecated)
        );

        if !temp_match {
            continue;
        }

        // Calculate date difference
        let date_diff = calculate_date_diff_days(frame.date_obs, &date_start, &date_end);
        let date_warning = check_date_warning(date_diff, "Bias", tolerance);

        // Calculate temperature difference for scoring
        let temp_diff = match (frame.ccd_temp, temp_min, temp_max) {
            (Some(f_temp), Some(min), Some(max)) => {
                let avg = (min + max) / 2.0;
                Some((f_temp - avg).abs())
            }
            _ => None,
        };

        // Score the match
        let score = score_calibration_match(date_diff, temp_diff);

        candidates.push(CalibrationCandidate {
            set_id,
            imagetyp: ImageType::Bias,
            match_score: score,
            date_diff_days: date_diff.unwrap_or(0),
            temp_diff,
            date_warning,
            temp_warning,
        });
    }

    Ok(candidates)
}

/// Rank calibration candidates by score (highest first)
pub fn rank_calibration_candidates(mut candidates: Vec<CalibrationCandidate>) -> Vec<CalibrationCandidate> {
    candidates.sort_by(|a, b| {
        // Sort by score descending (best first)
        b.match_score.partial_cmp(&a.match_score).unwrap_or(std::cmp::Ordering::Equal)
            // Then by date (nearest first)
            .then_with(|| a.date_diff_days.cmp(&b.date_diff_days))
            // Then by temperature (nearest first)
            .then_with(|| {
                match (a.temp_diff, b.temp_diff) {
                    (Some(a_temp), Some(b_temp)) => {
                        a_temp.partial_cmp(&b_temp).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            })
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_gain() {
        assert!(matches_gain(Some(100.0), Some(100.0)));
        assert!(matches_gain(Some(100.0), Some(100.005)));  // Within tolerance
        assert!(!matches_gain(Some(100.0), Some(101.0)));
        assert!(matches_gain(None, None));
        assert!(!matches_gain(Some(100.0), None));
    }

    #[test]
    fn test_matches_offset() {
        assert!(matches_offset(Some(10.0), Some(10.0)));
        assert!(matches_offset(Some(10.0), Some(10.005)));  // Within tolerance
        assert!(!matches_offset(Some(10.0), Some(11.0)));
        assert!(matches_offset(None, None));
        assert!(!matches_offset(Some(10.0), None));
    }

    #[test]
    fn test_matches_exptime() {
        assert!(matches_exptime(Some(300.0), Some(300.0)));
        assert!(matches_exptime(Some(300.0), Some(300.05)));  // Within tolerance
        assert!(!matches_exptime(Some(300.0), Some(301.0)));
    }

    #[test]
    fn test_matches_focallen() {
        assert!(matches_focallen(Some(400.0), Some(400.0)));
        assert!(matches_focallen(Some(400.0), Some(400.5)));  // Within tolerance
        assert!(!matches_focallen(Some(400.0), Some(405.0)));
    }

    #[test]
    fn test_temperature_tolerance() {
        let temp_threshold = 2.0;

        // Within tolerance
        let (matches, warning) = check_temperature_tolerance(
            Some(-10.0),
            Some(-11.0),
            Some(-9.0),
            temp_threshold,
        );
        assert!(matches);
        assert!(!warning);

        // Outside tolerance
        let (matches, warning) = check_temperature_tolerance(
            Some(-10.0),
            Some(-5.0),
            Some(-3.0),
            temp_threshold,
        );
        assert!(!matches);
        assert!(warning);
    }

    #[test]
    fn test_score_calibration_match() {
        // Perfect match
        let score = score_calibration_match(Some(0), Some(0.0));
        assert!(score > 0.99);

        // Good match (1 day, 0.5°C)
        let score = score_calibration_match(Some(1), Some(0.5));
        assert!(score > 0.8);

        // Moderate match (30 days, 2°C)
        let score = score_calibration_match(Some(30), Some(2.0));
        assert!(score > 0.2 && score < 0.6);

        // Poor match (365 days, 10°C)
        let score = score_calibration_match(Some(365), Some(10.0));
        assert!(score < 0.1);
    }

    #[test]
    fn test_date_warning() {
        let tolerance = CalibrationTolerance {
            flat_date_warning_days: 30,
            dark_date_warning_days: 365,
        };

        // Flat with recent date - no warning
        assert!(!check_date_warning(Some(15), "Flat", &tolerance));

        // Flat with old date - warning
        assert!(check_date_warning(Some(45), "Flat", &tolerance));

        // Dark with moderately old date - no warning
        assert!(!check_date_warning(Some(200), "Dark", &tolerance));

        // Dark with very old date - warning
        assert!(check_date_warning(Some(400), "Dark", &tolerance));

        // Bias - no warning regardless of age
        assert!(!check_date_warning(Some(1000), "Bias", &tolerance));
    }

    #[test]
    fn test_ranking() {
        let candidates = vec![
            CalibrationCandidate {
                set_id: 1,
                imagetyp: ImageType::Dark,
                match_score: 0.5,
                date_diff_days: 10,
                temp_diff: Some(1.0),
                date_warning: false,
                temp_warning: false,
            },
            CalibrationCandidate {
                set_id: 2,
                imagetyp: ImageType::Dark,
                match_score: 0.9,
                date_diff_days: 5,
                temp_diff: Some(0.5),
                date_warning: false,
                temp_warning: false,
            },
            CalibrationCandidate {
                set_id: 3,
                imagetyp: ImageType::Dark,
                match_score: 0.7,
                date_diff_days: 7,
                temp_diff: Some(0.8),
                date_warning: false,
                temp_warning: false,
            },
        ];

        let ranked = rank_calibration_candidates(candidates);

        // Should be sorted by score: 0.9, 0.7, 0.5
        assert_eq!(ranked[0].set_id, 2);
        assert_eq!(ranked[1].set_id, 3);
        assert_eq!(ranked[2].set_id, 1);
    }
}
