// Core modules
mod models;
mod db;
mod fits_parser;
mod scanner;
mod duplicates;
mod calibration;
mod export;
mod settings;
mod coordinates;
mod clustering;
mod sessions;
mod cache;
mod rustafits_processor;

// Commands (Tauri API endpoints)
mod commands;
mod commands_rustafits;

use cache::CacheManager;
use commands::AppState;
use settings::SettingsManager;
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
            commands::get_files,
            commands::get_files_by_directory,
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
            commands::rename_frames_set,
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
            commands::get_cache_stats,
            commands::clear_image_cache,
            commands_rustafits::read_fits_image_rustafits,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
