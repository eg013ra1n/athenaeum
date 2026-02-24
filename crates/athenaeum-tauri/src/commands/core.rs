// Core commands - app initialization and basic operations

use crate::db::Database;
use crate::settings;
use std::sync::Arc;
use tauri::{Manager, State};

// ── Update check types ────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub is_update_available: bool,
    pub download_url: String,
}

#[derive(serde::Deserialize)]
struct VersionJson {
    version: String,
    #[serde(default)]
    download_url: String,
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let mut parts = s.trim_start_matches('v').split('.');
        (
            parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
            parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
            parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
        )
    };
    parse(latest) > parse(current)
}

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
    let mut db_lock = state.ctx.db.lock().unwrap();
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
        let saved = state.ctx.settings
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

// ── Version / update commands ─────────────────────────────────────────────────

#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub async fn check_for_updates(state: State<'_, super::AppState>) -> Result<UpdateInfo, String> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    // Get or generate a persistent installation ID (stored in settings table)
    let installation_id = {
        let state_lock = state.ctx.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        match crate::db::get_setting(&conn, "installation.id").map_err(|e| e.to_string())? {
            Some(id) => id,
            None => {
                let new_id = uuid::Uuid::new_v4().to_string();
                crate::db::set_setting(&conn, "installation.id", &new_id)
                    .map_err(|e| e.to_string())?;
                new_id
            }
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Athenaeum-App")
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let url = format!(
        "https://artfrom.space/version.json?v={}&os={}&commit={}&id={}",
        current_version,
        std::env::consts::OS,
        env!("ATHENAEUM_GIT_HASH"),
        installation_id,
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| {
            eprintln!("check_for_updates: network error: {}", e);
            format!("Network error: {}", e)
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        eprintln!("check_for_updates: server returned {}", status);
        return Err(format!("Server returned {}", status));
    }

    let info: VersionJson = resp.json().await.map_err(|e| {
        eprintln!("check_for_updates: failed to parse version info: {}", e);
        format!("Failed to parse version info: {}", e)
    })?;

    let is_update_available = version_is_newer(&info.version, &current_version);
    let download_url = if info.download_url.is_empty() {
        "https://artfrom.space/releases/download".to_string()
    } else {
        info.download_url
    };

    Ok(UpdateInfo {
        current_version,
        latest_version: info.version,
        is_update_available,
        download_url,
    })
}
