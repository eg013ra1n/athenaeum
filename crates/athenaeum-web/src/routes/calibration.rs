// Calibration route handlers — mirrors athenaeum-tauri/src/commands/calibration.rs
//
// Thin wrappers only: extraction + handler call + error mapping. Business
// logic lives in athenaeum_core::api::calibration (Task 11 conversion — see
// .superpowers/sdd/p1-task-11-report.md).

use athenaeum_core::api::calibration as api;
use athenaeum_core::calibration::CalibrationMatchingConfig;
use axum::{extract::State, http::StatusCode, Json};
use std::collections::HashMap;

use crate::routes::api_err;
use crate::WebAppState;

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

/// Request body for `set_calibration_matching_config`. The frontend calls
/// `api.invoke("set_calibration_matching_config", { config })` per the Tauri
/// named-arg convention used by every `api.invoke` call, so the HTTP body is
/// `{ "config": { ... } }`, not a bare `CalibrationMatchingConfig`. See
/// `.superpowers/sdd/task-10-report.md` (Web wrapper rider).
///
/// STAYS web-side per the Task 11 conversion recipe — this is exactly the
/// `{config}`-wrapper extractor struct the `8999e33e` regression tests guard
/// (see `calibration_config_tests` below).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCalibrationMatchingConfigArgs {
    pub config: CalibrationMatchingConfig,
}

// ── Equipment & dark library ──────────────────────────────────────────────────

