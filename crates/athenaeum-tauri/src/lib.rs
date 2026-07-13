// Re-export modules from athenaeum-core
pub use athenaeum_core::models;
pub use athenaeum_core::coordinates;
pub use athenaeum_core::fingerprint;
pub use athenaeum_core::db;
pub use athenaeum_core::fits_parser;
pub use athenaeum_core::clustering;
pub use athenaeum_core::settings;
pub use athenaeum_core::sessions;
pub use athenaeum_core::duplicates;
pub use athenaeum_core::relinking;
pub use athenaeum_core::frames_set_metadata;
pub use athenaeum_core::frames_set_merge;
pub use athenaeum_core::logging;
pub use athenaeum_core::calibration;

pub use athenaeum_core::scanner;
pub use athenaeum_core::events;
pub use athenaeum_core::export;
pub use athenaeum_core::rustafits_processor;
pub use athenaeum_core::cache;
pub use athenaeum_core::auto_merge;
pub use athenaeum_core::monitor;

// Tauri-specific modules
pub mod tauri_events;


// Commands (Tauri API endpoints)
mod commands;
mod commands_rustafits;

use cache::MemoryImageCache;
use commands::AppState;
use athenaeum_core::services::ServiceContext;
use settings::SettingsManager;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{Manager, State};

