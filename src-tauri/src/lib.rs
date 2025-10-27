// Core modules
mod models;
mod db;
mod fits_parser;
mod scanner;
mod duplicates;
mod calibration;
mod export;

// Commands (Tauri API endpoints)
mod commands;

use commands::AppState;
use std::sync::Mutex;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(AppState {
            db: Mutex::new(None),
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
