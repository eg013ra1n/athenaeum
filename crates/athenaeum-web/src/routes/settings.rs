// Settings route handlers — mirrors athenaeum-tauri/src/commands/settings.rs

use athenaeum_core::db;
use athenaeum_core::logging;
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
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
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
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
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
    } else if args.key == settings::keys::BLINK_MEMORY_CACHE_MAX_MB {
        let mb: usize = args.value.parse().unwrap_or(512);
        state.ctx.memory_cache.lock().unwrap().set_max_bytes(mb * 1024 * 1024);
    } else if args.key == settings::keys::BLINK_MEMORY_RETENTION_MINUTES {
        let minutes: u64 = args.value.parse().unwrap_or(30);
        state.ctx.memory_cache.lock().unwrap().set_retention(minutes);
    } else if args.key == settings::keys::MONITORING_INTERVAL_MINUTES
        || args.key == settings::keys::MONITORING_ENABLED_GLOBAL
    {
        // Wake the monitor loop so the new interval / enabled flag takes
        // effect now instead of after the current sleep expires.
        state.monitor.kick();
    }

    Ok(Json(()))
}

/// POST /api/delete_setting
///
/// Removes the setting row for the given key. A no-op if the key does not
/// exist.
#[tracing::instrument(skip_all, err(Debug))]
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
#[tracing::instrument(skip_all, err(Debug))]
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

/// POST /api/get_blink_threads_max
///
/// Returns the CPU-based max so the frontend can set the slider/input max dynamically.
#[tracing::instrument(skip_all)]
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
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_blink_threads(
    State(state): State<WebAppState>,
    Json(args): Json<SetBlinkThreadsArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let threads = args.threads.clamp(0, state.max_blink_threads);
    let effective = if threads == 0 { (state.max_blink_threads / 2).max(2) } else { threads };

    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    state
        .ctx
        .settings
        .persist_setting(&conn, settings::keys::BLINK_THREADS, &threads.to_string())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Rebuild semaphore — 0 means use all available cores
    *state.image_semaphore.write().unwrap() =
        std::sync::Arc::new(tokio::sync::Semaphore::new(effective));

    tracing::info!(permits = effective, requested = threads, "blink semaphore rebuilt");
    Ok(Json(()))
}

// ── Logging config (Task 3) ──────────────────────────────────────────────────

/// POST /api/get_logging_config
///
/// Returns the effective logging config (persisted, or the default if unset)
/// plus whether the `ATHENAEUM_LOG` env override is currently active.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_logging_config(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<logging::config::LoggingConfigResponse>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let config = match db::get_setting(&conn, logging::config::SETTINGS_KEY)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        Some(raw) => serde_json::from_str::<logging::LoggingConfig>(&raw).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "invalid stored logging config; using default");
            logging::LoggingConfig::default()
        }),
        None => logging::LoggingConfig::default(),
    };
    let env_override_active = logging::global_handle()
        .map(|h| h.env_override_active())
        .unwrap_or(false);

    Ok(Json(logging::config::LoggingConfigResponse { config, env_override_active }))
}

/// Request body for `set_logging_config`. The frontend calls
/// `api.invoke('set_logging_config', { config })` per the Tauri named-arg
/// convention used by every `api.invoke` call, so the HTTP body is
/// `{ "config": { ... } }`, not a bare `LoggingConfig`. `LoggingConfig`
/// derives `#[serde(default)]`, so a bare-struct extractor here would
/// silently deserialize an unrecognized wrapped body to
/// `LoggingConfig::default()` instead of erroring — 200 OK, config quietly
/// reset. See `.superpowers/sdd/task-10-report.md`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLoggingConfigArgs {
    pub config: logging::LoggingConfig,
}

