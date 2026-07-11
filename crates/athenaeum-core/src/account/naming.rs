//! Default node name for the hub device list (Sync 2C, task 4).
//!
//! Replaces the old env-only `device_name()`: the machine hostname is the
//! natural default a user recognizes in the device list, so this reads the real
//! OS hostname cross-platform (via the `hostname` crate) and only falls back to
//! a synthetic `<prefix>-<short6>` label when the host is unavailable/empty. The
//! name is cosmetic and renamable later (`api::account::rename_device`).

/// The default node name registered with the hub on sign-in: the machine
/// hostname, else a synthetic `<prefix>-<short6>` label from the device node id.
///
/// GUI apps often do not inherit a useful `HOSTNAME` env var, so this reads the
/// real OS hostname (cross-platform) rather than the environment. An empty /
/// unavailable hostname falls back to [`fallback_name`].
pub fn default_device_name(prefix: &str, node_id_hex: &str) -> String {
    let host = hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    host.unwrap_or_else(|| fallback_name(prefix, node_id_hex))
}

/// The synthetic fallback name: `<prefix>-<first-6-chars-of-node-id-hex>`.
/// Factored out so it is unit-testable without mutating the host environment.
fn fallback_name(prefix: &str, node_id_hex: &str) -> String {
    format!("{prefix}-{}", node_id_hex.chars().take(6).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fallback formatter (used when the OS hostname is unavailable) yields
    /// `<prefix>-<first-6-hex>` — a stable, recognizable label derived from the
    /// device node id. Tested directly so the test never mutates the real host
    /// environment (the hostname itself can't be unset in-process).
    #[test]
    fn default_name_falls_back_to_prefix_short_id_when_no_host() {
        let n = fallback_name("perseus", "ab12cd34ef56");
        assert_eq!(n, "perseus-ab12cd");
    }
}
