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

/// Parse a version string into (major, minor, patch, is_prerelease).
/// Handles formats like "0.2.0", "0.2.0-beta.1", "v0.2.0-beta.1".
/// Pre-release versions sort below their base version (0.2.0 > 0.2.0-beta.1).
fn parse_version(s: &str) -> (u32, u32, u32, bool) {
    let s = s.trim_start_matches('v');
    // Split off pre-release suffix (e.g., "0.2.0-beta.1" → "0.2.0" + Some("beta.1"))
    let (base, prerelease) = match s.split_once('-') {
        Some((b, _)) => (b, true),
        None => (s, false),
    };
    let mut parts = base.split('.');
    (
        parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
        parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
        parts.next().and_then(|p| p.parse().ok()).unwrap_or(0),
        prerelease,
    )
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    let (lmaj, lmin, lpatch, lpre) = parse_version(latest);
    let (cmaj, cmin, cpatch, cpre) = parse_version(current);

    let l_base = (lmaj, lmin, lpatch);
    let c_base = (cmaj, cmin, cpatch);

    match l_base.cmp(&c_base) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        // Same base version: stable > pre-release
        std::cmp::Ordering::Equal => cpre && !lpre,
    }
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

/// Fetch a version.json file and return parsed info, or None on failure.
async fn fetch_version_json(
    client: &reqwest::Client,
    filename: &str,
    current_version: &str,
    installation_id: &str,
) -> Option<VersionJson> {
    let url = format!(
        "https://artfrom.space/{}?v={}&os={}&commit={}&id={}",
        filename,
        current_version,
        std::env::consts::OS,
        env!("ATHENAEUM_GIT_HASH"),
        installation_id,
    );

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

#[tauri::command]
pub async fn check_for_updates(state: State<'_, super::AppState>) -> Result<UpdateInfo, String> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    // Get or generate a persistent installation ID and read beta preference
    let (installation_id, check_beta) = {
        let state_lock = state.ctx.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        let id = match crate::db::get_setting(&conn, "installation.id").map_err(|e| e.to_string())? {
            Some(id) => id,
            None => {
                let new_id = uuid::Uuid::new_v4().to_string();
                crate::db::set_setting(&conn, "installation.id", &new_id)
                    .map_err(|e| e.to_string())?;
                new_id
            }
        };

        let beta = crate::db::get_setting(&conn, "updates.check_beta")
            .unwrap_or(None)
            .map(|v| v == "true")
            .unwrap_or(false);

        (id, beta)
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Athenaeum-App")
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    // Always check stable channel
    let stable = fetch_version_json(&client, "version.json", &current_version, &installation_id).await;

    // Optionally check beta channel
    let beta_info = if check_beta {
        fetch_version_json(&client, "version-beta.json", &current_version, &installation_id).await
    } else {
        None
    };

    // Pick whichever version is newer between stable and beta
    let info = match (&stable, &beta_info) {
        (Some(s), Some(b)) => {
            if version_is_newer(&b.version, &s.version) { b } else { s }
        }
        (Some(s), None) => s,
        (None, Some(b)) => b,
        (None, None) => {
            eprintln!("check_for_updates: failed to fetch any version info");
            return Err("Failed to fetch version info".to_string());
        }
    };

    let is_update_available = version_is_newer(&info.version, &current_version);
    let download_url = if info.download_url.is_empty() {
        "https://artfrom.space/releases/download".to_string()
    } else {
        info.download_url.clone()
    };

    Ok(UpdateInfo {
        current_version,
        latest_version: info.version.clone(),
        is_update_available,
        download_url,
    })
}