/// POST /api/set_logging_config
///
/// Validates the config (rejects an out-of-range level or per-module level,
/// or directives that fail to parse), persists it under `logging.config`,
/// then live-applies it via the process-global `LoggingHandle`. Validation
/// happens before the DB write; the live-apply happens after.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_logging_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetLoggingConfigArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let config = args.config;
    config.validate().map_err(|e| {
        tracing::warn!(error = %e, "rejected invalid logging config");
        (StatusCode::BAD_REQUEST, e)
    })?;

    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let json = serde_json::to_string(&config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    db::set_setting(&conn, logging::config::SETTINGS_KEY, &json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(handle) = logging::global_handle() {
        handle.apply_config(&config);
    }

    Ok(Json(()))
}

#[cfg(test)]
mod logging_config_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// these tests exercise actual `settings` table reads/writes for the
    /// logging config, so a real connection pool is required (unlike
    /// `routes::tests::test_state` in `routes/mod.rs`, which leaves `ctx.db`
    /// unset to test only the auth layer). Mirrors
    /// `scan_roots::relink_tests::test_state`.
    fn test_state(db: Database) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths: Vec::new(),
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
            sync: std::sync::Arc::new(athenaeum_core::sync::SyncRuntime::new()),
            sync_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
            collab_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
        }
    }

    #[tokio::test]
    async fn get_logging_config_returns_default_when_unset() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let resp = get_logging_config(State(state), Json(serde_json::json!({})))
            .await
            .unwrap()
            .0;

        assert_eq!(resp.config.level, "info");
        assert!(resp.config.modules.is_empty());
        // No `logging::init_global` call happens in this test binary, so the
        // process-global handle is never set — env_override_active must
        // fall back to false rather than panicking.
        assert!(!resp.env_override_active);
    }

    /// Regression guard for the real frontend payload: `api.invoke` sends
    /// `{ "config": { ... } }`, per the Tauri named-arg convention — not a
    /// bare `LoggingConfig`. Exercises the handler via the wrapped
    /// `SetLoggingConfigArgs` struct directly.
    #[tokio::test]
    async fn set_logging_config_then_get_reflects_change() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let mut modules = std::collections::BTreeMap::new();
        modules.insert("scanner".to_string(), "debug".to_string());
        let cfg = logging::LoggingConfig { level: "debug".to_string(), modules };

        let _ = set_logging_config(State(state.clone()), Json(SetLoggingConfigArgs { config: cfg }))
            .await
            .expect("valid config must be accepted");

        let resp = get_logging_config(State(state), Json(serde_json::json!({})))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.config.level, "debug");
        assert_eq!(
            resp.config.modules.get("scanner").map(String::as_str),
            Some("debug")
        );
    }

    #[tokio::test]
    async fn set_logging_config_rejects_invalid_level() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let cfg = logging::LoggingConfig { level: "chatty".to_string(), modules: Default::default() };
        let err = set_logging_config(State(state.clone()), Json(SetLoggingConfigArgs { config: cfg }))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Verify the DB was untouched on rejection: get_logging_config must still return default.
        let resp = get_logging_config(State(state), Json(serde_json::json!({})))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.config, logging::LoggingConfig::default());
    }

    /// Pins the fix: the handler now requires the `{ "config": ... }`
    /// wrapper (matching `SetLoggingConfigArgs`). Deserializing a bare
    /// `LoggingConfig` JSON body must fail hard (serde error), not silently
    /// fall back to `LoggingConfig::default()`. This mirrors what axum's
    /// `Json` extractor does at the HTTP boundary — a body that doesn't
    /// contain a `config` field fails deserialization into
    /// `SetLoggingConfigArgs` (its field is not optional/defaulted), which
    /// axum surfaces as a 4xx `JsonRejection` in production.
    #[test]
    fn bare_logging_config_body_fails_to_deserialize_into_wrapped_args() {
        let bare = serde_json::json!({
            "level": "debug",
            "modules": { "scanner": "debug" }
        });

        let result: Result<SetLoggingConfigArgs, _> = serde_json::from_value(bare);
        assert!(
            result.is_err(),
            "bare LoggingConfig body must NOT deserialize into SetLoggingConfigArgs — \
             this is what closes the silent-default hole (axum returns 422/400 for this shape)"
        );
    }
}

