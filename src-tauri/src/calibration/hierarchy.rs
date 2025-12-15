// Calibration hierarchy builder - constructs complete calibration trees
use crate::calibration::finder::CalibrationCandidate;
use crate::calibration::configurable_matcher::{
    load_config, find_dark_for_light, find_calibration_for_flat, find_calibration_for_dark,
};
use crate::calibration::config::{CalibrationMatchingConfig, MatchMode};
use crate::calibration::flat_matcher::{
    find_flat_groups_for_light_frame, apply_pattern_selection, FlatPattern,
};
use crate::calibration::flat_groups::create_flat_calibration_set;
use crate::calibration::dark_bias_groups::{
    detect_dark_groups, detect_bias_groups,
    create_dark_calibration_set, create_bias_calibration_set,
};
use crate::db::calibration_links::insert_calibration_link;
use crate::models::{
    CalibrationTolerance, Frame, CalibrationLink, CalibrationWarning,
    CalibrationHierarchy, CalibrationSetWithLinks, CalibrationSetDetail, ImageType,
};
use crate::commands::AppState;
use rusqlite::Connection;
use anyhow::{Result, Context};
use chrono::{Utc, DateTime, Duration};
use tauri::State;

// ============================================================================
// Helper functions for checking if warnings are enabled based on config mode
// ============================================================================

/// Check if temperature warnings are enabled for a calibration path
/// Returns true only if the mode is explicitly set to "Warning"
fn is_temp_warning_mode_enabled(config: &CalibrationMatchingConfig, source: &str, cal_type: &str) -> bool {
    match (source, cal_type) {
        ("lights", "flat") => config.lights.flat.as_ref()
            .map(|c| c.ccd_temp.mode == MatchMode::Warning).unwrap_or(false),
        ("lights", "dark") => config.lights.dark.as_ref()
            .map(|c| c.ccd_temp.mode == MatchMode::Warning).unwrap_or(false),
        ("lights", "bias") => config.lights.bias.as_ref()
            .map(|c| c.ccd_temp.mode == MatchMode::Warning).unwrap_or(false),
        ("flats", "darkflat") => config.flats.darkflat.as_ref()
            .map(|c| c.ccd_temp.mode == MatchMode::Warning).unwrap_or(false),
        ("flats", "dark") => config.flats.dark.as_ref()
            .map(|c| c.ccd_temp.mode == MatchMode::Warning).unwrap_or(false),
        ("flats", "bias") => config.flats.bias.as_ref()
            .map(|c| c.ccd_temp.mode == MatchMode::Warning).unwrap_or(false),
        _ => false,
    }
}

/// Check if date warnings should be shown (threshold is reasonable)
fn is_date_warning_enabled(threshold: i64) -> bool {
    threshold > 0 && threshold < 10000
}

/// Get frame metadata by ID
fn get_frame_by_id(conn: &Connection, frame_id: i64) -> Result<Frame> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, object, date_obs, telescop, instrume, exptime, filter,
                gain, offset, binning, xbinning, ybinning, ccd_temp, set_temp,
                focallen, xpixsz, pixsz, naxis1, naxis2, ra, dec, sitelat, lat_obs,
                sitelong, long_obs, objctra, objctdec, override, imagetyp, is_master
         FROM frames
         WHERE id = ?1"
    )?;

    let frame = stmt.query_row([frame_id], |row| {
        let date_obs_str: Option<String> = row.get(3)?;
        let date_obs = date_obs_str.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Frame {
            id: Some(row.get(0)?),
            file_id: row.get(1)?,
            object: row.get(2)?,
            date_obs,
            telescop: row.get(4)?,
            instrume: row.get(5)?,
            exptime: row.get(6)?,
            filter: row.get(7)?,
            gain: row.get(8)?,
            offset: row.get(9)?,
            binning: row.get(10)?,
            xbinning: row.get(11)?,
            ybinning: row.get(12)?,
            ccd_temp: row.get(13)?,
            set_temp: row.get(14)?,
            focallen: row.get(15)?,
            xpixsz: row.get(16)?,
            pixsz: row.get(17)?,
            naxis1: row.get(18)?,
            naxis2: row.get(19)?,
            ra: row.get(20)?,
            dec: row.get(21)?,
            sitelat: row.get(22)?,
            lat_obs: row.get(23)?,
            sitelong: row.get(24)?,
            long_obs: row.get(25)?,
            objctra: row.get(26)?,
            objctdec: row.get(27)?,
            override_: row.get::<_, i32>(28)? != 0,
            imagetyp: {
                let imagetyp_str: Option<String> = row.get(29)?;
                imagetyp_str.and_then(|s| ImageType::from_str(&s))
            },
            is_master: row.get::<_, i32>(30)? != 0,
        })
    })?;

    Ok(frame)
}

