/// Flat Calibration Matching
///
/// This module provides pattern-based matching of flat groups to light frames.
/// Unlike Darks/Bias (which use stable sets), flats are matched dynamically
/// based on date proximity and user's imaging pattern.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::models::Frame;
use super::flat_groups::{FlatGroup, detect_flat_groups};
use super::configurable_matcher::load_config;

/// Represents a flat group with match score and metadata
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct FlatGroupMatch {
    /// The flat group
    pub group: FlatGroup,

    /// Match score (0.0-1.0), higher is better
    pub match_score: f64,

    /// Days between light frame and flat group
    pub age_days: i64,

    /// Temperature difference (if available)
    pub temp_diff: Option<f64>,

    /// Relative timing to session
    pub timing: FlatTiming,
}

/// Timing relationship between flat group and light frame/session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
pub enum FlatTiming {
    /// Flat group taken before the light frame
    Before,
    /// Flat group taken after the light frame
    After,
    /// Flat group taken during the session (between first/last light)
    During,
}

/// User's flat-taking pattern preference
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FlatPattern {
    /// Automatic: Find flat group nearest in time to each light frame (default)
    Automatic,
    /// Long-term: Prefer oldest valid flat group (for stable reuse over weeks/months)
    LongTerm,
    /// Manual: User selects specific flat set per filter
    Manual,
}

impl FlatPattern {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "automatic" => Some(Self::Automatic),
            "long_term" => Some(Self::LongTerm),
            "manual" => Some(Self::Manual),
            // MIGRATION: Map old patterns to Automatic
            "before_session" | "after_session" | "before_filter_change" => {
                Some(Self::Automatic)
            }
            _ => None,
        }
    }

    /// Convert to string representation
    #[allow(dead_code)] // Used in tests
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::LongTerm => "long_term",
            Self::Manual => "manual",
        }
    }
}

