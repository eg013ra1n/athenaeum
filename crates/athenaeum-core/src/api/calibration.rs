//! Shared `calibration` command-layer handlers — single business-logic source
//! for the Tauri (`commands/calibration.rs`) and web (`routes/calibration.rs`)
//! wrappers. See `.superpowers/sdd/p1-task-9-brief.md` for the conversion
//! recipe this module follows (reused verbatim by Tasks 10-12).
//!
//! Desktop bodies are authoritative; web-only divergences (mostly HTTP-status
//! classification of validation failures, already `ApiError::Invalid` ->
//! 400 pre-conversion) are folded in per the recipe. This is the
//! highest-churn module (39 commits desktop vs 11 web) — several web
//! handlers were missing debug-log parity or a formatted error-context
//! string that desktop already had; see the Task 11 report for the full
//! drift catalog. No fatal-vs-lenient (rule 2) forks were found in this
//! module — every command already agreed on severity between platforms.

use std::collections::HashMap;

use chrono::Utc;

use crate::api::{db, ApiError};
use crate::calibration::processor::ProcessingStats;
use crate::calibration::scan_integration::{
    create_calibration_sets_from_scan_with_masters, CalibrationScanResult, MasterFrameIds,
};
use crate::calibration::CalibrationMatchingConfig;
use crate::models::{
    CalibrationHierarchyView, CalibrationLink, CalibrationMetadataEdits, CalibrationSetConsumer,
    CalibrationSetDetail, CalibrationSetParameters, CalibrationSetWithScore, CalibrationTolerance,
    CameraStats, FileWithFrame, LightFrameParameters,
};
use crate::services::ServiceContext;

const CALIBRATION_CONFIG_KEY: &str = "calibration.matching_config";

// ========== Equipment & Dark Library Commands ==========

pub fn get_equipment_cameras(ctx: &ServiceContext) -> Result<Vec<CameraStats>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_all_cameras(&conn)?)
}

pub fn get_dark_library(ctx: &ServiceContext, instrume: String) -> Result<Vec<CalibrationSetDetail>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_camera_dark_library(&conn, &instrume)?)
}

pub fn has_dark_library(ctx: &ServiceContext, instrume: String) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::has_dark_library(&conn, &instrume)?)
}

pub fn get_master_dark_library(ctx: &ServiceContext, instrume: String) -> Result<Vec<CalibrationSetDetail>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_camera_master_dark_library(&conn, &instrume)?)
}

pub fn has_master_dark_library(ctx: &ServiceContext, instrume: String) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::has_master_dark_library(&conn, &instrume)?)
}

pub fn get_master_flat_library(ctx: &ServiceContext, instrume: String) -> Result<Vec<CalibrationSetDetail>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_camera_master_flat_library(&conn, &instrume)?)
}

pub fn has_master_flat_library(ctx: &ServiceContext, instrume: String) -> Result<bool, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::has_master_flat_library(&conn, &instrume)?)
}

pub fn get_calibration_set_frames(ctx: &ServiceContext, set_id: i64) -> Result<Vec<FileWithFrame>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::get_frames_for_calibration_set(&conn, set_id)?)
}

pub fn get_calibration_set_consumers(ctx: &ServiceContext, set_id: i64) -> Result<Vec<CalibrationSetConsumer>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(crate::db::calibration_links::get_calibration_set_consumers(&conn, set_id)?)
}

// ===== CALIBRATION FINDER COMMANDS =====

/// Find and link calibration for all light frames in a frame set.
pub fn find_calibration_for_frame_set(
    ctx: &ServiceContext,
    frame_set_id: i64,
    flat_date_warning_days: Option<i64>,
    dark_date_warning_days: Option<i64>,
    flat_pattern: Option<String>,
    manual_flat_selections: Option<HashMap<String, i64>>,
) -> Result<ProcessingStats, ApiError> {
    use crate::calibration::processor::process_frame_set;

    let db = db(ctx)?;
    let conn = db.conn();

    // Get frame set metadata (flat_pattern, date ranges). Both platforms
    // pre-conversion mapped a prepare()/query_row() failure here to a generic
    // 500-equivalent (web's `db_err`, desktop's bare `.to_string()`) rather
    // than a NotFound-style status — severity preserved as-is
    // (`ApiError::Internal`), not upgraded to a 404 neither side had.
    //
    // The "Failed to prepare frame set query" context text is web's, not
    // desktop's — desktop pre-conversion had a bare `.map_err(|e| e.to_string())?`
    // with no wrapping message here. Kept web's (harmless, more-descriptive)
    // wrapping for both platforms since it's a pure message-quality
    // improvement with identical failure severity, not a fatal/lenient fork.
    let mut stmt = conn
        .prepare("SELECT flat_pattern, date_obs_start, date_obs_end FROM frames_set WHERE id = ?1")
        .map_err(|e| ApiError::Internal(format!("Failed to prepare frame set query: {}", e)))?;

    let (stored_flat_pattern, _date_obs_start, _date_obs_end): (Option<String>, Option<String>, Option<String>) =
        stmt.query_row([frame_set_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(|e| ApiError::Internal(format!("Frame set not found: {}", e)))?;

    // Use provided flat_pattern or fall back to stored pattern
    let final_flat_pattern = flat_pattern.or(stored_flat_pattern);

    // Load calibration config
    let config = crate::calibration::configurable_matcher::load_config(&conn);

    // Get flat calibration settings from config
    let max_age_days = config.clustering.get("flat").map(|c| c.max_age_days).unwrap_or(30);
    let time_cluster_minutes = config.clustering.get("flat").map(|c| c.time_cluster_minutes).unwrap_or(30);
    let temp_weight = config.scoring.temperature_match_weight;

    // Build tolerance from parameters, config warnings, or defaults
    let tolerance = CalibrationTolerance {
        flat_date_warning_days: flat_date_warning_days.unwrap_or(config.warnings.flat_date_warning_days),
        dark_date_warning_days: dark_date_warning_days.unwrap_or(config.warnings.dark_date_warning_days),
    };

    tracing::debug!(
        frame_set_id,
        flat_date_warning_days = tolerance.flat_date_warning_days,
        dark_date_warning_days = tolerance.dark_date_warning_days,
        flat_pattern = ?final_flat_pattern,
        max_age_days,
        time_cluster_minutes,
        temp_weight,
        "finding calibration for frame set"
    );

    let stats = process_frame_set(
        &conn,
        frame_set_id,
        &tolerance,
        final_flat_pattern.as_deref(),
        manual_flat_selections.as_ref(),
        max_age_days,
        time_cluster_minutes,
        temp_weight,
    )
    .map_err(|e| ApiError::Internal(format!("Failed to process frame set: {}", e)))?;

    tracing::info!(
        frame_set_id,
        total_frames = stats.total_frames,
        frames_with_full_calibration = stats.frames_with_full_calibration,
        "calibration processing complete"
    );

    Ok(stats)
}

/// Get calibration hierarchy organized by Date -> Camera -> Filter for a frame set.
pub fn get_calibration_hierarchy_for_frame_set(
    ctx: &ServiceContext,
    frame_set_id: i64,
) -> Result<CalibrationHierarchyView, ApiError> {
    use crate::db::calibration_links::get_calibration_hierarchy_for_frame_set as get_hierarchy;

    let db = db(ctx)?;
    let conn = db.conn();
    Ok(get_hierarchy(&conn, frame_set_id)?)
}

// ========== Calibration Matching Config Commands ==========

/// Get the current calibration matching configuration.
pub fn get_calibration_matching_config(ctx: &ServiceContext) -> Result<CalibrationMatchingConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let config_json = crate::db::get_setting(&conn, CALIBRATION_CONFIG_KEY)?;

    match config_json {
        Some(json) => CalibrationMatchingConfig::from_json(&json)
            .map(|c| c.migrate())
            .map_err(|e| ApiError::Internal(format!("Failed to parse calibration config: {}", e))),
        None => Ok(CalibrationMatchingConfig::default()),
    }
}

/// Set the calibration matching configuration.
///
/// Validation-failure status mapping: web pre-conversion already mapped
/// `config.validate()` errors to `StatusCode::BAD_REQUEST` explicitly;
/// desktop's Tauri command had no HTTP-status concept and just propagated
/// the bare `String`. `ApiError::Invalid` (-> 400 via `api_err`) preserves
/// both platforms' exact pre-conversion behavior — not a rule-2 case, both
/// sides already agreed this is a client error.
pub fn set_calibration_matching_config(ctx: &ServiceContext, config: CalibrationMatchingConfig) -> Result<(), ApiError> {
    config.validate().map_err(ApiError::Invalid)?;

    let db = db(ctx)?;
    let conn = db.conn();

    let json = config
        .to_json()
        .map_err(|e| ApiError::Internal(format!("Failed to serialize calibration config: {}", e)))?;

    crate::db::set_setting(&conn, CALIBRATION_CONFIG_KEY, &json)?;
    Ok(())
}

/// Reset calibration matching configuration to defaults.
pub fn reset_calibration_matching_config(ctx: &ServiceContext) -> Result<CalibrationMatchingConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let default_config = CalibrationMatchingConfig::default();
    let json = default_config
        .to_json()
        .map_err(|e| ApiError::Internal(format!("Failed to serialize default config: {}", e)))?;

    crate::db::set_setting(&conn, CALIBRATION_CONFIG_KEY, &json)?;

    Ok(default_config)
}

