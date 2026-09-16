//! In-app updates — the desktop half (spec §2.3). Check / notes come from
//! `athenaeum_core::updates`; download, signature verification, install and
//! relaunch are `tauri-plugin-updater` + `AppHandle::restart`.

use athenaeum_core::updates::{self, manifest::{Channel, MANIFEST_BASE_URL}, version, HostInfo, HostKind, UpdateCheck, WhatsNew};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::UpdaterExt;

use super::AppState;

/// The plugin's installer vocabulary for the bundle this binary was
/// installed from — what `platform_supported` keys on.
fn installer_name() -> Option<&'static str> {
    use tauri::utils::config::BundleType;
    // `BundleType` (tauri-utils 2.9.3) is not `#[non_exhaustive]`, so every
    // current variant above makes the `_` arm technically unreachable today —
    // kept anyway as a guard against a future variant tauri-utils adds
    // upstream, which would otherwise be a hard compile error here.
    #[allow(unreachable_patterns)]
    match tauri::utils::platform::bundle_type()? {
        BundleType::Nsis => Some("nsis"),
        BundleType::Msi => Some("msi"),
        BundleType::AppImage => Some("appimage"),
        BundleType::Deb => Some("deb"),
        BundleType::Rpm => Some("rpm"),
        BundleType::App | BundleType::Dmg => Some("app"),
        _ => None,
    }
}

fn host_info() -> HostInfo<'static> {
    HostInfo {
        kind: HostKind::Desktop,
        git_hash: env!("ATHENAEUM_GIT_HASH"),
        installer: installer_name(),
    }
}

