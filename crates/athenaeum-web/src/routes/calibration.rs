// Calibration route handlers — mirrors athenaeum-tauri/src/commands/calibration.rs

use athenaeum_core::calibration;
use athenaeum_core::calibration::configurable_matcher;
use athenaeum_core::calibration::processor;
use athenaeum_core::calibration::scan_integration::{
    create_calibration_sets_from_scan_with_masters, MasterFrameIds,
};
use athenaeum_core::db;
use athenaeum_core::calibration::CalibrationMatchingConfig;
use athenaeum_core::models::CalibrationTolerance;
use axum::{extract::State, http::StatusCode, Json};
use std::collections::HashMap;

use crate::WebAppState;

// ── Error helpers ─────────────────────────────────────────────────────────────

fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    eprintln!("calibration error: {}", s);
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    eprintln!("calibration error: database not initialized");
    (StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string())
}

// ── Constant ──────────────────────────────────────────────────────────────────

const CALIBRATION_CONFIG_KEY: &str = "calibration.matching_config";

// ── Request body types ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumeArgs {
    pub instrume: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIdArgs {
    pub set_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindCalibrationArgs {
    pub frame_set_id: i64,
    pub flat_date_warning_days: Option<i64>,
    pub dark_date_warning_days: Option<i64>,
    pub flat_pattern: Option<String>,
    pub manual_flat_selections: Option<HashMap<String, i64>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frame_set_id: i64,
}

// ── Equipment & dark library ──────────────────────────────────────────────────

/// POST /api/get_equipment_cameras
///
/// Returns all cameras (instrume) that have frames in the catalog.
pub async fn get_equipment_cameras(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Vec<athenaeum_core::models::CameraStats>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let cameras = db::get_all_cameras(&conn).map_err(db_err)?;
    Ok(Json(cameras))
}

/// POST /api/get_dark_library
///
/// Retrieve existing dark calibration sets for a camera.
pub async fn get_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetDetail>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let sets = db::get_camera_dark_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(sets))
}

/// POST /api/has_dark_library
///
/// Check whether a dark library exists for a camera.
pub async fn has_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let has = db::has_dark_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(has))
}

// ── Master dark library ───────────────────────────────────────────────────────

/// POST /api/create_master_dark_library
///
/// Build calibration sets from master dark frames for a camera.
pub async fn create_master_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<athenaeum_core::models::DarkLibraryResult>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let date_threshold = state.ctx.settings
        .get_dark_library_date_threshold(&conn)
        .map_err(db_err)?;

    let temp_threshold = state.ctx.settings
        .get_dark_library_temp_threshold(&conn)
        .map_err(db_err)?;

    let result = calibration::create_master_dark_library(
        &conn,
        &args.instrume,
        date_threshold,
        temp_threshold,
    ).map_err(db_err)?;

    Ok(Json(result))
}

/// POST /api/get_master_dark_library
///
/// Retrieve master dark calibration sets for a camera.
pub async fn get_master_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetDetail>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let sets = db::get_camera_master_dark_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(sets))
}

/// POST /api/has_master_dark_library
///
/// Check whether a master dark library exists for a camera.
pub async fn has_master_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let has = db::has_master_dark_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(has))
}

// ── Master flat library ───────────────────────────────────────────────────────

/// POST /api/create_master_flat_library
///
/// Build calibration sets from master flat frames for a camera.
pub async fn create_master_flat_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<athenaeum_core::models::DarkLibraryResult>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let date_threshold = state.ctx.settings
        .get_dark_library_date_threshold(&conn)
        .map_err(db_err)?;

    let temp_threshold = state.ctx.settings
        .get_dark_library_temp_threshold(&conn)
        .map_err(db_err)?;

    let result = calibration::create_master_flat_library(
        &conn,
        &args.instrume,
        date_threshold,
        temp_threshold,
    ).map_err(db_err)?;

    Ok(Json(result))
}

/// POST /api/get_master_flat_library
///
/// Retrieve master flat calibration sets for a camera.
pub async fn get_master_flat_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetDetail>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let sets = db::get_camera_master_flat_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(sets))
}

/// POST /api/has_master_flat_library
///
/// Check whether a master flat library exists for a camera.
pub async fn has_master_flat_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let has = db::has_master_flat_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(has))
}

// ── Library refresh & frame retrieval ────────────────────────────────────────