// ========== Manual Calibration Selection Commands ==========

/// Get average parameters of light frames for manual selection display.
pub fn get_light_frame_parameters(ctx: &ServiceContext, frame_ids: Vec<i64>) -> Result<LightFrameParameters, ApiError> {
    use crate::db::calibration_links::get_links_for_frame;

    let db = db(ctx)?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err(ApiError::Invalid("No frame IDs provided".to_string()));
    }

    // Query frames
    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT id, instrume, binning, gain, offset, filter, ccd_temp, exptime, date_obs
         FROM frames WHERE id IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;

    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

    let mut rows = stmt.query(params.as_slice())?;

    let mut instrumes: Vec<Option<String>> = Vec::new();
    let mut binnings: Vec<Option<String>> = Vec::new();
    let mut gains: Vec<Option<f64>> = Vec::new();
    let mut offsets: Vec<Option<f64>> = Vec::new();
    let mut filters: Vec<Option<String>> = Vec::new();
    let mut temps: Vec<f64> = Vec::new();
    let mut exptimes: Vec<f64> = Vec::new();
    let mut dates: Vec<String> = Vec::new();
    let mut frame_count = 0;

    while let Some(row) = rows.next()? {
        frame_count += 1;
        instrumes.push(row.get(1).ok());
        binnings.push(row.get(2).ok());
        gains.push(row.get(3).ok());
        offsets.push(row.get(4).ok());
        filters.push(row.get(5).ok());
        if let Ok(Some(temp)) = row.get::<_, Option<f64>>(6) {
            temps.push(temp);
        }
        if let Ok(Some(exp)) = row.get::<_, Option<f64>>(7) {
            exptimes.push(exp);
        }
        if let Ok(Some(date)) = row.get::<_, Option<String>>(8) {
            dates.push(date);
        }
    }

    // Get most common values
    let instrume = most_common_option(&instrumes);
    let binning = most_common_option(&binnings);
    let gain = most_common_f64(&gains);
    let offset = most_common_f64(&offsets);
    let filter = most_common_option(&filters);

    // Calculate averages and ranges
    let avg_ccd_temp = if temps.is_empty() { None } else { Some(temps.iter().sum::<f64>() / temps.len() as f64) };

    let avg_exptime = if exptimes.is_empty() { None } else { Some(exptimes.iter().sum::<f64>() / exptimes.len() as f64) };

    let exptime_range = if exptimes.is_empty() {
        None
    } else {
        let min = exptimes.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = exptimes.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        Some((min, max))
    };

    let date_range = if dates.is_empty() {
        None
    } else {
        let mut sorted = dates.clone();
        sorted.sort();
        Some((sorted.first().unwrap().clone(), sorted.last().unwrap().clone()))
    };

    // Get current calibration links for first frame (representative)
    let first_frame_id = frame_ids[0];
    let links = get_links_for_frame(&conn, first_frame_id)?;

    let current_flat_set_id = links.iter().find(|l| l.calibration_type == "Flat").map(|l| l.calibration_set_id);
    let current_dark_set_id = links.iter().find(|l| l.calibration_type == "Dark").map(|l| l.calibration_set_id);
    let current_bias_set_id = links.iter().find(|l| l.calibration_type == "Bias").map(|l| l.calibration_set_id);

    Ok(LightFrameParameters {
        instrume,
        binning,
        gain,
        offset,
        filter,
        avg_ccd_temp,
        avg_exptime,
        exptime_range,
        frame_count,
        date_range,
        current_flat_set_id,
        current_dark_set_id,
        current_bias_set_id,
    })
}

