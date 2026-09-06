// Settings commands - application configuration

use crate::db;
use crate::logging;
use crate::settings;
use std::sync::Arc;
use tauri::State;

use super::AppState;

/// Get a setting value by key (with precedence: runtime > DB > default)
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
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
#[tracing::instrument(skip_all, err, level = "debug")]
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
    } else if key == settings::keys::BLINK_MEMORY_CACHE_MAX_MB {
        let mb: usize = value.parse().unwrap_or(512);
        state.ctx.memory_cache.lock().unwrap().set_max_bytes(mb * 1024 * 1024);
    } else if key == settings::keys::BLINK_MEMORY_RETENTION_MINUTES {
        let minutes: u64 = value.parse().unwrap_or(30);
        state.ctx.memory_cache.lock().unwrap().set_retention(minutes);
    } else if key == settings::keys::MONITORING_INTERVAL_MINUTES
        || key == settings::keys::MONITORING_ENABLED_GLOBAL
    {
        // Wake the monitor loop so the new interval / enabled flag takes
        // effect now instead of after the current sleep expires.
        state.monitor.kick();
    }

    Ok(())
}

/// Delete a setting by key
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_setting(key: String, state: State<'_, AppState>) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_setting(&conn, &key).map_err(|e| e.to_string())
}

/// Set the number of concurrent blink image processing threads.
/// Validates 0..=max_blink_threads (0 = auto, use all cores), persists to DB,
/// and rebuilds the semaphore.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
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

    tracing::info!(permits = effective, requested = threads, "blink semaphore rebuilt");
    Ok(())
}

/// Returns the CPU-based max so the frontend can set the slider/input max dynamically.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_blink_threads_max(state: State<'_, AppState>) -> Result<u32, String> {
    Ok(state.max_blink_threads as u32)
}

// ── Logging config (Task 3) ──────────────────────────────────────────────────

/// Returns the effective logging config (persisted, or the default if
/// unset) plus whether the `ATHENAEUM_LOG` env override is currently active.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_logging_config(
    state: State<'_, AppState>,
) -> Result<logging::config::LoggingConfigResponse, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let config = match db::get_setting(&conn, logging::config::SETTINGS_KEY).map_err(|e| e.to_string())? {
        Some(raw) => serde_json::from_str::<logging::LoggingConfig>(&raw).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "invalid stored logging config; using default");
            logging::LoggingConfig::default()
        }),
        None => logging::LoggingConfig::default(),
    };
    let env_override_active = logging::global_handle()
        .map(|h| h.env_override_active())
        .unwrap_or(false);

    Ok(logging::config::LoggingConfigResponse { config, env_override_active })
}

/// Validates the config (rejects an out-of-range level or per-module level,
/// or directives that fail to parse), persists it under `logging.config`,
/// then live-applies it via the process-global `LoggingHandle`. Validation
/// happens before the DB write; the live-apply happens after.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_logging_config(
    config: logging::LoggingConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    config.validate().map_err(|e| {
        tracing::warn!(error = %e, "rejected invalid logging config");
        e
    })?;

    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    db::set_setting(&conn, logging::config::SETTINGS_KEY, &json).map_err(|e| e.to_string())?;

    if let Some(handle) = logging::global_handle() {
        handle.apply_config(&config);
    }

    Ok(())
}
