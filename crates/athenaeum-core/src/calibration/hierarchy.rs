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
use crate::calibration::geometry::{self, Geometry, LinkSource};
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
                uuid, updated_at, bayerpat, xbayroff, ybayroff, roworder
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
            // Same contract as the light loader in `calibration::processor`:
            // the matcher's mono/ambiguous-filter check reads `bayerpat`, so
            // these columns are selected rather than assumed. Absent values
            // stay None — never fabricated.
            bayerpat: row.get(33)?,
            xbayroff: row.get(34)?,
            ybayroff: row.get(35)?,
            roworder: row.get(36)?,
            rotation: None,
            uuid: row.get(31)?,
            updated_at: row.get(32)?,
        })
    })?;

    Ok(frame)
}

/// The geometry of the light a hierarchy is built for. The size and origin
/// come from the catalog row; a frame handed in that has no row (an unsaved
/// frame) is judged on its own recorded size and no origin, with a warning
/// - never an error that would stop Find Calibration.
fn light_geometry_of(conn: &Connection, light_frame: &Frame, frame_id: i64) -> Result<Geometry> {
    match geometry::geometry_for_frame(conn, frame_id) {
        Ok(g) => Ok(g),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            tracing::warn!(
                frame_id,
                "light frame has no catalog row; judging its geometry from the frame as given, no origin"
            );
            Ok(Geometry {
                size: geometry::size_of(light_frame),
                ..Geometry::default()
            })
        }
        Err(e) => Err(e.into()),
    }
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
        "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 ORDER BY frame_id LIMIT 1"
    )?;

    let frame_id: i64 = stmt.query_row([flat_set_id], |row| row.get(0))
        .context("Flat set has no frames")?;

    let frame = get_frame_by_id(conn, frame_id)?;
    let frame_geometry = geometry::geometry_for_frame(conn, frame_id)?;

    // Load configurable matching config
    let config = load_config(conn);

    // Use configurable matcher with fallback chain (DarkFlat → Dark → Bias)
    let mut candidates = find_calibration_for_flat(conn, &frame, &config)?;

    if candidates.is_empty() {
        tracing::debug!(set_id = flat_set_id, "no existing calibration sets found for flat via config matcher, trying on-demand creation");

        // Try to create Dark on-demand (fallback to old behavior)
        if let Some(created_dark_id) = try_create_dark_for_frame(conn, &frame, &frame_geometry)? {
            tracing::debug!(set_id = created_dark_id, flat_set_id, "created dark set for flat");

            // Re-query using configurable matcher
            candidates = find_calibration_for_flat(conn, &frame, &config)?;
        }
    }

    if candidates.is_empty() {
        // Try to create Bias on-demand as last resort
        if let Some(created_bias_id) = try_create_bias_for_frame(conn, &frame, &frame_geometry)? {
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
        "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 ORDER BY frame_id LIMIT 1"
    )?;

    let frame_id: i64 = stmt.query_row([dark_set_id], |row| row.get(0))
        .context("Dark set has no frames")?;

    let frame = get_frame_by_id(conn, frame_id)?;
    let frame_geometry = geometry::geometry_for_frame(conn, frame_id)?;

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
                if let Some(created_bias_id) = try_create_bias_for_frame(conn, &frame, &frame_geometry)? {
                    tracing::debug!(set_id = created_bias_id, dark_set_id, "created bias set for dark");

                    // Re-query using configurable matcher
                    candidates = find_calibration_for_dark(conn, &frame, &config)?;
                }
            }
        }
    }

    Ok(candidates)
}

/// Master flats the configurable matcher accepts for a light frame, read once
/// per frame and used at two points of the flats auto-link path.
#[derive(Debug, Default)]
struct FlatMasterCandidates {
    /// Set id of the top-ranked candidate, but only when that candidate is a
    /// master. Drives the pre-step short-circuit.
    top_master: Option<i64>,
    /// Best (highest-ranked) master anywhere in the list, whatever sits above
    /// it. Drives the fallback used when raw-frame grouping yields nothing.
    best_master: Option<i64>,
}

