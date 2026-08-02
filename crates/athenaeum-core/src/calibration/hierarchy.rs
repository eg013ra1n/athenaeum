// Calibration hierarchy builder - constructs complete calibration trees
use crate::calibration::finder::{CalibrationCandidate, CandidateMode};
use crate::calibration::configurable_matcher::{
    load_config, find_dark_for_light, find_calibration_for_flat, find_calibration_for_dark,
    find_calibration_candidates,
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
use rusqlite::Connection;
use anyhow::{Result, Context};
use chrono::{Utc, DateTime, Duration};

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
                focallen, xpixsz, ypixsz, naxis1, naxis2, ra, dec, sitelat, lat_obs,
                sitelong, long_obs, objctra, objctdec, override, imagetyp, is_master,
                uuid, updated_at
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
            ypixsz: row.get(17)?,
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
            swcreate: None,
            bayerpat: None,
            rotation: None,
            uuid: row.get(31)?,
            updated_at: row.get(32)?,
        })
    })?;

    Ok(frame)
}

/// Get calibration set detail by ID
fn get_calibration_set_by_id(conn: &Connection, set_id: i64) -> Result<CalibrationSetDetail> {
    let mut stmt = conn.prepare(
        "SELECT cs.id, cs.imagetyp, cs.exptime, cs.ccd_temp, cs.temp_min, cs.temp_max, cs.gain, cs.offset,
                cs.binning, cs.instrume, cs.filter, cs.date_start, cs.date_end, cs.date, cs.frame_count, cs.is_master_library,
                f.naxis1, f.naxis2, f.bayerpat, f.swcreate, f.xpixsz, fi.format, cs.focallen,
                cs.uuid, cs.updated_at, cs.superseded_by_set_id
         FROM calibration_set cs
         LEFT JOIN calibration_set_frames csf ON csf.set_id = cs.id
         LEFT JOIN frames f ON f.id = csf.frame_id
         LEFT JOIN files fi ON fi.id = f.file_id
         WHERE cs.id = ?1
         LIMIT 1"
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
            is_master: row.get::<_, i32>(15).unwrap_or(0) == 1,
            naxis1: row.get(16)?,
            naxis2: row.get(17)?,
            bayerpat: row.get(18)?,
            swcreate: row.get(19)?,
            xpixsz: row.get(20)?,
            format: row.get(21)?,
            focallen: row.get(22)?,
            uuid: row.get(23)?,
            updated_at: row.get(24)?,
            superseded_by_set_id: row.get(25)?,
        })
    })?;

    Ok(set)
}

/// Find Dark or Bias calibration for a Flat calibration set
/// Uses configurable matching rules with fallback chain (DarkFlat → Dark → Bias)
/// Creates sets on-demand if not found
/// Note: Master sets (is_master_library = 1) don't need sub-calibration
pub fn find_calibration_for_flat_set(
    conn: &Connection,
    flat_set_id: i64,
    _tolerance: &CalibrationTolerance,
) -> Result<Vec<CalibrationCandidate>> {
    // Check if this is a master set - masters don't need sub-calibration
    let is_master_library: bool = conn.query_row(
        "SELECT is_master_library FROM calibration_set WHERE id = ?1",
        [flat_set_id],
        |row| Ok(row.get::<_, i32>(0).unwrap_or(0) == 1),
    ).unwrap_or(false);

    if is_master_library {
        // Master sets are already calibrated - no sub-calibration needed
        tracing::debug!(set_id = flat_set_id, "flat set is a master, no sub-calibration needed");
        return Ok(Vec::new());
    }

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
        tracing::debug!(set_id = flat_set_id, "no existing calibration sets found for flat via config matcher, trying on-demand creation");

        // Try to create Dark on-demand (fallback to old behavior)
        if let Some(created_dark_id) = try_create_dark_for_frame(conn, &frame)? {
            tracing::debug!(set_id = created_dark_id, flat_set_id, "created dark set for flat");

            // Re-query using configurable matcher
            candidates = find_calibration_for_flat(conn, &frame, &config)?;
        }
    }

    if candidates.is_empty() {
        // Try to create Bias on-demand as last resort
        if let Some(created_bias_id) = try_create_bias_for_frame(conn, &frame)? {
            tracing::debug!(set_id = created_bias_id, flat_set_id, "created bias set for flat");

            // Re-query using configurable matcher
            candidates = find_calibration_for_flat(conn, &frame, &config)?;
        }
    }

    Ok(candidates)
}