/// Get calibration sets with match scores for manual selection.
///
/// Routes through the same `find_calibration_candidates` engine that auto-link
/// uses, so the modal's score and "compatible" decision agree with what
/// "Find Calibration" will actually pick. `show_all` toggles between
/// `OnlyCompatible` (config-driven hard filter) and `IncludeIncompatible`
/// (every set returned, with `passed_hard_filter = false` flagged via the
/// MatchDetails fields).
pub fn get_calibration_sets_for_manual_selection(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
    calibration_type: String, // "flat", "dark", "bias"
    show_all: bool,
) -> Result<Vec<CalibrationSetWithScore>, ApiError> {
    use crate::calibration::configurable_matcher::{find_calibration_candidates, load_config};
    use crate::calibration::finder::CandidateMode;

    let db = db(ctx)?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err(ApiError::Invalid("No frame IDs provided".to_string()));
    }

    // Validate calibration_type up front for a clear error message.
    match calibration_type.to_lowercase().as_str() {
        "flat" | "dark" | "bias" | "darkflat" => {}
        _ => return Err(ApiError::Invalid(format!("Invalid calibration type: {}", calibration_type))),
    };

    let config = load_config(&conn);
    let frame = crate::calibration::manual::synthesize_frame_for_lights(&conn, &frame_ids)?;

    // Look up the currently-linked set for this calibration_type so we can
    // *always* include it in the result, even if its score would otherwise
    // fall under the show_all floor or it's no longer compatible. Without
    // this, the "Current" badge has nowhere to render when the user's link
    // is now stale relative to the config.
    let cal_type_key = match calibration_type.to_lowercase().as_str() {
        "flat" => "Flat",
        "dark" => "Dark",
        "bias" => "Bias",
        "darkflat" => "DarkFlat",
        _ => "",
    };
    let current_link_set_id: Option<i64> = if !frame_ids.is_empty() && !cal_type_key.is_empty() {
        let links = crate::db::calibration_links::get_links_for_frame(&conn, frame_ids[0]).unwrap_or_default();
        links.iter().find(|l| l.calibration_type == cal_type_key).map(|l| l.calibration_set_id)
    } else {
        None
    };

    // Manual modal is a user override — show every candidate via the engine's
    // IncludeIncompatible mode. With the engine's score-zero rule for
    // hard-filter rejects, the score itself honestly reflects "your config
    // would refuse this": camera mismatch, filter mismatch, focal length /
    // temp / exptime exceeding matching_threshold all read 0.0 now.
    // `show_all` toggles between hiding score < 0.1 (default) and showing
    // everything (manual override visibility).
    let candidates = find_calibration_candidates(
        &conn,
        &frame,
        "lights",
        &calibration_type.to_lowercase(),
        &config,
        CandidateMode::IncludeIncompatible,
    )?;

    let mut out: Vec<CalibrationSetWithScore> = Vec::with_capacity(candidates.len());
    let mut current_seen = false;
    for candidate in candidates {
        let is_current = current_link_set_id == Some(candidate.set_id);
        if is_current {
            current_seen = true;
        }
        if !show_all && !is_current && candidate.match_score < 0.1 {
            continue;
        }
        if let Some(swc) = crate::calibration::manual::load_set_with_score(&conn, &candidate)? {
            out.push(swc);
        }
    }
    // Edge case: the currently-linked set isn't even in `find_calibration_candidates`
    // results (e.g., its imagetyp doesn't match the requested type after a
    // metadata edit). Synthesize a candidate-shaped row with score 0 so the
    // modal can still render the "Current" badge against it.
    if let (false, Some(cur_id)) = (current_seen, current_link_set_id) {
        let placeholder = crate::calibration::finder::CalibrationCandidate {
            set_id: cur_id,
            imagetyp: crate::models::ImageType::from_str(cal_type_key).unwrap_or(crate::models::ImageType::Dark),
            match_score: 0.0,
            date_diff_days: 0,
            temp_diff: None,
            date_warning: false,
            temp_warning: false,
            is_master: false,
            passed_hard_filter: false,
            details: crate::calibration::finder::CandidateMatchDetails::default(),
            warnings: Vec::new(),
        };
        if let Some(swc) = crate::calibration::manual::load_set_with_score(&conn, &placeholder)? {
            out.push(swc);
        }
    }
    Ok(out)
}

/// Manually assign a calibration set to light frames.
pub fn manual_assign_calibration(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
    calibration_set_id: i64,
    calibration_type: String, // "Flat", "Dark", "Bias"
) -> Result<usize, ApiError> {
    use crate::db::calibration_links::insert_calibration_link;

    let db = db(ctx)?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err(ApiError::Invalid("No frame IDs provided".to_string()));
    }

    // Validate calibration_type
    let valid_types = ["Flat", "Dark", "Bias", "DarkFlat"];
    if !valid_types.contains(&calibration_type.as_str()) {
        return Err(ApiError::Invalid(format!("Invalid calibration type: {}", calibration_type)));
    }

    let mut assigned_count = 0;

    for frame_id in &frame_ids {
        // Create the calibration link with is_manual_override = true
        let link = CalibrationLink {
            id: None,
            source_id: *frame_id,
            source_type: "frame".to_string(),
            calibration_set_id,
            calibration_type: calibration_type.clone(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(1.0), // Manual assignment gets perfect score
            date_warning: false,
            temp_warning: false,
            is_manual_override: true,
        };

        match insert_calibration_link(&conn, &link) {
            Ok(_) => assigned_count += 1,
            Err(e) => {
                tracing::warn!(frame_id, error = %e, "failed to assign calibration to frame");
            }
        }
    }

    tracing::info!(
        calibration_type = %calibration_type,
        set_id = calibration_set_id,
        count = assigned_count,
        "manually assigned calibration set to frames"
    );

    Ok(assigned_count)
}

/// Remove manual calibration override and allow auto-find to reassign.
pub fn clear_manual_calibration_override(
    ctx: &ServiceContext,
    frame_ids: Vec<i64>,
    calibration_type: Option<String>, // None = clear all types
) -> Result<usize, ApiError> {
    use crate::db::calibration_links::clear_manual_override;

    let db = db(ctx)?;
    let conn = db.conn();

    if frame_ids.is_empty() {
        return Err(ApiError::Invalid("No frame IDs provided".to_string()));
    }

    let deleted = clear_manual_override(&conn, &frame_ids, calibration_type.as_deref())?;

    tracing::info!(count = deleted, frames = frame_ids.len(), "cleared manual calibration override(s)");

    Ok(deleted)
}

fn most_common_option(values: &[Option<String>]) -> Option<String> {
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for v in values.iter().flatten() {
        *counts.entry(v).or_insert(0) += 1;
    }

    counts.into_iter().max_by_key(|(_, count)| *count).map(|(v, _)| v.clone())
}

fn most_common_f64(values: &[Option<f64>]) -> Option<f64> {
    // Round to 2 decimal places for comparison
    let mut counts: HashMap<i64, (f64, usize)> = HashMap::new();
    for v in values.iter().flatten() {
        let key = (*v * 100.0).round() as i64;
        let entry = counts.entry(key).or_insert((*v, 0));
        entry.1 += 1;
    }

    counts.into_iter().max_by_key(|(_, (_, count))| *count).map(|(_, (v, _))| v)
}

