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
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
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
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    state.ctx.settings
        .persist_setting(&conn, &key, &value)
        .map_err(|e| e.to_string())?;

    // Update in-memory cache settings when they change
    if key == settings::keys::BLINK_MEMORY_CACHE_SIZE {
        let size: usize = value.parse().unwrap_or(200);
        state.ctx.memory_cache.lock().unwrap().set_max_entries(size);
    } else if key == settings::keys::BLINK_MEMORY_RETENTION_MINUTES {
        let minutes: u64 = value.parse().unwrap_or(30);
        state.ctx.memory_cache.lock().unwrap().set_retention(minutes);
    }

    Ok(())
}

/// Get all settings from database
#[tauri::command]
pub async fn get_all_settings(state: State<'_, AppState>) -> Result<Vec<Setting>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_all_settings(&conn).map_err(|e| e.to_string())
}

/// Delete a setting by key
#[tauri::command]
pub async fn delete_setting(key: String, state: State<'_, AppState>) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_setting(&conn, &key).map_err(|e| e.to_string())
}

/// Get the grouping threshold in degrees (with unit conversion)
#[tauri::command]
pub async fn get_grouping_threshold_deg(state: State<'_, AppState>) -> Result<f64, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    state.ctx.settings
        .get_grouping_threshold_deg(&conn)
        .map_err(|e| e.to_string())
}

/// Set the number of concurrent blink image processing threads.
/// Validates 0..=max_blink_threads (0 = auto, use all cores), persists to DB,
/// and rebuilds the semaphore.
#[tauri::command]
pub async fn set_blink_threads(
    threads: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let max = state.max_blink_threads as u32;
    if threads > max {
        return Err(format!("Blink threads must be between 0 and {} (0 = auto)", max));
    }

    // Persist to DB
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        state.ctx.settings
            .persist_setting(&conn, settings::keys::BLINK_THREADS, &threads.to_string())
            .map_err(|e| e.to_string())?;
    }

    // Rebuild semaphore — 0 means auto (half of available cores)
    let effective = if threads == 0 { ((max as usize) / 2).max(2) } else { threads as usize };
    *state.image_semaphore.write().unwrap() =
        Arc::new(tokio::sync::Semaphore::new(effective));

    println!("🧵 Blink semaphore rebuilt with {} permits (requested {}, 0=auto)", effective, threads);
    Ok(())
}

/// Returns the CPU-based max so the frontend can set the slider/input max dynamically.
#[tauri::command]
pub async fn get_blink_threads_max(state: State<'_, AppState>) -> Result<u32, String> {
    Ok(state.max_blink_threads as u32)
}