/// POST /api/refresh_calibration_library_for_camera
///
/// Rebuild the calibration library for a single camera by re-running grouping
/// while preserving calibration set IDs for stability:
///   1. Clear frame memberships (keep sets to preserve IDs)
///   2. Re-query all calibration frame IDs for this camera
///   3. Re-cluster and assign frames to sets
///   4. Delete orphaned sets (sets with 0 frames)
pub async fn refresh_calibration_library_for_camera(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<athenaeum_core::calibration::scan_integration::CalibrationScanResult>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let instrume = &args.instrume;

    eprintln!("Refreshing calibration library for camera: {}", instrume);

    // Step 1: Clear frame memberships for this camera's non-master sets
    conn.execute(
        "DELETE FROM calibration_set_frames
         WHERE set_id IN (
             SELECT id FROM calibration_set WHERE instrume = ?1 AND is_master_library = 0
         )",
        rusqlite::params![instrume],
    ).map_err(|e| db_err(format!("Failed to clear frame memberships: {}", e)))?;

    // Reset frame counts to 0 for these sets
    conn.execute(
        "UPDATE calibration_set SET frame_count = 0
         WHERE instrume = ?1 AND is_master_library = 0",
        rusqlite::params![instrume],
    ).map_err(|e| db_err(format!("Failed to reset frame counts: {}", e)))?;

    // Step 2: Query all calibration frame IDs for this camera
    let flat_frame_ids = query_frame_ids_by_type(&conn, instrume, "FLAT")
        .map_err(|e| db_err(format!("Failed to query flat frames: {}", e)))?;

    let dark_frame_ids = query_frame_ids_by_type(&conn, instrume, "DARK")
        .map_err(|e| db_err(format!("Failed to query dark frames: {}", e)))?;

    let bias_frame_ids = query_frame_ids_by_type(&conn, instrume, "BIAS")
        .map_err(|e| db_err(format!("Failed to query bias frames: {}", e)))?;

    let darkflat_frame_ids = query_frame_ids_by_type(&conn, instrume, "DARKFLAT")
        .map_err(|e| db_err(format!("Failed to query darkflat frames: {}", e)))?;

    let master_dark_ids = query_frame_ids_by_type(&conn, instrume, "MASTERDARK")
        .map_err(|e| db_err(format!("Failed to query master dark frames: {}", e)))?;
    let master_flat_ids = query_frame_ids_by_type(&conn, instrume, "MASTERFLAT")
        .map_err(|e| db_err(format!("Failed to query master flat frames: {}", e)))?;
    let master_bias_ids = query_frame_ids_by_type(&conn, instrume, "MASTERBIAS")
        .map_err(|e| db_err(format!("Failed to query master bias frames: {}", e)))?;
    let master_darkflat_ids = query_frame_ids_by_type(&conn, instrume, "MASTERDARKFLAT")
        .map_err(|e| db_err(format!("Failed to query master darkflat frames: {}", e)))?;

    let master_frame_ids = MasterFrameIds {
        master_dark_ids,
        master_flat_ids,
        master_bias_ids,
        master_darkflat_ids,
    };

    // Step 3: Recreate calibration sets using the scan algorithm
    let result = create_calibration_sets_from_scan_with_masters(
        &conn,
        flat_frame_ids,
        dark_frame_ids,
        bias_frame_ids,
        darkflat_frame_ids,
        master_frame_ids,
    ).map_err(|e| db_err(format!("Failed to create calibration sets: {}", e)))?;

    // Step 4: Delete orphaned sets (sets with no frames after reclustering)
    let deleted_orphans = conn.execute(
        "DELETE FROM calibration_set
         WHERE instrume = ?1 AND is_master_library = 0 AND frame_count = 0",
        rusqlite::params![instrume],
    ).map_err(|e| db_err(format!("Failed to delete orphaned sets: {}", e)))?;

    if deleted_orphans > 0 {
        eprintln!("Deleted {} orphaned calibration sets", deleted_orphans);
    }

    eprintln!(
        "Refresh complete - {} calibration sets active (IDs preserved)",
        result.sets_created
    );

    Ok(Json(result))
}

/// POST /api/get_calibration_set_frames
///
/// List the files/frames belonging to a calibration set.
pub async fn get_calibration_set_frames(
    State(state): State<WebAppState>,
    Json(args): Json<SetIdArgs>,
) -> Result<Json<Vec<athenaeum_core::models::FileWithFrame>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let frames = db::get_frames_for_calibration_set(&conn, args.set_id).map_err(db_err)?;
    Ok(Json(frames))
}

/// POST /api/get_calibration_set_consumers
///
/// List the frame sets (objects) that ultimately consume a calibration set,
/// including transitive consumers via the sub-cal chain.
pub async fn get_calibration_set_consumers(
    State(state): State<WebAppState>,
    Json(args): Json<SetIdArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetConsumer>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let consumers = db::calibration_links::get_calibration_set_consumers(&conn, args.set_id)
        .map_err(db_err)?;
    Ok(Json(consumers))
}

// ── Calibration finder ────────────────────────────────────────────────────────