// ========== Refresh Calibration Library Command ==========

/// Refresh calibration library for a specific camera.
///
/// This command preserves calibration set IDs by:
/// 1. Clearing frame memberships (but keeping the sets)
/// 2. Queries all calibration frame IDs for the camera (Flat, Dark, Bias, DarkFlat)
/// 3. Reclusters and assigns frames to sets - sets are matched by params + date overlap
/// 4. Deletes orphaned sets (sets with 0 frames after reclustering)
///
/// Superseded raw sets (Phase 2: a master was built from them) are frozen
/// provenance history — every step skips them and their member frames.
///
/// Split into a conn-taking `refresh_calibration_library_inner` so the
/// superseded-set / trigger-suspension semantics can be pinned by unit tests
/// without constructing a full `ServiceContext`.
pub fn refresh_calibration_library_for_camera(ctx: &ServiceContext, instrume: String) -> Result<CalibrationScanResult, ApiError> {
    let db = db(ctx)?;
    let mut conn = db.conn();
    refresh_calibration_library_inner(&mut conn, &instrume)
}

pub(crate) fn refresh_calibration_library_inner(
    conn: &mut rusqlite::Connection,
    instrume: &str,
) -> Result<CalibrationScanResult, ApiError> {
    tracing::info!(instrume, "refreshing calibration library");

    let tx = conn
        .transaction()
        .map_err(|e| ApiError::Internal(format!("Failed to begin refresh transaction: {}", e)))?;

    // Suspend the empty-prune trigger for the clear + recluster window: Step 1
    // empties every live set BY DESIGN, and the trigger would delete the
    // emptied rows — destroying the set-ID stability this command exists to
    // provide and CASCADE-wiping every consumer link (including manual
    // overrides). The DROP is transaction-local: any early return rolls back
    // to the intact trigger, and the happy path re-creates the identical
    // definition before COMMIT.
    tx.execute("DROP TRIGGER IF EXISTS calibration_set_empty_prune", [])
        .map_err(|e| ApiError::Internal(format!("Failed to suspend empty-prune trigger: {}", e)))?;

    // Step 1: Clear frame memberships for this camera's live sets (keep the
    // sets themselves; superseded sets keep their memberships too).
    tx.execute(
        "DELETE FROM calibration_set_frames
         WHERE set_id IN (
             SELECT id FROM calibration_set
              WHERE instrume = ?1 AND is_master_library = 0
                AND superseded_by_set_id IS NULL
         )",
        rusqlite::params![instrume],
    )
    .map_err(|e| ApiError::Internal(format!("Failed to clear frame memberships: {}", e)))?;

    // Reset frame counts to 0 for these sets
    tx.execute(
        "UPDATE calibration_set SET frame_count = 0
         WHERE instrume = ?1 AND is_master_library = 0
           AND superseded_by_set_id IS NULL",
        rusqlite::params![instrume],
    )
    .map_err(|e| ApiError::Internal(format!("Failed to reset frame counts: {}", e)))?;

    tracing::debug!(instrume, "cleared existing frame memberships (sets preserved for ID stability)");

    // Step 2: Query all calibration frame IDs for this camera
    let flat_frame_ids = query_frame_ids_by_type(&tx, instrume, "FLAT")
        .map_err(|e| ApiError::Internal(format!("Failed to query flat frames: {}", e)))?;
    let dark_frame_ids = query_frame_ids_by_type(&tx, instrume, "DARK")
        .map_err(|e| ApiError::Internal(format!("Failed to query dark frames: {}", e)))?;
    let bias_frame_ids = query_frame_ids_by_type(&tx, instrume, "BIAS")
        .map_err(|e| ApiError::Internal(format!("Failed to query bias frames: {}", e)))?;
    let darkflat_frame_ids = query_frame_ids_by_type(&tx, instrume, "DARKFLAT")
        .map_err(|e| ApiError::Internal(format!("Failed to query darkflat frames: {}", e)))?;

    // Query master frame IDs
    let master_dark_ids = query_frame_ids_by_type(&tx, instrume, "MASTERDARK")
        .map_err(|e| ApiError::Internal(format!("Failed to query master dark frames: {}", e)))?;
    let master_flat_ids = query_frame_ids_by_type(&tx, instrume, "MASTERFLAT")
        .map_err(|e| ApiError::Internal(format!("Failed to query master flat frames: {}", e)))?;
    let master_bias_ids = query_frame_ids_by_type(&tx, instrume, "MASTERBIAS")
        .map_err(|e| ApiError::Internal(format!("Failed to query master bias frames: {}", e)))?;
    let master_darkflat_ids = query_frame_ids_by_type(&tx, instrume, "MASTERDARKFLAT")
        .map_err(|e| ApiError::Internal(format!("Failed to query master darkflat frames: {}", e)))?;

    let master_frame_ids = MasterFrameIds { master_dark_ids, master_flat_ids, master_bias_ids, master_darkflat_ids };

    tracing::debug!(
        flats = flat_frame_ids.len(),
        darks = dark_frame_ids.len(),
        bias = bias_frame_ids.len(),
        darkflats = darkflat_frame_ids.len(),
        "calibration frames found"
    );
    if !master_frame_ids.is_empty() {
        tracing::debug!(
            master_darks = master_frame_ids.master_dark_ids.len(),
            master_flats = master_frame_ids.master_flat_ids.len(),
            master_bias = master_frame_ids.master_bias_ids.len(),
            master_darkflats = master_frame_ids.master_darkflat_ids.len(),
            "master calibration frames found"
        );
    }

    // Step 3: Recreate calibration sets using the same algorithm as folder scanning
    let result = create_calibration_sets_from_scan_with_masters(
        &tx,
        flat_frame_ids,
        dark_frame_ids,
        bias_frame_ids,
        darkflat_frame_ids,
        master_frame_ids,
    )
    .map_err(|e| ApiError::Internal(format!("Failed to create calibration sets: {}", e)))?;

    // Step 4: Delete orphaned sets (sets with no frames after reclustering)
    let deleted_orphans = tx
        .execute(
            "DELETE FROM calibration_set
             WHERE instrume = ?1 AND is_master_library = 0 AND frame_count = 0
               AND superseded_by_set_id IS NULL",
            rusqlite::params![instrume],
        )
        .map_err(|e| ApiError::Internal(format!("Failed to delete orphaned sets: {}", e)))?;

    if deleted_orphans > 0 {
        tracing::debug!(count = deleted_orphans, "deleted orphaned calibration sets");
    }

    // Restore the empty-prune trigger before committing.
    crate::db::schema::create_calibration_set_empty_prune_trigger(&tx)
        .map_err(|e| ApiError::Internal(format!("Failed to restore empty-prune trigger: {}", e)))?;

    tx.commit()
        .map_err(|e| ApiError::Internal(format!("Failed to commit refresh transaction: {}", e)))?;

    tracing::info!(instrume, sets_created = result.sets_created, "calibration library refresh complete");

    Ok(result)
}

