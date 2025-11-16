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
    /// Flats taken before imaging session starts
    BeforeSession,
    /// Flats taken after imaging session ends
    AfterSession,
    /// Flats taken before each filter change
    BeforeFilterChange,
    /// Long-term flats (reuse for weeks/months)
    LongTerm,
    /// Manual selection by user
    Manual,
}

impl FlatPattern {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "before_session" => Some(Self::BeforeSession),
            "after_session" => Some(Self::AfterSession),
            "before_filter_change" => Some(Self::BeforeFilterChange),
            "long_term" => Some(Self::LongTerm),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    pub fn to_string(&self) -> &str {
        match self {
            Self::BeforeSession => "before_session",
            Self::AfterSession => "after_session",
            Self::BeforeFilterChange => "before_filter_change",
            Self::LongTerm => "long_term",
            Self::Manual => "manual",
        }
    }
}

/// Filter period for detecting filter changes
#[derive(Debug, Clone)]
pub struct FilterPeriod {
    pub filter: Option<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub frame_count: usize,
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

    // Detect flat groups matching parameters
    let flat_groups = detect_flat_groups(
        conn,
        instrume,
        filter,
        binning,
        gain,
        focal_length,
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
                (1.0 - (diff / 10.0).min(1.0))
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
/// * `session_start` - Start time of imaging session (optional)
/// * `session_end` - End time of imaging session (optional)
///
/// # Returns
/// The selected FlatGroupMatch, or None if no suitable match
pub fn apply_pattern_selection(
    matches: Vec<FlatGroupMatch>,
    pattern: &FlatPattern,
    session_start: Option<DateTime<Utc>>,
    session_end: Option<DateTime<Utc>>,
) -> Option<FlatGroupMatch> {
    if matches.is_empty() {
        return None;
    }

    match pattern {
        FlatPattern::BeforeSession => {
            // Prefer flats taken BEFORE session start
            if let Some(start) = session_start {
                // Find best match taken before session
                let before_matches: Vec<_> = matches
                    .iter()
                    .filter(|m| m.group.end_time < start)
                    .collect();

                if let Some(best) = before_matches.first() {
                    Some((*best).clone())
                } else {
                    // Fallback: use best overall match
                    matches.into_iter().next()
                }
            } else {
                // No session time - use best match with Before timing
                let before_matches: Vec<_> = matches
                    .iter()
                    .filter(|m| m.timing == FlatTiming::Before)
                    .collect();

                if let Some(best) = before_matches.first() {
                    Some((*best).clone())
                } else {
                    matches.into_iter().next()
                }
            }
        }

        FlatPattern::AfterSession => {
            // Prefer flats taken AFTER session end
            if let Some(end) = session_end {
                let after_matches: Vec<_> = matches
                    .iter()
                    .filter(|m| m.group.start_time > end)
                    .collect();

                if let Some(best) = after_matches.first() {
                    Some((*best).clone())
                } else {
                    matches.into_iter().next()
                }
            } else {
                let after_matches: Vec<_> = matches
                    .iter()
                    .filter(|m| m.timing == FlatTiming::After)
                    .collect();

                if let Some(best) = after_matches.first() {
                    Some((*best).clone())
                } else {
                    matches.into_iter().next()
                }
            }
        }

        FlatPattern::LongTerm => {
            // Prefer oldest valid flat group (stability)
            let mut sorted = matches;
            sorted.sort_by(|a, b| a.group.start_time.cmp(&b.group.start_time));
            sorted.into_iter().next()
        }

        FlatPattern::Manual | FlatPattern::BeforeFilterChange => {
            // For manual and filter change patterns, return best overall match
            // Actual selection will be done by user or filter change detection
            matches.into_iter().next()
        }
    }
}

/// Detects filter change periods in a set of light frames
///
/// # Arguments
/// * `frames` - Light frames in chronological order
///
/// # Returns
/// Vector of FilterPeriod, one for each continuous filter usage
pub fn detect_filter_changes(frames: &[Frame]) -> Vec<FilterPeriod> {
    if frames.is_empty() {
        return Vec::new();
    }

    let mut periods = Vec::new();
    let mut current_filter: Option<String> = None;
    let mut period_start: Option<DateTime<Utc>> = None;
    let mut period_frames = 0;

    for frame in frames {
        let frame_filter = frame.filter.clone();
        let frame_date = match frame.date_obs {
            Some(d) => d,
            None => continue, // Skip frames without date
        };

        if current_filter != frame_filter {
            // Filter changed - close previous period
            if let Some(start) = period_start {
                periods.push(FilterPeriod {
                    filter: current_filter.clone(),
                    start_time: start,
                    end_time: frame_date,
                    frame_count: period_frames,
                });
            }

            // Start new period
            current_filter = frame_filter;
            period_start = Some(frame_date);
            period_frames = 1;
        } else {
            period_frames += 1;
        }
    }

    // Close final period
    if let Some(start) = period_start {
        let end_time = frames
            .last()
            .and_then(|f| f.date_obs)
            .unwrap_or(start);

        periods.push(FilterPeriod {
            filter: current_filter,
            start_time: start,
            end_time,
            frame_count: period_frames,
        });
    }

    periods
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flat_pattern_from_str() {
        assert_eq!(
            FlatPattern::from_str("before_session"),
            Some(FlatPattern::BeforeSession)
        );
        assert_eq!(
            FlatPattern::from_str("after_session"),
            Some(FlatPattern::AfterSession)
        );
        assert_eq!(
            FlatPattern::from_str("invalid"),
            None
        );
    }

    #[test]
    fn test_flat_timing() {
        assert_eq!(FlatTiming::Before, FlatTiming::Before);
        assert_ne!(FlatTiming::Before, FlatTiming::After);
    }
}