/// Get calibration set detail by ID
fn get_calibration_set_by_id(conn: &Connection, set_id: i64) -> Result<CalibrationSetDetail> {
    let mut stmt = conn.prepare(
        "SELECT id, imagetyp, exptime, ccd_temp, temp_min, temp_max, gain, offset,
                binning, instrume, filter, date_start, date_end, date, frame_count
         FROM calibration_set
         WHERE id = ?1"
    )?;

    let set = stmt.query_row([set_id], |row| {
        let imagetyp_str: String = row.get(1)?;
        Ok(CalibrationSetDetail {
            id: Some(row.get(0)?),
            imagetyp: ImageType::from_str(&imagetyp_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
            exptime: row.get(2)?,
            ccd_temp: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            temp_min: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            temp_max: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            gain: row.get(6)?,
            offset: row.get(7)?,
            binning: row.get(8)?,
            instrume: row.get(9)?,
            filter: row.get(10)?,
            date_start: row.get(11)?,
            date_end: row.get(12)?,
            date_display: row.get(13)?,
            frame_count: row.get(14)?,
        })
    })?;

    Ok(set)
}

/// Find Dark or Bias calibration for a Flat calibration set
/// Uses configurable matching rules with fallback chain (DarkFlat → Dark → Bias)
/// Creates sets on-demand if not found
pub fn find_calibration_for_flat_set(
    conn: &Connection,
    flat_set_id: i64,
    _tolerance: &CalibrationTolerance,
    state: &State<'_, AppState>,
) -> Result<Vec<CalibrationCandidate>> {
    // Get a representative frame from the flat set to use for matching
    let mut stmt = conn.prepare(
        "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 LIMIT 1"
    )?;

    let frame_id: i64 = stmt.query_row([flat_set_id], |row| row.get(0))
        .context("Flat set has no frames")?;

    let frame = get_frame_by_id(conn, frame_id)?;

    // Load configurable matching config
    let config = load_config(conn);

    // Use configurable matcher with fallback chain (DarkFlat → Dark → Bias)
    let mut candidates = find_calibration_for_flat(conn, &frame, &config)?;

    if candidates.is_empty() {
        println!("  🔍 No existing calibration sets found for Flat via config matcher, trying on-demand creation...");

        // Try to create Dark on-demand (fallback to old behavior)
        if let Some(created_dark_id) = try_create_dark_for_frame(conn, &frame, state)? {
            println!("  ✅ Created Dark set {} for Flat", created_dark_id);

            // Re-query using configurable matcher
            candidates = find_calibration_for_flat(conn, &frame, &config)?;
        }
    }

    if candidates.is_empty() {
        // Try to create Bias on-demand as last resort
        if let Some(created_bias_id) = try_create_bias_for_frame(conn, &frame, state)? {
            println!("  ✅ Created Bias set {} for Flat", created_bias_id);

            // Re-query using configurable matcher
            candidates = find_calibration_for_flat(conn, &frame, &config)?;
        }
    }

    Ok(candidates)
}

/// Find Bias calibration for a Dark calibration set
/// Only returns Bias if "use_bias_for_dark_optimization" is enabled in config
/// Creates Bias on-demand if not found
pub fn find_calibration_for_dark_set(
    conn: &Connection,
    dark_set_id: i64,
    _tolerance: &CalibrationTolerance,
    state: &State<'_, AppState>,
) -> Result<Vec<CalibrationCandidate>> {
    // Get a representative frame from the dark set
    let mut stmt = conn.prepare(
        "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 LIMIT 1"
    )?;

    let frame_id: i64 = stmt.query_row([dark_set_id], |row| row.get(0))
        .context("Dark set has no frames")?;

    let frame = get_frame_by_id(conn, frame_id)?;

    // Load configurable matching config
    let config = load_config(conn);

    // Use configurable matcher - only returns Bias if enabled in config
    let mut candidates = find_calibration_for_dark(conn, &frame, &config)?;

    if candidates.is_empty() {
        // Check if bias for dark optimization is enabled before trying on-demand creation
        if let Some(opts) = config.get_behavioral_options("darks") {
            if opts.use_bias_for_dark_optimization {
                println!("  🔍 No existing Bias sets found for Dark, attempting on-demand creation...");

                // Try to create Bias on-demand
                if let Some(created_bias_id) = try_create_bias_for_frame(conn, &frame, state)? {
                    println!("  ✅ Created Bias set {} for Dark", created_bias_id);

                    // Re-query using configurable matcher
                    candidates = find_calibration_for_dark(conn, &frame, &config)?;
                }
            }
        }
    }

    Ok(candidates)
}

/// Build complete calibration hierarchy for a light frame
/// Returns hierarchy including all flats, darks, and their sub-calibrations
///
/// # Arguments
/// * `conn` - Database connection
/// * `light_frame` - The light frame to find calibration for
/// * `tolerance` - Tolerance settings for calibration matching
/// * `flat_pattern` - Optional flat pattern preference (e.g., "automatic", "long_term", "manual")
/// * `manual_flat_set_id` - Optional manually selected flat set ID for this specific frame
/// * `max_age_days` - Maximum age of flats to consider (from settings)
/// * `time_cluster_minutes` - Time threshold for grouping flats (from settings)
/// * `temp_weight` - Weight for temperature matching (from settings)
/// * `state` - AppState for accessing settings
pub fn build_complete_hierarchy(
    conn: &Connection,
    light_frame: &Frame,
    tolerance: &CalibrationTolerance,
    flat_pattern: Option<&str>,
    manual_flat_set_id: Option<i64>,
    max_age_days: i64,
    time_cluster_minutes: i64,
    temp_weight: f64,
    state: &State<'_, AppState>,
) -> Result<CalibrationHierarchy> {
    let frame_id = light_frame.id.context("Frame must have an ID")?;

    let mut flat_sets_with_links = Vec::new();
    let mut dark_sets_with_links = Vec::new();
    let mut missing_calibration = Vec::new();
    let mut warnings = Vec::new();

    // Load configurable matching config for threshold checks
    let config = load_config(conn);

    // Find Flat sets for the light frame using new pattern-based system
    let flat_set_id = if let Some(set_id) = manual_flat_set_id {
        // Manual selection - use the provided set ID directly
        Some(set_id)
    } else {
        // Auto-detect using pattern-based matching
        let flat_matches = find_flat_groups_for_light_frame(
            conn,
            light_frame,
            max_age_days,
            time_cluster_minutes,
            temp_weight,
        )?;

        if flat_matches.is_empty() {
            println!("  ⚠️  No flat groups found for frame {:?}", frame_id);
            None
        } else {
            println!("  ✅ Found {} flat group matches", flat_matches.len());
            for (i, m) in flat_matches.iter().enumerate() {
                println!("    Match {}: score={:.3}, age={}d, timing={:?}, frames={}",
                    i, m.match_score, m.age_days, m.timing, m.group.frame_count);
            }

            // Apply pattern-based selection
            let pattern = flat_pattern
                .and_then(|p| FlatPattern::from_str(p))
                .unwrap_or(FlatPattern::Automatic); // Default to Automatic (nearest by time)

            println!("  🎯 Applying pattern: {:?}", pattern);

            // Pass light frame date for temporal proximity calculation
            let light_frame_date = light_frame.date_obs;

            let selected_match = apply_pattern_selection(
                flat_matches,
                &pattern,
                light_frame_date,
            );

            if let Some(flat_match) = selected_match {
                println!("  ✅ Pattern selected match: age={}d, timing={:?}, frames={}",
                    flat_match.age_days, flat_match.timing, flat_match.group.frame_count);

                // Create calibration set from the flat group
                let set_id = create_flat_calibration_set(conn, &flat_match.group)?;

                // Add age warning if needed (only if threshold is reasonable)
                // Use config directly for consistency with UI
                let flat_date_threshold = config.warnings.flat_date_warning_days;
                if is_date_warning_enabled(flat_date_threshold)
                    && flat_match.age_days > flat_date_threshold {
                    warnings.push(CalibrationWarning {
                        warning_type: "date".to_string(),
                        message: format!(
                            "Flat calibration is {} days old (>{} days recommended)",
                            flat_match.age_days,
                            flat_date_threshold
                        ),
                        calibration_type: "Flat".to_string(),
                        set_id,
                    });
                }

                // Add temperature warning if temp diff exists, mode is "Warning", and threshold exceeded
                if let Some(temp_diff) = flat_match.temp_diff {
                    // Only generate warning if mode is "Warning" (not Ignore or Exact)
                    // and threshold is explicitly set in the config
                    if is_temp_warning_mode_enabled(&config, "lights", "flat") {
                        if let Some(threshold) = config.lights.flat.as_ref()
                            .and_then(|c| c.ccd_temp.warning_threshold) {
                            if temp_diff > threshold {
                                warnings.push(CalibrationWarning {
                                    warning_type: "temperature".to_string(),
                                    message: format!(
                                        "Flat temperature differs by {:.1}°C",
                                        temp_diff
                                    ),
                                    calibration_type: "Flat".to_string(),
                                    set_id,
                                });
                            }
                        }
                    }
                }

                Some(set_id)
            } else {
                println!("  ⚠️  Pattern selection returned no match");
                None
            }
        }
    };

    // Process the flat set if found
    if let Some(set_id) = flat_set_id {
        let flat_set = get_calibration_set_by_id(conn, set_id)?;

        // Find calibration for the Flat set (Dark or Bias)
        let flat_calib = find_calibration_for_flat_set(conn, set_id, tolerance, state)?;

        let mut flat_sub_calibration = Vec::new();
        if let Some(best_flat_calib) = flat_calib.first() {
            // Store the calibration link
            flat_sub_calibration.push(CalibrationLink {
                id: None,
                source_id: set_id,
                source_type: "calibration_set".to_string(),
                calibration_set_id: best_flat_calib.set_id,
                calibration_type: match best_flat_calib.imagetyp {
                    ImageType::Dark => "Dark".to_string(),
                    ImageType::Bias => "Bias".to_string(),
                    _ => "Unknown".to_string(),
                },
                matched_at: Utc::now().to_rfc3339(),
                match_score: Some(best_flat_calib.match_score),
                date_warning: best_flat_calib.date_warning,
                temp_warning: best_flat_calib.temp_warning,
                is_manual_override: false,
            });

            // Add date warning for sub-calibration (Dark/Bias for Flat)
            if best_flat_calib.date_warning {
                // Use dark date threshold from config for sub-calibration (Dark/Bias for Flats)
                let dark_date_threshold = config.warnings.dark_date_warning_days;
                if is_date_warning_enabled(dark_date_threshold) {
                    warnings.push(CalibrationWarning {
                        warning_type: "date".to_string(),
                        message: format!(
                            "{} for Flat is {} days old",
                            match best_flat_calib.imagetyp {
                                ImageType::Dark => "Dark",
                                ImageType::Bias => "Bias",
                                _ => "Calibration",
                            },
                            best_flat_calib.date_diff_days
                        ),
                        calibration_type: match best_flat_calib.imagetyp {
                            ImageType::Dark => "Dark".to_string(),
                            ImageType::Bias => "Bias".to_string(),
                            _ => "Unknown".to_string(),
                        },
                        set_id: best_flat_calib.set_id,
                    });
                }
            }
            // Add temp warning for sub-calibration (only if mode is "Warning")
            if best_flat_calib.temp_warning {
                let cal_type = match best_flat_calib.imagetyp {
                    ImageType::Dark => "dark",
                    ImageType::DarkFlat => "darkflat",
                    ImageType::Bias => "bias",
                    _ => "dark",
                };
                if is_temp_warning_mode_enabled(&config, "flats", cal_type) {
                    warnings.push(CalibrationWarning {
                        warning_type: "temperature".to_string(),
                        message: format!(
                            "{} for Flat temperature differs by {:.1}°C",
                            match best_flat_calib.imagetyp {
                                ImageType::Dark => "Dark",
                                ImageType::Bias => "Bias",
                                _ => "Calibration",
                            },
                            best_flat_calib.temp_diff.unwrap_or(0.0)
                        ),
                        calibration_type: match best_flat_calib.imagetyp {
                            ImageType::Dark => "Dark".to_string(),
                            ImageType::Bias => "Bias".to_string(),
                            _ => "Unknown".to_string(),
                        },
                        set_id: best_flat_calib.set_id,
                    });
                }
            }
        } else {
            missing_calibration.push("Dark/Bias for Flat".to_string());
        }

        flat_sets_with_links.push(CalibrationSetWithLinks {
            set: flat_set,
            sub_calibration: flat_sub_calibration,
        });
    } else {
        missing_calibration.push("Flat".to_string());
    }

    // Find Dark sets for the light frame using configurable matcher
    let config = load_config(conn);
    let mut ranked_darks = find_dark_for_light(conn, light_frame, &config)?;

    // Try to create Dark on-demand if not found
    if ranked_darks.is_empty() {
        println!("  🔍 No existing Dark sets found for Light frame, attempting on-demand creation...");

        if let Some(created_dark_id) = try_create_dark_for_frame(conn, light_frame, state)? {
            println!("  ✅ Created Dark set {} for Light frame", created_dark_id);

            // Re-query using configurable matcher
            ranked_darks = find_dark_for_light(conn, light_frame, &config)?;
        }
    }

    if ranked_darks.is_empty() {
        missing_calibration.push("Dark".to_string());
    } else {
        // Take the best dark match
        if let Some(best_dark) = ranked_darks.first() {
            let dark_set = get_calibration_set_by_id(conn, best_dark.set_id)?;

            // Add warnings for the dark (only if enabled in config)
            // Use config directly for consistency with UI
            let dark_date_threshold = config.warnings.dark_date_warning_days;
            if best_dark.date_warning && is_date_warning_enabled(dark_date_threshold) {
                warnings.push(CalibrationWarning {
                    warning_type: "date".to_string(),
                    message: format!(
                        "Dark calibration is {} days old (>{} days recommended)",
                        best_dark.date_diff_days,
                        dark_date_threshold
                    ),
                    calibration_type: "Dark".to_string(),
                    set_id: best_dark.set_id,
                });
            }
            // Only show temp warning if mode is "Warning" (not Ignore or Exact)
            if best_dark.temp_warning && is_temp_warning_mode_enabled(&config, "lights", "dark") {
                warnings.push(CalibrationWarning {
                    warning_type: "temperature".to_string(),
                    message: format!(
                        "Dark temperature differs by {:.1}°C",
                        best_dark.temp_diff.unwrap_or(0.0)
                    ),
                    calibration_type: "Dark".to_string(),
                    set_id: best_dark.set_id,
                });
            }

            // Find sub-calibration for Dark (Bias) if enabled in config
            let dark_sub_calibration = if let Some(opts) = config.get_behavioral_options("darks") {
                if opts.use_bias_for_dark_optimization {
                    if let Some(set_id) = dark_set.id {
                        match find_calibration_for_dark_set(conn, set_id, tolerance, state) {
                            Ok(candidates) => {
                                // Convert best candidate to CalibrationLink
                                if let Some(best_bias) = candidates.first() {
                                    vec![CalibrationLink {
                                        id: None,
                                        source_id: set_id,
                                        source_type: "calibration_set".to_string(),
                                        calibration_set_id: best_bias.set_id,
                                        calibration_type: "Bias".to_string(),
                                        matched_at: Utc::now().to_rfc3339(),
                                        match_score: Some(best_bias.match_score),
                                        date_warning: best_bias.date_warning,
                                        temp_warning: best_bias.temp_warning,
                                        is_manual_override: false,
                                    }]
                                } else {
                                    Vec::new()
                                }
                            }
                            Err(_) => Vec::new()
                        }
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            dark_sets_with_links.push(CalibrationSetWithLinks {
                set: dark_set,
                sub_calibration: dark_sub_calibration,
            });
        }
    }

    Ok(CalibrationHierarchy {
        light_frame_id: frame_id,
        flat_sets: flat_sets_with_links,
        dark_sets: dark_sets_with_links,
        missing_calibration,
        warnings,
    })
}

/// Store calibration hierarchy in database
/// Creates all necessary calibration links
/// Note: Warnings are NOT stored - they are calculated dynamically at display time
pub fn store_calibration_hierarchy(
    conn: &Connection,
    hierarchy: &CalibrationHierarchy,
) -> Result<()> {
    // Store Flat set links
    for flat_set_with_links in &hierarchy.flat_sets {
        if let Some(set_id) = flat_set_with_links.set.id {
            let link = CalibrationLink {
                id: None,
                source_id: hierarchy.light_frame_id,
                source_type: "frame".to_string(),
                calibration_set_id: set_id,
                calibration_type: "Flat".to_string(),
                matched_at: Utc::now().to_rfc3339(),
                match_score: Some(1.0), // Best match was selected
                // Warnings are calculated dynamically - not stored
                date_warning: false,
                temp_warning: false,
                is_manual_override: false,
            };
            insert_calibration_link(conn, &link)?;

            // Store sub-calibration links (Flat → Dark/Bias)
            for sub_link in &flat_set_with_links.sub_calibration {
                // Create a new link with warnings set to false
                let clean_sub_link = CalibrationLink {
                    date_warning: false,
                    temp_warning: false,
                    is_manual_override: false,
                    ..sub_link.clone()
                };
                insert_calibration_link(conn, &clean_sub_link)?;
            }
        }
    }

    // Store Dark set links
    for dark_set_with_links in &hierarchy.dark_sets {
        if let Some(set_id) = dark_set_with_links.set.id {
            let link = CalibrationLink {
                id: None,
                source_id: hierarchy.light_frame_id,
                source_type: "frame".to_string(),
                calibration_set_id: set_id,
                calibration_type: "Dark".to_string(),
                matched_at: Utc::now().to_rfc3339(),
                match_score: Some(1.0),
                // Warnings are calculated dynamically - not stored
                date_warning: false,
                temp_warning: false,
                is_manual_override: false,
            };
            insert_calibration_link(conn, &link)?;

            // Store sub-calibration links (Dark → Bias)
            for sub_link in &dark_set_with_links.sub_calibration {
                // Create a new link with warnings set to false
                let clean_sub_link = CalibrationLink {
                    date_warning: false,
                    temp_warning: false,
                    is_manual_override: false,
                    ..sub_link.clone()
                };
                insert_calibration_link(conn, &clean_sub_link)?;
            }
        }
    }

    Ok(())
}

/// Try to create a Dark calibration set on-demand for a given frame
/// Returns the created set ID if successful, None otherwise
fn try_create_dark_for_frame(
    conn: &Connection,
    frame: &Frame,
    _state: &State<'_, AppState>,
) -> Result<Option<i64>> {
    // Extract frame parameters
    let instrume = match &frame.instrume {
        Some(i) => i.as_str(),
        None => {
            println!("    ⚠️  Frame missing instrume, cannot create Dark");
            return Ok(None);
        }
    };

    let binning = match &frame.binning {
        Some(b) => b.as_str(),
        None => {
            println!("    ⚠️  Frame missing binning, cannot create Dark");
            return Ok(None);
        }
    };

    let exptime = frame.exptime;
    if exptime.is_none() {
        println!("    ⚠️  Frame missing exptime, cannot create Dark");
        return Ok(None);
    }

    let date_obs = match &frame.date_obs {
        Some(d) => d,
        None => {
            println!("    ⚠️  Frame missing date_obs, cannot create Dark");
            return Ok(None);
        }
    };

    // Get settings from config
    let config = crate::calibration::configurable_matcher::load_config(conn);
    let max_age_days = config.clustering.get("dark")
        .map(|c| c.max_age_days)
        .unwrap_or(30);
    let time_cluster_minutes = config.clustering.get("dark")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(30);

    println!("    📋 Dark search parameters: max_age_days={}, time_cluster_minutes={}",
        max_age_days, time_cluster_minutes);

    // Calculate date range: ±max_age_days from frame date
    let start_date = *date_obs - Duration::days(max_age_days);
    let end_date = *date_obs + Duration::days(max_age_days);

    println!("    📅 Dark date range: {} to {}", start_date, end_date);

    // Detect dark groups
    // Note: focal_length is NOT used for Dark matching - Darks are sensor-only calibrations
    let dark_groups = detect_dark_groups(
        conn,
        instrume,
        binning,
        frame.gain,
        frame.offset,
        exptime,
        None, // focal_length not relevant for Dark calibration
        time_cluster_minutes,
        Some((start_date, end_date)),
    )?;

    if dark_groups.is_empty() {
        println!("    ⚠️  No dark groups found for on-demand creation");
        return Ok(None);
    }

    // Select best group (first one - they're sorted newest first)
    let best_group = &dark_groups[0];
    println!("    🎯 Selected best dark group: {} frames, from {} to {}",
        best_group.frame_count, best_group.start_time, best_group.end_time);

    // Create calibration set from best group
    let set_id = create_dark_calibration_set(conn, best_group)?;

    Ok(Some(set_id))
}

/// Try to create a Bias calibration set on-demand for a given frame
/// Returns the created set ID if successful, None otherwise
fn try_create_bias_for_frame(
    conn: &Connection,
    frame: &Frame,
    _state: &State<'_, AppState>,
) -> Result<Option<i64>> {
    // Extract frame parameters
    let instrume = match &frame.instrume {
        Some(i) => i.as_str(),
        None => {
            println!("    ⚠️  Frame missing instrume, cannot create Bias");
            return Ok(None);
        }
    };

    let binning = match &frame.binning {
        Some(b) => b.as_str(),
        None => {
            println!("    ⚠️  Frame missing binning, cannot create Bias");
            return Ok(None);
        }
    };

    let date_obs = match &frame.date_obs {
        Some(d) => d,
        None => {
            println!("    ⚠️  Frame missing date_obs, cannot create Bias");
            return Ok(None);
        }
    };

    // Get settings from config
    let config = crate::calibration::configurable_matcher::load_config(conn);
    let max_age_days = config.clustering.get("bias")
        .map(|c| c.max_age_days)
        .unwrap_or(30);
    let time_cluster_minutes = config.clustering.get("bias")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(30);

    println!("    📋 Bias search parameters: max_age_days={}, time_cluster_minutes={}",
        max_age_days, time_cluster_minutes);

    // Calculate date range: ±max_age_days from frame date
    let start_date = *date_obs - Duration::days(max_age_days);
    let end_date = *date_obs + Duration::days(max_age_days);

    println!("    📅 Bias date range: {} to {}", start_date, end_date);

    // Detect bias groups
    // Note: focal_length is NOT used for Bias matching - Bias frames are sensor-only calibrations
    let bias_groups = detect_bias_groups(
        conn,
        instrume,
        binning,
        frame.gain,
        frame.offset,
        None, // focal_length not relevant for Bias calibration
        time_cluster_minutes,
        Some((start_date, end_date)),
    )?;

    if bias_groups.is_empty() {
        println!("    ⚠️  No bias groups found for on-demand creation");
        return Ok(None);
    }

    // Select best group (first one - they're sorted newest first)
    let best_group = &bias_groups[0];
    println!("    🎯 Selected best bias group: {} frames, from {} to {}",
        best_group.frame_count, best_group.start_time, best_group.end_time);

    // Create calibration set from best group
    let set_id = create_bias_calibration_set(conn, best_group)?;

    Ok(Some(set_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hierarchy_structure() {
        // Test that hierarchy can be built with empty sets
        let hierarchy = CalibrationHierarchy {
            light_frame_id: 123,
            flat_sets: Vec::new(),
            dark_sets: Vec::new(),
            missing_calibration: vec!["Flat".to_string(), "Dark".to_string()],
            warnings: Vec::new(),
        };

        assert_eq!(hierarchy.light_frame_id, 123);
        assert_eq!(hierarchy.missing_calibration.len(), 2);
        assert!(hierarchy.flat_sets.is_empty());
        assert!(hierarchy.dark_sets.is_empty());
    }

    #[test]
    fn test_missing_calibration_tracking() {
        let mut missing = Vec::new();

        // Simulate no flats found
        missing.push("Flat".to_string());

        // Simulate no darks found
        missing.push("Dark".to_string());

        // Simulate no bias for dark found
        missing.push("Bias for Dark".to_string());

        assert_eq!(missing.len(), 3);
        assert!(missing.contains(&"Flat".to_string()));
        assert!(missing.contains(&"Dark".to_string()));
        assert!(missing.contains(&"Bias for Dark".to_string()));
    }

    #[test]
    fn test_warning_accumulation() {
        let mut warnings = Vec::new();

        warnings.push(CalibrationWarning {
            warning_type: "date".to_string(),
            message: "Flat is 45 days old".to_string(),
            calibration_type: "Flat".to_string(),
            set_id: 10,
        });

        warnings.push(CalibrationWarning {
            warning_type: "temperature".to_string(),
            message: "Dark temperature differs by 5°C".to_string(),
            calibration_type: "Dark".to_string(),
            set_id: 20,
        });

        assert_eq!(warnings.len(), 2);

        let date_warnings: Vec<_> = warnings.iter()
            .filter(|w| w.warning_type == "date")
            .collect();
        assert_eq!(date_warnings.len(), 1);

        let temp_warnings: Vec<_> = warnings.iter()
            .filter(|w| w.warning_type == "temperature")
            .collect();
        assert_eq!(temp_warnings.len(), 1);
    }
}