/// Query frame IDs for a specific camera and image type (case-insensitive).
/// Members of superseded sets are excluded: those frames are frozen
/// provenance history, already represented in matching by the built master.
fn query_frame_ids_by_type(conn: &rusqlite::Connection, instrume: &str, imagetyp: &str) -> Result<Vec<i64>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id FROM frames
         WHERE instrume = ?1 AND UPPER(imagetyp) = UPPER(?2)
           AND id NOT IN (
               SELECT csf.frame_id
                 FROM calibration_set_frames csf
                 JOIN calibration_set cs ON cs.id = csf.set_id
                WHERE cs.superseded_by_set_id IS NOT NULL
           )",
    )?;

    let ids: Vec<i64> = stmt.query_map([instrume, imagetyp], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?;

    Ok(ids)
}

// ========== Sub-Calibration Selection Commands ==========

/// Get parameters of a calibration set for sub-calibration selection display.
pub fn get_calibration_set_parameters(ctx: &ServiceContext, set_id: i64) -> Result<CalibrationSetParameters, ApiError> {
    use crate::db::calibration_links::get_links_for_calibration_set;

    let db = db(ctx)?;
    let conn = db.conn();

    // Get calibration set details
    let mut stmt = conn.prepare(
        "SELECT id, imagetyp, instrume, binning, gain, offset, exptime, filter,
                ccd_temp, date_start, date_end, frame_count
         FROM calibration_set WHERE id = ?1",
    )?;

    let (imagetyp, instrume, binning, gain, offset, exptime, filter, ccd_temp, date_start, date_end, frame_count): (
        String,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<String>,
        Option<f64>,
        Option<String>,
        Option<String>,
        i64,
    ) = stmt
        .query_row([set_id], |row| {
            Ok((
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            ))
        })
        .map_err(|e| ApiError::Internal(format!("Calibration set not found: {}", e)))?;

    // Get current sub-calibration links
    let links = get_links_for_calibration_set(&conn, set_id)?;

    let current_dark_set_id = links.iter().find(|l| l.calibration_type == "Dark").map(|l| l.calibration_set_id);
    let current_darkflat_set_id = links.iter().find(|l| l.calibration_type == "DarkFlat").map(|l| l.calibration_set_id);
    let current_bias_set_id = links.iter().find(|l| l.calibration_type == "Bias").map(|l| l.calibration_set_id);

    Ok(CalibrationSetParameters {
        set_id,
        imagetyp,
        instrume,
        binning,
        gain,
        offset,
        exptime,
        filter,
        ccd_temp,
        date_start,
        date_end,
        frame_count,
        current_dark_set_id,
        current_darkflat_set_id,
        current_bias_set_id,
    })
}

/// Get sub-calibration candidates for a Flat or Dark calibration set.
///
/// Routes through the same `find_calibration_candidates` engine that auto-link
/// uses, with the parent set's parameters synthesized as a "frame" so the
/// engine's `flats→{darkflat,dark,bias}` and `darks→bias` configs apply.
pub fn get_subcalibration_sets_for_manual_selection(
    ctx: &ServiceContext,
    set_id: i64,
    calibration_type: String, // "dark", "darkflat", "bias"
    show_all: bool,
) -> Result<Vec<CalibrationSetWithScore>, ApiError> {
    use crate::calibration::configurable_matcher::{find_calibration_candidates, load_config};
    use crate::calibration::finder::CandidateMode;

    let db = db(ctx)?;
    let conn = db.conn();

    // Validate up front.
    match calibration_type.to_lowercase().as_str() {
        "dark" | "darkflat" | "bias" => {}
        _ => return Err(ApiError::Invalid(format!("Invalid calibration type for sub-calibration: {}", calibration_type))),
    };

    // Master sets are already calibrated — they have no sub-cal.
    let is_master_library: bool = conn
        .query_row(
            "SELECT is_master_library FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| Ok(row.get::<_, i32>(0).unwrap_or(0) == 1),
        )
        .unwrap_or(false);
    if is_master_library {
        return Ok(Vec::new());
    }

    let (frame, source_type) = crate::calibration::manual::synthesize_frame_for_set(&conn, set_id)?;
    let config = load_config(&conn);

    let cal_type_key = match calibration_type.to_lowercase().as_str() {
        "dark" => "Dark",
        "darkflat" => "DarkFlat",
        "bias" => "Bias",
        _ => "",
    };
    // Always include the currently-linked sub-cal set, regardless of score.
    let current_link_set_id: Option<i64> = if !cal_type_key.is_empty() {
        let links = crate::db::calibration_links::get_links_for_calibration_set(&conn, set_id).unwrap_or_default();
        links.iter().find(|l| l.calibration_type == cal_type_key).map(|l| l.calibration_set_id)
    } else {
        None
    };

    // Manual sub-cal modal: same lenient policy — show every candidate, with
    // the engine's honest score (0 for hard-filter rejects).
    let candidates = find_calibration_candidates(
        &conn,
        &frame,
        &source_type,
        &calibration_type.to_lowercase(),
        &config,
        CandidateMode::IncludeIncompatible,
    )?;

    let mut out: Vec<CalibrationSetWithScore> = Vec::with_capacity(candidates.len());
    let mut current_seen = false;
    for candidate in candidates {
        let is_current = current_link_set_id == Some(candidate.set_id);
        if is_current {
            current_seen = true;
        }
        if !show_all && !is_current && candidate.match_score < 0.1 {
            continue;
        }
        if let Some(swc) = crate::calibration::manual::load_set_with_score(&conn, &candidate)? {
            out.push(swc);
        }
    }
    if let (false, Some(cur_id)) = (current_seen, current_link_set_id) {
        let placeholder = crate::calibration::finder::CalibrationCandidate {
            set_id: cur_id,
            imagetyp: crate::models::ImageType::from_str(cal_type_key).unwrap_or(crate::models::ImageType::Dark),
            match_score: 0.0,
            date_diff_days: 0,
            temp_diff: None,
            date_warning: false,
            temp_warning: false,
            is_master: false,
            passed_hard_filter: false,
            details: crate::calibration::finder::CandidateMatchDetails::default(),
            warnings: Vec::new(),
        };
        if let Some(swc) = crate::calibration::manual::load_set_with_score(&conn, &placeholder)? {
            out.push(swc);
        }
    }
    Ok(out)
}

