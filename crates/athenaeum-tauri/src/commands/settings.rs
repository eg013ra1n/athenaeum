// Settings commands - application configuration

use crate::db;
use crate::models::Setting;
use crate::settings;
use std::sync::Arc;
use tauri::State;

use super::AppState;

/// Get a setting value by key (with precedence: runtime > DB > default)
#[tauri::command]
pub async fn get_setting(
    key: String,
    default_value: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let state_lock = state.ctx.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let default = default_value.unwrap_or_default();
    state.ctx.settings
        .get_with_precedence(&conn, &key, &default)
        .map_err(|e| e.to_string())
}

/// Set a setting value (persists to database)
#[tauri::command]
pub async fn set_setting(
    key: String,
    value: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.ctx.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    state.ctx.settings
        .persist_setting(&conn, &key, &value)
        .map_err(|e| e.to_string())
}

/// Get all settings from database
#[tauri::command]
pub async fn get_all_settings(state: State<'_, AppState>) -> Result<Vec<Setting>, String> {
    let state_lock = state.ctx.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_all_settings(&conn).map_err(|e| e.to_string())
}

/// Delete a setting by key
#[tauri::command]
pub async fn delete_setting(key: String, state: State<'_, AppState>) -> Result<(), String> {
    let state_lock = state.ctx.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_setting(&conn, &key).map_err(|e| e.to_string())
}

/// Get the grouping threshold in degrees (with unit conversion)
#[tauri::command]
pub async fn get_grouping_threshold_deg(state: State<'_, AppState>) -> Result<f64, String> {
    let state_lock = state.ctx.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    state.ctx.settings
        .get_grouping_threshold_deg(&conn)
        .map_err(|e| e.to_string())
}

/// Set the number of concurrent blink image processing threads.
/// Validates 1..=max_blink_threads, persists to DB, and rebuilds the semaphore.
#[tauri::command]
pub async fn set_blink_threads(
    threads: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let max = state.max_blink_threads as u32;
    if threads < 1 || threads > max {
        return Err(format!("Blink threads must be between 1 and {}", max));
    }

    // Persist to DB
    {
        let state_lock = state.ctx.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();
        state.ctx.settings
            .persist_setting(&conn, settings::keys::BLINK_THREADS, &threads.to_string())
            .map_err(|e| e.to_string())?;
    }

    // Rebuild semaphore — in-flight permits on the old Arc complete naturally
    *state.image_semaphore.write().unwrap() =
        Arc::new(tokio::sync::Semaphore::new(threads as usize));

    println!("🧵 Blink semaphore rebuilt with {} permits", threads);
    Ok(())
}

/// Returns the CPU-based max so the frontend can set the slider/input max dynamically.
#[tauri::command]
pub async fn get_blink_threads_max(state: State<'_, AppState>) -> Result<u32, String> {
    Ok(state.max_blink_threads as u32)
}