/// POST /api/find_calibration_for_frame_set
///
/// Match and link calibration frames to all light frames in a frame set.
/// Loads config for tolerance defaults, then delegates to
/// `athenaeum_core::calibration::processor::process_frame_set`.
pub async fn find_calibration_for_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FindCalibrationArgs>,
) -> Result<Json<processor::ProcessingStats>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let frame_set_id = args.frame_set_id;

    // Get frame set metadata (flat_pattern, date ranges)
    let mut stmt = conn.prepare(
        "SELECT flat_pattern, date_obs_start, date_obs_end FROM frames_set WHERE id = ?1",
    ).map_err(|e| db_err(format!("Failed to prepare frame set query: {}", e)))?;

    let (stored_flat_pattern, _date_obs_start, _date_obs_end): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = stmt
        .query_row([frame_set_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|e| db_err(format!("Frame set not found: {}", e)))?;

    // Use provided flat_pattern or fall back to stored pattern
    let final_flat_pattern = args.flat_pattern.or(stored_flat_pattern);

    // Load calibration config
    let config = configurable_matcher::load_config(&conn);

    // Get flat calibration settings from config
    let max_age_days = config
        .clustering
        .get("flat")
        .map(|c| c.max_age_days)
        .unwrap_or(30);
    let time_cluster_minutes = config
        .clustering
        .get("flat")
        .map(|c| c.time_cluster_minutes)
        .unwrap_or(30);
    let temp_weight = config.scoring.temperature_match_weight;

    // Build tolerance from parameters, config warnings, or defaults
    let tolerance = CalibrationTolerance {
        flat_date_warning_days: args
            .flat_date_warning_days
            .unwrap_or(config.warnings.flat_date_warning_days),
        dark_date_warning_days: args
            .dark_date_warning_days
            .unwrap_or(config.warnings.dark_date_warning_days),
    };

    eprintln!(
        "Finding calibration for frame set {} with tolerance: flat_date={} days, dark_date={} days",
        frame_set_id,
        tolerance.flat_date_warning_days,
        tolerance.dark_date_warning_days,
    );
    eprintln!(
        "Flat settings: pattern={:?}, max_age={} days, time_cluster={} min, temp_weight={}",
        final_flat_pattern, max_age_days, time_cluster_minutes, temp_weight,
    );

    let stats = processor::process_frame_set(
        &conn,
        frame_set_id,
        &tolerance,
        final_flat_pattern.as_deref(),
        args.manual_flat_selections.as_ref(),
        max_age_days,
        time_cluster_minutes,
        temp_weight,
    )
    .map_err(|e| db_err(format!("Failed to process frame set: {}", e)))?;

    eprintln!(
        "Calibration processing complete: {} frames, {} with full calibration",
        stats.total_frames, stats.frames_with_full_calibration,
    );

    Ok(Json(stats))
}

// ── Calibration matching config ───────────────────────────────────────────────

/// POST /api/get_calibration_matching_config
///
/// Load the configurable calibration matching rules.
/// Returns defaults if the setting has not been persisted yet.
pub async fn get_calibration_matching_config(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<CalibrationMatchingConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let config_json = db::get_setting(&conn, CALIBRATION_CONFIG_KEY).map_err(db_err)?;

    let config = match config_json {
        Some(json) => CalibrationMatchingConfig::from_json(&json)
            .map(|c| c.migrate())
            .map_err(|e| db_err(format!("Failed to parse calibration config: {}", e)))?,
        None => CalibrationMatchingConfig::default(),
    };

    Ok(Json(config))
}

/// POST /api/set_calibration_matching_config
///
/// Validate and persist updated calibration matching rules.
pub async fn set_calibration_matching_config(
    State(state): State<WebAppState>,
    Json(config): Json<CalibrationMatchingConfig>,
) -> Result<Json<()>, (StatusCode, String)> {
    // Validate before acquiring the DB lock so we fail fast on bad input.
    config.validate().map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let json = config
        .to_json()
        .map_err(|e| db_err(format!("Failed to serialize calibration config: {}", e)))?;

    db::set_setting(&conn, CALIBRATION_CONFIG_KEY, &json).map_err(db_err)?;

    Ok(Json(()))
}

/// POST /api/reset_calibration_matching_config
///
/// Reset calibration matching rules to built-in defaults and persist them.
pub async fn reset_calibration_matching_config(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<CalibrationMatchingConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let default_config = CalibrationMatchingConfig::default();

    let json = default_config
        .to_json()
        .map_err(|e| db_err(format!("Failed to serialize default config: {}", e)))?;

    db::set_setting(&conn, CALIBRATION_CONFIG_KEY, &json).map_err(db_err)?;

    Ok(Json(default_config))
}

// ── Phase 4: Calibration hierarchy, manual selection, sub-calibration, metadata

/// POST /api/get_calibration_hierarchy_for_frame_set
///
/// Get calibration hierarchy organized by Date → Camera → Filter for a frame set.
pub async fn get_calibration_hierarchy_for_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::CalibrationHierarchyView>, (StatusCode, String)> {
    use athenaeum_core::db::calibration_links::get_calibration_hierarchy_for_frame_set as get_hierarchy;

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let hierarchy = get_hierarchy(&conn, args.frame_set_id).map_err(db_err)?;
    Ok(Json(hierarchy))
}