/// Manually assign a sub-calibration set to a calibration set.
pub fn manual_assign_subcalibration(
    ctx: &ServiceContext,
    source_set_id: i64,
    calibration_set_id: i64,
    calibration_type: String, // "Dark", "DarkFlat", "Bias"
) -> Result<(), ApiError> {
    use crate::db::calibration_links::insert_calibration_link;

    let db = db(ctx)?;
    let conn = db.conn();

    // Validate calibration_type
    let valid_types = ["Dark", "DarkFlat", "Bias"];
    if !valid_types.contains(&calibration_type.as_str()) {
        return Err(ApiError::Invalid(format!("Invalid sub-calibration type: {}", calibration_type)));
    }

    // Create the calibration link with source_type = 'calibration_set'
    let link = CalibrationLink {
        id: None,
        source_id: source_set_id,
        source_type: "calibration_set".to_string(),
        calibration_set_id,
        calibration_type: calibration_type.clone(),
        matched_at: Utc::now().to_rfc3339(),
        match_score: Some(1.0), // Manual assignment gets perfect score
        date_warning: false,
        temp_warning: false,
        is_manual_override: true,
    };

    insert_calibration_link(&conn, &link).map_err(|e| ApiError::Internal(format!("Failed to assign sub-calibration: {}", e)))?;

    tracing::info!(
        calibration_type = %calibration_type,
        set_id = calibration_set_id,
        source_set_id,
        "manually assigned sub-calibration"
    );

    Ok(())
}

/// Clear sub-calibration override for a calibration set.
pub fn clear_subcalibration_override(
    ctx: &ServiceContext,
    source_set_id: i64,
    calibration_type: Option<String>, // None = clear all sub-calibrations
) -> Result<usize, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let deleted = match &calibration_type {
        Some(ct) => conn.execute(
            "DELETE FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = 'calibration_set' AND calibration_type = ?2",
            rusqlite::params![source_set_id, ct],
        )?,
        None => conn.execute(
            "DELETE FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = 'calibration_set'",
            rusqlite::params![source_set_id],
        )?,
    };

    tracing::info!(count = deleted, set_id = source_set_id, "cleared sub-calibration link(s)");

    Ok(deleted)
}

// ========== Custom Metadata Editing Commands ==========

/// Bulk update calibration set metadata.
/// Saves original values to calibration_set_originals table before updating.
///
/// Web pre-conversion lost desktop's per-field/per-set formatted error
/// context (`"Failed to update temp for set {}: {}"` etc — web fell back to
/// a generic `db_err` with no field/set context) and the per-field/per-set
/// `tracing::debug!` calls. Desktop's formatted messages and logging are
/// authoritative and restored here for both platforms — failure severity
/// (fail the whole command) was already identical on both sides, only the
/// diagnostic text/log volume improves on web.
pub fn bulk_update_calibration_metadata(
    ctx: &ServiceContext,
    set_ids: Vec<i64>,
    edits: CalibrationMetadataEdits,
) -> Result<usize, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    if set_ids.is_empty() {
        return Err(ApiError::Invalid("No set IDs provided".to_string()));
    }

    let now = Utc::now().to_rfc3339();
    let mut updated_count = 0;

    // All-or-nothing across the whole selection: matches the existing
    // failure semantics (any per-set error fails the command) — previously
    // earlier sets stayed committed and the failing set stayed half-written
    // (audit I1).
    let sp = crate::db::SavepointGuard::new(&conn, "bulk_update_calibration_metadata")
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    for set_id in &set_ids {
        // First, check if we already have originals saved for this set
        let has_originals: bool = conn
            .query_row("SELECT COUNT(*) > 0 FROM calibration_set_originals WHERE set_id = ?1", [set_id], |row| row.get(0))
            .unwrap_or(false);

        // If no originals exist, save current values before editing
        if !has_originals {
            conn.execute(
                "INSERT INTO calibration_set_originals (set_id, ccd_temp, temp_min, temp_max, gain, offset, binning, exptime, saved_at)
                 SELECT id, ccd_temp, temp_min, temp_max, gain, offset, binning, exptime, ?2
                 FROM calibration_set WHERE id = ?1",
                rusqlite::params![set_id, now],
            )
            .map_err(|e| ApiError::Internal(format!("Failed to save originals for set {}: {}", set_id, e)))?;
        }

        // Build update statements for each field that should be changed
        let mut any_update = false;

        if let Some(temp) = edits.ccd_temp {
            conn.execute(
                "UPDATE calibration_set SET ccd_temp = ?1, temp_min = ?1, temp_max = ?1 WHERE id = ?2",
                rusqlite::params![temp, set_id],
            )
            .map_err(|e| ApiError::Internal(format!("Failed to update temp for set {}: {}", set_id, e)))?;
            any_update = true;
            tracing::debug!(set_id, ccd_temp = temp, "updated calibration set field");
        }

        if let Some(gain) = edits.gain {
            conn.execute("UPDATE calibration_set SET gain = ?1 WHERE id = ?2", rusqlite::params![gain, set_id])
                .map_err(|e| ApiError::Internal(format!("Failed to update gain for set {}: {}", set_id, e)))?;
            any_update = true;
            tracing::debug!(set_id, gain, "updated calibration set field");
        }

        if let Some(offset) = edits.offset {
            conn.execute("UPDATE calibration_set SET offset = ?1 WHERE id = ?2", rusqlite::params![offset, set_id])
                .map_err(|e| ApiError::Internal(format!("Failed to update offset for set {}: {}", set_id, e)))?;
            any_update = true;
            tracing::debug!(set_id, offset, "updated calibration set field");
        }

        if let Some(ref binning) = edits.binning {
            conn.execute("UPDATE calibration_set SET binning = ?1 WHERE id = ?2", rusqlite::params![binning, set_id])
                .map_err(|e| ApiError::Internal(format!("Failed to update binning for set {}: {}", set_id, e)))?;
            any_update = true;
            tracing::debug!(set_id, binning = %binning, "updated calibration set field");
        }

        if let Some(exptime) = edits.exptime {
            conn.execute("UPDATE calibration_set SET exptime = ?1 WHERE id = ?2", rusqlite::params![exptime, set_id])
                .map_err(|e| ApiError::Internal(format!("Failed to update exptime for set {}: {}", set_id, e)))?;
            any_update = true;
            tracing::debug!(set_id, exptime, "updated calibration set field");
        }

        if !any_update {
            continue; // No fields to update
        }

        // Propagate edits down to the member frames so frames.* stays in
        // sync with the calibration_set the user just edited. Without this
        // an edit on a master flat / dark only retunes calibration matching
        // — frames.ccd_temp etc. would still report stale values to anything
        // querying frames directly (catalog views, dual-pane editor, etc.).
        // Frames are flagged override = 1 to protect them from scanner
        // re-overwrites on the next rescan. Reset-to-original on the set
        // restores the set columns only; the frame override stays.
        let mut frame_set_clauses: Vec<&str> = Vec::new();
        let mut frame_values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(temp) = edits.ccd_temp {
            frame_set_clauses.push("ccd_temp = ?");
            frame_values.push(rusqlite::types::Value::Real(temp));
        }
        if let Some(gain) = edits.gain {
            frame_set_clauses.push("gain = ?");
            frame_values.push(rusqlite::types::Value::Real(gain));
        }
        if let Some(offset) = edits.offset {
            frame_set_clauses.push("offset = ?");
            frame_values.push(rusqlite::types::Value::Real(offset));
        }
        if let Some(ref binning) = edits.binning {
            frame_set_clauses.push("binning = ?");
            frame_values.push(rusqlite::types::Value::Text(binning.clone()));
            // Mirror the AxB string into xbinning / ybinning so derived
            // queries see consistent values.
            if let Some((xs, ys)) = binning.split_once('x') {
                if let (Ok(xb), Ok(yb)) = (xs.parse::<i64>(), ys.parse::<i64>()) {
                    frame_set_clauses.push("xbinning = ?");
                    frame_values.push(rusqlite::types::Value::Integer(xb));
                    frame_set_clauses.push("ybinning = ?");
                    frame_values.push(rusqlite::types::Value::Integer(yb));
                }
            }
        }
        if let Some(exptime) = edits.exptime {
            frame_set_clauses.push("exptime = ?");
            frame_values.push(rusqlite::types::Value::Real(exptime));
        }
        if !frame_set_clauses.is_empty() {
            let sql = format!(
                "UPDATE frames SET {}, override = 1
                 WHERE id IN (SELECT frame_id FROM calibration_set_frames WHERE set_id = ?)",
                frame_set_clauses.join(", "),
            );
            let mut all_values = frame_values.clone();
            all_values.push(rusqlite::types::Value::Integer(*set_id));
            let n = conn
                .execute(&sql, rusqlite::params_from_iter(all_values.iter()))
                .map_err(|e| ApiError::Internal(format!("Failed to propagate edits to frames for set {}: {}", set_id, e)))?;
            tracing::debug!(set_id, count = n, "propagated edits to member frames");
        }

        updated_count += 1;
    }

    sp.commit().map_err(|e| ApiError::Internal(e.to_string()))?;

    tracing::info!(count = updated_count, "updated calibration set metadata");

    Ok(updated_count)
}

