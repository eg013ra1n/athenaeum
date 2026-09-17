//! The update manifest (spec §3, the plugin's static format) and everything
//! needed to ask for it: the channel file, the telemetry query, the platform
//! key the running build should look for.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Where the channel files live. `option_env!` lets the local rehearsal
/// (plan Task 11) point a throwaway build at a local server AT BUILD TIME;
/// nothing at runtime can redirect a release build.
pub const MANIFEST_BASE_URL: &str = match option_env!("ATHENAEUM_UPDATES_BASE_URL") {
    Some(u) => u,
    None => "https://artfrom.space/updates",
};

/// Which channel file answers the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    Stable,
    Beta,
}

impl Channel {
    pub fn file(self) -> &'static str {
        match self {
            Channel::Stable => "latest.json",
            Channel::Beta => "latest-beta.json",
        }
    }

    /// The lowercase string a log field carries — never `{:?}`'s
    /// `Stable`/`Beta`, which would be a second, capitalized spelling of the
    /// same value the logging dictionary already documents as lowercase.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PlatformEntry {
    pub url: String,
    pub signature: String,
}

/// serde mirror of the plugin's static manifest. `version` accepts the
/// plugin's `name` alias; a leading `v` is tolerated by the comparison, not
/// stripped here.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateManifest {
    #[serde(alias = "name")]
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub pub_date: Option<String>,
    pub platforms: HashMap<String, PlatformEntry>,
}

impl UpdateManifest {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| anyhow::anyhow!("update manifest is not the expected JSON: {e}"))
    }
}

/// The query the manifest GET carries — the same five parameters the
/// retired `version.json` GET carried, because the nginx access log on
/// artfrom.space is the adoption dashboard's only capture point.
pub struct TelemetryPing<'a> {
    pub app_version: &'a str,
    pub os: &'a str,
    pub arch: &'a str,
    pub commit: &'a str,
    pub installation_id: &'a str,
}

pub fn manifest_url(base: &str, channel: Channel, ping: &TelemetryPing<'_>) -> String {
    format!(
        "{}/{}?v={}&os={}&arch={}&commit={}&id={}",
        base.trim_end_matches('/'),
        channel.file(),
        ping.app_version,
        ping.os,
        ping.arch,
        ping.commit,
        ping.installation_id,
    )
}

/// The plugin's target vocabulary spells macOS `darwin`; every other
/// `std::env::consts::OS` value is used as is.
pub fn plugin_target_os(rust_os: &str) -> &'static str {
    match rust_os {
        "macos" => "darwin",
        "windows" => "windows",
        _ => "linux",
    }
}

/// Whether a manifest with `keys` can serve this build. Mirrors the plugin's
/// own lookup order — `{os}-{arch}-{installer}` first, then `{os}-{arch}` —
/// with one rule of our own in front: an install a package manager owns
/// (`deb`, `rpm`) is never served, because the fallback would hand it the
/// AppImage entry and `dpkg -i` would choke on it after a 100 MB download.
pub fn platform_supported(keys: &[&str], os: &str, arch: &str, installer: Option<&str>) -> bool {
    if matches!(installer, Some("deb") | Some("rpm")) {
        return false;
    }
    let plain = format!("{os}-{arch}");
    match installer {
        Some(i) => {
            let specific = format!("{os}-{arch}-{i}");
            keys.contains(&specific.as_str()) || keys.contains(&plain.as_str())
        }
        None => keys.contains(&plain.as_str()),
    }
}