/// POST /api/get_equipment_cameras
///
/// Returns all cameras (instrume) that have frames in the catalog.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_equipment_cameras(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Vec<athenaeum_core::models::CameraStats>>, (StatusCode, String)> {
    api::get_equipment_cameras(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/get_dark_library
///
/// Retrieve existing dark calibration sets for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetDetail>>, (StatusCode, String)> {
    api::get_dark_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

/// POST /api/has_dark_library
///
/// Check whether a dark library exists for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn has_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    api::has_dark_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

// ── Master dark library ───────────────────────────────────────────────────────

/// POST /api/create_master_dark_library
///
/// Build calibration sets from master dark frames for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn create_master_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<athenaeum_core::models::DarkLibraryResult>, (StatusCode, String)> {
    api::create_master_dark_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

/// POST /api/get_master_dark_library
///
/// Retrieve master dark calibration sets for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_master_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetDetail>>, (StatusCode, String)> {
    api::get_master_dark_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

/// POST /api/has_master_dark_library
///
/// Check whether a master dark library exists for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn has_master_dark_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    api::has_master_dark_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

// ── Master flat library ───────────────────────────────────────────────────────

/// POST /api/create_master_flat_library
///
/// Build calibration sets from master flat frames for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn create_master_flat_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<athenaeum_core::models::DarkLibraryResult>, (StatusCode, String)> {
    api::create_master_flat_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

/// POST /api/get_master_flat_library
///
/// Retrieve master flat calibration sets for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_master_flat_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetDetail>>, (StatusCode, String)> {
    api::get_master_flat_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

/// POST /api/has_master_flat_library
///
/// Check whether a master flat library exists for a camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn has_master_flat_library(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    api::has_master_flat_library(&state.ctx, args.instrume).map(Json).map_err(api_err)
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
#[tracing::instrument(skip_all, err(Debug))]
pub async fn refresh_calibration_library_for_camera(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<athenaeum_core::calibration::scan_integration::CalibrationScanResult>, (StatusCode, String)> {
    api::refresh_calibration_library_for_camera(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

/// POST /api/get_calibration_set_frames
///
/// List the files/frames belonging to a calibration set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_set_frames(
    State(state): State<WebAppState>,
    Json(args): Json<SetIdArgs>,
) -> Result<Json<Vec<athenaeum_core::models::FileWithFrame>>, (StatusCode, String)> {
    api::get_calibration_set_frames(&state.ctx, args.set_id).map(Json).map_err(api_err)
}

/// POST /api/get_calibration_set_consumers
///
/// List the frame sets (objects) that ultimately consume a calibration set,
/// including transitive consumers via the sub-cal chain.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_set_consumers(
    State(state): State<WebAppState>,
    Json(args): Json<SetIdArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetConsumer>>, (StatusCode, String)> {
    api::get_calibration_set_consumers(&state.ctx, args.set_id).map(Json).map_err(api_err)
}

// ── Calibration finder ────────────────────────────────────────────────────────

/// POST /api/find_calibration_for_frame_set
///
/// Match and link calibration frames to all light frames in a frame set.
/// Loads config for tolerance defaults, then delegates to
/// `athenaeum_core::calibration::processor::process_frame_set`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn find_calibration_for_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FindCalibrationArgs>,
) -> Result<Json<athenaeum_core::calibration::processor::ProcessingStats>, (StatusCode, String)> {
    api::find_calibration_for_frame_set(
        &state.ctx,
        args.frame_set_id,
        args.flat_date_warning_days,
        args.dark_date_warning_days,
        args.flat_pattern,
        args.manual_flat_selections,
    )
    .map(Json)
    .map_err(api_err)
}

// ── Calibration matching config ───────────────────────────────────────────────

/// POST /api/get_calibration_matching_config
///
/// Load the configurable calibration matching rules.
/// Returns defaults if the setting has not been persisted yet.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_matching_config(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<CalibrationMatchingConfig>, (StatusCode, String)> {
    api::get_calibration_matching_config(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/set_calibration_matching_config
///
/// Validate and persist updated calibration matching rules.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_calibration_matching_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetCalibrationMatchingConfigArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_calibration_matching_config(&state.ctx, args.config).map(Json).map_err(api_err)
}

/// POST /api/reset_calibration_matching_config
///
/// Reset calibration matching rules to built-in defaults and persist them.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn reset_calibration_matching_config(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<CalibrationMatchingConfig>, (StatusCode, String)> {
    api::reset_calibration_matching_config(&state.ctx).map(Json).map_err(api_err)
}

// ── Phase 4: Calibration hierarchy, manual selection, sub-calibration, metadata

/// POST /api/get_calibration_hierarchy_for_frame_set
///
/// Get calibration hierarchy organized by Date → Camera → Filter for a frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_hierarchy_for_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::CalibrationHierarchyView>, (StatusCode, String)> {
    api::get_calibration_hierarchy_for_frame_set(&state.ctx, args.frame_set_id).map(Json).map_err(api_err)
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
pub struct ClearManualCalibrationOverrideArgs {
    pub frame_ids: Vec<i64>,
    pub calibration_type: Option<String>,
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
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_set_parameters(
    State(state): State<WebAppState>,
    Json(args): Json<SetIdArgs>,
) -> Result<Json<athenaeum_core::models::CalibrationSetParameters>, (StatusCode, String)> {
    api::get_calibration_set_parameters(&state.ctx, args.set_id).map(Json).map_err(api_err)
}

/// POST /api/get_light_frame_parameters
///
/// Get average parameters of light frames for manual selection display.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_light_frame_parameters(
    State(state): State<WebAppState>,
    Json(args): Json<GetLightFrameParametersArgs>,
) -> Result<Json<athenaeum_core::models::LightFrameParameters>, (StatusCode, String)> {
    api::get_light_frame_parameters(&state.ctx, args.frame_ids).map(Json).map_err(api_err)
}

/// POST /api/get_calibration_sets_for_manual_selection
///
/// Get calibration sets with match scores for manual selection. Routes through
/// the same `find_calibration_candidates` engine that auto-link uses, so the
/// modal's score and "compatible" decision agree with what "Find Calibration"
/// will actually pick.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_sets_for_manual_selection(
    State(state): State<WebAppState>,
    Json(args): Json<GetCalibrationSetsForManualSelectionArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetWithScore>>, (StatusCode, String)> {
    api::get_calibration_sets_for_manual_selection(&state.ctx, args.frame_ids, args.calibration_type, args.show_all)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_subcalibration_sets_for_manual_selection
///
/// Sub-calibration candidates for a Flat or Dark set. Routes through
/// `find_calibration_candidates` with the parent set's parameters synthesized
/// as a frame so the engine's `flats→{darkflat,dark,bias}` and `darks→bias`
/// configs apply.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_subcalibration_sets_for_manual_selection(
    State(state): State<WebAppState>,
    Json(args): Json<GetSubcalibrationSetsArgs>,
) -> Result<Json<Vec<athenaeum_core::models::CalibrationSetWithScore>>, (StatusCode, String)> {
    api::get_subcalibration_sets_for_manual_selection(&state.ctx, args.set_id, args.calibration_type, args.show_all)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/manual_assign_calibration
///
/// Manually assign a calibration set to light frames.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn manual_assign_calibration(
    State(state): State<WebAppState>,
    Json(args): Json<ManualAssignCalibrationArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    api::manual_assign_calibration(&state.ctx, args.frame_ids, args.calibration_set_id, args.calibration_type)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/clear_manual_calibration_override
///
/// Remove manual calibration override(s) for light frames and allow
/// auto-find to reassign. Mirrors `athenaeum-tauri`'s command of the same
/// name; both call `athenaeum_core::api::calibration::clear_manual_calibration_override`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_manual_calibration_override(
    State(state): State<WebAppState>,
    Json(args): Json<ClearManualCalibrationOverrideArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    api::clear_manual_calibration_override(&state.ctx, args.frame_ids, args.calibration_type)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/manual_assign_subcalibration
///
/// Manually assign a sub-calibration set to a calibration set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn manual_assign_subcalibration(
    State(state): State<WebAppState>,
    Json(args): Json<ManualAssignSubcalibrationArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::manual_assign_subcalibration(&state.ctx, args.source_set_id, args.calibration_set_id, args.calibration_type)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/clear_subcalibration_override
///
/// Clear sub-calibration override for a calibration set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_subcalibration_override(
    State(state): State<WebAppState>,
    Json(args): Json<ClearSubcalibrationOverrideArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    api::clear_subcalibration_override(&state.ctx, args.source_set_id, args.calibration_type).map(Json).map_err(api_err)
}

/// POST /api/bulk_update_calibration_metadata
///
/// Bulk update calibration set metadata. Saves originals before updating.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn bulk_update_calibration_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<BulkUpdateCalibrationMetadataArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    api::bulk_update_calibration_metadata(&state.ctx, args.set_ids, args.edits).map(Json).map_err(api_err)
}