// ── Request types for manual selection routes ─────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLightFrameParametersArgs {
    pub frame_ids: Vec<i64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCalibrationSetsForManualSelectionArgs {
    pub frame_ids: Vec<i64>,
    pub calibration_type: String,
    pub show_all: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualAssignCalibrationArgs {
    pub frame_ids: Vec<i64>,
    pub calibration_set_id: i64,
    pub calibration_type: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSubcalibrationSetsArgs {
    pub set_id: i64,
    pub calibration_type: String,
    pub show_all: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualAssignSubcalibrationArgs {
    pub source_set_id: i64,
    pub calibration_set_id: i64,
    pub calibration_type: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearSubcalibrationOverrideArgs {
    pub source_set_id: i64,
    pub calibration_type: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkUpdateCalibrationMetadataArgs {
    pub set_ids: Vec<i64>,
    pub edits: athenaeum_core::models::CalibrationMetadataEdits,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkRestoreCalibrationMetadataArgs {
    pub set_ids: Vec<i64>,
}

/// POST /api/get_calibration_set_parameters
///
/// Get parameters of a calibration set for sub-calibration selection display.
pub async fn get_calibration_set_parameters(
    State(state): State<WebAppState>,
    Json(args): Json<SetIdArgs>,
) -> Result<Json<athenaeum_core::models::CalibrationSetParameters>, (StatusCode, String)> {
    use athenaeum_core::db::calibration_links::get_links_for_calibration_set;

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let mut stmt = conn.prepare(
        "SELECT id, imagetyp, instrume, binning, gain, offset, exptime, filter,
                ccd_temp, date_start, date_end, frame_count
         FROM calibration_set WHERE id = ?1"
    ).map_err(db_err)?;

    let (imagetyp, instrume, binning, gain, offset, exptime, filter, ccd_temp,
         date_start, date_end, frame_count): (
        String, Option<String>, Option<String>, Option<f64>, Option<f64>,
        Option<f64>, Option<String>, Option<f64>, Option<String>, Option<String>, i64
    ) = stmt.query_row([args.set_id], |row| {
        Ok((
            row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
            row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?,
            row.get(9)?, row.get(10)?, row.get(11)?,
        ))
    }).map_err(|e| db_err(format!("Calibration set not found: {}", e)))?;

    let links = get_links_for_calibration_set(&conn, args.set_id).map_err(db_err)?;

    let current_dark_set_id = links.iter()
        .find(|l| l.calibration_type == "Dark")
        .map(|l| l.calibration_set_id);
    let current_darkflat_set_id = links.iter()
        .find(|l| l.calibration_type == "DarkFlat")
        .map(|l| l.calibration_set_id);
    let current_bias_set_id = links.iter()
        .find(|l| l.calibration_type == "Bias")
        .map(|l| l.calibration_set_id);

    Ok(Json(athenaeum_core::models::CalibrationSetParameters {
        set_id: args.set_id,
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
    }))
}

/// POST /api/get_light_frame_parameters
///
/// Get average parameters of light frames for manual selection display.
pub async fn get_light_frame_parameters(
    State(state): State<WebAppState>,
    Json(args): Json<GetLightFrameParametersArgs>,
) -> Result<Json<athenaeum_core::models::LightFrameParameters>, (StatusCode, String)> {
    use athenaeum_core::db::calibration_links::get_links_for_frame;

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    if args.frame_ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No frame IDs provided".to_string()));
    }

    let params = get_light_params_internal(&conn, &args.frame_ids)?;

    // Get current calibration links for first frame
    let first_frame_id = args.frame_ids[0];
    let links = get_links_for_frame(&conn, first_frame_id).map_err(db_err)?;

    let current_flat_set_id = links.iter()
        .find(|l| l.calibration_type == "Flat")
        .map(|l| l.calibration_set_id);
    let current_dark_set_id = links.iter()
        .find(|l| l.calibration_type == "Dark")
        .map(|l| l.calibration_set_id);
    let current_bias_set_id = links.iter()
        .find(|l| l.calibration_type == "Bias")
        .map(|l| l.calibration_set_id);

    Ok(Json(athenaeum_core::models::LightFrameParameters {
        current_flat_set_id,
        current_dark_set_id,
        current_bias_set_id,
        ..params
    }))
}

/// POST /api/get_calibration_sets_for_manual_selection
///
/// Get calibration sets with match scores for manual selection. Routes through
/// the same `find_calibration_candidates` engine that auto-link uses, so the
/// modal's score and "compatible" decision agree with what "Find Calibration"
/// will actually pick.
pub async fn get_calibration_sets_for_manual_selection(
    State(state): State<WebAppState>,
    Json(args): Json<GetCalibrationSetsForManualSelectionArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetWithScore>>, (StatusCode, String)> {
    use athenaeum_core::calibration::configurable_matcher::{find_calibration_candidates, load_config};
    use athenaeum_core::calibration::finder::CandidateMode;
    use athenaeum_core::calibration::manual::{load_set_with_score, synthesize_frame_for_lights};

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    if args.frame_ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No frame IDs provided".to_string()));
    }
    match args.calibration_type.to_lowercase().as_str() {
        "flat" | "dark" | "bias" | "darkflat" => {}
        _ => return Err((StatusCode::BAD_REQUEST, format!("Invalid calibration type: {}", args.calibration_type))),
    }

    let frame = synthesize_frame_for_lights(&conn, &args.frame_ids).map_err(internal_err)?;
    let config = load_config(&conn);

    let cal_type_key = match args.calibration_type.to_lowercase().as_str() {
        "flat" => "Flat",
        "dark" => "Dark",
        "bias" => "Bias",
        "darkflat" => "DarkFlat",
        _ => "",
    };
    let current_link_set_id: Option<i64> = if !args.frame_ids.is_empty() && !cal_type_key.is_empty() {
        let links = athenaeum_core::db::calibration_links::get_links_for_frame(&conn, args.frame_ids[0])
            .unwrap_or_default();
        links.iter()
            .find(|l| l.calibration_type == cal_type_key)
            .map(|l| l.calibration_set_id)
    } else {
        None
    };

    // Manual modal: always include incompatible candidates with engine's
    // score (0 for hard-filter rejects). Always include the currently-linked
    // set so the "Current" badge can render.
    let candidates = find_calibration_candidates(
        &conn, &frame, "lights", &args.calibration_type.to_lowercase(),
        &config, CandidateMode::IncludeIncompatible,
    ).map_err(internal_err)?;

    let mut out = Vec::with_capacity(candidates.len());
    let mut current_seen = false;
    for candidate in candidates {
        let is_current = current_link_set_id == Some(candidate.set_id);
        if is_current { current_seen = true; }
        if !args.show_all && !is_current && candidate.match_score < 0.1 {
            continue;
        }
        if let Some(swc) = load_set_with_score(&conn, &candidate).map_err(internal_err)? {
            out.push(swc);
        }
    }
    if let (false, Some(cur_id)) = (current_seen, current_link_set_id) {
        let placeholder = athenaeum_core::calibration::finder::CalibrationCandidate {
            set_id: cur_id,
            imagetyp: athenaeum_core::models::ImageType::from_str(cal_type_key)
                .unwrap_or(athenaeum_core::models::ImageType::Dark),
            match_score: 0.0,
            date_diff_days: 0,
            temp_diff: None,
            date_warning: false,
            temp_warning: false,
            is_master: false,
            passed_hard_filter: false,
            details: athenaeum_core::calibration::finder::CandidateMatchDetails::default(),
            warnings: Vec::new(),
        };
        if let Some(swc) = load_set_with_score(&conn, &placeholder).map_err(internal_err)? {
            out.push(swc);
        }
    }
    Ok(Json(out))
}

/// POST /api/get_subcalibration_sets_for_manual_selection
///
/// Sub-calibration candidates for a Flat or Dark set. Routes through
/// `find_calibration_candidates` with the parent set's parameters synthesized
/// as a frame so the engine's `flats→{darkflat,dark,bias}` and `darks→bias`
/// configs apply.
pub async fn get_subcalibration_sets_for_manual_selection(
    State(state): State<WebAppState>,
    Json(args): Json<GetSubcalibrationSetsArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetWithScore>>, (StatusCode, String)> {
    use athenaeum_core::calibration::configurable_matcher::{find_calibration_candidates, load_config};
    use athenaeum_core::calibration::finder::CandidateMode;
    use athenaeum_core::calibration::manual::{load_set_with_score, synthesize_frame_for_set};

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    match args.calibration_type.to_lowercase().as_str() {
        "dark" | "darkflat" | "bias" => {}
        _ => return Err((StatusCode::BAD_REQUEST, format!("Invalid calibration type for sub-calibration: {}", args.calibration_type))),
    }

    let is_master: bool = conn
        .query_row(
            "SELECT is_master_library FROM calibration_set WHERE id = ?1",
            [args.set_id],
            |row| Ok(row.get::<_, i32>(0).unwrap_or(0) == 1),
        )
        .unwrap_or(false);
    if is_master {
        return Ok(Json(Vec::new()));
    }

    let (frame, source_type) = synthesize_frame_for_set(&conn, args.set_id).map_err(internal_err)?;
    let config = load_config(&conn);

    let cal_type_key = match args.calibration_type.to_lowercase().as_str() {
        "dark" => "Dark",
        "darkflat" => "DarkFlat",
        "bias" => "Bias",
        _ => "",
    };
    let current_link_set_id: Option<i64> = if !cal_type_key.is_empty() {
        let links = athenaeum_core::db::calibration_links::get_links_for_calibration_set(&conn, args.set_id)
            .unwrap_or_default();
        links.iter()
            .find(|l| l.calibration_type == cal_type_key)
            .map(|l| l.calibration_set_id)
    } else {
        None
    };

    // Manual sub-cal modal: same lenient policy. Score = 0 for hard-filter
    // rejects. Always surface currently-linked set.
    let candidates = find_calibration_candidates(
        &conn, &frame, &source_type, &args.calibration_type.to_lowercase(),
        &config, CandidateMode::IncludeIncompatible,
    ).map_err(internal_err)?;

    let mut out = Vec::with_capacity(candidates.len());
    let mut current_seen = false;
    for candidate in candidates {
        let is_current = current_link_set_id == Some(candidate.set_id);
        if is_current { current_seen = true; }
        if !args.show_all && !is_current && candidate.match_score < 0.1 {
            continue;
        }
        if let Some(swc) = load_set_with_score(&conn, &candidate).map_err(internal_err)? {
            out.push(swc);
        }
    }
    if let (false, Some(cur_id)) = (current_seen, current_link_set_id) {
        let placeholder = athenaeum_core::calibration::finder::CalibrationCandidate {
            set_id: cur_id,
            imagetyp: athenaeum_core::models::ImageType::from_str(cal_type_key)
                .unwrap_or(athenaeum_core::models::ImageType::Dark),
            match_score: 0.0,
            date_diff_days: 0,
            temp_diff: None,
            date_warning: false,
            temp_warning: false,
            is_master: false,
            passed_hard_filter: false,
            details: athenaeum_core::calibration::finder::CandidateMatchDetails::default(),
            warnings: Vec::new(),
        };
        if let Some(swc) = load_set_with_score(&conn, &placeholder).map_err(internal_err)? {
            out.push(swc);
        }
    }
    Ok(Json(out))
}

fn internal_err<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// POST /api/manual_assign_calibration
///
/// Manually assign a calibration set to light frames.
pub async fn manual_assign_calibration(
    State(state): State<WebAppState>,
    Json(args): Json<ManualAssignCalibrationArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    use athenaeum_core::db::calibration_links::insert_calibration_link;
    use athenaeum_core::models::CalibrationLink;

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    if args.frame_ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No frame IDs provided".to_string()));
    }

    let valid_types = ["Flat", "Dark", "Bias", "DarkFlat"];
    if !valid_types.contains(&args.calibration_type.as_str()) {
        return Err((StatusCode::BAD_REQUEST, format!("Invalid calibration type: {}", args.calibration_type)));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut assigned_count = 0;

    for frame_id in &args.frame_ids {
        let link = CalibrationLink {
            id: None,
            source_id: *frame_id,
            source_type: "frame".to_string(),
            calibration_set_id: args.calibration_set_id,
            calibration_type: args.calibration_type.clone(),
            matched_at: now.clone(),
            match_score: Some(1.0),
            date_warning: false,
            temp_warning: false,
            is_manual_override: true,
        };

        match insert_calibration_link(&conn, &link) {
            Ok(_) => assigned_count += 1,
            Err(e) => {
                eprintln!("Failed to assign calibration to frame {}: {}", frame_id, e);
            }
        }
    }

    eprintln!(
        "Manually assigned {} set {} to {} frames",
        args.calibration_type, args.calibration_set_id, assigned_count
    );

    Ok(Json(assigned_count))
}

/// POST /api/manual_assign_subcalibration
///
/// Manually assign a sub-calibration set to a calibration set.
pub async fn manual_assign_subcalibration(
    State(state): State<WebAppState>,
    Json(args): Json<ManualAssignSubcalibrationArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    use athenaeum_core::db::calibration_links::insert_calibration_link;
    use athenaeum_core::models::CalibrationLink;

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let valid_types = ["Dark", "DarkFlat", "Bias"];
    if !valid_types.contains(&args.calibration_type.as_str()) {
        return Err((StatusCode::BAD_REQUEST, format!("Invalid sub-calibration type: {}", args.calibration_type)));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let link = CalibrationLink {
        id: None,
        source_id: args.source_set_id,
        source_type: "calibration_set".to_string(),
        calibration_set_id: args.calibration_set_id,
        calibration_type: args.calibration_type.clone(),
        matched_at: now,
        match_score: Some(1.0),
        date_warning: false,
        temp_warning: false,
        is_manual_override: true,
    };

    insert_calibration_link(&conn, &link)
        .map_err(|e| db_err(format!("Failed to assign sub-calibration: {}", e)))?;

    eprintln!(
        "Manually assigned {} set #{} as sub-calibration for set #{}",
        args.calibration_type, args.calibration_set_id, args.source_set_id
    );

    Ok(Json(()))
}

/// POST /api/clear_subcalibration_override
///
/// Clear sub-calibration override for a calibration set.
pub async fn clear_subcalibration_override(
    State(state): State<WebAppState>,
    Json(args): Json<ClearSubcalibrationOverrideArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let deleted = match &args.calibration_type {
        Some(ct) => {
            conn.execute(
                "DELETE FROM calibration_set_to_frames
                 WHERE source_id = ?1 AND source_type = 'calibration_set' AND calibration_type = ?2",
                rusqlite::params![args.source_set_id, ct],
            ).map_err(db_err)?
        }
        None => {
            conn.execute(
                "DELETE FROM calibration_set_to_frames
                 WHERE source_id = ?1 AND source_type = 'calibration_set'",
                rusqlite::params![args.source_set_id],
            ).map_err(db_err)?
        }
    };

    eprintln!(
        "Cleared {} sub-calibration link(s) for set #{}",
        deleted, args.source_set_id
    );

    Ok(Json(deleted))
}

/// POST /api/bulk_update_calibration_metadata
///
/// Bulk update calibration set metadata. Saves originals before updating.
pub async fn bulk_update_calibration_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<BulkUpdateCalibrationMetadataArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    if args.set_ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No set IDs provided".to_string()));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut updated_count = 0;

    for set_id in &args.set_ids {
        // Save originals if not already saved
        let has_originals: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM calibration_set_originals WHERE set_id = ?1",
            [set_id],
            |row| row.get(0),
        ).unwrap_or(false);

        if !has_originals {
            conn.execute(
                "INSERT INTO calibration_set_originals (set_id, ccd_temp, temp_min, temp_max, gain, offset, binning, exptime, saved_at)
                 SELECT id, ccd_temp, temp_min, temp_max, gain, offset, binning, exptime, ?2
                 FROM calibration_set WHERE id = ?1",
                rusqlite::params![set_id, now],
            ).map_err(|e| db_err(format!("Failed to save originals for set {}: {}", set_id, e)))?;
        }

        let mut any_update = false;

        if let Some(temp) = args.edits.ccd_temp {
            conn.execute(
                "UPDATE calibration_set SET ccd_temp = ?1, temp_min = ?1, temp_max = ?1 WHERE id = ?2",
                rusqlite::params![temp, set_id],
            ).map_err(db_err)?;
            any_update = true;
        }

        if let Some(gain) = args.edits.gain {
            conn.execute(
                "UPDATE calibration_set SET gain = ?1 WHERE id = ?2",
                rusqlite::params![gain, set_id],
            ).map_err(db_err)?;
            any_update = true;
        }

        if let Some(offset) = args.edits.offset {
            conn.execute(
                "UPDATE calibration_set SET offset = ?1 WHERE id = ?2",
                rusqlite::params![offset, set_id],
            ).map_err(db_err)?;
            any_update = true;
        }

        if let Some(ref binning) = args.edits.binning {
            conn.execute(
                "UPDATE calibration_set SET binning = ?1 WHERE id = ?2",
                rusqlite::params![binning, set_id],
            ).map_err(db_err)?;
            any_update = true;
        }

        if let Some(exptime) = args.edits.exptime {
            conn.execute(
                "UPDATE calibration_set SET exptime = ?1 WHERE id = ?2",
                rusqlite::params![exptime, set_id],
            ).map_err(db_err)?;
            any_update = true;
        }

        if any_update {
            // Mirror the calibration-set edits into the member frames so
            // frames.* stays in sync (see the Tauri-side comment in
            // bulk_update_calibration_metadata for the rationale).
            let mut frame_set_clauses: Vec<&str> = Vec::new();
            let mut frame_values: Vec<rusqlite::types::Value> = Vec::new();
            if let Some(temp) = args.edits.ccd_temp {
                frame_set_clauses.push("ccd_temp = ?");
                frame_values.push(rusqlite::types::Value::Real(temp));
            }
            if let Some(gain) = args.edits.gain {
                frame_set_clauses.push("gain = ?");
                frame_values.push(rusqlite::types::Value::Real(gain));
            }
            if let Some(offset) = args.edits.offset {
                frame_set_clauses.push("offset = ?");
                frame_values.push(rusqlite::types::Value::Real(offset));
            }
            if let Some(ref binning) = args.edits.binning {
                frame_set_clauses.push("binning = ?");
                frame_values.push(rusqlite::types::Value::Text(binning.clone()));
                if let Some((xs, ys)) = binning.split_once('x') {
                    if let (Ok(xb), Ok(yb)) = (xs.parse::<i64>(), ys.parse::<i64>()) {
                        frame_set_clauses.push("xbinning = ?");
                        frame_values.push(rusqlite::types::Value::Integer(xb));
                        frame_set_clauses.push("ybinning = ?");
                        frame_values.push(rusqlite::types::Value::Integer(yb));
                    }
                }
            }
            if let Some(exptime) = args.edits.exptime {
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
                conn.execute(&sql, rusqlite::params_from_iter(all_values.iter()))
                    .map_err(db_err)?;
            }
            updated_count += 1;
        }
    }

    eprintln!("Updated metadata for {} calibration sets", updated_count);
    Ok(Json(updated_count))
}

/// POST /api/bulk_restore_calibration_metadata
///
/// Restore calibration set metadata from originals.
pub async fn bulk_restore_calibration_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<BulkRestoreCalibrationMetadataArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    if args.set_ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No set IDs provided".to_string()));
    }

    let mut restored_count = 0;

    for set_id in &args.set_ids {
        let rows_affected = conn.execute(
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
        ).map_err(db_err)?;

        if rows_affected > 0 {
            conn.execute(
                "DELETE FROM calibration_set_originals WHERE set_id = ?1",
                rusqlite::params![set_id],
            ).map_err(db_err)?;
            restored_count += 1;
        }
    }

    eprintln!("Restored original metadata for {} calibration sets", restored_count);
    Ok(Json(restored_count))
}

/// POST /api/get_custom_metadata_set_ids
///
/// Get all set IDs that have custom metadata edits for a given camera.
pub async fn get_custom_metadata_set_ids(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<i64>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let mut stmt = conn.prepare(
        "SELECT cso.set_id FROM calibration_set_originals cso
         JOIN calibration_set cs ON cs.id = cso.set_id
         WHERE cs.instrume = ?1"
    ).map_err(db_err)?;

    let ids: Vec<i64> = stmt.query_map([&args.instrume], |row| row.get(0))
        .map_err(db_err)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(ids))
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Query frame IDs for a specific camera and image type (case-insensitive).
fn query_frame_ids_by_type(
    conn: &rusqlite::Connection,
    instrume: &str,
    imagetyp: &str,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id FROM frames
         WHERE instrume = ?1 AND UPPER(imagetyp) = UPPER(?2)",
    )?;

    let ids: Vec<i64> = stmt
        .query_map([instrume, imagetyp], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ids)
}

/// Internal helper to get light frame parameters (without calibration link lookups).
fn get_light_params_internal(
    conn: &rusqlite::Connection,
    frame_ids: &[i64],
) -> Result<athenaeum_core::models::LightFrameParameters, (StatusCode, String)> {
    if frame_ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No frame IDs provided".to_string()));
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT id, instrume, binning, gain, offset, filter, ccd_temp, exptime, date_obs
         FROM frames WHERE id IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let mut rows = stmt.query(params.as_slice()).map_err(db_err)?;

    let mut instrumes: Vec<Option<String>> = Vec::new();
    let mut binnings: Vec<Option<String>> = Vec::new();
    let mut gains: Vec<Option<f64>> = Vec::new();
    let mut offsets: Vec<Option<f64>> = Vec::new();
    let mut filters: Vec<Option<String>> = Vec::new();
    let mut temps: Vec<f64> = Vec::new();
    let mut exptimes: Vec<f64> = Vec::new();
    let mut dates: Vec<String> = Vec::new();
    let mut frame_count = 0;

    while let Some(row) = rows.next().map_err(db_err)? {
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

    let instrume = most_common_option(&instrumes);
    let binning = most_common_option(&binnings);
    let gain = most_common_f64(&gains);
    let offset = most_common_f64(&offsets);
    let filter = most_common_option(&filters);

    let avg_ccd_temp = if temps.is_empty() { None } else {
        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    };
    let avg_exptime = if exptimes.is_empty() { None } else {
        Some(exptimes.iter().sum::<f64>() / exptimes.len() as f64)
    };
    let exptime_range = if exptimes.is_empty() { None } else {
        let min = exptimes.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = exptimes.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        Some((min, max))
    };
    let date_range = if dates.is_empty() { None } else {
        let mut sorted = dates.clone();
        sorted.sort();
        Some((sorted.first().unwrap().clone(), sorted.last().unwrap().clone()))
    };

    Ok(athenaeum_core::models::LightFrameParameters {
        instrume, binning, gain, offset, filter,
        avg_ccd_temp, avg_exptime, exptime_range, frame_count, date_range,
        current_flat_set_id: None,
        current_dark_set_id: None,
        current_bias_set_id: None,
    })
}

/// Query all calibration sets of a given type and score them against light frame params.
fn most_common_option(values: &[Option<String>]) -> Option<String> {
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for v in values.iter().flatten() {
        *counts.entry(v).or_insert(0) += 1;
    }
    counts.into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(v, _)| v.clone())
}

fn most_common_f64(values: &[Option<f64>]) -> Option<f64> {
    let mut counts: HashMap<i64, (f64, usize)> = HashMap::new();
    for v in values.iter().flatten() {
        let key = (*v * 100.0).round() as i64;
        let entry = counts.entry(key).or_insert((*v, 0));
        entry.1 += 1;
    }
    counts.into_iter()
        .max_by_key(|(_, (_, count))| *count)
        .map(|(_, (v, _))| v)
}
