// Core modules
mod models;
mod db;
mod fits_parser;
mod scanner;
mod duplicates;
mod calibration;
mod settings;
mod coordinates;
mod clustering;
mod sessions;
mod cache;
mod rustafits_processor;
mod fingerprint;
mod relinking;
mod selection;
mod frames_set_metadata;
mod frames_set_merge;
mod export;
mod stacking;

// Commands (Tauri API endpoints)
mod commands;
mod commands_rustafits;

use cache::CacheManager;
use commands::AppState;
use settings::SettingsManager;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(AppState {
            db: Mutex::new(None),
            settings: Arc::new(SettingsManager::new()),
            cache: Arc::new(Mutex::new(None)),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
        })
        .setup(|app| {
            // Initialize cache manager after app is ready
            let app_handle = app.handle();
            let state: State<AppState> = app.state();

            // Get app data directory for cache
            if let Ok(app_dir) = app_handle.path().app_data_dir() {
                match CacheManager::new(&app_dir, state.settings.clone()) {
                    Ok(cache_mgr) => {
                        *state.cache.lock().unwrap() = Some(cache_mgr);
                        println!("✅ Cache manager initialized");
                    }
                    Err(e) => {
                        eprintln!("⚠️  Failed to initialize cache manager: {}", e);
                    }
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::greet,
            commands::initialize_database,
            commands::add_scan_root,
            commands::get_scan_roots,
            commands::delete_scan_root,
            commands::start_scan,
            commands::start_scan_with_progress,
            commands::cancel_scan,
            commands::get_active_scans,
            commands::rescan_all_for_content_hash,
            commands::get_files,
            commands::get_files_by_directory,
            commands::get_frames_with_missing_metadata,
            commands::get_duplicates,
            commands::get_directory_contents,
            commands::get_setting,
            commands::set_setting,
            commands::get_all_settings,
            commands::delete_setting,
            commands::get_grouping_threshold_deg,
            commands::auto_generate_frame_sets,
            commands::get_frames_sets,
            commands::delete_frames_set,
            commands::delete_auto_generated_frame_sets,
            commands::rename_frames_set,
            commands::mark_frame_set_custom,
            commands::recalculate_frame_set_metadata,
            commands::update_frame_set_flat_pattern,
            commands::merge_frame_sets,
            commands::can_split,
            commands::split_frame_set,
            commands::get_frame_set_detail,
            commands::create_custom_frames_set,
            commands::get_equipment_cameras,
            commands::create_dark_library,
            commands::get_dark_library,
            commands::delete_dark_library,
            commands::has_dark_library,
            commands::create_master_dark_library,
            commands::get_master_dark_library,
            commands::has_master_dark_library,
            commands::create_master_flat_library,
            commands::get_master_flat_library,
            commands::has_master_flat_library,
            commands::refresh_calibration_library_for_camera,
            commands::get_calibration_set_frames,
            commands::get_cache_stats,
            commands::clear_image_cache,
            commands::set_scan_root_duplicates_flag,
            commands::move_to_black_hole,
            commands::get_black_hole_files,
            commands::get_blackholed_file_ids,
            commands::restore_from_black_hole,
            commands::send_to_void,
            commands::send_all_to_void,
            commands::get_duplicate_folders,
            commands::backfill_header_fingerprints,
            commands::relink_scan_root,
            commands::get_orphaned_files,
            commands::delete_orphaned_files,
            commands::check_scan_root_availability,
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
            commands::query_frames_in_circle,
            commands::query_frames_in_bounds,
            commands::query_frames_in_polygon,
            commands::get_calendar_month_data,
            commands::create_frame_set_from_selection,
            commands::find_calibration_for_frame_set,
            commands::get_calibration_status,
            commands::get_frame_set_calibration_groups,
            commands::get_calibration_hierarchy_for_frame_set,
            commands::get_frame_calibration_hierarchy,
            commands::get_flat_group_options_for_frame_set,
            commands::clear_calibration_links,
            commands::get_frame_calibration_links,
            commands::get_frame_status,
            commands::get_calibration_matching_config,
            commands::set_calibration_matching_config,
            commands::reset_calibration_matching_config,
            commands::get_light_frame_parameters,
            commands::get_calibration_sets_for_manual_selection,
            commands::manual_assign_calibration,
            commands::clear_manual_calibration_override,
            commands::cleanup_duplicate_flat_subcalibrations,
            commands::get_calibration_set_parameters,
            commands::get_subcalibration_sets_for_manual_selection,
            commands::manual_assign_subcalibration,
            commands::clear_subcalibration_override,
            commands::bulk_update_calibration_metadata,
            commands::bulk_restore_calibration_metadata,
            commands::get_custom_metadata_set_ids,
            commands::get_calibration_set_originals,
            commands::get_export_preview,
            commands::get_calibration_route,
            commands::get_export_groups_summary,
            commands::get_siril_path,
            commands::set_siril_path,
            commands::get_exportable_frame_sets,
            commands::diagnose_calibration_links,
            commands::export_frame_set_v3,
            commands::get_export_preview_v3,
            commands::organize_registered_files_cmd,
            commands::get_registered_files_status,
            commands::run_siril_export_pipeline,
            commands::run_siril_export_pipeline_v2,
            // Pipeline job management commands
            commands::create_export_job,
            commands::get_export_job,
            commands::get_export_jobs,
            commands::start_export_job,
            commands::pause_export_job,
            commands::cancel_export_job,
            commands::resume_export_job,
            commands::retry_failed_step,
            commands::validate_export_sources,
            commands::get_pipeline_plan,
            commands::delete_export_job,
            commands::get_job_history,
            commands_rustafits::read_fits_image_rustafits,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
