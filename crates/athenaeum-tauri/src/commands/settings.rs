// Settings commands - application configuration

use crate::db;
use crate::logging;
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
    state
        .ctx
        .settings
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
    athenaeum_core::api::settings::set_setting(&state.ctx, &key, &value, &state.monitor)
        .map_err(|e| e.to_string())
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
/// Validates 0..=max_blink_threads (0 = auto, half the cores), persists to DB,
/// and rebuilds the semaphore.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_blink_threads(threads: u32, state: State<'_, AppState>) -> Result<(), String> {
    let effective = athenaeum_core::api::settings::set_blink_threads(
        &state.ctx,
        threads,
        state.max_blink_threads,
    )
    .map_err(|e| e.to_string())?;

    // Rebuild semaphore
    *state.image_semaphore.write().unwrap() = Arc::new(tokio::sync::Semaphore::new(effective));

    tracing::info!(
        permits = effective,
        requested = threads,
        "blink semaphore rebuilt"
    );
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

    let config =
        match db::get_setting(&conn, logging::config::SETTINGS_KEY).map_err(|e| e.to_string())? {
            Some(raw) => serde_json::from_str::<logging::LoggingConfig>(&raw).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "invalid stored logging config; using default");
                logging::LoggingConfig::default()
            }),
            None => logging::LoggingConfig::default(),
        };
    let env_override_active = logging::global_handle()
        .map(|h| h.env_override_active())
        .unwrap_or(false);

    Ok(logging::config::LoggingConfigResponse {
        config,
        env_override_active,
    })
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

// ── Settings page defaults (Settings redesign) ───────────────────────────────

/// Every default the Settings page needs, in one round trip: every KV
/// default plus the typed defaults of the four `reset_*`-backed configs.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_settings_defaults(
    state: State<'_, AppState>,
) -> Result<athenaeum_core::api::settings::SettingsDefaults, String> {
    athenaeum_core::api::settings::get_settings_defaults(&state.ctx).map_err(|e| e.to_string())
}

/// The user's ordered header keywords per catalog field (expanded document, `retired` included).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_header_fallbacks(
    state: State<'_, AppState>,
) -> Result<athenaeum_core::header_fallbacks::HeaderFallbackConfig, String> {
    athenaeum_core::api::header_fallbacks::get_header_fallbacks(&state.ctx).map_err(|e| e.to_string())
}

/// Validate and store the keyword rules; keywords dropped from an order are retired.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_header_fallbacks(
    config: athenaeum_core::header_fallbacks::HeaderFallbackConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    athenaeum_core::api::header_fallbacks::set_header_fallbacks(&state.ctx, config)
        .map_err(|e| e.to_string())
}

/// Every field back to its default keyword chain; returns the stored document.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reset_header_fallbacks(
    state: State<'_, AppState>,
) -> Result<athenaeum_core::header_fallbacks::HeaderFallbackConfig, String> {
    athenaeum_core::api::header_fallbacks::reset_header_fallbacks(&state.ctx).map_err(|e| e.to_string())
}

/// What the stored keyword rules would change in the catalog (a read-only count). Runs on a
/// blocking thread: it reads every frame.
#[tauri::command]
#[tracing::instrument(level = "debug", skip_all, err)]
pub async fn preview_header_fallbacks(
    state: State<'_, AppState>,
) -> Result<athenaeum_core::header_fallbacks::apply::HeaderFallbackCounts, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || athenaeum_core::api::header_fallbacks::preview_header_fallbacks(&ctx))
        .await
        .map_err(|e| format!("FITS keyword preview task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

/// Write what the stored keyword rules change in the catalog (regrouping the raw calibration
/// frames whose grouping values change). Emits `header-fallbacks-progress`. Runs on a blocking
/// thread; a second Apply while one runs is refused.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn apply_header_fallbacks(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<athenaeum_core::header_fallbacks::apply::HeaderFallbackCounts, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || {
        let emitter = crate::tauri_events::TauriProgressEmitter(app_handle);
        athenaeum_core::api::header_fallbacks::apply_header_fallbacks(&ctx, &emitter)
    })
    .await
    .map_err(|e| format!("FITS keyword apply task panicked: {e}"))?
    .map_err(|e| e.to_string())
}

/// The stored keyword rules as the JSON file of the spec's §5.7. With `path` the text is also
/// written there (the desktop reads and writes the user's chosen file in core); the text is
/// returned either way.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn export_header_fallbacks(
    path: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || {
        athenaeum_core::api::header_fallbacks::export_header_fallbacks(
            &ctx,
            path,
            &athenaeum_core::api::PathPolicy::AllowAll,
        )
    })
    .await
    .map_err(|e| format!("FITS keyword export task panicked: {e}"))?
    .map_err(|e| e.to_string())
}

/// What importing a keyword-rules file (its `text`, or a `path`) would change. Stores nothing;
/// the frontend saves `plan.config` after the user confirms. Runs on a blocking thread: it reads
/// the file.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn plan_header_fallbacks_import(
    text: Option<String>,
    path: Option<String>,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::header_fallbacks::transfer::HeaderFallbackImportPlan, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || {
        athenaeum_core::api::header_fallbacks::plan_header_fallbacks_import(
            &ctx,
            text,
            path,
            &athenaeum_core::api::PathPolicy::AllowAll,
        )
    })
    .await
    .map_err(|e| format!("FITS keyword import task panicked: {e}"))?
    .map_err(|e| e.to_string())
}
