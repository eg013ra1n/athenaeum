//! Claude desktop's own configuration file (`claude_desktop_config.json`): where it lives and how Athenaeum's server
//! entry is merged into it (owner smoke 2026-11-21, check П2).
//!
//! The file belongs to Claude desktop and holds more than the servers (`preferences`, `coworkUserFilesPath`, other
//! servers), so the merge keeps every other key, refuses a file it cannot read as a JSON object, and never leaves a
//! half-written file: the old file is copied to `.bak`, the new text goes to a temp file in the same folder and is renamed
//! over the original. Keys are written sorted (serde_json's default map; `preserve_order` stays off, it would reorder
//! every `Value` map in the workspace).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use super::channel::write_atomic;
use crate::api::ApiError;

/// The file's name inside Claude desktop's folder.
pub const CONFIG_FILE: &str = "claude_desktop_config.json";

/// What `add_mcp_to_claude_desktop` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeDesktopAdded {
    /// The configuration file that now holds the entry.
    pub path: String,
    /// An entry of this name was there and has been replaced.
    pub replaced: bool,
    /// The copy of the file as it was; `null` when there was no file.
    pub backup: Option<String>,
}

/// The first candidate that is an existing folder. Pure, so the choice (the Store/MSIX package's folder before the
/// classic `%APPDATA%` one) is tested on every OS.
pub fn pick_config_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|dir| dir.is_dir()).cloned()
}

/// Claude desktop's folder for this user, when it exists. macOS: `~/Library/Application Support/Claude`. Windows: the
/// Store/MSIX build reads only `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude`, the installer build
/// `%APPDATA%\Claude`; the package folder comes first. Other systems: `None`.
pub fn claude_desktop_config_dir() -> Option<PathBuf> {
    pick_config_dir(&config_dir_candidates())
}

fn config_dir_candidates() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME")
            .map(|home| vec![PathBuf::from(home).join("Library").join("Application Support").join("Claude")])
            .unwrap_or_default();
    }
    if cfg!(windows) {
        let mut candidates = Vec::new();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let packages = PathBuf::from(local).join("Packages");
            if let Ok(entries) = std::fs::read_dir(&packages) {
                let mut found: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().starts_with("Claude_"))
                    .map(|e| e.path().join("LocalCache").join("Roaming").join("Claude"))
                    .collect();
                found.sort();
                candidates.extend(found);
            }
        }
        if let Some(roaming) = std::env::var_os("APPDATA") {
            candidates.push(PathBuf::from(roaming).join("Claude"));
        }
        return candidates;
    }
    Vec::new()
}

/// `existing` (the file's text, `None` when there is no file) with `entry` set as `mcpServers.<server_name>`: the new
/// text (pretty, trailing newline) and whether an entry of that name was replaced. `Err` is the refusal sentence; the
/// caller leaves the file as it is. A leading UTF-8 BOM is accepted; an empty file counts as no file.
pub fn merge_desktop_entry(existing: Option<&str>, server_name: &str, entry: Value) -> Result<(String, bool), String> {
    let text = existing.map(|t| t.strip_prefix('\u{feff}').unwrap_or(t)).filter(|t| !t.trim().is_empty());
    let mut root = match text {
        None => Map::new(),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => return Err(format!("{CONFIG_FILE} does not hold a JSON object at its top level; it was left as it is.")),
            Err(e) => {
                return Err(format!(
                    "{CONFIG_FILE} is not valid JSON (line {}, column {}); it was left as it is.",
                    e.line(),
                    e.column()
                ))
            }
        },
    };
    let servers = root.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(format!("\"mcpServers\" in {CONFIG_FILE} is not a JSON object; the file was left as it is."));
    };
    let replaced = servers.insert(server_name.to_string(), entry).is_some();
    let mut out = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| format!("the configuration could not be written: {e}"))?;
    out.push('\n');
    Ok((out, replaced))
}

/// Merges `entry` into `<dir>/claude_desktop_config.json`. The refusals warn and leave the file untouched.
pub fn add_entry_in(dir: &Path, server_name: &str, entry: Value) -> Result<ClaudeDesktopAdded, ApiError> {
    let path = dir.join(CONFIG_FILE);
    let refuse = |reason: String| {
        tracing::warn!(path = %path.display(), reason = %reason, "add to claude desktop refused");
        ApiError::Invalid(reason)
    };
    let fail = |what: &str, e: std::io::Error| {
        tracing::error!(path = %path.display(), error = %e, "add to claude desktop failed: {what}");
        ApiError::Internal(format!("{what}: {e}"))
    };
    let present = match std::fs::symlink_metadata(&path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(fail("Claude desktop's configuration could not be read", e)),
    };
    let existing = if present {
        let bytes = std::fs::read(&path).map_err(|e| fail("Claude desktop's configuration could not be read", e))?;
        Some(String::from_utf8(bytes).map_err(|_| refuse(format!("{CONFIG_FILE} is not UTF-8 text; it was left as it is.")))?)
    } else {
        None
    };
    let (text, replaced) = merge_desktop_entry(existing.as_deref(), server_name, entry).map_err(refuse)?;

    // A symlinked file (a dotfiles manager) is rewritten where it points, so the link survives.
    let target = if present && std::fs::symlink_metadata(&path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        std::fs::canonicalize(&path).map_err(|e| fail("Claude desktop's configuration could not be resolved", e))?
    } else {
        path.clone()
    };
    let backup = if present {
        let bak = dir.join(format!("{CONFIG_FILE}.bak"));
        std::fs::copy(&path, &bak).map_err(|e| fail("a copy of Claude desktop's configuration could not be made", e))?;
        Some(bak)
    } else {
        None
    };
    write_atomic(&target, text.as_bytes(), false).map_err(|e| fail("Claude desktop's configuration could not be written", e))?;
    tracing::info!(path = %path.display(), replaced, "athenaeum added to claude desktop");
    Ok(ClaudeDesktopAdded {
        path: path.to_string_lossy().into_owned(),
        replaced,
        backup: backup.map(|b| b.to_string_lossy().into_owned()),
    })
}
