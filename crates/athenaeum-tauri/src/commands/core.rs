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

/// True iff `latest` is strictly newer than `current` per SemVer 2.0
/// precedence. Both sides may have an optional leading `v`. Parse failures
/// are logged and treated as "not newer" so a malformed manifest can never
/// produce a false-positive update prompt.
fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Option<semver::Version> {
        let trimmed = s.trim().trim_start_matches('v');
        match semver::Version::parse(trimmed) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(input = %s, error = %e, "version_is_newer: failed to parse version");
                None
            }
        }
    };
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

use super::AppState;

#[tauri::command]
#[tracing::instrument(skip_all, err)]
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

    // OnceLock ensures only one thread can initialize; subsequent calls are no-ops
    if state.ctx.db.get().is_some() {
        return Ok(db_path.to_string_lossy().to_string());
    }

    let db = Database::new(db_path.clone()).map_err(|e| {
        tracing::error!(error = %e, "database init failed");
        e.to_string()
    })?;

    // set() returns Err if already set (race with StrictMode) — that's fine
    let _ = state.ctx.db.set(db);

    // Apply persisted blink.threads setting to the semaphore
    if let Some(db) = state.ctx.db.get() {
        let conn = db.conn();
        let saved = state.ctx.settings
            .get_with_precedence(&conn, settings::keys::BLINK_THREADS, settings::defaults::BLINK_THREADS)
            .unwrap_or_else(|_| settings::defaults::BLINK_THREADS.to_string());
        if let Ok(threads) = saved.parse::<usize>() {
            let max = state.max_blink_threads;
            let effective = if threads == 0 { (max / 2).max(2) } else { threads.clamp(1, max) };
            *state.image_semaphore.write().unwrap() =
                Arc::new(tokio::sync::Semaphore::new(effective));
            tracing::info!(permits = effective, "blink semaphore set from DB");
        }
    }

    // Apply persisted logging config now that the DB is available. A parse
    // failure falls back to the default (info) filter rather than failing DB
    // init; a missing setting also falls back to the default silently
    // (that's the expected first-run state) — but a DB read error is never
    // swallowed silently, unlike the parse/missing cases.
    if let Some(db) = state.ctx.db.get() {
        let conn = db.conn();
        let cfg = match crate::db::get_setting(&conn, crate::logging::config::SETTINGS_KEY) {
            Ok(Some(raw)) => serde_json::from_str::<crate::logging::LoggingConfig>(&raw)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "invalid stored logging config; using default");
                    crate::logging::LoggingConfig::default()
                }),
            Ok(None) => crate::logging::LoggingConfig::default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read stored logging config; using default");
                crate::logging::LoggingConfig::default()
            }
        };
        if let Some(handle) = crate::logging::global_handle() {
            handle.apply_config(&cfg);
        }
    }

    // Apply persisted compute.max_concurrent to the global compute queue.
    // Same DB-availability reasoning as blink.threads above: the desktop
    // app's `ctx.db` OnceLock is only populated here (frontend-triggered,
    // after `setup()` has already returned), so this is the sole spot on
    // desktop where a saved value actually takes effect at startup.
    if let Some(db) = state.ctx.db.get() {
        let conn = db.conn();
        match state.ctx.settings.get_compute_max_concurrent(&conn) {
            Ok(n) => {
                state.ctx.compute_queue.set_max_concurrent(n);
                tracing::info!(max_concurrent = n, "compute queue concurrency set from DB");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to read compute.max_concurrent; leaving default in place");
            }
        }
    }

    tracing::info!(path = %db_path.display(), "database initialized");
    Ok(db_path.to_string_lossy().to_string())
}

// ── Version / update commands ─────────────────────────────────────────────────

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
#[tracing::instrument(skip_all, err)]
pub async fn check_for_updates(state: State<'_, super::AppState>) -> Result<UpdateInfo, String> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    // Get or generate a persistent installation ID and read beta preference
    let (installation_id, check_beta) = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
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

#[cfg(test)]
mod tests {
    use super::version_is_newer;

    #[test]
    fn beta_8_is_newer_than_beta_7() {
        assert!(version_is_newer("0.2.0-beta.8", "0.2.0-beta.7"));
    }

    #[test]
    fn stable_is_newer_than_pre_release_of_same_base() {
        assert!(version_is_newer("0.2.0", "0.2.0-beta.9"));
    }

    #[test]
    fn pre_release_is_not_newer_than_stable_of_same_base() {
        assert!(!version_is_newer("0.2.0-beta.9", "0.2.0"));
    }

    #[test]
    fn higher_minor_is_newer_regardless_of_pre_release() {
        assert!(version_is_newer("0.3.0", "0.2.0-beta.99"));
        assert!(version_is_newer("0.3.0-beta.1", "0.2.0"));
    }

    #[test]
    fn equal_versions_are_not_newer() {
        assert!(!version_is_newer("0.2.0-beta.8", "0.2.0-beta.8"));
        assert!(!version_is_newer("0.2.0", "0.2.0"));
    }

    #[test]
    fn leading_v_is_stripped() {
        assert!(version_is_newer("v0.2.0-beta.8", "0.2.0-beta.7"));
        assert!(version_is_newer("0.2.0-beta.8", "v0.2.0-beta.7"));
    }

    #[test]
    fn unparseable_returns_false() {
        assert!(!version_is_newer("garbage", "0.2.0-beta.8"));
        assert!(!version_is_newer("0.2.0-beta.8", ""));
    }
}
