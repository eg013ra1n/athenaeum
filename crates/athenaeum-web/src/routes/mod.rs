use axum::{
    Router,
    extract::State,
    response::{
        sse::{Event, Sse},
        IntoResponse, Json, Response,
    },
    routing::{get, post},
    http::StatusCode,
};
use std::convert::Infallible;
use std::path::PathBuf;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::WebAppState;

mod scan_roots;
mod files;
mod settings;
mod frame_sets;
mod calibration;
mod duplicates;
mod export;
mod spatial;
mod images;

/// Helper to extract DB connection from state, returning a JSON error on failure.
fn db_err() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "Database not initialized" })),
    )
}

/// Build the complete Axum router.
pub fn build_router(state: WebAppState, static_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        // SSE events
        .route("/api/events", get(sse_handler))
        // Scan roots
        .route("/api/add_scan_root", post(scan_roots::add_scan_root))
        .route("/api/get_scan_roots", post(scan_roots::get_scan_roots))
        .route("/api/delete_scan_root", post(scan_roots::delete_scan_root))
        .route("/api/start_scan_with_progress", post(scan_roots::start_scan_with_progress))
        .route("/api/cancel_scan", post(scan_roots::cancel_scan))
        .route("/api/get_active_scans", post(scan_roots::get_active_scans))
        // Files
        .route("/api/get_files", post(files::get_files))
        .route("/api/get_files_by_directory", post(files::get_files_by_directory))
        .route("/api/get_directory_contents", post(files::get_directory_contents))
        .route("/api/get_camera_directories", post(files::get_camera_directories))
        .route("/api/get_camera_directory_contents", post(files::get_camera_directory_contents))
        .route("/api/get_frames_with_missing_metadata", post(files::get_frames_with_missing_metadata))
        .route("/api/get_files_with_frames_by_ids", post(files::get_files_with_frames_by_ids))
        // Settings
        .route("/api/get_setting", post(settings::get_setting))
        .route("/api/set_setting", post(settings::set_setting))
        .route("/api/get_all_settings", post(settings::get_all_settings))
        .route("/api/delete_setting", post(settings::delete_setting))
        .route("/api/get_grouping_threshold_deg", post(settings::get_grouping_threshold_deg))
        // Frame sets
        .route("/api/auto_generate_frame_sets", post(frame_sets::auto_generate_frame_sets))
        .route("/api/get_frames_sets", post(frame_sets::get_frames_sets))
        .route("/api/get_frame_set_detail", post(frame_sets::get_frame_set_detail))
        .route("/api/delete_frames_set", post(frame_sets::delete_frames_set))
        .route("/api/delete_auto_generated_frame_sets", post(frame_sets::delete_auto_generated_frame_sets))
        .route("/api/rename_frames_set", post(frame_sets::rename_frames_set))
        .route("/api/mark_frame_set_custom", post(frame_sets::mark_frame_set_custom))
        .route("/api/recalculate_frame_set_metadata", post(frame_sets::recalculate_frame_set_metadata))
        .route("/api/merge_frame_sets", post(frame_sets::merge_frame_sets))
        .route("/api/can_split", post(frame_sets::can_split))
        .route("/api/split_frame_set", post(frame_sets::split_frame_set))
        .route("/api/create_custom_frames_set", post(frame_sets::create_custom_frames_set))
        .route("/api/create_frame_set_from_selection", post(frame_sets::create_frame_set_from_selection))
        .route("/api/create_frame_set_from_excluded", post(frame_sets::create_frame_set_from_excluded))
        .route("/api/update_frame_set_flat_pattern", post(frame_sets::update_frame_set_flat_pattern))
        // Calibration
        .route("/api/get_equipment_cameras", post(calibration::get_equipment_cameras))
        .route("/api/create_dark_library", post(calibration::create_dark_library))
        .route("/api/get_dark_library", post(calibration::get_dark_library))
        .route("/api/delete_dark_library", post(calibration::delete_dark_library))
        .route("/api/has_dark_library", post(calibration::has_dark_library))
        .route("/api/create_master_dark_library", post(calibration::create_master_dark_library))
        .route("/api/get_master_dark_library", post(calibration::get_master_dark_library))
        .route("/api/has_master_dark_library", post(calibration::has_master_dark_library))
        .route("/api/create_master_flat_library", post(calibration::create_master_flat_library))
        .route("/api/get_master_flat_library", post(calibration::get_master_flat_library))
        .route("/api/has_master_flat_library", post(calibration::has_master_flat_library))
        .route("/api/refresh_calibration_library_for_camera", post(calibration::refresh_calibration_library_for_camera))
        .route("/api/get_calibration_set_frames", post(calibration::get_calibration_set_frames))
        .route("/api/find_calibration_for_frame_set", post(calibration::find_calibration_for_frame_set))
        .route("/api/get_calibration_status", post(calibration::get_calibration_status))
        .route("/api/get_calibration_matching_config", post(calibration::get_calibration_matching_config))
        .route("/api/set_calibration_matching_config", post(calibration::set_calibration_matching_config))
        .route("/api/reset_calibration_matching_config", post(calibration::reset_calibration_matching_config))
        // Duplicates / black hole
        .route("/api/get_duplicates", post(duplicates::get_duplicates))
        .route("/api/move_to_black_hole", post(duplicates::move_to_black_hole))
        .route("/api/get_black_hole_files", post(duplicates::get_black_hole_files))
        .route("/api/get_blackholed_file_ids", post(duplicates::get_blackholed_file_ids))
        .route("/api/restore_from_black_hole", post(duplicates::restore_from_black_hole))
        .route("/api/send_to_void", post(duplicates::send_to_void))
        .route("/api/send_all_to_void", post(duplicates::send_all_to_void))
        .route("/api/get_duplicate_folders", post(duplicates::get_duplicate_folders))
        // Export
        .route("/api/get_wbpp_export_config", post(export::get_wbpp_export_config))
        .route("/api/set_wbpp_export_config", post(export::set_wbpp_export_config))
        .route("/api/reset_wbpp_export_config", post(export::reset_wbpp_export_config))
        .route("/api/get_export_preview", post(export::get_export_preview))
        .route("/api/get_exportable_frame_sets", post(export::get_exportable_frame_sets))
        .route("/api/get_calibration_route", post(export::get_calibration_route))
        .route("/api/export_to_wbpp", post(export::export_to_wbpp))
        .route("/api/cancel_export", post(export::cancel_export))
        .route("/api/get_export_summary", post(export::get_export_summary))
        // Spatial
        .route("/api/get_imaging_locations", post(spatial::get_imaging_locations))
        .route("/api/query_frames_in_bounds", post(spatial::query_frames_in_bounds))
        .route("/api/get_frame_preview", post(images::get_frame_preview))
        // Calendar
        .route("/api/get_calendar_month_data", post(spatial::get_calendar_month_data))
        // Core
        .route("/api/initialize_database", post(initialize_database))
        .route("/api/get_app_version", post(get_app_version))
        .route("/api/get_log_path", post(get_log_path))
        .route("/api/get_database_path", post(get_database_path))
        .with_state(state);

    // Optionally serve static frontend files
    if let Some(dir) = static_dir {
        let serve = tower_http::services::ServeDir::new(&dir)
            .fallback(tower_http::services::ServeFile::new(dir.join("index.html")));
        api.fallback_service(serve)
    } else {
        api
    }
}