/// GET the manifest. Every failure is an `Err` with the cause logged here —
/// the old `version.json` fetch returned `None` and lost it.
pub async fn fetch_manifest(client: &reqwest::Client, url: &str) -> Result<UpdateManifest> {
    let resp = client.get(url).send().await.map_err(|e| {
        tracing::error!(url, error = %e, "update manifest request failed");
        anyhow::anyhow!("could not reach the update server: {e}")
    })?;
    let status = resp.status();
    if !status.is_success() {
        tracing::error!(url, status = status.as_u16(), "update manifest request rejected");
        anyhow::bail!("the update server answered HTTP {}", status.as_u16());
    }
    let body = resp.text().await.map_err(|e| {
        tracing::error!(url, error = %e, "update manifest body could not be read");
        anyhow::anyhow!("could not read the update manifest: {e}")
    })?;
    UpdateManifest::parse(&body).map_err(|e| {
        tracing::error!(url, error = %e, "update manifest could not be decoded");
        e
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
      "version": "0.6.5",
      "notes": "*Athenaeum v0.6.5: notes*\n\n## What's New\n- a",
      "pub_date": "2026-09-20T10:00:00Z",
      "platforms": {
        "darwin-aarch64": { "url": "https://artfrom.space/builds/v0.6.5/macos/athenaeum-0.6.5-macos-arm64.app.tar.gz", "signature": "c2ln" },
        "windows-x86_64-nsis": { "url": "https://artfrom.space/builds/v0.6.5/windows/athenaeum-0.6.5-windows-x64-setup.exe", "signature": "c2ln" },
        "windows-x86_64-msi": { "url": "https://artfrom.space/builds/v0.6.5/windows/athenaeum-0.6.5-windows-x64.msi", "signature": "c2ln" },
        "linux-x86_64": { "url": "https://artfrom.space/builds/v0.6.5/linux/athenaeum-0.6.5-linux-x64.AppImage", "signature": "c2ln" }
      }
    }"#;

    #[test]
    fn parses_the_static_format() {
        let m = UpdateManifest::parse(GOOD).unwrap();
        assert_eq!(m.version, "0.6.5");
        assert_eq!(m.pub_date.as_deref(), Some("2026-09-20T10:00:00Z"));
        assert!(m.notes.unwrap().contains("## What's New"));
        assert_eq!(m.platforms.len(), 4);
        assert_eq!(m.platforms["linux-x86_64"].signature, "c2ln");
    }

    #[test]
    fn accepts_the_name_alias_and_missing_optionals() {
        let m = UpdateManifest::parse(r#"{"name":"v0.7.0","platforms":{}}"#).unwrap();
        assert_eq!(m.version, "v0.7.0");
        assert!(m.notes.is_none() && m.pub_date.is_none());
    }

    #[test]
    fn an_entry_without_a_signature_is_an_error() {
        let bad = r#"{"version":"0.6.5","platforms":{"linux-x86_64":{"url":"https://x/y"}}}"#;
        let err = UpdateManifest::parse(bad).unwrap_err().to_string();
        assert!(err.contains("signature"), "{err}");
    }

    #[test]
    fn not_json_is_an_error() {
        assert!(UpdateManifest::parse("<html>").is_err());
    }

    #[test]
    fn channel_files() {
        assert_eq!(Channel::Stable.file(), "latest.json");
        assert_eq!(Channel::Beta.file(), "latest-beta.json");
    }

    #[test]
    fn channel_as_str_is_lowercase() {
        assert_eq!(Channel::Stable.as_str(), "stable");
        assert_eq!(Channel::Beta.as_str(), "beta");
    }

    /// The compiled-in default must never point anywhere but the real
    /// production host over plaintext; the ONLY escape hatch is a
    /// build-time env var for the local rehearsal (see `MANIFEST_BASE_URL`'s
    /// own doc comment) — never a runtime one.
    #[test]
    fn base_url_is_https_unless_overridden_at_build_time() {
        assert!(
            MANIFEST_BASE_URL.starts_with("https://") || option_env!("ATHENAEUM_UPDATES_BASE_URL").is_some(),
            "MANIFEST_BASE_URL = {MANIFEST_BASE_URL:?}"
        );
    }

    #[test]
    fn manifest_url_carries_every_telemetry_parameter() {
        let ping = TelemetryPing {
            app_version: "0.6.4-beta.1",
            os: "macos",
            arch: "aarch64",
            commit: "abc1234",
            installation_id: "11111111-2222-3333-4444-555555555555",
        };
        let url = manifest_url("https://artfrom.space/updates", Channel::Beta, &ping);
        assert_eq!(
            url,
            "https://artfrom.space/updates/latest-beta.json?v=0.6.4-beta.1&os=macos&arch=aarch64&commit=abc1234&id=11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn manifest_url_tolerates_a_trailing_slash_in_the_base() {
        let ping = TelemetryPing { app_version: "1", os: "linux", arch: "x86_64", commit: "c", installation_id: "i" };
        assert!(manifest_url("http://127.0.0.1:8765/", Channel::Stable, &ping)
            .starts_with("http://127.0.0.1:8765/latest.json?"));
    }

    #[test]
    fn plugin_os_names() {
        assert_eq!(plugin_target_os("macos"), "darwin");
        assert_eq!(plugin_target_os("windows"), "windows");
        assert_eq!(plugin_target_os("linux"), "linux");
    }

    #[test]
    fn platform_support_follows_the_plugin_lookup_order() {
        let keys = ["darwin-aarch64", "windows-x86_64-nsis", "windows-x86_64-msi", "linux-x86_64"];
        assert!(platform_supported(&keys, "darwin", "aarch64", Some("app")));
        assert!(platform_supported(&keys, "darwin", "aarch64", None));
        assert!(!platform_supported(&keys, "darwin", "x86_64", Some("app")));
        assert!(platform_supported(&keys, "windows", "x86_64", Some("nsis")));
        assert!(platform_supported(&keys, "windows", "x86_64", Some("msi")));
        // no plain windows key: an unknown Windows installer is NOT served
        assert!(!platform_supported(&keys, "windows", "x86_64", None));
        assert!(platform_supported(&keys, "linux", "x86_64", Some("appimage")));
    }

    #[test]
    fn package_manager_installs_are_never_served() {
        let keys = ["linux-x86_64"];
        assert!(!platform_supported(&keys, "linux", "x86_64", Some("deb")));
        assert!(!platform_supported(&keys, "linux", "x86_64", Some("rpm")));
    }

    #[test]
    fn empty_manifest_supports_nothing() {
        assert!(!platform_supported(&[], "darwin", "aarch64", Some("app")));
    }
}