/// Initialize file-based logging before Tauri starts.
/// Called from main() so panics during Tauri init are captured.
pub fn init_logging() {
    // Retains the LoggingHandle (and its WorkerGuard) for the process
    // lifetime via a global OnceLock; reachable later via
    // `logging::global_handle()` for settings-driven `apply_config`.
    logging::init_global(logging::Process::Desktop);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage({
            let max_threads = std::thread::available_parallelism()
                .map(|n| n.get().min(16))
                .unwrap_or(4);
            // Limit concurrent image processing to half of available cores.
            // Each annotated request runs converter + analyzer sequentially,
            // both using pool.install(). Too many concurrent requests starve
            // each other on the pool. Half the cores gives each request ~2
            // pool threads — good balance of throughput vs per-frame speed.
            let default_permits = (max_threads / 2).max(2);
            tracing::info!(max_threads, default_permits, "blink semaphore initialized");
            AppState {
                ctx: Arc::new(ServiceContext {
                    db: OnceLock::new(),
                    settings: Arc::new(SettingsManager::new()),
                    memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(400, 30))),
                    active_scans: Arc::new(Mutex::new(HashMap::new())),
                    active_exports: Arc::new(Mutex::new(HashMap::new())),
                    active_analyses: Arc::new(Mutex::new(HashMap::new())),
                    active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
                    active_registrations: Arc::new(Mutex::new(HashMap::new())),
                    active_archives: Arc::new(Mutex::new(HashMap::new())),
                    active_master_builds: Arc::new(Mutex::new(HashMap::new())),
                    active_light_cal: Arc::new(Mutex::new(HashMap::new())),
                    dso_catalog: Arc::new(std::sync::RwLock::new(None)),
                    star_cache: Arc::new(std::sync::RwLock::new(None)),
                    bright_cache: Arc::new(std::sync::RwLock::new(None)),
                    image_pool: Arc::new(
                        rayon::ThreadPoolBuilder::new()
                            .num_threads(max_threads)
                            .build()
                            .expect("Failed to create image processing thread pool"),
                    ),
                    operation_queue: athenaeum_core::services::operation_queue::OperationQueue::start(),
                    compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
                }),
                image_semaphore: std::sync::RwLock::new(Arc::new(
                    tokio::sync::Semaphore::new(default_permits),
                )),
                max_blink_threads: max_threads,
                monitor: athenaeum_core::monitor::MonitorService::new(),
                sync: Arc::new(athenaeum_core::sync::SyncRuntime::new()),
                sync_sender: Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
            }
        })
        .setup(|app| {
            let app_handle = app.handle();
            let state: State<AppState> = app.state();

            // Auto-reconcile abandoned cross-volume moves — enqueue this as
            // the FIRST job on the operation queue so it serializes ahead of
            // any file op the user triggers. The desktop app initializes its
            // DB handle lazily (frontend calls `initialize_database` after
            // this closure returns), so the DB is not guaranteed to be ready
            // yet when this job actually runs; it checks at run time and
            // no-ops with a stderr line rather than failing. The pre-enqueue
            // trigger in `enqueue_move_operation` is what does the real work
            // in practice — see `athenaeum_core::file_op::reconcile`.
            {
                use athenaeum_core::services::operation_queue::{OperationKind, QueuedJob};
                let ctx_for_reconcile = state.ctx.clone();
                let app_for_reconcile = app_handle.clone();
                state.ctx.operation_queue.enqueue(QueuedJob {
                    kind: OperationKind::FileOpReconcile,
                    operation_id: 0,
                    run: Box::new(move || {
                        let Some(db) = ctx_for_reconcile.db.get() else {
                            // Expected on desktop: the DB is initialized lazily by the
                            // frontend's `initialize_database` call after this closure is
                            // enqueued, so this job commonly races it. The pre-enqueue
                            // trigger in `enqueue_move_operation` does the real work.
                            tracing::debug!("file_op reconcile (startup): database not yet initialized, skipping");
                            return;
                        };
                        let conn = db.conn();
                        match athenaeum_core::file_op::reconcile::reconcile_abandoned_commit_moves(&conn) {
                            Ok(summary) if summary.healed > 0 || !summary.skipped.is_empty() => {
                                let emitter = tauri_events::TauriProgressEmitter(app_for_reconcile);
                                athenaeum_core::events::emit_event(
                                    &emitter,
                                    "file-op-reconciled",
                                    &athenaeum_core::file_op::models::FileOpReconciled {
                                        healed: summary.healed,
                                        skipped: summary.skipped.len(),
                                        operation_ids: summary.touched_operation_ids(),
                                    },
                                );
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = ?e, "file_op reconcile (startup) failed"),
                        }
                    }),
                });
            }

            // Clean up old file-based cache directory if it exists (one-time migration)
            if let Ok(app_dir) = app_handle.path().app_data_dir() {
                let old_cache_dir = app_dir.join("cache");
                if old_cache_dir.exists() {
                    match std::fs::remove_dir_all(&old_cache_dir) {
                        Ok(()) => tracing::info!(path = ?old_cache_dir, "removed old file cache directory"),
                        Err(e) => tracing::warn!(path = ?old_cache_dir, error = %e, "failed to remove old cache directory"),
                    }
                }
            }

            // Read memory cache settings from DB and apply
            {
                if let Some(db) = state.ctx.db.get() {
                    let conn = db.conn();
                    let cache_size: usize = db::get_setting(&conn, settings::keys::BLINK_MEMORY_CACHE_SIZE)
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(200);
                    let retention_minutes: u64 = db::get_setting(&conn, settings::keys::BLINK_MEMORY_RETENTION_MINUTES)
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(30);
                    let mut cache = state.ctx.memory_cache.lock().unwrap();
                    cache.set_max_entries(cache_size);
                    cache.set_retention(retention_minutes);
                    tracing::info!(cache_size, retention_minutes, "memory cache initialized");
                }
            }

            // Wire the compute-queue transport notifier. DB-independent (just
            // needs the AppHandle), so this belongs here rather than
            // `initialize_database` — unlike the persisted `compute.max_concurrent`
            // value itself, which is applied in `initialize_database` (see its
            // doc comment: the desktop app's DB handle is initialized lazily by
            // the frontend AFTER this closure returns, so a read attempted here
            // would silently always no-op, exactly like `blink.threads`).
            {
                let emitter_handle = app_handle.clone();
                state.ctx.compute_queue.set_notifier(Box::new(move |entries| {
                    let emitter = tauri_events::TauriProgressEmitter(emitter_handle.clone());
                    athenaeum_core::events::emit_event(
                        &emitter,
                        "compute-queue-changed",
                        &serde_json::json!({ "entries": entries }),
                    );
                }));
            }

            // Spawn background sweeper for stale memory-cache entries (every 60s)
            let memory_cache = Arc::clone(&state.ctx.memory_cache);
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let mut cache = memory_cache.lock().unwrap();
                    cache.evict_stale();
                }
            });

            // Spawn background folder-monitoring service. Deferred 3s so the
            // initial UI render doesn't compete with scanner I/O. The handle
            // lives in AppState so commands can `kick()` it when relevant
            // settings change; we run a clone here.
            {
                let ctx_clone = Arc::clone(&state.ctx);
                let emitter_arc = Arc::new(tauri_events::TauriProgressEmitter(app_handle.clone()));
                let monitor = state.monitor.clone();
                tauri::async_runtime::spawn(async move {
                    monitor
                        .run_loop(ctx_clone, emitter_arc, Duration::from_secs(3))
                        .await;
                });
            }

            // Full-app capture-node retention loop (task M4). Hourly; every tick
            // is gated inside `run_app_retention_once` (a no-op unless this is a
            // signed-in `capture` node with a policy other than keep_everything),
            // and dry-run is the enforced default — live deletion needs the
            // explicit `sync.retention.live_confirmed` opt-in. Safe to arm
            // unconditionally: on any other device it never touches disk.
            {
                let ctx_for_retention = Arc::clone(&state.ctx);
                tauri::async_runtime::spawn(async move {
                    athenaeum_core::api::retention::run_app_retention_loop(
                        ctx_for_retention,
                        Duration::from_secs(
                            athenaeum_core::api::retention::DEFAULT_RETENTION_INTERVAL_SECS,
                        ),
                    )
                    .await;
                });
            }

            tracing::info!("Athenaeum started, Tauri setup complete");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::initialize_database,
            commands::check_for_updates,
            commands::add_scan_root,
            commands::get_scan_roots,
            commands::get_calibration_library_root,
            commands::get_calibration_library_dir,
            commands::set_calibration_library_dir,
            commands::clear_calibration_library_dir,
            commands::get_sync_incoming_dir,
            commands::set_sync_incoming_dir,
            commands::clear_sync_incoming_dir,
            commands::get_collaboration_dir,
            commands::set_collaboration_dir,
            commands::clear_collaboration_dir,
            commands::delete_scan_root,
            commands::start_scan,
            commands::start_scan_with_progress,
            commands::cancel_scan,
            commands::rescan_all_for_content_hash,
            commands::get_files,
            commands::get_files_by_directory,
            commands::get_frames_with_missing_metadata,
            commands::bulk_update_frame_metadata,
            commands::count_frame_metadata_relations,
            commands::get_frame_memberships,
            commands::get_frame_metadata_originals,
            commands::get_distinct_instrumes,
            commands::enqueue_move_operation,
            commands::search_catalog,
            commands::resolve_frame_ids_for_paths,
            commands::mkdir_in_scan_root,
            commands::rename_path,
            commands::get_duplicates,
            commands::get_directory_contents,
            commands::get_camera_directories,
            commands::get_camera_directory_contents,
            commands::get_setting,
            commands::set_setting,
            commands::delete_setting,
            commands::get_logging_config,
            commands::set_logging_config,
            commands::set_blink_threads,
            commands::get_blink_threads_max,
            commands::auto_generate_frame_sets,
            commands::get_frames_sets,
            commands::delete_frames_set,
            commands::delete_auto_generated_frame_sets,
            commands::rename_frames_set,
            commands::mark_frame_set_custom,
            commands::merge_frame_sets,
            commands::split_frame_set,
            commands::get_frame_set_detail,
            commands::archive_frame_set,
            commands::find_new_frames_for_set,
            commands::auto_merge_new_frames_for_set,
            commands::get_frame_set_merge_log,
            commands::get_equipment_cameras,
            commands::get_dark_library,
            commands::has_dark_library,
            commands::get_master_dark_library,
            commands::has_master_dark_library,
            commands::get_master_flat_library,
            commands::has_master_flat_library,
            commands::refresh_calibration_library_for_camera,
            commands::get_calibration_set_frames,
            commands::get_calibration_set_consumers,
            commands::set_scan_root_duplicates_flag,
            commands::set_scan_root_unique_camera_flag,
            commands::set_scan_root_monitor_enabled,
            commands::move_to_black_hole,
            commands::bulk_move_to_black_hole,
            commands::get_black_hole_files,
            commands::get_blackholed_file_ids,
            commands::restore_from_black_hole,
            commands::send_to_void,
            commands::send_all_to_void,
            commands::get_duplicate_folders,
            commands::verify_files_byte_identical,
            commands::relink_scan_root,
            commands::check_all_scan_roots_availability,
            commands::check_missing_files_in_scan_root,
            commands::sync_missing_files,
            commands::get_missing_files,
            commands::get_missing_files_counts,
            commands::recheck_missing_files,
            commands::ignore_missing_file,
            commands::unignore_missing_file,
            commands::delete_missing_files,
            commands::relocate_missing_file,
            commands::get_imaging_locations,
            commands::get_frame_preview,
            commands::get_files_with_frames_by_ids,
            commands::query_frames_in_bounds,
            commands::get_calendar_month_data,
            commands::create_frame_set_from_selection,
            commands::get_excluded_frames_with_metadata,
            commands::get_excluded_frames_count,
            commands::remove_files_from_excluded,
            commands::find_calibration_for_frame_set,
            commands::get_calibration_hierarchy_for_frame_set,
            commands::get_calibration_matching_config,
            commands::set_calibration_matching_config,
            commands::reset_calibration_matching_config,
            commands::get_light_frame_parameters,
            commands::get_calibration_sets_for_manual_selection,
            commands::manual_assign_calibration,
            commands::clear_manual_calibration_override,
            commands::get_calibration_set_parameters,
            commands::get_subcalibration_sets_for_manual_selection,
            commands::manual_assign_subcalibration,
            commands::clear_subcalibration_override,
            commands::bulk_update_calibration_metadata,
            commands::bulk_restore_calibration_metadata,
            commands::get_custom_metadata_set_ids,
            // Export commands
            commands::get_wbpp_export_config,
            commands::set_wbpp_export_config,
            commands::reset_wbpp_export_config,
            commands::get_export_preview,
            commands::get_exportable_frame_sets,
            commands::get_calibration_route,
            commands::get_export_readiness,
            commands::export_to_wbpp,
            commands::cancel_export,
            commands::get_export_summary,
            commands::get_log_path,
            commands::get_database_path,
            // Analysis commands
            commands::get_analysis_config,
            commands::set_analysis_config,
            commands::reset_analysis_config,
            commands::analyze_frame_set,
            commands::cancel_analysis,
            commands::get_analysis_for_frame_set,
            commands::get_frame_star_metrics,
            commands::compute_flat_contour_plot,
            // Compute queue commands
            commands::get_compute_queue,
            commands::cancel_compute_job,
            commands::set_compute_max_concurrent,
            // Master build commands
            commands::preview_master_build,
            commands::start_master_build,
            commands::start_master_builds_batch,
            commands::rebuild_master,
            commands::cancel_master_build,
            commands::get_master_provenance,
            commands::archive_calibration_originals,
            commands::restore_calibration_originals,
            commands::clear_stale_archive_markers,
            // Light calibration commands
            commands::get_light_calibration_readiness,
            commands::get_light_calibration_details,
            commands::start_light_calibration,
            commands::cancel_light_calibration,
            // Plate solve commands
            commands::get_plate_solve_config,
            commands::set_plate_solve_config,
            commands::reset_plate_solve_config,
            commands::plate_solve_batch,
            commands::cancel_plate_solve,
            commands::autofind_objects_from_coordinates,
            commands::cancel_autofind_objects,
            commands::get_plate_solve_result,
            commands::delete_plate_solve_for_frame,
            commands::get_catalog_status,
            commands::get_frame_fov_summary,
            commands::download_catalog_layers,
            commands_rustafits::read_fits_image_rustafits,
            // Archive commands
            commands::get_archive_settings,
            commands::set_archive_root_path,
            commands::set_archive_compression,
            commands::plan_archive_operation,
            commands::start_archive_operation,
            commands::cancel_archive_operation,
            commands::list_unfinished_archive_operations,
            commands::resume_archive_operation,
            commands::rollback_archive_operation,
            commands::list_archived_frame_sets,
            commands::list_archive_zips,
            commands::start_restore_operation,
            commands::get_restore_suggestions,
            commands::delete_archive,
            commands::list_archive_roots,
            commands::add_archive_root,
            commands::delete_archive_root,
            commands::set_default_archive_root,
            // Registration (stacking preparation)
            commands::register_frame_set,
            commands::get_frame_set_registration,
            commands::cancel_frame_set_registration,
            commands::set_frame_set_reference,
            commands::get_frame_set_reference,
            commands::get_sync_pairing_ticket,
            commands::get_sync_status,
            commands::list_sync_history,
            commands::enqueue_sync_selection,
            commands::get_sync_auto_mode,
            commands::set_sync_auto_mode,
            commands::get_sync_device_names,
            commands::account_sign_in_start,
            commands::account_sign_in_verify,
            commands::account_status,
            commands::account_sign_out,
            commands::list_account_devices,
            commands::revoke_account_device,
            commands::rename_device,
            commands::list_collab_projects,
            commands::refresh_collab_projects,
            commands::get_collab_project_detail,
            commands::evaluate_collab_gate,
            commands::list_collab_link_suggestions,
            commands::set_collab_link,
            commands::create_collab_link_intent,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
