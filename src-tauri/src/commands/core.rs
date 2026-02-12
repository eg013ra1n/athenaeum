// Core commands - app initialization and basic operations

use crate::db::Database;
use crate::settings;
use std::sync::Arc;
use tauri::{Manager, State};

use super::AppState;

#[tauri::command]
pub fn greet(name: &str) -> String {
    format!("Hello, {}! Welcome to Athenaeum!", name)
}

#[tauri::command]
pub async fn initialize_database(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let app_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;

    std::fs::create_dir_all(&app_dir).map_err(|e| e.to_string())?;

    let db_path = app_dir.join("athenaeum.db");

    // Hold the lock for the entire initialization to prevent concurrent
    // Database::new calls (React StrictMode fires effects twice in dev)
    let mut db_lock = state.db.lock().unwrap();
    if db_lock.is_some() {
        return Ok(db_path.to_string_lossy().to_string());
    }

    let db = Database::new(db_path.clone()).map_err(|e| {
        crate::logging::log("ERROR", &format!("Database init failed: {}", e));
        e.to_string()
    })?;

    *db_lock = Some(db);

    // Apply persisted blink.threads setting to the semaphore
    if let Some(ref db) = *db_lock {
        let conn = db.conn();
        let saved = state.settings
            .get_with_precedence(&conn, settings::keys::BLINK_THREADS, settings::defaults::BLINK_THREADS)
            .unwrap_or_else(|_| settings::defaults::BLINK_THREADS.to_string());
        if let Ok(threads) = saved.parse::<usize>() {
            let max = state.max_blink_threads;
            let clamped = threads.clamp(1, max);
            *state.image_semaphore.write().unwrap() =
                Arc::new(tokio::sync::Semaphore::new(clamped));
            println!("🧵 Blink semaphore set to {} permits (from DB)", clamped);
        }
    }

    crate::logging::log("INFO", &format!("Database initialized: {}", db_path.display()));
    Ok(db_path.to_string_lossy().to_string())
}
