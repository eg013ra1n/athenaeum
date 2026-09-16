//! Version arithmetic for the update check (spec §4).

use semver::Version;

/// Parse `s` as SemVer 2.0, tolerating a leading `v`. An unparseable input
/// is logged and yields `None` — a malformed manifest must never produce a
/// false-positive update prompt.
pub fn parse(s: &str) -> Option<Version> {
    let trimmed = s.trim().trim_start_matches('v');
    match Version::parse(trimmed) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(input = %s, error = %e, "failed to parse version");
            None
        }
    }
}

/// True iff `remote` is strictly newer than `current` under SemVer 2.0
/// precedence. Either side unparseable → `false`.
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse(remote), parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

/// `tauri.conf.json` carries a beta as `X.Y.Z-N` (its bundler rejects the
/// dotted form); the tag and the manifest carry `X.Y.Z-beta.N`. SemVer ranks
/// a numeric prerelease BELOW an alphanumeric one, so without this the running
/// beta reads its own manifest as an update. Applied in exactly one place:
/// the comparator the desktop host hands to the updater plugin.
pub fn normalize_tauri_version(s: &str) -> String {
    if let Some((base, pre)) = s.split_once('-') {
        let three_numeric = base.split('.').count() == 3
            && base.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        if three_numeric && !pre.is_empty() && pre.bytes().all(|b| b.is_ascii_digit()) {
            return format!("{base}-beta.{pre}");
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beta_8_is_newer_than_beta_7() {
        assert!(is_newer("0.2.0-beta.8", "0.2.0-beta.7"));
    }

    #[test]
    fn stable_is_newer_than_pre_release_of_same_base() {
        assert!(is_newer("0.2.0", "0.2.0-beta.9"));
    }

    #[test]
    fn pre_release_is_not_newer_than_stable_of_same_base() {
        assert!(!is_newer("0.2.0-beta.9", "0.2.0"));
    }

    #[test]
    fn higher_minor_is_newer_regardless_of_pre_release() {
        assert!(is_newer("0.3.0", "0.2.0-beta.99"));
        assert!(is_newer("0.3.0-beta.1", "0.2.0"));
    }

    #[test]
    fn equal_versions_are_not_newer() {
        assert!(!is_newer("0.2.0-beta.8", "0.2.0-beta.8"));
        assert!(!is_newer("0.2.0", "0.2.0"));
    }

    #[test]
    fn leading_v_is_stripped() {
        assert!(is_newer("v0.2.0-beta.8", "0.2.0-beta.7"));
        assert!(is_newer("0.2.0-beta.8", "v0.2.0-beta.7"));
    }

    #[test]
    fn unparseable_returns_false() {
        assert!(!is_newer("garbage", "0.2.0-beta.8"));
        assert!(!is_newer("0.2.0-beta.8", ""));
    }

    #[test]
    fn older_manifest_is_never_an_update() {
        assert!(!is_newer("0.6.4", "0.6.5"));
        assert!(!is_newer("0.6.5-beta.1", "0.6.5"));
    }

    #[test]
    fn normalize_maps_numeric_prerelease_to_beta() {
        assert_eq!(normalize_tauri_version("0.6.5-1"), "0.6.5-beta.1");
        assert_eq!(normalize_tauri_version("0.6.5-12"), "0.6.5-beta.12");
    }

    #[test]
    fn normalize_leaves_every_other_form_alone() {
        assert_eq!(normalize_tauri_version("0.6.5"), "0.6.5");
        assert_eq!(normalize_tauri_version("0.6.5-beta.1"), "0.6.5-beta.1");
        assert_eq!(normalize_tauri_version("0.6.5-rc.1"), "0.6.5-rc.1");
        assert_eq!(normalize_tauri_version("garbage"), "garbage");
        assert_eq!(normalize_tauri_version("0.6-1"), "0.6-1");
    }

    /// The whole point of the normalizer: the running beta must not see its
    /// own manifest as an update, and must see the next beta and the stable.
    #[test]
    fn running_beta_against_its_own_manifest() {
        let current = normalize_tauri_version("0.6.5-1");
        assert!(!is_newer("0.6.5-beta.1", &current));
        assert!(is_newer("0.6.5-beta.2", &current));
        assert!(is_newer("0.6.5", &current));
    }
}