// ── SSE endpoint ─────────────────────────────────────────────────────────────

async fn sse_handler(
    State(state): State<WebAppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(event) => Some(Ok(Event::default()
                .event(event.event_name)
                .json_data(event.data)
                .unwrap_or_else(|_| Event::default()))),
            Err(_) => None, // Lagged — skip
        }
    });
    Sse::new(stream)
}

// ── Core routes ──────────────────────────────────────────────────────────────

async fn initialize_database(
    State(state): State<WebAppState>,
) -> Result<Json<String>, (StatusCode, Json<serde_json::Value>)> {
    let lock = state.ctx.db.lock().unwrap();
    if lock.is_some() {
        return Ok(Json("Database already initialized".to_string()));
    }
    drop(lock);
    // In web mode, DB is initialized at server start — this is a no-op
    Ok(Json("Database initialized at server start".to_string()))
}

async fn get_app_version() -> Json<String> {
    Json(env!("CARGO_PKG_VERSION").to_string())
}

async fn get_log_path() -> Json<Option<String>> {
    // In web mode, log path is not directly accessible to the client
    Json(None)
}

async fn get_database_path(
    State(state): State<WebAppState>,
) -> Json<Option<String>> {
    let lock = state.ctx.db.lock().unwrap();
    let path = lock.as_ref().map(|db| db.path().to_string_lossy().to_string());
    Json(path)
}
