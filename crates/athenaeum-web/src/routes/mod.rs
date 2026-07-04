use axum::{
    Router,
    extract::State,
    response::{
        sse::{Event, Sse},
        Json,
    },
    routing::{get, post},
    http::StatusCode,
    middleware,
};
use std::convert::Infallible;
use std::path::PathBuf;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::WebAppState;

mod auth;
mod scan_roots;
mod files;
mod settings;
mod frame_sets;
mod calibration;
mod duplicates;
mod export;
mod spatial;
mod images;
mod missing_files;
mod analysis;
mod plate_solve;
mod registration;
mod archive;

/// Build the complete Axum router.
pub fn build_router(state: WebAppState, static_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        // SSE events
        .route("/api/events", get(sse_handler))
        // Scan roots
        .route("/api/add_scan_root", post(scan_roots::add_scan_root))
        .route("/api/get_scan_roots", post(scan_roots::get_scan_roots))
        .route("/api/delete_scan_root", post(scan_roots::delete_scan_root))
        .route("/api/start_scan", post(scan_roots::start_scan))
        .route("/api/start_scan_with_progress", post(scan_roots::start_scan_with_progress))
        .route("/api/cancel_scan", post(scan_roots::cancel_scan))
        .route("/api/check_all_scan_roots_availability", post(scan_roots::check_all_scan_roots_availability))
        .route("/api/get_missing_files_counts", post(scan_roots::get_missing_files_counts))
        .route("/api/rescan_all_for_content_hash", post(scan_roots::rescan_all_for_content_hash))
        .route("/api/relink_scan_root", post(scan_roots::relink_scan_root))
        .route("/api/set_scan_root_monitor_enabled", post(scan_roots::set_scan_root_monitor_enabled))
        // Missing files
        .route("/api/check_missing_files_in_scan_root", post(missing_files::check_missing_files_in_scan_root))
        .route("/api/sync_missing_files", post(missing_files::sync_missing_files))
        .route("/api/get_missing_files", post(missing_files::get_missing_files))
        .route("/api/recheck_missing_files", post(missing_files::recheck_missing_files))
        .route("/api/ignore_missing_file", post(missing_files::ignore_missing_file))
        .route("/api/unignore_missing_file", post(missing_files::unignore_missing_file))
        .route("/api/delete_missing_files", post(missing_files::delete_missing_files))
        .route("/api/relocate_missing_file", post(relocate_missing_file_stub))
        // Files
        .route("/api/get_files", post(files::get_files))
        .route("/api/get_files_by_directory", post(files::get_files_by_directory))
        .route("/api/get_directory_contents", post(files::get_directory_contents))
        .route("/api/get_camera_directories", post(files::get_camera_directories))
        .route("/api/get_camera_directory_contents", post(files::get_camera_directory_contents))
        .route("/api/get_frames_with_missing_metadata", post(files::get_frames_with_missing_metadata))
        .route("/api/bulk_update_frame_metadata", post(files::bulk_update_frame_metadata))
        .route("/api/count_frame_metadata_relations", post(files::count_frame_metadata_relations))
        .route("/api/get_frame_memberships", post(files::get_frame_memberships))
        .route("/api/get_frame_metadata_originals", post(files::get_frame_metadata_originals))
        .route("/api/get_distinct_instrumes", post(files::get_distinct_instrumes))
        .route("/api/get_files_with_frames_by_ids", post(files::get_files_with_frames_by_ids))
        .route("/api/browse_directories", post(files::browse_directories))
        // Dual-pane file browser
        .route("/api/enqueue_move_operation", post(files::enqueue_move_operation))
        .route("/api/search_catalog", post(files::search_catalog))
        .route("/api/mkdir_in_scan_root", post(files::mkdir_in_scan_root))
        .route("/api/rename_path", post(files::rename_path))
        // Settings
        .route("/api/get_setting", post(settings::get_setting))
        .route("/api/set_setting", post(settings::set_setting))
        .route("/api/delete_setting", post(settings::delete_setting))
        .route("/api/get_logging_config", post(settings::get_logging_config))
        .route("/api/set_logging_config", post(settings::set_logging_config))
        // Cache & blink (Category C — modified behavior in web mode)
        .route("/api/get_cache_stats", post(settings::get_cache_stats))
        .route("/api/get_blink_threads_max", post(settings::get_blink_threads_max))
        .route("/api/set_blink_threads", post(settings::set_blink_threads))
        // Frame sets
        .route("/api/auto_generate_frame_sets", post(frame_sets::auto_generate_frame_sets))
        .route("/api/get_frames_sets", post(frame_sets::get_frames_sets))
        .route("/api/get_frame_set_detail", post(frame_sets::get_frame_set_detail))
        .route("/api/delete_frames_set", post(frame_sets::delete_frames_set))
        .route("/api/delete_auto_generated_frame_sets", post(frame_sets::delete_auto_generated_frame_sets))
        .route("/api/rename_frames_set", post(frame_sets::rename_frames_set))
        .route("/api/mark_frame_set_custom", post(frame_sets::mark_frame_set_custom))
        .route("/api/merge_frame_sets", post(frame_sets::merge_frame_sets))
        .route("/api/split_frame_set", post(frame_sets::split_frame_set))
        .route("/api/create_frame_set_from_selection", post(frame_sets::create_frame_set_from_selection))
        .route("/api/archive_frame_set", post(frame_sets::archive_frame_set))
        .route("/api/find_new_frames_for_set", post(frame_sets::find_new_frames_for_set))
        .route("/api/auto_merge_new_frames_for_set", post(frame_sets::auto_merge_new_frames_for_set))
        .route("/api/get_frame_set_merge_log", post(frame_sets::get_frame_set_merge_log))
        // Excluded frames
        .route("/api/get_excluded_frames_with_metadata", post(frame_sets::get_excluded_frames_with_metadata))
        .route("/api/remove_files_from_excluded", post(frame_sets::remove_files_from_excluded))
        .route("/api/get_excluded_frames_count", post(frame_sets::get_excluded_frames_count))
        // Calibration
        .route("/api/get_equipment_cameras", post(calibration::get_equipment_cameras))
        .route("/api/get_dark_library", post(calibration::get_dark_library))
        .route("/api/has_dark_library", post(calibration::has_dark_library))
        .route("/api/create_master_dark_library", post(calibration::create_master_dark_library))
        .route("/api/get_master_dark_library", post(calibration::get_master_dark_library))
        .route("/api/has_master_dark_library", post(calibration::has_master_dark_library))
        .route("/api/create_master_flat_library", post(calibration::create_master_flat_library))
        .route("/api/get_master_flat_library", post(calibration::get_master_flat_library))
        .route("/api/has_master_flat_library", post(calibration::has_master_flat_library))
        .route("/api/refresh_calibration_library_for_camera", post(calibration::refresh_calibration_library_for_camera))
        .route("/api/get_calibration_set_frames", post(calibration::get_calibration_set_frames))
        .route("/api/get_calibration_set_consumers", post(calibration::get_calibration_set_consumers))
        .route("/api/find_calibration_for_frame_set", post(calibration::find_calibration_for_frame_set))
        .route("/api/get_calibration_matching_config", post(calibration::get_calibration_matching_config))
        .route("/api/set_calibration_matching_config", post(calibration::set_calibration_matching_config))
        .route("/api/reset_calibration_matching_config", post(calibration::reset_calibration_matching_config))
        // Calibration — Phase 4 routes (hierarchy, manual selection, sub-calibration, metadata)
        .route("/api/get_calibration_hierarchy_for_frame_set", post(calibration::get_calibration_hierarchy_for_frame_set))
        .route("/api/get_calibration_set_parameters", post(calibration::get_calibration_set_parameters))
        .route("/api/get_calibration_sets_for_manual_selection", post(calibration::get_calibration_sets_for_manual_selection))
        .route("/api/get_subcalibration_sets_for_manual_selection", post(calibration::get_subcalibration_sets_for_manual_selection))
        .route("/api/get_light_frame_parameters", post(calibration::get_light_frame_parameters))
        .route("/api/manual_assign_calibration", post(calibration::manual_assign_calibration))
        .route("/api/clear_manual_calibration_override", post(calibration::clear_manual_calibration_override))
        .route("/api/manual_assign_subcalibration", post(calibration::manual_assign_subcalibration))
        .route("/api/clear_subcalibration_override", post(calibration::clear_subcalibration_override))
        .route("/api/bulk_update_calibration_metadata", post(calibration::bulk_update_calibration_metadata))
        .route("/api/bulk_restore_calibration_metadata", post(calibration::bulk_restore_calibration_metadata))
        .route("/api/get_custom_metadata_set_ids", post(calibration::get_custom_metadata_set_ids))
        // Duplicates / black hole
        .route("/api/get_duplicates", post(duplicates::get_duplicates))
        .route("/api/move_to_black_hole", post(duplicates::move_to_black_hole))
        .route("/api/bulk_move_to_black_hole", post(duplicates::bulk_move_to_black_hole))
        .route("/api/get_black_hole_files", post(duplicates::get_black_hole_files))
        .route("/api/get_blackholed_file_ids", post(duplicates::get_blackholed_file_ids))
        .route("/api/restore_from_black_hole", post(duplicates::restore_from_black_hole))
        .route("/api/send_to_void", post(duplicates::send_to_void))
        .route("/api/send_all_to_void", post(duplicates::send_all_to_void))
        .route("/api/get_duplicate_folders", post(duplicates::get_duplicate_folders))
        .route("/api/set_scan_root_duplicates_flag", post(duplicates::set_scan_root_duplicates_flag))
        .route("/api/set_scan_root_unique_camera_flag", post(duplicates::set_scan_root_unique_camera_flag))
        .route("/api/verify_files_byte_identical", post(duplicates::verify_files_byte_identical))
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
        .route("/api/get_export_dir", post(export::get_export_dir))
        // Spatial
        .route("/api/get_imaging_locations", post(spatial::get_imaging_locations))
        .route("/api/query_frames_in_bounds", post(spatial::query_frames_in_bounds))
        .route("/api/get_frame_preview", post(images::get_frame_preview))
        // Calendar
        .route("/api/get_calendar_month_data", post(spatial::get_calendar_month_data))
        // Analysis
        .route("/api/get_analysis_config", post(analysis::get_analysis_config))
        .route("/api/set_analysis_config", post(analysis::set_analysis_config))
        .route("/api/reset_analysis_config", post(analysis::reset_analysis_config))
        .route("/api/get_analysis_for_frame_set", post(analysis::get_analysis_for_frame_set))
        .route("/api/analyze_frame_set", post(analysis::analyze_frame_set))
        .route("/api/cancel_analysis", post(analysis::cancel_analysis))
        .route("/api/get_frame_star_metrics", post(analysis::get_frame_star_metrics))
        .route("/api/compute_flat_contour_plot", post(analysis::compute_flat_contour_plot))
        // Plate solving
        .route("/api/get_plate_solve_config", post(plate_solve::get_plate_solve_config))
        .route("/api/set_plate_solve_config", post(plate_solve::set_plate_solve_config))
        .route("/api/reset_plate_solve_config", post(plate_solve::reset_plate_solve_config))
        .route("/api/plate_solve_batch", post(plate_solve::plate_solve_batch))
        .route("/api/cancel_plate_solve", post(plate_solve::cancel_plate_solve))
        .route("/api/autofind_objects_from_coordinates", post(plate_solve::autofind_objects_from_coordinates))
        .route("/api/cancel_autofind_objects", post(plate_solve::cancel_autofind_objects))
        .route("/api/get_plate_solve_result", post(plate_solve::get_plate_solve_result))
        .route("/api/delete_plate_solve_for_frame", post(plate_solve::delete_plate_solve_for_frame))
        .route("/api/get_catalog_status", post(plate_solve::get_catalog_status))
        .route("/api/get_frame_fov_summary", post(plate_solve::get_frame_fov_summary))
        .route("/api/download_catalog_layers", post(plate_solve::download_catalog_layers))
        // Registration (stacking preparation)
        .route("/api/register_frame_set", post(registration::register_frame_set))
        .route("/api/get_frame_set_registration", post(registration::get_frame_set_registration))
        .route("/api/cancel_frame_set_registration", post(registration::cancel_frame_set_registration))
        .route("/api/set_frame_set_reference", post(registration::set_frame_set_reference))
        .route("/api/get_frame_set_reference", post(registration::get_frame_set_reference))
        // Archive feature
        .route("/api/get_archive_settings", post(archive::get_archive_settings))
        .route("/api/set_archive_root_path", post(archive::set_archive_root_path))
        .route("/api/set_archive_compression", post(archive::set_archive_compression))
        .route("/api/plan_archive_operation", post(archive::plan_archive_operation))
        .route("/api/start_archive_operation", post(archive::start_archive_operation))
        .route("/api/cancel_archive_operation", post(archive::cancel_archive_operation))
        .route("/api/list_unfinished_archive_operations", post(archive::list_unfinished_archive_operations))
        .route("/api/resume_archive_operation", post(archive::resume_archive_operation))
        .route("/api/rollback_archive_operation", post(archive::rollback_archive_operation))
        .route("/api/list_archived_frame_sets", post(archive::list_archived_frame_sets))
        .route("/api/list_archive_zips", post(archive::list_archive_zips))
        .route("/api/start_restore_operation", post(archive::start_restore_operation))
        .route("/api/get_restore_suggestions", post(archive::get_restore_suggestions))
        .route("/api/delete_archive", post(archive::delete_archive))
        .route("/api/list_archive_roots", post(archive::list_archive_roots))
        .route("/api/add_archive_root", post(archive::add_archive_root))
        .route("/api/delete_archive_root", post(archive::delete_archive_root))
        .route("/api/set_default_archive_root", post(archive::set_default_archive_root))
        // Core
        .route("/api/initialize_database", post(initialize_database))
        .route("/api/get_log_path", post(get_log_path))
        .route("/api/get_database_path", post(get_database_path))
        // Category A — Desktop-only stubs
        .route("/api/check_for_updates", post(check_for_updates))
        .route("/api/read_fits_image_rustafits", post(read_fits_image_rustafits_stub))
        // Opt-in ATHENAEUM_API_KEY auth (routes/auth.rs). Layered BEFORE
        // `.with_state` below, on a self-contained key-holder state rather
        // than the full WebAppState — see auth.rs for why. Only wraps the
        // `/api/*` routes registered above; the static SPA fallback is
        // attached to `api` further down, after this layer, so it is never
        // touched by the middleware even before its own internal `/api/`
        // path check kicks in.
        .layer(middleware::from_fn_with_state(
            auth::ApiKeyState { api_key: state.api_key.clone() },
            auth::require_api_key,
        ))
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

/// Maps a shared-core `ApiError` onto an Axum status/body pair. Lives here
/// (not as a `From` impl) because both `ApiError` and `(StatusCode, String)`
/// are foreign to this crate — the orphan rule blocks a trait impl on a
/// tuple of two externally-defined types.
pub(crate) fn api_err(e: athenaeum_core::api::ApiError) -> (StatusCode, String) {
    use athenaeum_core::api::ApiError as E;
    use axum::http::StatusCode as S;
    let code = match &e {
        E::NotFound(_) => S::NOT_FOUND,
        E::Invalid(_) => S::BAD_REQUEST,
        E::Conflict(_) => S::CONFLICT,
        E::Forbidden(_) => S::FORBIDDEN,
        E::Internal(_) => S::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string())
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

#[tracing::instrument(skip_all, err(Debug))]
async fn initialize_database(
    State(state): State<WebAppState>,
) -> Result<Json<String>, (StatusCode, Json<serde_json::Value>)> {
    if state.ctx.db.get().is_some() {
        return Ok(Json("Database already initialized".to_string()));
    }
    // In web mode, DB is initialized at server start — this is a no-op
    Ok(Json("Database initialized at server start".to_string()))
}

#[tracing::instrument(skip_all)]
async fn get_log_path() -> Json<Option<String>> {
    // Mirrors the desktop command's semantics: the log directory (rolling
    // daily JSONL files live inside it), not a single file path. `None` if
    // logging failed to initialize (e.g. no writable log dir resolved).
    Json(athenaeum_core::logging::get_path().map(|p| p.to_string_lossy().to_string()))
}

#[tracing::instrument(skip_all)]
async fn get_database_path(
    State(state): State<WebAppState>,
) -> Json<Option<String>> {
    let path = state.ctx.db.get().map(|db| db.path().to_string_lossy().to_string());
    Json(path)
}

// ── Category A — Desktop-only stubs ──────────────────────────────────────────

/// POST /api/check_for_updates — returns static no-update response
#[tracing::instrument(skip_all)]
async fn check_for_updates(
    Json(_): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let version = env!("CARGO_PKG_VERSION").to_string();
    Json(serde_json::json!({
        "current_version": version,
        "latest_version": version,
        "is_update_available": false,
    }))
}

/// POST /api/relocate_missing_file — 501 in web mode (needs native file picker)
#[tracing::instrument(skip_all)]
async fn relocate_missing_file_stub(
    Json(_): Json<serde_json::Value>,
) -> (StatusCode, String) {
    (StatusCode::NOT_IMPLEMENTED, "relocate_missing_file is not available in web mode".to_string())
}

/// POST /api/read_fits_image_rustafits — 501 in web mode (web uses get_frame_preview)
#[tracing::instrument(skip_all)]
async fn read_fits_image_rustafits_stub(
    Json(_): Json<serde_json::Value>,
) -> (StatusCode, String) {
    (StatusCode::NOT_IMPLEMENTED, "read_fits_image_rustafits is not available in web mode".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SseEvent;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tower::ServiceExt; // oneshot

    /// Builds a real `WebAppState` for router-level tests — unlike
    /// `auth::tests`'s hand-rolled mini router (which only proves the
    /// middleware fn itself is correct), this drives the REAL
    /// `build_router()` output, so it catches a route registered AFTER the
    /// auth `.layer()` call silently losing coverage (fail-open), which a
    /// mini-router test structurally cannot catch.
    ///
    /// `ctx.db` is deliberately left unset (empty `OnceLock`) rather than
    /// wired to a real database: no test in this crate constructs a real
    /// `athenaeum_core::db::Database` (every DB-touching test elsewhere in
    /// the workspace uses a raw in-memory `rusqlite::Connection` +
    /// `init_db`, which doesn't fit `Database`'s pooled-connection shape),
    /// and it isn't needed here — `get_scan_roots` (see below) checks
    /// `ctx.db.get()` and returns a clean 500 rather than panicking when
    /// it's `None`. That 500 is exactly the "request got past auth" signal
    /// this test needs; a real DB would add setup cost without changing
    /// what's being verified (auth middleware coverage, not handler logic).
    fn test_state(api_key: Option<&str>) -> WebAppState {
        let ctx = Arc::new(ServiceContext {
            db: OnceLock::new(),
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            // Spawns one real worker thread (see operation_queue.rs — there
            // is no lighter-weight constructor); matches that module's own
            // tests, which accept the same one-thread-per-test-run cost.
            operation_queue: OperationQueue::start(),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths: Vec::new(),
            export_dir: None,
            api_key: api_key.map(str::to_string),
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
        }
    }

    #[tokio::test]
    async fn real_router_enforces_api_key_on_get_scan_roots() {
        let app = build_router(test_state(Some("secret")), None);

        let resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/get_scan_roots")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/get_scan_roots")
                    .header("content-type", "application/json")
                    .header("X-API-Key", "secret")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // The test state's DB OnceLock is intentionally unset, so the
        // handler itself fails once it runs — but with a clean 500
        // ("Database not initialized"), not a 401. A non-401 response here
        // proves the request reached the handler, i.e. got past the real
        // router's auth layer.
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
