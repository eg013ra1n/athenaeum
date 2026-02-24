// Calibration route handlers — mirrors athenaeum-tauri/src/commands/calibration.rs

use athenaeum_core::calibration;
use athenaeum_core::calibration::configurable_matcher;
use athenaeum_core::calibration::processor;
use athenaeum_core::calibration::scan_integration::{
    create_calibration_sets_from_scan_with_masters, MasterFrameIds,
};
use athenaeum_core::db;
use athenaeum_core::db::calibration_links;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let cameras = db::get_all_cameras(&conn).map_err(db_err)?;
    Ok(Json(cameras))
}

/// POST /api/create_dark_library
///
/// Build calibration sets for a camera's dark frames using configured thresholds.
pub async fn create_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<athenaeum_core::models::DarkLibraryResult>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let date_threshold = state.ctx.settings
        .get_dark_library_date_threshold(&conn)
        .map_err(db_err)?;

    let temp_threshold = state.ctx.settings
        .get_dark_library_temp_threshold(&conn)
        .map_err(db_err)?;

    let result = calibration::create_dark_library(
        &conn,
        &args.instrume,
        date_threshold,
        temp_threshold,
    ).map_err(db_err)?;

    Ok(Json(result))
}

/// POST /api/get_dark_library
///
/// Retrieve existing dark calibration sets for a camera.
pub async fn get_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetDetail>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let sets = db::get_camera_dark_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(sets))
}

/// POST /api/delete_dark_library
///
/// Delete all dark calibration sets for a camera.
pub async fn delete_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    db::delete_camera_dark_library(&conn, &args.instrume).map_err(db_err)?;
    Ok(Json(()))
}

/// POST /api/has_dark_library
///
/// Check whether a dark library exists for a camera.
pub async fn has_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let frames = db::get_frames_for_calibration_set(&conn, args.set_id).map_err(db_err)?;
    Ok(Json(frames))
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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

/// POST /api/get_calibration_status
///
/// Return calibration link statistics for a frame set.
pub async fn get_calibration_status(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::CalibrationStats>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let stats = calibration_links::get_calibration_statistics(&conn, args.frame_set_id)
        .map_err(db_err)?;

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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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

    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let default_config = CalibrationMatchingConfig::default();

    let json = default_config
        .to_json()
        .map_err(|e| db_err(format!("Failed to serialize default config: {}", e)))?;

    db::set_setting(&conn, CALIBRATION_CONFIG_KEY, &json).map_err(db_err)?;

    Ok(Json(default_config))
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