/// Bulk restore calibration set metadata from originals.
pub fn bulk_restore_calibration_metadata(ctx: &ServiceContext, set_ids: Vec<i64>) -> Result<usize, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    if set_ids.is_empty() {
        return Err(ApiError::Invalid("No set IDs provided".to_string()));
    }

    let mut restored_count = 0;

    for set_id in &set_ids {
        // Restore from originals
        let rows_affected = conn
            .execute(
                "UPDATE calibration_set SET
                    ccd_temp = (SELECT ccd_temp FROM calibration_set_originals WHERE set_id = ?1),
                    temp_min = (SELECT temp_min FROM calibration_set_originals WHERE set_id = ?1),
                    temp_max = (SELECT temp_max FROM calibration_set_originals WHERE set_id = ?1),
                    gain = (SELECT gain FROM calibration_set_originals WHERE set_id = ?1),
                    offset = (SELECT offset FROM calibration_set_originals WHERE set_id = ?1),
                    binning = (SELECT binning FROM calibration_set_originals WHERE set_id = ?1),
                    exptime = (SELECT exptime FROM calibration_set_originals WHERE set_id = ?1)
                 WHERE id = ?1 AND EXISTS (SELECT 1 FROM calibration_set_originals WHERE set_id = ?1)",
                rusqlite::params![set_id],
            )
            .map_err(|e| ApiError::Internal(format!("Failed to restore set {}: {}", set_id, e)))?;

        if rows_affected > 0 {
            // Delete the originals entry after successful restore
            conn.execute("DELETE FROM calibration_set_originals WHERE set_id = ?1", rusqlite::params![set_id])
                .map_err(|e| ApiError::Internal(format!("Failed to delete originals for set {}: {}", set_id, e)))?;

            restored_count += 1;
        }
    }

    tracing::info!(count = restored_count, "restored original calibration set metadata");

    Ok(restored_count)
}