/// POST /api/bulk_restore_calibration_metadata
///
/// Restore calibration set metadata from originals.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn bulk_restore_calibration_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<BulkRestoreCalibrationMetadataArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    api::bulk_restore_calibration_metadata(&state.ctx, args.set_ids).map(Json).map_err(api_err)
}

/// POST /api/get_custom_metadata_set_ids
///
/// Get all set IDs that have custom metadata edits for a given camera.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_custom_metadata_set_ids(
    State(state): State<WebAppState>,
    Json(args): Json<InstrumeArgs>,
) -> Result<Json<Vec<i64>>, (StatusCode, String)> {
    api::get_custom_metadata_set_ids(&state.ctx, args.instrume).map(Json).map_err(api_err)
}

#[cfg(test)]
mod calibration_config_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// these tests exercise actual `settings` table reads/writes for the
    /// calibration matching config. Mirrors
    /// `settings::logging_config_tests::test_state`.
    fn test_state(db: Database) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths: Vec::new(),
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
        }
    }

    /// Regression guard for the real frontend payload: `api.invoke` sends
    /// `{ "config": { ... } }`, per the Tauri named-arg convention — not a
    /// bare `CalibrationMatchingConfig`. Exercises the handler via the
    /// wrapped `SetCalibrationMatchingConfigArgs` struct directly.
    #[tokio::test]
    async fn set_calibration_matching_config_then_get_reflects_change() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let mut cfg = CalibrationMatchingConfig::default();
        cfg.warnings.flat_date_warning_days = 999;

        let _ = set_calibration_matching_config(
            State(state.clone()),
            Json(SetCalibrationMatchingConfigArgs { config: cfg }),
        )
        .await
        .expect("valid config must be accepted");

        let resp = get_calibration_matching_config(State(state), Json(serde_json::json!({})))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.warnings.flat_date_warning_days, 999);
    }

    /// Pins the fix: the handler now requires the `{ "config": ... }`
    /// wrapper (matching `SetCalibrationMatchingConfigArgs`). Deserializing
    /// a bare `CalibrationMatchingConfig` JSON body must fail hard (serde
    /// error), not silently succeed with the wrong shape.
    #[test]
    fn bare_calibration_matching_config_body_fails_to_deserialize_into_wrapped_args() {
        let bare = serde_json::to_value(CalibrationMatchingConfig::default()).unwrap();

        let result: Result<SetCalibrationMatchingConfigArgs, _> = serde_json::from_value(bare);
        assert!(
            result.is_err(),
            "bare CalibrationMatchingConfig body must NOT deserialize into \
             SetCalibrationMatchingConfigArgs — this is what closes the \
             silent-mismatch hole (axum returns 422/400 for this shape)"
        );
    }
}
