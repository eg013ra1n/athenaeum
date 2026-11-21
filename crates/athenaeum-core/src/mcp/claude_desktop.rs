//! Claude desktop's own configuration file (`claude_desktop_config.json`): where it lives and how Athenaeum's server
//! entry is merged into it (owner smoke 2026-11-21, check П2).
//!
//! The file belongs to Claude desktop and holds more than the servers (`preferences`, `coworkUserFilesPath`, other
//! servers), so the merge keeps every other key, refuses a file it cannot read as a JSON object, and never leaves a
//! half-written file: the old file (when it holds anything) is copied to `.bak`, the new text goes to a temp file in the same folder
//! with the old file's permissions and is renamed over the original, unless the original changed meanwhile.
//! Keys are written sorted (serde_json's default map; `preserve_order` stays off, it would reorder
//! every `Value` map in the workspace).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::{Map, Value};

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

/// Package folders, most recently modified first (a folder with no known time last, ties by path), so that with several
/// `Claude_*` packages the one Claude desktop used last comes first. Pure.
pub fn order_by_recency(mut dirs: Vec<(PathBuf, Option<std::time::SystemTime>)>) -> Vec<PathBuf> {
    dirs.sort_by(|(a_path, a_time), (b_path, b_time)| b_time.cmp(a_time).then_with(|| a_path.cmp(b_path)));
    dirs.into_iter().map(|(path, _)| path).collect()
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
            match std::fs::read_dir(&packages) {
                Ok(entries) => {
                    let found: Vec<(PathBuf, Option<std::time::SystemTime>)> = entries
                        .filter_map(Result::ok)
                        .filter(|e| e.file_name().to_string_lossy().starts_with("Claude_"))
                        .map(|e| e.path().join("LocalCache").join("Roaming").join("Claude"))
                        .map(|dir| {
                            let modified = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
                            (dir, modified)
                        })
                        .collect();
                    candidates.extend(order_by_recency(found));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(path = %packages.display(), error = %e, "claude desktop package folders could not be listed"),
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

/// Whether a config file's text holds nothing but a BOM and white space; such a file counts as absent.
fn is_blank(text: &str) -> bool {
    text.strip_prefix('\u{feff}').unwrap_or(text).trim().is_empty()
}

/// Merges `entry` into `<dir>/claude_desktop_config.json`. The refusals warn and leave the file untouched.
pub fn add_entry_in(dir: &Path, server_name: &str, entry: Value) -> Result<ClaudeDesktopAdded, ApiError> {
    add_entry_checked(dir, server_name, entry, &|| {})
}

/// A temp name no other call in this process (or another process) shares.
fn temp_path_for(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!("{name}.{}.{}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)))
}

/// Why [`replace_file`] did not replace the file.
enum ReplaceError {
    Io(std::io::Error),
    /// The target's bytes are not the ones that were merged.
    Changed,
}

/// Writes `text` to a temp file next to `target` (the old file's permissions on it), checks that `target` still holds
/// `expected` (`None`: still absent), then renames the temp file over it. `before_check` runs between the write and the
/// check (the tests' seam for a second writer). The temp file never outlives a failure.
fn replace_file(target: &Path, text: &[u8], expected: Option<&[u8]>, before_check: &dyn Fn()) -> Result<(), ReplaceError> {
    use std::io::Write;
    let tmp = temp_path_for(target);
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(ReplaceError::Io)?;
        file.write_all(text).map_err(ReplaceError::Io)?;
        file.sync_all().map_err(ReplaceError::Io)?;
        drop(file);
        match std::fs::metadata(target) {
            Ok(meta) => std::fs::set_permissions(&tmp, meta.permissions()).map_err(ReplaceError::Io)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ReplaceError::Io(e)),
        }
        before_check();
        let now = match std::fs::read(target) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(ReplaceError::Io(e)),
        };
        if now.as_deref() != expected {
            return Err(ReplaceError::Changed);
        }
        std::fs::rename(&tmp, target).map_err(ReplaceError::Io)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// [`add_entry_in`] with a hook that runs just before the file is checked for a change since it was read.
pub(crate) fn add_entry_checked(dir: &Path, server_name: &str, entry: Value, before_check: &dyn Fn()) -> Result<ClaudeDesktopAdded, ApiError> {
    let path = dir.join(CONFIG_FILE);
    let refuse = |reason: String| {
        tracing::warn!(path = %path.display(), reason = %reason, "add to claude desktop refused");
        ApiError::Invalid(reason)
    };
    let fail = |what: &str, e: std::io::Error| {
        tracing::error!(path = %path.display(), reason = what, error = %e, "add to claude desktop failed");
        ApiError::Internal(format!("{what}: {e}"))
    };
    let present = match std::fs::symlink_metadata(&path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(fail("Claude desktop's configuration could not be read", e)),
    };
    let original = if present {
        Some(std::fs::read(&path).map_err(|e| fail("Claude desktop's configuration could not be read", e))?)
    } else {
        None
    };
    let existing = match &original {
        Some(bytes) => Some(std::str::from_utf8(bytes).map_err(|_| refuse(format!("{CONFIG_FILE} is not UTF-8 text; it was left as it is.")))?),
        None => None,
    };
    let (text, replaced) = merge_desktop_entry(existing, server_name, entry).map_err(refuse)?;

    // A symlinked file (a dotfiles manager) is rewritten where it points, so the link survives.
    let target = if present && std::fs::symlink_metadata(&path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        std::fs::canonicalize(&path).map_err(|e| fail("Claude desktop's configuration could not be resolved", e))?
    } else {
        path.clone()
    };
    // An empty file holds nothing to lose: it is no reason to replace an older backup that may hold the user's servers.
    let backup = if existing.is_some_and(|t| !is_blank(t)) {
        let bak = dir.join(format!("{CONFIG_FILE}.bak"));
        std::fs::copy(&path, &bak).map_err(|e| fail("a copy of Claude desktop's configuration could not be made", e))?;
        Some(bak)
    } else {
        None
    };
    match replace_file(&target, text.as_bytes(), original.as_deref(), before_check) {
        Ok(()) => {}
        Err(ReplaceError::Changed) => {
            return Err(refuse(format!("Claude desktop changed {CONFIG_FILE} just now; it was left as it is. Try again.")));
        }
        Err(ReplaceError::Io(e)) => return Err(fail("Claude desktop's configuration could not be written", e)),
    }
    tracing::info!(path = %path.display(), "athenaeum added to claude desktop");
    Ok(ClaudeDesktopAdded {
        path: path.to_string_lossy().into_owned(),
        replaced,
        backup: backup.map(|b| b.to_string_lossy().into_owned()),
    })
}