/// Find Bias calibration for a Dark calibration set
/// Only returns Bias if "use_bias_for_dark_optimization" is enabled in config
/// Creates Bias on-demand if not found
/// Note: Master sets (is_master_library = 1) don't need sub-calibration
pub fn find_calibration_for_dark_set(
    conn: &Connection,
    dark_set_id: i64,
    _tolerance: &CalibrationTolerance,
) -> Result<Vec<CalibrationCandidate>> {
    // Check if this is a master set - masters don't need sub-calibration
    let is_master_library: bool = conn.query_row(
        "SELECT is_master_library FROM calibration_set WHERE id = ?1",
        [dark_set_id],
        |row| Ok(row.get::<_, i32>(0).unwrap_or(0) == 1),
    ).unwrap_or(false);

    if is_master_library {
        // Master sets are already calibrated - no sub-calibration needed
        tracing::debug!(set_id = dark_set_id, "dark set is a master, no sub-calibration needed");
        return Ok(Vec::new());
    }

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
                tracing::debug!(set_id = dark_set_id, "no existing bias sets found for dark, trying on-demand creation");

                // Try to create Bias on-demand
                if let Some(created_bias_id) = try_create_bias_for_frame(conn, &frame)? {
                    tracing::debug!(set_id = created_bias_id, dark_set_id, "created bias set for dark");

                    // Re-query using configurable matcher
                    candidates = find_calibration_for_dark(conn, &frame, &config)?;
                }
            }
        }
    }

    Ok(candidates)
}

