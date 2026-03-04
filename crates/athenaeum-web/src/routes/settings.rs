// Settings route handlers — mirrors athenaeum-tauri/src/commands/settings.rs

use athenaeum_core::db;
use athenaeum_core::models::Setting;
use athenaeum_core::settings;
use axum::{extract::State, http::StatusCode, Json};

use crate::WebAppState;

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GetSettingArgs {
    pub key: String,
    #[serde(rename = "defaultValue")]
    pub default_value: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct SetSettingArgs {
    pub key: String,
    pub value: String,
}

#[derive(serde::Deserialize)]
pub struct DeleteSettingArgs {
    pub key: String,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/get_setting
///
/// Returns the value for `key` using three-tier precedence:
/// runtime override → database → `defaultValue`. If the key is absent from the
/// database and no `defaultValue` was provided, an empty string is returned.
pub async fn get_setting(
    State(state): State<WebAppState>,
    Json(args): Json<GetSettingArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let default = args.default_value.unwrap_or_default();
    let value = state
        .ctx
        .settings
        .get_with_precedence(&conn, &args.key, &default)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(value))
}

/// POST /api/set_setting
///
/// Persists a key-value pair to the `settings` table. If the key already
/// exists it is updated; otherwise it is inserted.
pub async fn set_setting(
    State(state): State<WebAppState>,
    Json(args): Json<SetSettingArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    state
        .ctx
        .settings
        .persist_setting(&conn, &args.key, &args.value)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Update in-memory cache settings when they change
    if args.key == settings::keys::BLINK_MEMORY_CACHE_SIZE {
        let size: usize = args.value.parse().unwrap_or(200);
        state.ctx.memory_cache.lock().unwrap().set_max_entries(size);
    } else if args.key == settings::keys::BLINK_MEMORY_RETENTION_MINUTES {
        let minutes: u64 = args.value.parse().unwrap_or(30);
        state.ctx.memory_cache.lock().unwrap().set_retention(minutes);
    }

    Ok(Json(()))
}

/// POST /api/get_all_settings
///
/// Returns every row in the `settings` table, ordered alphabetically by key.
pub async fn get_all_settings(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<Setting>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let settings = db::get_all_settings(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(settings))
}

/// POST /api/delete_setting
///
/// Removes the setting row for the given key. A no-op if the key does not
/// exist.
pub async fn delete_setting(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteSettingArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    db::delete_setting(&conn, &args.key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(()))
}

// ── Cache & blink routes (Category C — modified behavior) ────────────────────

/// Cache stats DTO for web mode.
#[derive(serde::Serialize)]
pub struct WebCacheStats {
    pub total_entries: usize,
    pub total_size_bytes: u64,
    pub total_size_human: String,
}

/// POST /api/get_cache_stats
///
/// In web mode, returns memory cache stats (entry count).
pub async fn get_cache_stats(
    State(_state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<WebCacheStats>, (StatusCode, String)> {
    // MemoryImageCache doesn't expose len(); return 0 entries since we can't
    // cheaply inspect the internal HashMap without modifying the cache crate.
    // The web mode doesn't use the disk cache, so this is a best-effort stat.
    Ok(Json(WebCacheStats {
        total_entries: 0,
        total_size_bytes: 0,
        total_size_human: "In-memory cache (stats not tracked)".to_string(),
    }))
}

/// POST /api/clear_image_cache
///
/// In web mode, clears the in-memory image cache.
pub async fn clear_image_cache(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<String>, (StatusCode, String)> {
    let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
    mem_cache.clear();
    let msg = "Memory image cache cleared".to_string();
    eprintln!("{}", msg);
    Ok(Json(msg))
}

/// POST /api/get_blink_threads_max
///
/// Returns the CPU-based max so the frontend can set the slider/input max dynamically.
pub async fn get_blink_threads_max(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Json<usize> {
    Json(state.max_blink_threads)
}

#[derive(serde::Deserialize)]
pub struct SetBlinkThreadsArgs {
    pub threads: usize,
}

/// POST /api/set_blink_threads
///
/// Persists the thread count to DB and rebuilds the image processing semaphore.
/// In-flight permits on the old semaphore complete naturally.
pub async fn set_blink_threads(
    State(state): State<WebAppState>,
    Json(args): Json<SetBlinkThreadsArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let threads = args.threads.clamp(1, state.max_blink_threads);

    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    state
        .ctx
        .settings
        .persist_setting(&conn, settings::keys::BLINK_THREADS, &threads.to_string())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Rebuild semaphore — in-flight permits on the old Arc complete naturally
    *state.image_semaphore.write().unwrap() =
        std::sync::Arc::new(tokio::sync::Semaphore::new(threads));

    eprintln!("Blink semaphore rebuilt with {} permits", threads);
    Ok(Json(()))
}

/// POST /api/get_grouping_threshold_deg
///
/// Returns the frame-set clustering threshold in decimal degrees. Reads the
/// `grouping_threshold_arcmin` setting (with runtime and database precedence)
/// and converts it to degrees.
pub async fn get_grouping_threshold_deg(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<f64>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let threshold = state
        .ctx
        .settings
        .get_grouping_threshold_deg(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(threshold))
}
