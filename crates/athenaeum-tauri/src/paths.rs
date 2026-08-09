//! Desktop app-data resolution — the single place the Tauri host converts
//! `app_data_dir()` into THIS build flavor's data tree.
//!
//! `ATHENAEUM_APP_DATA_DIR` wins verbatim (bug-triage / deliberate
//! debug-against-prod escape hatch). Otherwise Tauri's platform dir has its
//! final component swapped for `athenaeum_core::paths::app_data_dir_name()`
//! — identical in release, the `.dev` sibling in debug.
//! Spec: docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md

use std::path::PathBuf;
use tauri::Manager;

pub(crate) fn resolve_app_data_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    // A set-but-empty override is treated as unset — an empty value would
    // otherwise resolve the data tree to the process CWD.
    if let Some(dir) = std::env::var_os("ATHENAEUM_APP_DATA_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let platform_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(rename_leaf(platform_dir))
}

/// Swap the final path component for the build-flavor identifier. Component
/// replacement, never `set_extension` — the identifier is dotted, an
/// extension API would truncate it.
fn rename_leaf(platform_dir: PathBuf) -> PathBuf {
    match platform_dir.parent() {
        Some(parent) => parent.join(athenaeum_core::paths::app_data_dir_name()),
        None => platform_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_leaf_swaps_only_the_final_component() {
        let out = rename_leaf(PathBuf::from(
            "/x/Application Support/com.vsharifov.athenaeum",
        ));
        assert_eq!(
            out,
            PathBuf::from("/x/Application Support")
                .join(athenaeum_core::paths::app_data_dir_name())
        );
    }
}