/// Auto-link pre-step for the flats arm.
///
/// A Master Flat exists in the catalog only as a `calibration_set` row — its
/// MASTERFLAT frame is invisible to raw-frame flat grouping — so the
/// pattern-based path in `build_complete_hierarchy` can never reach one, and
/// once the raw flats it superseded are gone from the catalog the light frame
/// ends up with no flat at all (2026-08-02 audit, C1 flats arm).
///
/// Returns the best *master* flat the configurable matcher accepts for this
/// light frame, or `None` so the caller falls back to pattern-based grouping.
/// Note the master is taken whenever one is compatible: `master_preferences`
/// orders the candidate list but never filters it (Task 1 contract), and raw
/// flat sets keep going through the grouping path, which is the only one that
/// models flat timing patterns.
fn find_master_flat_for_light(
    conn: &Connection,
    light_frame: &Frame,
    config: &CalibrationMatchingConfig,
) -> Result<Option<i64>> {
    let master = find_calibration_candidates(
        conn,
        light_frame,
        "lights",
        "flat",
        config,
        CandidateMode::OnlyCompatible,
    )?
    .into_iter()
    .find(|c| c.is_master);

    Ok(master.map(|c| c.set_id))
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
/// * `manual_dark_set_id` - Optional manually selected dark set ID for this specific frame
/// * `max_age_days` - Maximum age of flats to consider (from settings)
/// * `time_cluster_minutes` - Time threshold for grouping flats (from settings)
/// * `temp_weight` - Weight for temperature matching (from settings)
pub fn build_complete_hierarchy(
    conn: &Connection,
    light_frame: &Frame,
    tolerance: &CalibrationTolerance,
    flat_pattern: Option<&str>,
    manual_flat_set_id: Option<i64>,
    manual_dark_set_id: Option<i64>,
    max_age_days: i64,
    time_cluster_minutes: i64,
    temp_weight: f64,
) -> Result<CalibrationHierarchy> {
    let frame_id = light_frame.id.context("Frame must have an ID")?;

    let mut flat_sets_with_links = Vec::new();
    let mut dark_sets_with_links = Vec::new();
    let mut missing_calibration = Vec::new();
    let mut warnings = Vec::new();

    // Load configurable matching config for threshold checks
    let config = load_config(conn);

    // Derive focallen tolerance from lights→flat config
    let focallen_tolerance: Option<f64> = config.lights.flat.as_ref()
        .map(|flat_cfg| match flat_cfg.focallen.mode {
            MatchMode::Exact => Some(0.0),
            MatchMode::Warning => Some(flat_cfg.focallen.warning_threshold.unwrap_or(5.0)),
            MatchMode::Ignore => None,
        })
        .unwrap_or(None);

    // Find Flat sets for the light frame using new pattern-based system
    let flat_set_id = if let Some(set_id) = manual_flat_set_id {
        // Manual selection - use the provided set ID directly
        Some(set_id)
    } else if let Some(master_set_id) = find_master_flat_for_light(conn, light_frame, &config)? {
        // Master flats exist only as calibration_set rows (their MASTERFLAT
        // frame is invisible to raw-frame grouping), so consult the
        // configurable matcher first; fall back to pattern-based grouping
        // when no master matches (2026-08-02 audit C1, flats arm).
        tracing::debug!(frame_id, set_id = master_set_id, "auto-linked master flat via configurable matcher");
        Some(master_set_id)
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
            tracing::warn!(frame_id, "no flat groups found for light frame");
            None
        } else {
            tracing::debug!(frame_id, count = flat_matches.len(), "found flat group matches");
            for (i, m) in flat_matches.iter().enumerate() {
                tracing::trace!(
                    frame_id,
                    index = i,
                    score = m.match_score,
                    age_days = m.age_days,
                    timing = ?m.timing,
                    count = m.group.frame_count,
                    "flat group match candidate"
                );
            }

            // Apply pattern-based selection
            let pattern = flat_pattern
                .and_then(|p| FlatPattern::from_str(p))
                .unwrap_or(FlatPattern::Automatic); // Default to Automatic (nearest by time)

            tracing::debug!(frame_id, pattern = ?pattern, "applying flat pattern selection");

            // Pass light frame date for temporal proximity calculation
            let light_frame_date = light_frame.date_obs;

            let selected_match = apply_pattern_selection(
                flat_matches,
                &pattern,
                light_frame_date,
            );

            if let Some(flat_match) = selected_match {
                tracing::debug!(
                    frame_id,
                    age_days = flat_match.age_days,
                    timing = ?flat_match.timing,
                    count = flat_match.group.frame_count,
                    "pattern selected flat match"
                );

                // Find/reuse calibration set from the flat group (don't modify existing sets)
                let set_id = create_flat_calibration_set(conn, &flat_match.group, false, focallen_tolerance)?;

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
                tracing::warn!(frame_id, "flat pattern selection returned no match");
                None
            }
        }
    };

    // Process the flat set if found
    if let Some(set_id) = flat_set_id {
        let flat_set = get_calibration_set_by_id(conn, set_id)?;

        // Find calibration for the Flat set (Dark or Bias)
        let flat_calib = find_calibration_for_flat_set(conn, set_id, tolerance)?;

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

    // Find Dark sets for the light frame.
    // If a manual override is provided, use it directly and skip auto-detect /
    // warning generation. Otherwise run the configurable auto-matcher.
    let config = load_config(conn);

    let dark_set_id_to_use: Option<i64> = if let Some(set_id) = manual_dark_set_id {
        Some(set_id)
    } else {
        let mut ranked_darks = find_dark_for_light(conn, light_frame, &config)?;

        // Try to create Dark on-demand if not found
        if ranked_darks.is_empty() {
            tracing::debug!(frame_id, "no existing dark sets found for light frame, trying on-demand creation");

            if let Some(created_dark_id) = try_create_dark_for_frame(conn, light_frame)? {
                tracing::debug!(set_id = created_dark_id, frame_id, "created dark set for light frame");

                // Re-query using configurable matcher
                ranked_darks = find_dark_for_light(conn, light_frame, &config)?;
            }
        }

        if let Some(best_dark) = ranked_darks.first() {
            // Add warnings for the auto-picked dark (only if enabled in config)
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
            Some(best_dark.set_id)
        } else {
            None
        }
    };

    if let Some(set_id) = dark_set_id_to_use {
        let dark_set = get_calibration_set_by_id(conn, set_id)?;

        // Find sub-calibration for Dark (Bias) if enabled in config
        let dark_sub_calibration = if let Some(opts) = config.get_behavioral_options("darks") {
            if opts.use_bias_for_dark_optimization {
                match find_calibration_for_dark_set(conn, set_id, tolerance) {
                    Ok(candidates) => {
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
                    Err(_) => Vec::new(),
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
    } else {
        missing_calibration.push("Dark".to_string());
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
) -> Result<Option<i64>> {
    // Extract frame parameters
    let instrume = match &frame.instrume {
        Some(i) => i.as_str(),
        None => {
            tracing::warn!(frame_id = frame.id.unwrap_or(-1), "frame missing instrume, cannot create dark on-demand");
            return Ok(None);
        }
    };

    let binning = match &frame.binning {
        Some(b) => b.as_str(),
        None => {
            tracing::warn!(frame_id = frame.id.unwrap_or(-1), "frame missing binning, cannot create dark on-demand");
            return Ok(None);
        }
    };

    let exptime = frame.exptime;
    if exptime.is_none() {
        tracing::warn!(frame_id = frame.id.unwrap_or(-1), "frame missing exptime, cannot create dark on-demand");
        return Ok(None);
    }

    let date_obs = match &frame.date_obs {
        Some(d) => d,
        None => {
            tracing::warn!(frame_id = frame.id.unwrap_or(-1), "frame missing date_obs, cannot create dark on-demand");
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

    tracing::debug!(frame_id = frame.id.unwrap_or(-1), max_age_days, time_cluster_minutes, "dark on-demand search parameters");

    // Calculate date range: ±max_age_days from frame date
    let start_date = *date_obs - Duration::days(max_age_days);
    let end_date = *date_obs + Duration::days(max_age_days);

    tracing::debug!(
        frame_id = frame.id.unwrap_or(-1),
        date_start = %start_date,
        date_end = %end_date,
        "dark on-demand date range"
    );

    // Detect dark groups
    // Note: focal_length is NOT used for Dark matching - Darks are sensor-only calibrations
    let dark_temp_threshold = config.clustering.get("dark")
        .map(|c| c.temp_threshold_celsius);
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
        dark_temp_threshold,
    )?;

    if dark_groups.is_empty() {
        tracing::warn!(frame_id = frame.id.unwrap_or(-1), "no dark groups found for on-demand creation");
        return Ok(None);
    }

    // Select best group (first one - they're sorted newest first)
    let best_group = &dark_groups[0];
    tracing::debug!(
        frame_id = frame.id.unwrap_or(-1),
        count = best_group.frame_count,
        date_start = %best_group.start_time,
        date_end = %best_group.end_time,
        "selected best dark group for on-demand creation"
    );

    // Find/reuse calibration set from best group (don't modify existing sets)
    let set_id = create_dark_calibration_set(conn, best_group, false)?;

    Ok(Some(set_id))
}

/// Try to create a Bias calibration set on-demand for a given frame
/// Returns the created set ID if successful, None otherwise
fn try_create_bias_for_frame(
    conn: &Connection,
    frame: &Frame,
) -> Result<Option<i64>> {
    // Extract frame parameters
    let instrume = match &frame.instrume {
        Some(i) => i.as_str(),
        None => {
            tracing::warn!(frame_id = frame.id.unwrap_or(-1), "frame missing instrume, cannot create bias on-demand");
            return Ok(None);
        }
    };

    let binning = match &frame.binning {
        Some(b) => b.as_str(),
        None => {
            tracing::warn!(frame_id = frame.id.unwrap_or(-1), "frame missing binning, cannot create bias on-demand");
            return Ok(None);
        }
    };

    let date_obs = match &frame.date_obs {
        Some(d) => d,
        None => {
            tracing::warn!(frame_id = frame.id.unwrap_or(-1), "frame missing date_obs, cannot create bias on-demand");
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

    tracing::debug!(frame_id = frame.id.unwrap_or(-1), max_age_days, time_cluster_minutes, "bias on-demand search parameters");

    // Calculate date range: ±max_age_days from frame date
    let start_date = *date_obs - Duration::days(max_age_days);
    let end_date = *date_obs + Duration::days(max_age_days);

    tracing::debug!(
        frame_id = frame.id.unwrap_or(-1),
        date_start = %start_date,
        date_end = %end_date,
        "bias on-demand date range"
    );

    // Detect bias groups
    // Note: focal_length is NOT used for Bias matching - Bias frames are sensor-only calibrations
    let bias_temp_threshold = config.clustering.get("bias")
        .map(|c| c.temp_threshold_celsius);
    let bias_groups = detect_bias_groups(
        conn,
        instrume,
        binning,
        frame.gain,
        frame.offset,
        None, // focal_length not relevant for Bias calibration
        time_cluster_minutes,
        Some((start_date, end_date)),
        bias_temp_threshold,
    )?;

    if bias_groups.is_empty() {
        tracing::warn!(frame_id = frame.id.unwrap_or(-1), "no bias groups found for on-demand creation");
        return Ok(None);
    }

    // Select best group (first one - they're sorted newest first)
    let best_group = &bias_groups[0];
    tracing::debug!(
        frame_id = frame.id.unwrap_or(-1),
        count = best_group.frame_count,
        date_start = %best_group.start_time,
        date_end = %best_group.end_time,
        "selected best bias group for on-demand creation"
    );

    // Find/reuse calibration set from best group (don't modify existing sets)
    let set_id = create_bias_calibration_set(conn, best_group, false)?;

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

    // ── Auto flat path: master flats ────────────────────────────────────
    // 2026-08-02 audit C1 (flats arm). The auto flat path resolved flats
    // exclusively by grouping RAW flat frames, so a Master Flat — which
    // lives in the catalog as a calibration_set row whose only member frame
    // is a MasterFlat (invisible to raw-frame grouping) — could never be
    // auto-linked once its raw frames had been superseded away.

    use crate::db::schema::init_db;
    use rusqlite::params;

    fn auto_flat_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Insert a flat calibration_set row carrying every parameter the
    /// lights→flat config checks Exact (instrume / binning / gain / offset /
    /// filter) plus focallen (Warning mode, 5mm warn / 10mm reject).
    fn insert_flat_set(
        conn: &Connection,
        id: i64,
        imagetyp: &str,
        filter: &str,
        is_master_library: i32,
        superseded_by: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO calibration_set
             (id, imagetyp, exptime, filter, ccd_temp, temp_min, temp_max, gain, offset,
              binning, instrume, focallen, date, date_start, date_end, frame_count,
              is_master_library, superseded_by_set_id)
             VALUES (?1, ?2, 2.0, ?3, -10.0, -10.0, -10.0, 100.0, 30.0,
                     '1x1', 'ASI2600MM', 448.0, '2025-09-25', '2025-09-25T00:00:00+00:00',
                     '2025-09-25T00:10:00+00:00', 20, ?4, ?5)",
            params![id, imagetyp, filter, is_master_library, superseded_by],
        ).unwrap();
    }

    /// Insert a file + frame pair. `imagetyp` is written verbatim so callers
    /// can create both raw ('Flat') and master ('MasterFlat') rows.
    fn insert_frame_row(
        conn: &Connection,
        id: i64,
        imagetyp: &str,
        filter: &str,
        date_obs: &str,
        is_master: i32,
    ) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 1024, '2025-09-25T00:00:00+00:00', 'FITS')",
            params![id, format!("/data/frame_{}.fits", id), format!("frame_{}.fits", id)],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames
             (id, file_id, date_obs, instrume, exptime, filter, gain, offset, binning,
              xbinning, ybinning, ccd_temp, focallen, imagetyp, is_master)
             VALUES (?1, ?1, ?2, 'ASI2600MM', 2.0, ?3, 100.0, 30.0, '1x1',
                     1, 1, -10.0, 448.0, ?4, ?5)",
            params![id, date_obs, filter, imagetyp, is_master],
        ).unwrap();
    }

    fn light_frame_for_flat() -> Frame {
        Frame {
            id: Some(1),
            file_id: 1,
            object: Some("M42".to_string()),
            date_obs: Some(
                DateTime::parse_from_rfc3339("2025-10-01T00:00:00+00:00")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            telescop: None,
            instrume: Some("ASI2600MM".to_string()),
            exptime: Some(300.0),
            filter: Some("Ha".to_string()),
            imagetyp: Some(ImageType::Light),
            is_master: false,
            gain: Some(100.0),
            offset: Some(30.0),
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
            rotation: None,
            uuid: None,
            updated_at: None,
        }
    }

    #[test]
    fn auto_flat_reaches_master_when_raw_flat_frames_are_gone() {
        let conn = auto_flat_test_db();

        // Master flat (set 100) + its MasterFlat member frame. The raw set it
        // superseded (set 101) survives as a row, but its raw Flat frames are
        // no longer in the catalog — the only state raw-frame grouping can
        // see is nothing at all.
        insert_flat_set(&conn, 100, "MasterFlat", "Ha", 1, None);
        insert_frame_row(&conn, 100, "MasterFlat", "Ha", "2025-09-25T00:00:00+00:00", 1);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 100)",
            [],
        ).unwrap();
        insert_flat_set(&conn, 101, "Flat", "Ha", 0, Some(100));

        let light = light_frame_for_flat();
        let hierarchy = build_complete_hierarchy(
            &conn,
            &light,
            &CalibrationTolerance::default(),
            None,
            None,
            None,
            365,
            240,
            1.0,
        ).unwrap();

        assert_eq!(
            hierarchy.flat_sets.len(), 1,
            "auto path must land on the master flat, got missing: {:?}",
            hierarchy.missing_calibration
        );
        assert_eq!(hierarchy.flat_sets[0].set.id, Some(100));
        assert!(hierarchy.flat_sets[0].set.is_master, "auto path must land on the master flat");
        assert!(!hierarchy.missing_calibration.contains(&"Flat".to_string()));
    }

    #[test]
    fn auto_flat_falls_back_to_raw_grouping_when_no_master_matches() {
        let conn = auto_flat_test_db();

        // A master flat for a DIFFERENT filter — incompatible per the Exact
        // filter rule, so the matcher must decline it and the legacy
        // raw-frame grouping path must still produce a set.
        insert_flat_set(&conn, 100, "MasterFlat", "OIII", 1, None);
        insert_frame_row(&conn, 100, "MasterFlat", "OIII", "2025-09-25T00:00:00+00:00", 1);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 100)",
            [],
        ).unwrap();

        // Raw Ha flats the light frame can actually use.
        insert_frame_row(&conn, 10, "Flat", "Ha", "2025-09-25T20:00:00+00:00", 0);
        insert_frame_row(&conn, 11, "Flat", "Ha", "2025-09-25T20:01:00+00:00", 0);

        let light = light_frame_for_flat();
        let hierarchy = build_complete_hierarchy(
            &conn,
            &light,
            &CalibrationTolerance::default(),
            None,
            None,
            None,
            365,
            240,
            1.0,
        ).unwrap();

        assert_eq!(hierarchy.flat_sets.len(), 1, "raw grouping must still resolve a flat set");
        assert!(
            !hierarchy.flat_sets[0].set.is_master,
            "an incompatible master must not hijack the auto flat path"
        );
        assert_eq!(hierarchy.flat_sets[0].set.filter.as_deref(), Some("Ha"));
    }

    #[test]
    fn manual_flat_selection_wins_over_a_compatible_master() {
        let conn = auto_flat_test_db();

        // A compatible master exists, but the user picked a specific raw set.
        insert_flat_set(&conn, 100, "MasterFlat", "Ha", 1, None);
        insert_frame_row(&conn, 100, "MasterFlat", "Ha", "2025-09-25T00:00:00+00:00", 1);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 100)",
            [],
        ).unwrap();

        insert_flat_set(&conn, 200, "Flat", "Ha", 0, None);
        insert_frame_row(&conn, 20, "Flat", "Ha", "2025-09-25T20:00:00+00:00", 0);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (200, 20)",
            [],
        ).unwrap();

        let light = light_frame_for_flat();
        let hierarchy = build_complete_hierarchy(
            &conn,
            &light,
            &CalibrationTolerance::default(),
            None,
            Some(200),
            None,
            365,
            240,
            1.0,
        ).unwrap();

        assert_eq!(hierarchy.flat_sets.len(), 1);
        assert_eq!(
            hierarchy.flat_sets[0].set.id, Some(200),
            "manual selection must be used verbatim"
        );
        assert!(!hierarchy.flat_sets[0].set.is_master);
    }
}