/// Get all set IDs that have custom metadata edits for a given camera.
pub fn get_custom_metadata_set_ids(ctx: &ServiceContext, instrume: String) -> Result<Vec<i64>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let mut stmt = conn.prepare(
        "SELECT cso.set_id FROM calibration_set_originals cso
         JOIN calibration_set cs ON cs.id = cso.set_id
         WHERE cs.instrume = ?1",
    )?;

    let ids: Vec<i64> = stmt.query_map([&instrume], |row| row.get(0))?.filter_map(|r| r.ok()).collect();

    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn seed_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    /// Raw Dark calibration set with one member frame per entry in `times`
    /// (all shot on `date`, minutes apart so they cluster into one group).
    fn seed_raw_dark_set(conn: &Connection, date: &str, times: &[&str]) -> (i64, Vec<i64>) {
        let date_start = format!("{date}T{}Z", times.first().unwrap());
        let date_end = format!("{date}T{}Z", times.last().unwrap());
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, date,
              date_start, date_end, temp_min, temp_max, frame_count)
             VALUES ('Dark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', ?1, ?2, ?3, -10.5, -9.5, ?4)",
            rusqlite::params![date, date_start, date_end, times.len() as i64],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();
        let mut frame_ids = Vec::new();
        for (i, t) in times.iter().enumerate() {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 100, ?3, 'FITS')",
                rusqlite::params![format!("/cal/{date}/d{i}.fits"), format!("d{i}.fits"), date],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset, binning, ccd_temp, date_obs)
                 VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', -10.0, ?2)",
                rusqlite::params![file_id, format!("{date}T{t}Z")],
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id],
            )
            .unwrap();
            frame_ids.push(frame_id);
        }
        (set_id, frame_ids)
    }

    /// Master set (is_master_library = 1) with its single MasterDark member.
    fn seed_master_set(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES ('/lib/master_dark.fits', 'master_dark.fits', 100, '2026-06-02', 'FITS')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset, binning, ccd_temp, date_obs)
             VALUES (?1, 'MasterDark', 'TestCam', 300.0, 100.0, 50.0, '1x1', -10.0, '2026-06-02T00:00:00Z')",
            [file_id],
        )
        .unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, date,
              date_start, date_end, temp_min, temp_max, frame_count, is_master_library)
             VALUES ('MasterDark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', '2026-06-02',
              '2026-06-02T00:00:00Z', '2026-06-02T00:00:00Z', -10.0, -10.0, 1, 1)",
            [],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        )
        .unwrap();
        set_id
    }

    /// Mark `raw_set_id` superseded by `master_set_id` and record provenance,
    /// like `calibration_library::register` does in one transaction.
    fn supersede(conn: &Connection, raw_set_id: i64, master_set_id: i64) {
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            rusqlite::params![master_set_id, raw_set_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO master_provenance
             (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (?1, ?2, '{}', '[]', 'hash', '2026-06-02T00:00:00Z')",
            rusqlite::params![master_set_id, raw_set_id],
        )
        .unwrap();
    }

    /// Regression: refresh must leave superseded raw sets (frozen provenance
    /// history) untouched. Pre-fix, Step 1 emptied them, the empty-prune
    /// trigger tried to delete them, and `master_provenance.source_set_id`
    /// (FK, NO ACTION) aborted the whole command with
    /// "FOREIGN KEY constraint failed".
    #[test]
    fn refresh_skips_superseded_sets() {
        let mut conn = seed_db();
        let (raw_id, raw_frames) = seed_raw_dark_set(&conn, "2026-06-01", &["20:00:00", "20:10:00"]);
        let master_id = seed_master_set(&conn);
        supersede(&conn, raw_id, master_id);
        let (live_id, _) = seed_raw_dark_set(&conn, "2026-06-15", &["21:00:00", "21:10:00"]);

        refresh_calibration_library_inner(&mut conn, "TestCam").unwrap();

        let (count, superseded_by): (i64, Option<i64>) = conn
            .query_row(
                "SELECT frame_count, superseded_by_set_id FROM calibration_set WHERE id = ?1",
                [raw_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(superseded_by, Some(master_id), "supersede pointer must survive");
        assert_eq!(count, 2, "frozen membership must not be cleared");
        for fid in &raw_frames {
            let set_of: i64 = conn
                .query_row("SELECT set_id FROM calibration_set_frames WHERE frame_id = ?1", [fid], |r| r.get(0))
                .unwrap();
            assert_eq!(set_of, raw_id, "superseded member must not be re-clustered into a live set");
        }
        let prov: i64 = conn
            .query_row("SELECT COUNT(*) FROM master_provenance WHERE source_set_id = ?1", [raw_id], |r| r.get(0))
            .unwrap();
        assert_eq!(prov, 1, "provenance row must survive");
        let live: i64 = conn
            .query_row("SELECT COUNT(*) FROM calibration_set WHERE id = ?1", [live_id], |r| r.get(0))
            .unwrap();
        assert_eq!(live, 1, "live set must survive the refresh");
    }

    /// Refresh exists to preserve set identity: a live raw set must keep its
    /// row (same id) and its consumer links across the clear + recluster
    /// cycle. Pre-fix, the Phase-2 empty-prune trigger fired while Step 1
    /// emptied the set, deleted the row, and ON DELETE CASCADE silently wiped
    /// the light's calibration link (including manual overrides).
    #[test]
    fn refresh_preserves_live_set_identity_and_consumer_links() {
        let mut conn = seed_db();
        let (live_id, _) = seed_raw_dark_set(&conn, "2026-06-15", &["21:00:00", "21:10:00"]);
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES ('/l/light.fits', 'light.fits', 100, '2026-06-15', 'FITS')",
            [],
        )
        .unwrap();
        let light_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, instrume, exptime) VALUES (?1, 'Light', 'TestCam', 300.0)",
            [light_file],
        )
        .unwrap();
        let light_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
             VALUES (?1, 'frame', ?2, 'Dark', 0.9, 1)",
            rusqlite::params![light_id, live_id],
        )
        .unwrap();

        refresh_calibration_library_inner(&mut conn, "TestCam").unwrap();

        let count: i64 = conn
            .query_row("SELECT frame_count FROM calibration_set WHERE id = ?1", [live_id], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "set must be refilled under its original id");
        let link: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_to_frames WHERE calibration_set_id = ?1 AND source_id = ?2",
                rusqlite::params![live_id, light_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(link, 1, "consumer link must survive refresh");
    }

    /// audit I1: the per-set loop is all-or-nothing. Any per-set error fails
    /// the whole command, so a failure on set N must not leave sets 1..N-1
    /// committed (and set N half-written, with an originals row saved for an
    /// update that never landed). The abort is injected with a trigger
    /// scoped to the second set — deterministic, and the same shape as a
    /// real constraint failure on the calibration_set UPDATE.
    #[test]
    fn bulk_update_calibration_metadata_rolls_back_earlier_sets_when_a_later_one_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let (first_id, second_id) = {
            let conn = db(&ctx).unwrap().conn();
            let (first_id, _) = seed_raw_dark_set(&conn, "2026-06-01", &["20:00:00", "20:10:00"]);
            let (second_id, _) = seed_raw_dark_set(&conn, "2026-06-02", &["20:00:00", "20:10:00"]);
            // CREATE TRIGGER cannot take bound parameters — the id is
            // interpolated into the stored trigger text.
            conn.execute_batch(&format!(
                "CREATE TRIGGER injected_calibration_set_failure
                 BEFORE UPDATE ON calibration_set WHEN NEW.id = {second_id}
                 BEGIN SELECT RAISE(ABORT, 'injected per-set failure'); END"
            ))
            .unwrap();
            (first_id, second_id)
        };

        let edits = CalibrationMetadataEdits {
            ccd_temp: Some(-20.0),
            gain: None,
            offset: None,
            binning: None,
            exptime: None,
        };
        let res = bulk_update_calibration_metadata(&ctx, vec![first_id, second_id], edits);
        assert!(
            res.is_err(),
            "injected per-set failure must fail the command"
        );

        let conn = db(&ctx).unwrap().conn();
        let temp: f64 = conn
            .query_row(
                "SELECT ccd_temp FROM calibration_set WHERE id = ?1",
                [first_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(temp, -10.0, "the earlier set's edit must be rolled back");
        let frame_temps: Vec<f64> = conn
            .prepare(
                "SELECT ccd_temp FROM frames
                 WHERE id IN (SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1)",
            )
            .unwrap()
            .query_map([first_id], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            frame_temps.iter().all(|t| *t == -10.0),
            "the earlier set's member-frame propagation must be rolled back, got {frame_temps:?}"
        );
        let originals: i64 = conn
            .query_row("SELECT COUNT(*) FROM calibration_set_originals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            originals, 0,
            "no originals row may survive a failed command"
        );
    }
}
