//! App-data directory identity, shared by every consumer that must locate
//! the desktop data tree without a Tauri handle (the logging fallback in
//! `crate::logging`, the desktop resolver in `athenaeum-tauri/src/paths.rs`).
//!
//! Debug builds resolve a `.dev` SIBLING directory so `npm run tauri dev`
//! can never touch the production catalog on the same machine — the same
//! debug/release split as the test-hub default in
//! `settings::defaults::ACCOUNT_HUB_URL`. Release builds are unaffected.
//! The `ATHENAEUM_APP_DATA_DIR` env override (honored by the resolvers
//! that consume this name, not here) wins over both.
//! Spec: docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md

/// Directory name under the platform app-data root
/// (`~/Library/Application Support`, `%APPDATA%`, `~/.local/share`).
pub fn app_data_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "com.vsharifov.athenaeum.dev"
    } else {
        "com.vsharifov.athenaeum"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_suffix_tracks_build_profile() {
        if cfg!(debug_assertions) {
            assert_eq!(app_data_dir_name(), "com.vsharifov.athenaeum.dev");
        } else {
            assert_eq!(app_data_dir_name(), "com.vsharifov.athenaeum");
        }
    }
}
