// Assistant access (MCP) commands: thin wrappers over `athenaeum_core::api::mcp`, plus what only the desktop knows:
// where the bridge program is (`mcp_bridge`), the copy of it kept in the app-data folder, and raising the window.
// Mirrors `crates/athenaeum-web/src/routes/mcp.rs`.

use std::sync::Arc;

use athenaeum_core::api::mcp as api;
use athenaeum_core::mcp::{desktop_client_config, McpHost, McpStatus};
use tauri::{AppHandle, Manager, State};

use crate::mcp_bridge::{bridge_path_for, copies_to_app_data, refresh_bridge_copy, CopyOutcome};

/// The assistant-access host, built in `.setup` (`lib.rs`) and managed by Tauri.
pub struct McpHostState(pub Arc<McpHost>);

/// Shows Athenaeum's window: restores it when minimized, shows it when hidden, gives it focus. Every step that
/// fails is a warning; none stops the next.
pub(crate) fn bring_to_front(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("mcp bring to front: the main window was not found");
        return;
    };
    if let Err(e) = window.unminimize() {
        tracing::warn!(error = %e, "mcp bring to front: the window could not be restored");
    }
    if let Err(e) = window.show() {
        tracing::warn!(error = %e, "mcp bring to front: the window could not be shown");
    }
    if let Err(e) = window.set_focus() {
        tracing::warn!(error = %e, "mcp bring to front: the window could not be focused");
    }
}

/// The bridge program's source: next to the app's executable.
fn installed_bridge(exe: &std::path::Path) -> std::path::PathBuf {
    bridge_path_for(exe, false, std::path::Path::new(""))
}

/// Keeps the bridge copy in the app-data folder current (AppImage and Windows only; elsewhere nothing to do).
fn refresh_copy(app: &AppHandle) {
    if !copies_to_app_data() {
        return;
    }
    let app_data = match crate::paths::resolve_app_data_dir(app) {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!(error = %e, "mcp bridge copy skipped: the data folder is not known");
            return;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(error = %e, "mcp bridge copy skipped: the app's own path is not known");
            return;
        }
    };
    let outcome = refresh_bridge_copy(&installed_bridge(&exe), &app_data.join("bin"), env!("CARGO_PKG_VERSION"), |from, to| {
        std::fs::rename(from, to)
    });
    // `Kept` has warned already, with its reason.
    if outcome == CopyOutcome::Current {
        tracing::debug!("mcp bridge copy is current");
    }
}

/// At each start while assistant access is on (after the catalog is open): refresh the bridge copy.
pub(crate) fn refresh_copy_if_enabled(app: &AppHandle, host: &McpHost) {
    if host.status().enabled {
        refresh_copy(app);
    }
}

/// Fills what only the desktop wrapper knows: the bridge's path (the app-data copy on an AppImage or Windows), whether
/// it is there, and the configuration that names it (always with `--app-data-dir`, ruling P9).
fn with_bridge(app: &AppHandle, mut status: McpStatus) -> McpStatus {
    let app_data = match crate::paths::resolve_app_data_dir(app) {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!(error = %e, "mcp bridge path left as the core found it: the data folder is not known");
            return status;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(error = %e, "mcp bridge path left as the core found it: the app's own path is not known");
            return status;
        }
    };
    let bridge = bridge_path_for(&exe, copies_to_app_data(), &app_data);
    let build = if status.server_name.ends_with("-dev") { "dev" } else { "release" };
    let (config, command) = desktop_client_config(&status.server_name, &bridge, build, Some(&app_data));
    status.bridge_found = bridge.is_file();
    status.bridge_path = Some(bridge.to_string_lossy().into_owned());
    status.client_config = Some(config);
    status.claude_code_command = Some(command);
    status
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_mcp_status(app_handle: AppHandle, host: State<'_, McpHostState>) -> Result<McpStatus, String> {
    Ok(with_bridge(&app_handle, api::get_mcp_status(&host.0)))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_mcp_enabled(app_handle: AppHandle, host: State<'_, McpHostState>, enabled: bool) -> Result<McpStatus, String> {
    let status = api::set_mcp_enabled(&host.0, enabled).map_err(|e| e.to_string())?;
    if status.enabled {
        refresh_copy(&app_handle);
    }
    Ok(with_bridge(&app_handle, status))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn regenerate_mcp_token(app_handle: AppHandle, host: State<'_, McpHostState>) -> Result<McpStatus, String> {
    let status = api::regenerate_mcp_token(&host.0).map_err(|e| e.to_string())?;
    Ok(with_bridge(&app_handle, status))
}