/// The comparator handed to the plugin: its `current` is `tauri.conf.json`'s
/// `X.Y.Z-N`, the manifest's is dotted — normalize, then the ONE core
/// function decides (spec §4). Kept as a free function so the parity test
/// below can call exactly what the plugin calls.
pub(crate) fn plugin_says_newer(current_tauri_form: &str, manifest_version: &str) -> bool {
    version::is_newer(manifest_version, &version::normalize_tauri_version(current_tauri_form))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn check_for_updates(state: State<'_, AppState>) -> Result<UpdateCheck, String> {
    updates::check(&state.ctx, &host_info()).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_whats_new(state: State<'_, AppState>) -> Result<Option<WhatsNew>, String> {
    updates::whats_new(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all)]
pub fn get_release_notes() -> WhatsNew {
    updates::release_notes()
}

/// Clears `update_in_flight` on every exit path of `install_update`.
struct InFlightGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Cheap refusals BEFORE any download (spec §6.2). Each one is `warn!`-logged
/// with `path` and becomes the command's `Err`.
fn early_refusals() -> Result<(), String> {
    if cfg!(debug_assertions) {
        tracing::warn!("install_update refused: development build");
        return Err("updates are disabled in development builds".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the running executable: {e}"))?;

    #[cfg(target_os = "macos")]
    {
        let exe_s = exe.to_string_lossy();
        if exe_s.contains("/AppTranslocation/") {
            tracing::warn!(path = %exe_s, "install_update refused: translocated bundle");
            return Err(format!("Athenaeum can't replace itself at {exe_s} — move it to Applications first"));
        }
        // The plugin renames the whole .app, so its PARENT must be writable.
        let app_dir = exe
            .ancestors()
            .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
            .ok_or_else(|| "cannot find the .app bundle of the running executable".to_string())?;
        let parent = app_dir.parent().ok_or_else(|| "the .app bundle has no parent directory".to_string())?;
        probe_writable(parent, app_dir)?;
    }

    #[cfg(target_os = "linux")]
    {
        let appimage = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from);
        let Some(appimage) = appimage else {
            tracing::warn!(path = %exe.display(), "install_update refused: not running as an AppImage");
            return Err("Athenaeum can't replace itself — this copy was not started as an AppImage".to_string());
        };
        let parent = appimage.parent().ok_or_else(|| "the AppImage has no parent directory".to_string())?;
        probe_writable(parent, &appimage)?;
    }

    #[cfg(target_os = "windows")]
    {
        let _ = exe; // the NSIS/MSI installer handles replacement itself
    }
    Ok(())
}

/// Can we create and remove a file in `dir`? (Opening the running binary
/// itself for write would fail with ETXTBSY on Linux — never do that.)
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn probe_writable(dir: &std::path::Path, subject: &std::path::Path) -> Result<(), String> {
    let probe = dir.join(format!(".athenaeum-update-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new().create_new(true).write(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => {
            tracing::warn!(path = %subject.display(), error = %e, "install_update refused: location not writable");
            Err(format!("Athenaeum can't replace itself at {} — move it to a folder you can write to", subject.display()))
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallArgs {
    pub channel: Channel,
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn install_update(app: AppHandle, state: State<'_, AppState>, args: InstallArgs) -> Result<(), String> {
    if state.update_in_flight.swap(true, Ordering::SeqCst) {
        tracing::warn!("install_update refused: another install in flight");
        return Err("an update is already being installed".to_string());
    }
    let _guard = InFlightGuard(&state.update_in_flight);
    early_refusals()?;

    let url = format!("{}/{}", MANIFEST_BASE_URL.trim_end_matches('/'), args.channel.file());
    let endpoint: tauri::Url = url.parse().map_err(|e| format!("bad update endpoint {url}: {e}"))?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| e.to_string())?
        .version_comparator(|current, release| plugin_says_newer(&current.to_string(), &release.version.to_string()))
        .build()
        .map_err(|e| e.to_string())?;

    let update = updater
        .check()
        .await
        .map_err(|e| {
            tracing::error!(url, error = %e, "updater check failed");
            e.to_string()
        })?
        .ok_or_else(|| "no update is available for this build".to_string())?;
    let version = update.version.clone();
    tracing::info!(channel = ?args.channel, latest_version = %version, "update download started");

    let mut downloaded: u64 = 0;
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let app_for_progress = app.clone();
    let app_for_finish = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                if last_emit.elapsed() >= Duration::from_millis(300) {
                    last_emit = Instant::now();
                    let _ = app_for_progress.emit("update-progress", serde_json::json!({ "downloaded": downloaded, "total": total }));
                }
            },
            move || {
                let _ = app_for_finish.emit("update-progress", serde_json::json!({ "downloaded": null, "total": null, "finished": true }));
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(latest_version = %version, error = %e, "update download or install failed");
            format!("update failed: {e}")
        })?;

    tracing::info!(latest_version = %version, "update installed — restart to apply");
    let _ = app.emit("update-ready", serde_json::json!({ "version": version.to_string() }));
    Ok(())
}

/// macOS / Linux: relaunch into the replaced bundle. On Windows the
/// installer relaunches the app itself and this is never reached.
#[tauri::command]
#[tracing::instrument(skip_all)]
pub fn restart_app(app: AppHandle) {
    tracing::info!("restarting to apply the update");
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::plugin_says_newer;
    use athenaeum_core::updates::version::is_newer;

    /// The plugin's answer (through the normalizer) and core's answer (dotted
    /// vs dotted) must agree on every pair — the two "is it newer" decisions
    /// the design makes must never diverge (spec §4).
    #[test]
    fn plugin_and_core_agree() {
        // (tauri.conf form, the same version dotted, manifest version)
        let table = [
            ("0.6.5-1", "0.6.5-beta.1", "0.6.5-beta.1"),
            ("0.6.5-1", "0.6.5-beta.1", "0.6.5-beta.2"),
            ("0.6.5-1", "0.6.5-beta.1", "0.6.5"),
            ("0.6.5-2", "0.6.5-beta.2", "0.6.5-beta.1"),
            ("0.6.5", "0.6.5", "0.6.5-beta.3"),
            ("0.6.5", "0.6.5", "0.6.6"),
            ("0.6.5", "0.6.5", "0.6.5"),
            ("0.6.5", "0.6.5", "v0.7.0"),
            ("0.6.5-1", "0.6.5-beta.1", "garbage"),
        ];
        for (tauri_form, dotted, manifest) in table {
            assert_eq!(
                plugin_says_newer(tauri_form, manifest),
                is_newer(manifest, dotted),
                "disagreement on current={tauri_form}/{dotted} manifest={manifest}"
            );
        }
        assert!(!plugin_says_newer("0.6.5-1", "0.6.5-beta.1"), "the running beta is not its own update");
    }
}
