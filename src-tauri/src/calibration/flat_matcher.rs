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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    let frame_date = light_frame.date_obs
        .ok_or_else(|| anyhow::anyhow!("Light frame missing date_obs"))?;

    // Diagnostic logging
    println!("🔍 Finding flats for light frame ID {}",
        light_frame.id.map(|id| id.to_string()).unwrap_or_else(|| "None".to_string()));
    println!("  📷 instrume: {}", instrume);
    println!("  🎨 filter: {:?}", filter);
    println!("  📐 binning: {}", binning);
    println!("  ⚡ gain: {:?}", gain);
    println!("  🔭 focallen: {:?}", focal_length);
    println!("  📅 date: {}", frame_date);
    println!("  ⏰ max_age_days: {}", max_age_days);

    // Calculate date range for search (±max_age_days from light frame)
    let start_date = frame_date - chrono::Duration::days(max_age_days);
    let end_date = frame_date + chrono::Duration::days(max_age_days);

    // Load config to get focallen threshold
    let config = load_config(conn);
    let focallen_threshold = config.lights.flat
        .as_ref()
        .and_then(|f| f.focallen.matching_threshold);

    println!("  🔧 focallen_threshold from config: {:?}", focallen_threshold);

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
        Some((start_date, end_date)),
    ).map_err(|e| {
        eprintln!("  ❌ detect_flat_groups failed: {}", e);
        eprintln!("  📋 Search params - instrume: {}, filter: {:?}, binning: {}, gain: {:?}, focallen: {:?}",
            instrume, filter, binning, gain, focal_length);
        eprintln!("  📅 Date range: {} to {}", start_date, end_date);
        e
    })?;

    println!("  🎯 detect_flat_groups found {} groups", flat_groups.len());

    // Calculate match scores for each group
    let mut matches: Vec<FlatGroupMatch> = flat_groups
        .into_iter()
        .map(|group| {
            // Calculate age in days (use group's midpoint)
            let group_midpoint = group.start_time + (group.end_time - group.start_time) / 2;
            let age_days = (frame_date - group_midpoint).num_days().abs();

            // Determine timing relationship
            let timing = if group.end_time < frame_date {
                FlatTiming::Before
            } else if group.start_time > frame_date {
                FlatTiming::After
            } else {
                FlatTiming::During
            };

            // Calculate temperature difference (if available)
            let temp_diff = if let (Some(frame_temp), Some(group_temp)) =
                (light_frame.ccd_temp, group.avg_temp)
            {
                Some((frame_temp - group_temp).abs())
            } else {
                None
            };

            // Calculate match score
            // Base score from date proximity (newer = better)
            let max_days = max_age_days as f64;
            let date_score = 1.0 - (age_days as f64 / max_days).min(1.0);

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
            // Find flat group nearest in time to the light frame
            if let Some(frame_date) = light_frame_date {
                matches.into_iter().min_by_key(|m| {
                    // Calculate midpoint of flat group
                    let duration = m.group.end_time - m.group.start_time;
                    let flat_midpoint = m.group.start_time + duration / 2;
                    // Return absolute time difference in seconds
                    (frame_date - flat_midpoint).num_seconds().abs()
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
    fn test_automatic_prefers_before_when_equidistant() {
        // Light frame taken at 20:00
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

        // When equidistant, min_by_key returns first encountered
        let matches = vec![match_a.clone(), match_b.clone()];
        let result = apply_pattern_selection(matches, &FlatPattern::Automatic, Some(light_date));

        assert!(result.is_some());
        // First match in iterator should be returned when equal
        assert_eq!(result.unwrap().timing, FlatTiming::Before);
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