/// Finds flat groups matching a light frame's parameters
///
/// # Arguments
/// * `conn` - Database connection
/// * `light_frame` - The light frame to find flats for
/// * `max_age_days` - Maximum age of flats to consider (from settings)
/// * `time_cluster_minutes` - Time threshold for grouping flats
/// * `temp_weight` - Weight for temperature matching (0.0-1.0)
///
/// # Returns
/// Vector of FlatGroupMatch, sorted by match score (best first)
pub fn find_flat_groups_for_light_frame(
    conn: &Connection,
    light_frame: &Frame,
    max_age_days: i64,
    time_cluster_minutes: i64,
    temp_weight: f64,
) -> Result<Vec<FlatGroupMatch>> {
    // Extract parameters from light frame
    let instrume = light_frame.instrume.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Light frame missing instrume"))?;
    let filter = light_frame.filter.as_deref();
    let binning = light_frame.binning.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Light frame missing binning"))?;
    let gain = light_frame.gain;
    let focal_length = light_frame.focallen;
    // A3: don't hard-fail on a missing DATE-OBS. Legacy FITS, hand-edited
    // headers, or archive→restore round-trips can leave one light without a
    // date — that shouldn't block calibration for the rest of the frame set.
    // When the date is unknown, search across all candidate flats (no date
    // window) and skip date-based scoring/timing later.
    let frame_date_opt: Option<DateTime<Utc>> = light_frame.date_obs;

    tracing::debug!(
        frame_id = light_frame.id.unwrap_or(-1),
        instrume,
        filter = ?filter,
        binning,
        gain = ?gain,
        focal_length = ?focal_length,
        date = ?frame_date_opt,
        max_age_days,
        "finding flats for light frame"
    );

    // A7: warn when the light is "filter-ambiguous mono" — no Bayer pattern
    // (i.e., a mono sensor) AND a missing FILTER keyword. Auto-link can't tell
    // which physical filter the frame came through; results may include flats
    // shot through different filters, all stored with FILTER=NULL. Logged
    // here so it shows up in stderr / app logs; behavior is unchanged so a
    // pure-mono single-filter setup doesn't silently regress.
    if crate::models::is_mono_with_ambiguous_filter(&light_frame.bayerpat, &light_frame.filter) {
        tracing::warn!(
            frame_id = light_frame.id.unwrap_or(-1),
            "filter-ambiguous mono frame: bayerpat and filter both NULL, auto-link will match any flat with FILTER=NULL, verify manually"
        );
    }

    // Calculate date range for search (±max_age_days from light frame).
    // When the light has no date_obs, omit the date filter entirely so we
    // can still surface candidates the user may have manually linked.
    let date_range = frame_date_opt.map(|d| (
        d - chrono::Duration::days(max_age_days),
        d + chrono::Duration::days(max_age_days),
    ));

    // Load config to get focallen threshold
    let config = load_config(conn);
    let focallen_threshold = config.lights.flat
        .as_ref()
        .and_then(|f| f.focallen.matching_threshold);

    tracing::debug!(focallen_threshold = ?focallen_threshold, "focallen_threshold from config");

    // Detect flat groups matching parameters
    let flat_groups = detect_flat_groups(
        conn,
        instrume,
        filter,
        binning,
        gain,
        focal_length,
        focallen_threshold,
        time_cluster_minutes,
        date_range,
    ).map_err(|e| {
        tracing::error!(
            frame_id = light_frame.id.unwrap_or(-1),
            instrume,
            filter = ?filter,
            binning,
            gain = ?gain,
            focal_length = ?focal_length,
            date_start = ?date_range.map(|(s, _)| s.to_rfc3339()),
            date_end = ?date_range.map(|(_, e)| e.to_rfc3339()),
            error = %e,
            "detect_flat_groups failed"
        );
        e
    })?;

    tracing::debug!(count = flat_groups.len(), "detect_flat_groups found groups");

    // Calculate match scores for each group. When the light frame has no
    // date_obs, we can't compute proximity — fall back to a neutral date
    // score (0.5) and an explicit `FlatTiming::During` so downstream pattern
    // selection (Automatic / LongTerm) still has something to compare.
    let mut matches: Vec<FlatGroupMatch> = flat_groups
        .into_iter()
        .map(|group| {
            let group_midpoint = group.start_time + (group.end_time - group.start_time) / 2;
            let (age_days, timing, date_score) = match frame_date_opt {
                Some(frame_date) => {
                    let age = (frame_date - group_midpoint).num_days().abs();
                    let timing = if group.end_time < frame_date {
                        FlatTiming::Before
                    } else if group.start_time > frame_date {
                        FlatTiming::After
                    } else {
                        FlatTiming::During
                    };
                    let max_days = max_age_days as f64;
                    let score = 1.0 - (age as f64 / max_days).min(1.0);
                    (age, timing, score)
                }
                None => (0, FlatTiming::During, 0.5),
            };

            // Calculate temperature difference (if available)
            let temp_diff = if let (Some(frame_temp), Some(group_temp)) =
                (light_frame.ccd_temp, group.avg_temp)
            {
                Some((frame_temp - group_temp).abs())
            } else {
                None
            };

            // Temperature score (if available)
            let temp_score = if let Some(diff) = temp_diff {
                // Perfect match = 1.0, 10°C difference = 0.0
                1.0 - (diff / 10.0).min(1.0)
            } else {
                0.5 // Neutral score if temp not available
            };

            // Weighted combination
            let match_score = (date_score * (1.0 - temp_weight)) + (temp_score * temp_weight);

            FlatGroupMatch {
                group,
                match_score,
                age_days,
                temp_diff,
                timing,
            }
        })
        .collect();

    // Sort by match score (best first)
    matches.sort_by(|a, b| {
        b.match_score
            .partial_cmp(&a.match_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(matches)
}

/// Applies pattern-based selection to choose the best flat group
///
/// # Arguments
/// * `matches` - Available flat group matches (should be sorted by score)
/// * `pattern` - User's imaging pattern
/// * `light_frame_date` - Date of the light frame (needed for Automatic pattern)
///
/// # Returns
/// The selected FlatGroupMatch, or None if no suitable match
pub fn apply_pattern_selection(
    matches: Vec<FlatGroupMatch>,
    pattern: &FlatPattern,
    light_frame_date: Option<DateTime<Utc>>,
) -> Option<FlatGroupMatch> {
    if matches.is_empty() {
        return None;
    }

    match pattern {
        FlatPattern::Automatic => {
            // Find flat group nearest in time to the light frame.
            //
            // B6: when two groups are equidistant, prefer FlatTiming::After
            // deterministically — astrophotographers conventionally shoot
            // flats AFTER the imaging session (dawn flats / panel flats once
            // the rig is parked). The post-session flat captures the actual
            // optical state the lights were taken under (dust drift, dewing
            // accumulation during the session). The previous code relied on
            // `min_by_key` returning the first encountered on ties, which
            // depended on the input vec's score-sort order — unstable across
            // re-runs as scores shift by tiny amounts.
            if let Some(frame_date) = light_frame_date {
                // Composite key: (time-distance ascending, timing priority).
                // Timing priority: During (0) < After (1) < Before (2).
                // The `min_by_key` picks the smallest tuple, so equidistant
                // After beats equidistant Before.
                fn timing_priority(t: &FlatTiming) -> u8 {
                    match t {
                        FlatTiming::During => 0,
                        FlatTiming::After => 1,
                        FlatTiming::Before => 2,
                    }
                }
                matches.into_iter().min_by_key(|m| {
                    let duration = m.group.end_time - m.group.start_time;
                    let flat_midpoint = m.group.start_time + duration / 2;
                    let abs_distance = (frame_date - flat_midpoint).num_seconds().abs();
                    (abs_distance, timing_priority(&m.timing))
                })
            } else {
                // Fallback: return best scored match (first in sorted list)
                matches.into_iter().next()
            }
        }

        FlatPattern::LongTerm => {
            // Prefer oldest valid flat group (for stable/long-term reuse)
            matches.into_iter()
                .min_by_key(|m| m.group.start_time)
        }

        FlatPattern::Manual => {
            // Return best scored match - user will override via ManualFlatSelectionModal
            matches.into_iter().next()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::flat_groups::FlatGroup;

    // Helper to create a FlatGroupMatch for testing
    fn make_match(
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        match_score: f64,
        timing: FlatTiming,
    ) -> FlatGroupMatch {
        FlatGroupMatch {
            group: FlatGroup {
                frame_ids: vec![1, 2, 3],
                start_time,
                end_time,
                avg_temp: Some(-10.0),
                temp_min: Some(-10.0),
                temp_max: Some(-10.0),
                frame_count: 3,
                filter: Some("L".to_string()),
                instrume: Some("ASI2600MM".to_string()),
                binning: Some("1x1".to_string()),
                gain: Some(100.0),
                offset: Some(10.0),
                exptime: Some(1.0),
                focal_length: Some(600.0),
            },
            match_score,
            age_days: 1,
            temp_diff: Some(0.5),
            timing,
        }
    }

    // ========== FlatPattern::from_str tests ==========

    #[test]
    fn test_flat_pattern_from_str_automatic() {
        assert_eq!(
            FlatPattern::from_str("automatic"),
            Some(FlatPattern::Automatic)
        );
    }

    #[test]
    fn test_flat_pattern_from_str_long_term() {
        assert_eq!(
            FlatPattern::from_str("long_term"),
            Some(FlatPattern::LongTerm)
        );
    }

    #[test]
    fn test_flat_pattern_from_str_manual() {
        assert_eq!(
            FlatPattern::from_str("manual"),
            Some(FlatPattern::Manual)
        );
    }

    #[test]
    fn test_flat_pattern_from_str_invalid() {
        assert_eq!(
            FlatPattern::from_str("invalid"),
            None
        );
        assert_eq!(
            FlatPattern::from_str(""),
            None
        );
    }

    // ========== Migration from old pattern values ==========

    #[test]
    fn test_flat_pattern_migration_before_session() {
        // Old "before_session" should migrate to Automatic
        assert_eq!(
            FlatPattern::from_str("before_session"),
            Some(FlatPattern::Automatic)
        );
    }

    #[test]
    fn test_flat_pattern_migration_after_session() {
        // Old "after_session" should migrate to Automatic
        assert_eq!(
            FlatPattern::from_str("after_session"),
            Some(FlatPattern::Automatic)
        );
    }

    #[test]
    fn test_flat_pattern_migration_before_filter_change() {
        // Old "before_filter_change" should migrate to Automatic
        assert_eq!(
            FlatPattern::from_str("before_filter_change"),
            Some(FlatPattern::Automatic)
        );
    }

    // ========== FlatPattern::as_str tests ==========

    #[test]
    fn test_flat_pattern_as_str() {
        assert_eq!(FlatPattern::Automatic.as_str(), "automatic");
        assert_eq!(FlatPattern::LongTerm.as_str(), "long_term");
        assert_eq!(FlatPattern::Manual.as_str(), "manual");
    }

    // ========== apply_pattern_selection tests ==========

    #[test]
    fn test_apply_pattern_empty_matches_returns_none() {
        let matches: Vec<FlatGroupMatch> = vec![];
        let result = apply_pattern_selection(matches, &FlatPattern::Automatic, None);
        assert!(result.is_none());
    }

    #[test]
    fn is_mono_with_ambiguous_filter_heuristic() {
        // A7: helper that drives the warning logged by find_flat_groups_*.
        use crate::models::is_mono_with_ambiguous_filter;

        // Mono (no Bayer) + no filter = ambiguous.
        assert!(is_mono_with_ambiguous_filter(&None, &None));
        // Mono with a real filter label = not ambiguous.
        assert!(!is_mono_with_ambiguous_filter(&None, &Some("L".to_string())));
        // OSC (Bayer present) with no filter = NOT ambiguous (normal).
        assert!(!is_mono_with_ambiguous_filter(&Some("RGGB".to_string()), &None));
        // OSC with a filter label = not ambiguous.
        assert!(!is_mono_with_ambiguous_filter(&Some("RGGB".to_string()), &Some("L".to_string())));
    }

    #[test]
    fn find_flat_groups_for_light_with_no_date_obs_returns_ok() {
        // A3 regression: a light frame missing DATE-OBS used to hard-fail the
        // entire flat search (the function returned Err and aborted the
        // per-frame loop in process_frame_set). After A3 it must return Ok —
        // the engine's existing handling for missing dates kicks in (no date
        // window, neutral date scoring) so the rest of the frame set still
        // calibrates.
        use crate::db::schema::init_db;
        use crate::models::{Frame, ImageType};
        use rusqlite::Connection;

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let light = Frame {
            id: Some(1),
            file_id: 1,
            object: None,
            date_obs: None, // ← the case under test
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
        };

        let result = find_flat_groups_for_light_frame(&conn, &light, 30, 30, 0.5);
        assert!(
            result.is_ok(),
            "missing date_obs must NOT abort the search, got Err: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_automatic_selects_nearest_flat_group() {
        // Light frame taken at 20:00
        let light_date = DateTime::parse_from_rfc3339("2025-01-15T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Flat group A: 18:00-18:30 (midpoint 18:15, 1h45m before light)
        let match_a = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T18:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T18:30:00Z").unwrap().with_timezone(&Utc),
            0.9,
            FlatTiming::Before,
        );

        // Flat group B: 21:00-21:30 (midpoint 21:15, 1h15m after light) - CLOSER
        let match_b = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T21:30:00Z").unwrap().with_timezone(&Utc),
            0.8,
            FlatTiming::After,
        );

        let matches = vec![match_a.clone(), match_b.clone()];
        let result = apply_pattern_selection(matches, &FlatPattern::Automatic, Some(light_date));

        assert!(result.is_some());
        // Should select match_b because it's closer in time (1h15m vs 1h45m)
        let selected = result.unwrap();
        assert_eq!(selected.timing, FlatTiming::After);
    }

    #[test]
    fn test_automatic_prefers_after_when_equidistant() {
        // B6: equidistant flats must deterministically prefer FlatTiming::After
        // — astrophotographers shoot flats after the imaging session, so the
        // After flat reflects the actual optical state of the night's lights.
        let light_date = DateTime::parse_from_rfc3339("2025-01-15T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Flat group A: 19:00-19:00 (exactly 1h before)
        let match_a = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T19:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T19:00:00Z").unwrap().with_timezone(&Utc),
            0.9,
            FlatTiming::Before,
        );

        // Flat group B: 21:00-21:00 (exactly 1h after)
        let match_b = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            0.8,
            FlatTiming::After,
        );

        let matches = vec![match_a.clone(), match_b.clone()];
        let result = apply_pattern_selection(matches, &FlatPattern::Automatic, Some(light_date));

        assert!(result.is_some());
        assert_eq!(result.unwrap().timing, FlatTiming::After,
            "equidistant tie must pick the post-session flat");
    }

    #[test]
    fn test_automatic_prefers_after_independent_of_input_order() {
        // B6 invariant: the prefer-After rule must hold regardless of the
        // order in which the matches arrive. The previous code passed the
        // sister test only because the input vec happened to be [Before,
        // After] and `min_by_key` returned the first-encountered on ties.
        // With the composite-key tie-break the Before-first input also picks
        // After, and so does the After-first input.
        let light_date = DateTime::parse_from_rfc3339("2025-01-15T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let match_before = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T19:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T19:00:00Z").unwrap().with_timezone(&Utc),
            0.9,
            FlatTiming::Before,
        );
        let match_after = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            0.8,
            FlatTiming::After,
        );

        // Before-first
        let r1 = apply_pattern_selection(
            vec![match_before.clone(), match_after.clone()],
            &FlatPattern::Automatic, Some(light_date));
        assert_eq!(r1.unwrap().timing, FlatTiming::After);

        // After-first
        let r2 = apply_pattern_selection(
            vec![match_after.clone(), match_before.clone()],
            &FlatPattern::Automatic, Some(light_date));
        assert_eq!(r2.unwrap().timing, FlatTiming::After);
    }

    #[test]
    fn test_automatic_fallback_without_date() {
        // No light frame date provided
        let match_a = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T18:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T18:30:00Z").unwrap().with_timezone(&Utc),
            0.9, // Higher score
            FlatTiming::Before,
        );

        let match_b = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T21:30:00Z").unwrap().with_timezone(&Utc),
            0.8,
            FlatTiming::After,
        );

        let matches = vec![match_a.clone(), match_b.clone()];
        let result = apply_pattern_selection(matches, &FlatPattern::Automatic, None);

        assert!(result.is_some());
        // Should fall back to first match (best scored, as list is pre-sorted)
        assert_eq!(result.unwrap().match_score, 0.9);
    }

    #[test]
    fn test_long_term_selects_oldest_flat_group() {
        // Flat group A: January 10 (older)
        let match_a = make_match(
            DateTime::parse_from_rfc3339("2025-01-10T18:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-10T18:30:00Z").unwrap().with_timezone(&Utc),
            0.7, // Lower score but older
            FlatTiming::Before,
        );

        // Flat group B: January 15 (newer, higher score)
        let match_b = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T21:30:00Z").unwrap().with_timezone(&Utc),
            0.95,
            FlatTiming::After,
        );

        let light_date = DateTime::parse_from_rfc3339("2025-01-15T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let matches = vec![match_b.clone(), match_a.clone()]; // B first (higher score)
        let result = apply_pattern_selection(matches, &FlatPattern::LongTerm, Some(light_date));

        assert!(result.is_some());
        // Should select match_a because it's oldest (Jan 10 vs Jan 15)
        let selected = result.unwrap();
        assert_eq!(selected.match_score, 0.7);
    }

    #[test]
    fn test_manual_returns_best_scored_match() {
        let match_a = make_match(
            DateTime::parse_from_rfc3339("2025-01-10T18:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-10T18:30:00Z").unwrap().with_timezone(&Utc),
            0.95, // Highest score - first in list
            FlatTiming::Before,
        );

        let match_b = make_match(
            DateTime::parse_from_rfc3339("2025-01-15T21:00:00Z").unwrap().with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2025-01-15T21:30:00Z").unwrap().with_timezone(&Utc),
            0.8,
            FlatTiming::After,
        );

        // Matches are pre-sorted by score (best first)
        let matches = vec![match_a.clone(), match_b.clone()];
        let result = apply_pattern_selection(matches, &FlatPattern::Manual, None);

        assert!(result.is_some());
        // Should return first match (best scored)
        assert_eq!(result.unwrap().match_score, 0.95);
    }

    // ========== FlatTiming tests ==========

    #[test]
    fn test_flat_timing_equality() {
        assert_eq!(FlatTiming::Before, FlatTiming::Before);
        assert_eq!(FlatTiming::After, FlatTiming::After);
        assert_eq!(FlatTiming::During, FlatTiming::During);
        assert_ne!(FlatTiming::Before, FlatTiming::After);
        assert_ne!(FlatTiming::Before, FlatTiming::During);
        assert_ne!(FlatTiming::After, FlatTiming::During);
    }

}