/// Auto-link pre-step for the flats arm.
///
/// A Master Flat exists in the catalog only as a `calibration_set` row — its
/// MASTERFLAT frame is invisible to raw-frame flat grouping — so the
/// pattern-based path in `build_complete_hierarchy` can never reach one, and
/// once the raw flats it superseded are gone from the catalog the light frame
/// ends up with no flat at all (2026-08-02 audit, C1 flats arm).
///
/// Honors `master_preferences`: the candidate list comes back already ordered
/// by `apply_master_preference`, so `top_master` is set only when the user's
/// preference actually put a master first. PreferMaster (the default) takes
/// any compatible master; NoPreference takes one only when it tops the score
/// order; PreferFrameset lets a compatible raw set go first.
///
/// `best_master` exists because deferring to a raw set is not the same as
/// reaching it: grouping rebuilds flat groups from raw `frames` rows under its
/// own thresholds, so a raw set whose frames are archived, deleted, or outside
/// the grouping window is irreproducible. The caller uses `best_master` when
/// grouping comes back empty, which is what makes the guarantee true: a
/// compatible master is never stranded.
fn find_master_flats_for_light(
    conn: &Connection,
    light_frame: &Frame,
    config: &CalibrationMatchingConfig,
) -> Result<FlatMasterCandidates> {
    let candidates = find_calibration_candidates(
        conn,
        light_frame,
        "lights",
        "flat",
        config,
        CandidateMode::OnlyCompatible,
    )?;

    Ok(FlatMasterCandidates {
        top_master: candidates.first().filter(|c| c.is_master).map(|c| c.set_id),
        best_master: candidates.iter().find(|c| c.is_master).map(|c| c.set_id),
    })
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
    // One stored-header read per light per Find Calibration, used by the flat
    // path and the dark path below.
    let light_geometry = light_geometry_of(conn, light_frame, frame_id)?;

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

    // Master flats exist only as calibration_set rows (their MASTERFLAT frame
    // is invisible to raw-frame grouping), so the auto path consults the
    // configurable matcher (2026-08-02 audit C1, flats arm). Read once here,
    // used twice below; skipped entirely when the user picked a set by hand.
    let flat_masters = match manual_flat_set_id {
        Some(_) => FlatMasterCandidates::default(),
        None => find_master_flats_for_light(conn, light_frame, &config)?,
    };

    // Find Flat sets for the light frame using new pattern-based system
    let flat_set_id = if let Some(set_id) = manual_flat_set_id {
        // Manual selection - use the provided set ID directly
        Some(set_id)
    } else if let Some(master_set_id) = flat_masters.top_master {
        // The matcher's top-ranked candidate is a master, so `master_preferences`
        // wants it: take it and skip grouping. A top-ranked RAW set instead
        // falls through to grouping below — and if grouping can't reproduce it,
        // the empty branch there still lands on a compatible master, so a
        // master is never stranded under any preference.
        tracing::debug!(frame_id, set_id = master_set_id, pattern = ?flat_pattern, "auto-linked master flat via configurable matcher");
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
            // Grouping rebuilds flat groups from raw `frames` rows under its own
            // thresholds, so the raw set that outranked the master may simply be
            // irreproducible (frames archived, deleted, or outside the window).
            // Don't leave the frame uncalibrated while a compatible master sits
            // unused (2026-08-02 audit C1, flats arm).
            if let Some(master_set_id) = flat_masters.best_master {
                tracing::debug!(frame_id, set_id = master_set_id, pattern = ?flat_pattern, "no flat groups, falling back to compatible master flat");
                Some(master_set_id)
            } else {
                tracing::warn!(frame_id, "no flat groups found for light frame");
                None
            }
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

            // Owner ruling 2026-10-20: the grouping already gathered only flats
            // of the light's size. The chosen GROUP is judged before any set row
            // is written (ruling G-8): another sub-frame origin makes way for the
            // next group and leaves nothing behind. A reused set (an existing one,
            // or the master that superseded this lineage) can differ from its
            // group, so it is re-checked too - its call wrote nothing.
            let mut remaining = flat_matches;
            let selected = loop {
                let Some(flat_match) = apply_pattern_selection(remaining.clone(), &pattern, light_frame_date) else {
                    break None;
                };
                let group_geometry = geometry::geometry_for_frames(conn, &flat_match.group.frame_ids)?;
                let fitting_set = if geometry::fit(&light_geometry, &group_geometry).refused() {
                    tracing::warn!(
                        frame_id,
                        count = flat_match.group.frame_count,
                        source_geometry = %light_geometry.label(),
                        set_geometry = %group_geometry.label(),
                        "flat group does not fit the light's frame geometry; trying the next"
                    );
                    None
                } else {
                    // Find/reuse calibration set from the flat group (don't modify existing sets)
                    let set_id = create_flat_calibration_set(conn, &flat_match.group, false, focallen_tolerance)?;
                    let set_geometry = geometry::geometry_for_set(conn, set_id)?;
                    if geometry::fit(&light_geometry, &set_geometry).refused() {
                        tracing::warn!(
                            frame_id,
                            set_id,
                            source_geometry = %light_geometry.label(),
                            set_geometry = %set_geometry.label(),
                            "reused flat set does not fit the light's frame geometry; trying the next"
                        );
                        None
                    } else {
                        Some(set_id)
                    }
                };
                match fitting_set {
                    Some(set_id) => break Some((flat_match, set_id)),
                    None => remaining.retain(|m| m.group.frame_ids != flat_match.group.frame_ids),
                }
            };

            if let Some((flat_match, set_id)) = selected {
                tracing::debug!(
                    frame_id,
                    age_days = flat_match.age_days,
                    timing = ?flat_match.timing,
                    count = flat_match.group.frame_count,
                    "pattern selected flat match"
                );

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
            } else if let Some(master_set_id) = flat_masters.best_master {
                // Every raw group was refused (another sub-frame origin). A master
                // is never stranded (2026-08-02 audit C1), and that now holds for
                // the geometry refusal too; `best_master` already passed the
                // matcher's size / origin gate.
                tracing::debug!(frame_id, set_id = master_set_id, pattern = ?flat_pattern, "every flat group refused by frame geometry, falling back to compatible master flat");
                Some(master_set_id)
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

            if let Some(created_dark_id) = try_create_dark_for_frame(conn, light_frame, &light_geometry)? {
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
    // Plan ruling R9 (G-2, G-7): before writing this run's links, drop every
    // AUTOMATIC link of the light - and of the flat and dark sets it now uses -
    // whose set does not fit: another frame size, mixed sizes, or another
    // recorded sub-frame origin (auto never keeps one). Find Calibration is the
    // only path that can clear an automatic link (the picker's deselect clears
    // manual ones only), and an upsert replaces a link only when a new candidate
    // of the SAME type is found; a geometry-stale automatic link would otherwise
    // block the gate forever. Manual links are the user's and stay (R2).
    let light_geometry = geometry::geometry_for_frame(conn, hierarchy.light_frame_id)?;
    geometry::prune_unfit_auto_links(conn, LinkSource::Frame(hierarchy.light_frame_id), &light_geometry)?;
    for set_id in hierarchy.flat_sets.iter().chain(hierarchy.dark_sets.iter()).filter_map(|s| s.set.id) {
        let set_geometry = geometry::geometry_for_set(conn, set_id)?;
        geometry::prune_unfit_auto_links(conn, LinkSource::Set(set_id), &set_geometry)?;
    }

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
    source: &Geometry,
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
        geometry::size_of(frame),
    )?;

    if dark_groups.is_empty() {
        tracing::warn!(frame_id = frame.id.unwrap_or(-1), "no dark groups found for on-demand creation");
        return Ok(None);
    }

    // Ruling G-8 (review I-2): judge each group before any set row is written
    // - newest first, the first one that fits the source's geometry. The
    // grouping already filtered by size; this catches another sub-frame
    // origin, which auto-link never takes (G-7).
    let mut chosen = None;
    for group in &dark_groups {
        let group_geometry = geometry::geometry_for_frames(conn, &group.frame_ids)?;
        if geometry::fit(source, &group_geometry).refused() {
            tracing::warn!(
                frame_id = frame.id.unwrap_or(-1),
                count = group.frame_count,
                source_geometry = %source.label(),
                set_geometry = %group_geometry.label(),
                "dark group does not fit the frame geometry; trying the next"
            );
            continue;
        }
        chosen = Some(group);
        break;
    }
    let Some(best_group) = chosen else {
        tracing::warn!(
            frame_id = frame.id.unwrap_or(-1),
            count = dark_groups.len(),
            "no dark group fits the frame geometry; none created on demand"
        );
        return Ok(None);
    };
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
    source: &Geometry,
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
        geometry::size_of(frame),
    )?;

    if bias_groups.is_empty() {
        tracing::warn!(frame_id = frame.id.unwrap_or(-1), "no bias groups found for on-demand creation");
        return Ok(None);
    }

    // Ruling G-8 (review I-2): judge each group before any set row is written
    // - newest first, the first one that fits the source's geometry. The
    // grouping already filtered by size; this catches another sub-frame
    // origin, which auto-link never takes (G-7).
    let mut chosen = None;
    for group in &bias_groups {
        let group_geometry = geometry::geometry_for_frames(conn, &group.frame_ids)?;
        if geometry::fit(source, &group_geometry).refused() {
            tracing::warn!(
                frame_id = frame.id.unwrap_or(-1),
                count = group.frame_count,
                source_geometry = %source.label(),
                set_geometry = %group_geometry.label(),
                "bias group does not fit the frame geometry; trying the next"
            );
            continue;
        }
        chosen = Some(group);
        break;
    }
    let Some(best_group) = chosen else {
        tracing::warn!(
            frame_id = frame.id.unwrap_or(-1),
            count = bias_groups.len(),
            "no bias group fits the frame geometry; none created on demand"
        );
        return Ok(None);
    };
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

    use crate::calibration::config::MasterPreference;
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
            xbayroff: None,
            ybayroff: None,
            roworder: None,
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

    /// Persist a lights→flat master preference the way the UI does: one
    /// `CalibrationMatchingConfig` JSON under `calibration.matching_config`,
    /// which `load_config` reads inside `build_complete_hierarchy`.
    fn set_flat_master_preference(conn: &Connection, preference: MasterPreference) {
        let mut config = CalibrationMatchingConfig::default();
        config.master_preferences.insert("flat".to_string(), preference);
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('calibration.matching_config', ?1)",
            params![config.to_json().unwrap()],
        ).unwrap();
    }

    #[test]
    fn prefer_frameset_defers_to_a_compatible_raw_flat_set() {
        let conn = auto_flat_test_db();
        set_flat_master_preference(&conn, MasterPreference::PreferFrameset);

        // Both a compatible master AND a compatible raw flat set (with its
        // raw frames still in the catalog) are available.
        insert_flat_set(&conn, 100, "MasterFlat", "Ha", 1, None);
        insert_frame_row(&conn, 100, "MasterFlat", "Ha", "2025-09-25T00:00:00+00:00", 1);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 100)",
            [],
        ).unwrap();

        insert_flat_set(&conn, 200, "Flat", "Ha", 0, None);
        insert_frame_row(&conn, 30, "Flat", "Ha", "2025-09-25T20:00:00+00:00", 0);
        insert_frame_row(&conn, 31, "Flat", "Ha", "2025-09-25T20:01:00+00:00", 0);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (200, 30), (200, 31)",
            [],
        ).unwrap();

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

        assert_eq!(hierarchy.flat_sets.len(), 1);
        assert!(
            !hierarchy.flat_sets[0].set.is_master,
            "PreferFrameset must defer to the raw flat set, got set {:?}",
            hierarchy.flat_sets[0].set.id
        );
    }

    #[test]
    fn master_flat_rescues_a_top_ranked_raw_set_grouping_cannot_reproduce() {
        // PreferFrameset puts the raw set on top, so the pre-step defers — but
        // the set's frames are no longer in the catalog (archived / deleted),
        // so grouping cannot rebuild it. Before the fallback this ended as NO
        // flat while a compatible master sat unused.
        let conn = auto_flat_test_db();
        set_flat_master_preference(&conn, MasterPreference::PreferFrameset);

        insert_flat_set(&conn, 100, "MasterFlat", "Ha", 1, None);
        insert_frame_row(&conn, 100, "MasterFlat", "Ha", "2025-09-25T00:00:00+00:00", 1);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 100)",
            [],
        ).unwrap();

        // Compatible raw set row, top-ranked under PreferFrameset, but with no
        // surviving member frames for grouping to find.
        insert_flat_set(&conn, 200, "Flat", "Ha", 0, None);

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
            "an irreproducible raw set must not strand the master, got missing: {:?}",
            hierarchy.missing_calibration
        );
        assert_eq!(hierarchy.flat_sets[0].set.id, Some(100));
        assert!(hierarchy.flat_sets[0].set.is_master);
    }

    #[test]
    fn prefer_master_takes_the_master_over_a_reproducible_raw_set() {
        // Positive control for the preference axis: same fixture as the
        // PreferFrameset test (master + raw set WITH its frames), opposite
        // preference, no manual selection.
        let conn = auto_flat_test_db();
        set_flat_master_preference(&conn, MasterPreference::PreferMaster);

        insert_flat_set(&conn, 100, "MasterFlat", "Ha", 1, None);
        insert_frame_row(&conn, 100, "MasterFlat", "Ha", "2025-09-25T00:00:00+00:00", 1);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 100)",
            [],
        ).unwrap();

        insert_flat_set(&conn, 200, "Flat", "Ha", 0, None);
        insert_frame_row(&conn, 40, "Flat", "Ha", "2025-09-25T20:00:00+00:00", 0);
        insert_frame_row(&conn, 41, "Flat", "Ha", "2025-09-25T20:01:00+00:00", 0);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (200, 40), (200, 41)",
            [],
        ).unwrap();

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

        assert_eq!(hierarchy.flat_sets.len(), 1);
        assert_eq!(hierarchy.flat_sets[0].set.id, Some(100));
        assert!(
            hierarchy.flat_sets[0].set.is_master,
            "PreferMaster must take the master even when a raw set is reproducible"
        );
    }

    #[test]
    fn prefer_frameset_still_reaches_the_master_when_no_raw_set_exists() {
        // The lineage must never be stranded: with the raw flats superseded
        // away there is nothing for grouping to find, so even PreferFrameset
        // has to land on the master.
        let conn = auto_flat_test_db();
        set_flat_master_preference(&conn, MasterPreference::PreferFrameset);

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
            "master must stay reachable under PreferFrameset, got missing: {:?}",
            hierarchy.missing_calibration
        );
        assert_eq!(hierarchy.flat_sets[0].set.id, Some(100));
        assert!(hierarchy.flat_sets[0].set.is_master);
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

    /// The representative frame of a flat/dark set goes into the configurable
    /// matcher, which shares the flat matcher's mono/ambiguous-filter check —
    /// so the loader must carry the CFA columns rather than assume mono.
    #[test]
    fn representative_frame_carries_cfa_metadata() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (7, '/data/flat.fits', 'flat.fits', 1024, '2025-09-25T00:00:00+00:00', 'FITS')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames
             (id, file_id, date_obs, instrume, exptime, imagetyp, is_master,
              bayerpat, xbayroff, ybayroff, roworder)
             VALUES (7, 7, '2025-09-25T00:00:00+00:00', 'ASI2600MC', 2.0, 'Flat', 0,
                     'RGGB', 0, 1, 'TOP-DOWN')",
            [],
        ).unwrap();

        let frame = get_frame_by_id(&conn, 7).unwrap();

        assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(frame.xbayroff, Some(0));
        assert_eq!(frame.ybayroff, Some(1));
        assert_eq!(frame.roworder.as_deref(), Some("TOP-DOWN"));
        assert!(
            !crate::models::is_mono_with_ambiguous_filter(&frame.bayerpat, &frame.filter),
            "a frame that declares a CFA pattern must never be classified as filter-ambiguous mono"
        );
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;
    use crate::calibration::geometry::test_support::fits_header_text;
    use crate::db::schema::init_db;
    use rusqlite::params;

    const FULL: (i32, i32) = (9576, 6388);
    const ROI: (i32, i32) = (6384, 4258);

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// One file + frame (`id` = file id) with every parameter the matcher and
    /// the groupers compare, a size, and a stored header recording `origin`.
    fn raw_frame(conn: &Connection, id: i64, imagetyp: &str, date_obs: &str, size: (i32, i32), origin: Option<(i32, i32)>) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 1024, '2025-09-25T00:00:00+00:00', 'FITS')",
            params![id, format!("/geom/{id}.fits"), format!("{id}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, date_obs, instrume, exptime, filter, gain, offset,
                                 binning, xbinning, ybinning, ccd_temp, focallen, imagetyp, is_master,
                                 naxis1, naxis2)
             VALUES (?1, ?1, ?2, 'ASI6200MM', 2.0, 'Ha', 100.0, 30.0, '1x1', 1, 1, -10.0, 448.0,
                     ?3, 0, ?4, ?5)",
            params![id, date_obs, imagetyp, size.0, size.1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![id, fits_header_text(origin)],
        )
        .unwrap();
    }

    fn members(conn: &Connection, set_id: i64) -> Vec<i64> {
        let mut stmt = conn.prepare("SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1 ORDER BY frame_id").unwrap();
        stmt.query_map([set_id], |r| r.get(0)).unwrap().collect::<rusqlite::Result<Vec<i64>>>().unwrap()
    }

    fn hierarchy_for(conn: &Connection, light_id: i64) -> CalibrationHierarchy {
        let light = get_frame_by_id(conn, light_id).unwrap();
        build_complete_hierarchy(conn, &light, &CalibrationTolerance::default(), Some("automatic"),
            None, None, 30, 30, 0.3).unwrap()
    }

    /// How many `calibration_set_frames` rows hold any of `frame_ids` — 0 means
    /// no set was ever written for them (ruling G-8, review I-2).
    fn set_rows_holding(conn: &Connection, frame_ids: &[i64]) -> i64 {
        let list = frame_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        conn.query_row(
            &format!("SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id IN ({list})"),
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// The flat pattern path: the nearest flats of another size never become
    /// candidates (grouping filter), and the nearest flats at another origin
    /// are judged as a GROUP and refused before any set is written for them —
    /// the next group is taken (review I-2).
    #[test]
    fn flat_pattern_path_takes_the_nearest_group_that_fits() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 30, "Flat", "2025-09-30T23:30:00+00:00", FULL, Some((0, 0)));
        raw_frame(&conn, 31, "Flat", "2025-09-30T23:31:00+00:00", FULL, Some((0, 0)));
        raw_frame(&conn, 10, "Flat", "2025-09-30T23:00:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 11, "Flat", "2025-09-30T23:01:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 20, "Flat", "2025-09-29T23:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 21, "Flat", "2025-09-29T23:01:00+00:00", ROI, Some((1584, 1064)));

        let h = hierarchy_for(&conn, 1);
        assert_eq!(h.flat_sets.len(), 1, "{:?}", h.missing_calibration);
        assert_eq!(members(&conn, h.flat_sets[0].set.id.unwrap()), vec![20, 21]);
        assert_eq!(set_rows_holding(&conn, &[10, 11, 30, 31]), 0, "no set row for a refused group");
    }

    /// A master flat set for the light (ASI6200MM / Ha / gain 100 / offset 30 /
    /// 1x1 / 448 mm) whose single member frame records `size` and `origin`.
    fn master_flat_set(conn: &Connection, set_id: i64, member: i64, size: (i32, i32), origin: Option<(i32, i32)>) {
        conn.execute(
            "INSERT INTO calibration_set
             (id, imagetyp, exptime, filter, ccd_temp, temp_min, temp_max, gain, offset,
              binning, instrume, focallen, date, date_start, date_end, frame_count, is_master_library)
             VALUES (?1, 'MasterFlat', 2.0, 'Ha', -10.0, -10.0, -10.0, 100.0, 30.0,
                     '1x1', 'ASI6200MM', 448.0, '2025-09-25', '2025-09-25T00:00:00+00:00',
                     '2025-09-25T00:10:00+00:00', 1, 1)",
            params![set_id],
        )
        .unwrap();
        raw_frame(conn, member, "MasterFlat", "2025-09-25T00:00:00+00:00", size, origin);
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", params![set_id, member]).unwrap();
    }

    fn prefer_raw_flats(conn: &Connection) {
        let mut config = CalibrationMatchingConfig::default();
        config.master_preferences.insert("flat".to_string(), crate::calibration::config::MasterPreference::PreferFrameset);
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('calibration.matching_config', ?1)",
            params![config.to_json().unwrap()],
        )
        .unwrap();
    }

    /// Review I-1: every raw flat group is at another origin and refused; the
    /// master that fits (ranked below the raw set) is taken, never stranded.
    /// `best_master` is already origin-gated by the matcher, so it cannot be a misfit.
    #[test]
    fn flat_falls_back_to_the_master_when_every_raw_group_is_refused() {
        let conn = db();
        prefer_raw_flats(&conn);
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 10, "Flat", "2025-09-30T23:00:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 11, "Flat", "2025-09-30T23:01:00+00:00", ROI, Some((0, 0)));
        master_flat_set(&conn, 500, 501, ROI, Some((1584, 1064)));
        // A compatible raw set (origin unrecorded, so the matcher admits it) that
        // PreferFrameset ranks above the master: the master is not `top_master`.
        // Its frames are gone from grouping's view, which sees only 10/11.
        conn.execute(
            "INSERT INTO calibration_set
             (id, imagetyp, exptime, filter, ccd_temp, temp_min, temp_max, gain, offset,
              binning, instrume, focallen, date, date_start, date_end, frame_count, is_master_library)
             VALUES (400, 'Flat', 2.0, 'Ha', -10.0, -10.0, -10.0, 100.0, 30.0,
                     '1x1', 'ASI6200MM', 448.0, '2025-09-30', '2025-09-30T23:00:00+00:00',
                     '2025-09-30T23:01:00+00:00', 1, 0)",
            [],
        )
        .unwrap();
        raw_frame(&conn, 402, "Flat", "2025-09-30T23:00:00+00:00", ROI, None);
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (400, 402)", []).unwrap();

        let h = hierarchy_for(&conn, 1);
        assert_eq!(h.flat_sets.len(), 1, "{:?}", h.missing_calibration);
        assert_eq!(h.flat_sets[0].set.id, Some(500));
        assert_eq!(set_rows_holding(&conn, &[10, 11]), 0, "no set row for the refused group");
    }

    /// The same refused raw group with no master at all: no flat, no set row (G-8).
    #[test]
    fn flat_refused_group_without_a_master_leaves_no_flat_and_no_set_row() {
        let conn = db();
        prefer_raw_flats(&conn);
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 10, "Flat", "2025-09-30T23:00:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 11, "Flat", "2025-09-30T23:01:00+00:00", ROI, Some((0, 0)));

        let h = hierarchy_for(&conn, 1);
        assert!(h.flat_sets.is_empty(), "{:?}", h.flat_sets.len());
        assert_eq!(set_rows_holding(&conn, &[10, 11]), 0, "no set row for the refused group");
    }

    /// On-demand dark creation groups only frames of the light's size, so the
    /// newer ROI darks never become the light's dark.
    #[test]
    fn on_demand_dark_is_built_from_frames_of_the_lights_size() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", FULL, None);
        raw_frame(&conn, 40, "Dark", "2025-09-25T22:00:00+00:00", FULL, None);
        raw_frame(&conn, 41, "Dark", "2025-09-25T22:01:00+00:00", FULL, None);
        raw_frame(&conn, 50, "Dark", "2025-09-28T22:00:00+00:00", ROI, None);
        raw_frame(&conn, 51, "Dark", "2025-09-28T22:01:00+00:00", ROI, None);

        let h = hierarchy_for(&conn, 1);
        assert_eq!(h.dark_sets.len(), 1, "{:?}", h.missing_calibration);
        assert_eq!(members(&conn, h.dark_sets[0].set.id.unwrap()), vec![40, 41]);
    }

    /// Review I-2 on the on-demand path: the newest dark group is the light's
    /// size but at another recorded origin — it is judged first and skipped,
    /// and no set row is ever written for it; the older group that fits is built.
    #[test]
    fn on_demand_dark_skips_a_group_at_another_origin_without_writing_it() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        // The dark clustering gap is 30 days: the fitting group sits months before
        // the newer, refused one so they stay two groups.
        raw_frame(&conn, 60, "Dark", "2025-06-20T22:00:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 61, "Dark", "2025-06-20T22:01:00+00:00", ROI, Some((1584, 1064)));
        raw_frame(&conn, 70, "Dark", "2025-09-28T22:00:00+00:00", ROI, Some((0, 0)));
        raw_frame(&conn, 71, "Dark", "2025-09-28T22:01:00+00:00", ROI, Some((0, 0)));

        let h = hierarchy_for(&conn, 1);
        assert_eq!(h.dark_sets.len(), 1, "{:?}", h.missing_calibration);
        assert_eq!(members(&conn, h.dark_sets[0].set.id.unwrap()), vec![60, 61]);
        assert_eq!(set_rows_holding(&conn, &[70, 71]), 0, "no set row for a refused group");
    }

    fn set_with_member_at(
        conn: &Connection,
        set_id: i64,
        imagetyp: &str,
        member: i64,
        size: (i32, i32),
        origin: Option<(i32, i32)>,
    ) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, date_start, date_end, frame_count)
             VALUES (?1, ?2, '2025-09-25', '2025-09-25T22:00:00+00:00', '2025-09-25T22:00:00+00:00', 1)",
            params![set_id, imagetyp],
        )
        .unwrap();
        raw_frame(conn, member, imagetyp, "2025-09-25T22:00:00+00:00", size, origin);
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", params![set_id, member]).unwrap();
    }

    fn set_with_member(conn: &Connection, set_id: i64, imagetyp: &str, member: i64, size: (i32, i32)) {
        set_with_member_at(conn, set_id, imagetyp, member, size, None);
    }

    fn link(conn: &Connection, source_type: &str, source_id: i64, set_id: i64, cal_type: &str, manual: bool) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at, is_manual_override)
             VALUES (?1, ?2, ?3, ?4, '2025-09-25T00:00:00Z', ?5)",
            params![source_id, source_type, set_id, cal_type, manual as i32],
        )
        .unwrap();
    }

    fn links_of(conn: &Connection, source_type: &str, source_id: i64) -> Vec<(String, i64, bool)> {
        let mut stmt = conn.prepare(
            "SELECT calibration_type, calibration_set_id, is_manual_override FROM calibration_set_to_frames
             WHERE source_type = ?1 AND source_id = ?2 ORDER BY calibration_type").unwrap();
        stmt.query_map(params![source_type, source_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i32>(2)? == 1)))
            .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    /// Plan ruling R9 (Review Focus 2): storing a hierarchy drops every
    /// AUTOMATIC link of the light — and of the flat set it now uses — whose
    /// set does not fit; manual links stay for the gate to name.
    #[test]
    fn store_prunes_unfit_auto_links_and_keeps_manual_ones() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", FULL, None);
        set_with_member(&conn, 200, "Dark", 60, ROI);
        set_with_member(&conn, 201, "Flat", 61, ROI);
        set_with_member(&conn, 202, "Bias", 62, FULL);
        set_with_member(&conn, 300, "Flat", 63, FULL);
        link(&conn, "frame", 1, 200, "Dark", false);
        link(&conn, "frame", 1, 201, "Flat", true);
        link(&conn, "frame", 1, 202, "Bias", false);
        link(&conn, "calibration_set", 300, 200, "Dark", false);

        let h = CalibrationHierarchy {
            light_frame_id: 1,
            flat_sets: vec![CalibrationSetWithLinks { set: get_calibration_set_by_id(&conn, 300).unwrap(), sub_calibration: Vec::new() }],
            dark_sets: Vec::new(),
            missing_calibration: Vec::new(),
            warnings: Vec::new(),
        };
        store_calibration_hierarchy(&conn, &h).unwrap();

        assert_eq!(links_of(&conn, "frame", 1), vec![
            ("Bias".to_string(), 202, false),
            ("Flat".to_string(), 201, true),
        ], "the auto ROI dark is gone; the manual ROI flat stays (and wins over the auto flat 300)");
        assert!(links_of(&conn, "calibration_set", 300).is_empty(), "flat 300's auto ROI dark sub-link is gone");
    }

    /// G-2 + G-7: the same size at another recorded origin — an AUTOMATIC link
    /// to it is removed (auto never keeps one), a MANUAL one stays.
    #[test]
    fn store_prunes_an_auto_link_at_another_origin_and_keeps_a_manual_one() {
        let conn = db();
        raw_frame(&conn, 1, "Light", "2025-10-01T00:00:00+00:00", ROI, Some((1584, 1064)));
        set_with_member_at(&conn, 210, "Dark", 64, ROI, Some((0, 0)));
        set_with_member_at(&conn, 211, "Flat", 65, ROI, Some((0, 0)));
        link(&conn, "frame", 1, 210, "Dark", false);
        link(&conn, "frame", 1, 211, "Flat", true);

        let h = CalibrationHierarchy {
            light_frame_id: 1,
            flat_sets: Vec::new(),
            dark_sets: Vec::new(),
            missing_calibration: Vec::new(),
            warnings: Vec::new(),
        };
        store_calibration_hierarchy(&conn, &h).unwrap();
        assert_eq!(links_of(&conn, "frame", 1), vec![("Flat".to_string(), 211, true)]);
    }
}
