# In-app updates with release notes — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Desktop users check for, read about, download, install and restart into a new Athenaeum version inside the app; web/Docker users see the same notes and the `docker pull` line; every host shows "What's new" once after an update.

**Architecture:** `athenaeum-core::updates` owns the check (manifest fetch with the telemetry query, channel choice, SemVer comparison, notes, `updates.last_seen_version`); `tauri-plugin-updater` owns download, minisign verification, install and relaunch on desktop only; the GitLab tag pipeline produces signed updater artifacts, a frozen per-tag manifest that `verify:release` checks (URLs + signatures) before the channel file moves. One `UpdateDialog` in two modes serves both hosts.

**Tech Stack:** Rust (`semver`, `reqwest`, `serde`), `tauri-plugin-updater` 2.x (`AppHandle::restart` from core Tauri — no `tauri-plugin-process`, see Deviations), React + `react-markdown` + `remark-gfm`, bash + python3 CI helpers, `rsign2` for signature verification on the runner.

**Spec:** `docs/superpowers/specs/2026-09-16-in-app-updates-design.md` — read it first; every task cites its sections.

## Global Constraints

- Two backends in sync: every Tauri command in `commands/updates.rs` has its mirror in `routes/updates.rs`, same name, registered in `invoke_handler![]` and `build_router` in the same task.
- Real logic lives in `athenaeum-core`; hosts are 3–10-line wrappers.
- No `@tauri-apps/*` import outside `src/api/` — the frontend never calls the updater plugin's JS API; it goes through `api.invoke('install_update')`.
- Serde `#[serde(rename_all = "camelCase")]` on every new struct crossing the wire; TS types are GENERATED (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`), never hand-written in `src/types/models.ts`.
- Never swallow: every `Err` is logged with `tracing::error!` and fields before it is returned; the launch-time check is the ONE place a failure stays at `console.error`.
- Log field names come from the dictionary in `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`; this plan adds `channel`, `current_version`, `latest_version`, `bytes_total` (Task 10 records them).
- Design tokens only (`bg-surface-elevated`, `text-content-muted`, `bg-accent`, `text-warning`, `text-error`, `border-border`), icons from `lucide-react`.
- Command boundaries wear `#[tracing::instrument(skip_all, err)]` (Tauri) / `err(Debug)` (Axum, `(StatusCode, String)` errors).
- The three version forms (spec §4): Cargo/`CARGO_PKG_VERSION` dotted `0.6.5-beta.1`; `tauri.conf.json` `0.6.5-1`; tag + manifest dotted. `normalize_tauri_version` is applied in exactly one place: the comparator handed to the plugin.
- Manifest URL base `https://artfrom.space/updates/`, files `latest.json` / `latest-beta.json` / `<tag>.json`.
- Filenames come from `.gitlab/ci/scripts/artifact_names.sh` only.
- Commit as the user (`eg013ra1n`), trailers `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y`. No push without the owner's word.
- Gates after every Rust task: `cargo check -p athenaeum-core --all-targets`, `cargo check -p athenaeum-core --no-default-features` (the `updates` module is ungated — it MUST build headless), `cargo test -p <crate>`; after frontend tasks: `npx tsc --noEmit` and `npm run build:web`; after CI-script tasks: `.gitlab/ci/scripts/test/run_tests.sh`.

## Deviations from the spec (recorded here, not silently)

- **No `tauri-plugin-process`.** Spec §2.3 lists it for relaunch; core Tauri's `tauri::AppHandle::restart(&self) -> !` (verified in `tauri-2.11.5/src/app.rs:588`) does the same from a command, so the plugin — whose only job is exposing that to JS — adds a dependency for nothing. `restart_app` calls `app.restart()`.
- **Spec §3.9 is settled: both Windows installers.** The plugin's `check()` builds `["{os}-{arch}-{installer}", "{os}-{arch}"]` and takes the first key present (read from `plugins/updater/src/updater.rs` on the `v2` branch, 2026-09-16). The manifest carries `windows-x86_64-nsis` and `windows-x86_64-msi`, and NO plain `windows-x86_64` key (a build of unknown installer type must not be handed the NSIS exe). The "no" branch of §3.9 (an MSI early refusal) is not built.
- **`.deb`/`.rpm` installs are refused by rule, not by key absence.** The same fallback order means a `.deb` install would match the plain `linux-x86_64` AppImage entry and try `dpkg -i` on an AppImage. `platform_supported` therefore returns `false` whenever the host reports installer `deb` or `rpm`, before any key lookup (Task 2).
- **Manifest `pub_date` is passed through as a string** (RFC 3339 as published); the frontend formats it. No `chrono` parsing in core.
- **The rehearsal's endpoint override is compile-time**: `option_env!("ATHENAEUM_UPDATES_BASE_URL")` in core, read at build time only (Task 3), plus a `--config` JSON override for `dangerousInsecureTransportProtocol` in the rehearsal build command (Task 11). Nothing at runtime can redirect a release build.

## File structure

| Path | Responsibility |
| ---- | -------------- |
| `crates/athenaeum-core/src/updates/mod.rs` | `HostInfo`, `HostKind`, `Channel`, `UpdateCheck`, `WhatsNew`; `check`, `check_at`, `whats_new`, `release_notes`; installation id |
| `crates/athenaeum-core/src/updates/version.rs` | `parse`, `is_newer`, `normalize_tauri_version` |
| `crates/athenaeum-core/src/updates/manifest.rs` | `UpdateManifest`, `PlatformEntry`, `MANIFEST_BASE_URL`, `manifest_url`, `TelemetryPing`, `plugin_target_os`, `platform_supported`, `fetch_manifest` |
| `crates/athenaeum-core/src/updates/notes.rs` | `EMBEDDED_RELEASE_NOTES`, `blog_url` |
| `crates/athenaeum-tauri/src/commands/updates.rs` | `check_for_updates`, `get_whats_new`, `get_release_notes`, `install_update`, `restart_app`, early refusals, `installer_name` |
| `crates/athenaeum-web/build.rs` | `ATHENAEUM_GIT_HASH` for the web binary |
| `crates/athenaeum-web/src/routes/updates.rs` | the five mirrors (two are `501`) |
| `src/contexts/UpdatesContext.tsx` | check result, dialog mode, install phase, listeners |
| `src/components/updates/UpdateDialog.tsx` | the modal, two modes |
| `src/components/updates/ReleaseNotes.tsx` | Markdown rendering with token-styled elements and `openUrl` links |
| `src/hooks/useAutoUpdateCheck.ts` | launch sequence: what's-new, then check |
| `.gitlab/ci/scripts/gen_updater_manifest.sh` | manifest generator |
| `.gitlab/ci/scripts/artifact_names.sh` | `updater_artifacts()` inventory |
| `.gitlab/ci/scripts/verify_release.sh` | section 1b: manifest + signatures |
| `.gitlab-ci.yml` | build artifacts, F5.1 reversal, deploy, `publish:updater-manifest`, `publish:updater-channel` |

---

### Task 1: `core::updates::version` — SemVer comparison and the tauri-form normalizer

**Files:**
- Create: `crates/athenaeum-core/src/updates/mod.rs` (module skeleton only, filled in Task 3)
- Create: `crates/athenaeum-core/src/updates/version.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod updates;` after `pub mod api;`, ungated)
- Modify: `crates/athenaeum-core/Cargo.toml` (add `semver = "1"` under `[dependencies]`)

**Interfaces:**
- Produces: `updates::version::parse(&str) -> Option<semver::Version>`, `updates::version::is_newer(remote: &str, current: &str) -> bool`, `updates::version::normalize_tauri_version(&str) -> String`.

- [ ] **Step 1: Add the dependency and the module skeleton**

`crates/athenaeum-core/Cargo.toml`, in `[dependencies]` next to `uuid`:

```toml
# SemVer 2.0 precedence for the update check (moved here from athenaeum-tauri
# so both hosts compare versions with one function).
semver = "1"
```

`crates/athenaeum-core/src/lib.rs`, after `pub mod api;`:

```rust
pub mod updates;
```

`crates/athenaeum-core/src/updates/mod.rs`:

```rust
//! In-app updates — the host-independent half (spec
//! `docs/superpowers/specs/2026-09-16-in-app-updates-design.md` §2.2):
//! manifest fetch, channel choice, SemVer comparison, release notes and the
//! once-per-version "What's new". Download/verify/install live in the
//! desktop host (`tauri-plugin-updater`); this module never touches a file.

pub mod version;
```

- [ ] **Step 2: Write the failing tests**

`crates/athenaeum-core/src/updates/version.rs`:

```rust
//! Version arithmetic for the update check (spec §4).

use semver::Version;

/// Parse `s` as SemVer 2.0, tolerating a leading `v`. An unparseable input
/// is logged and yields `None` — a malformed manifest must never produce a
/// false-positive update prompt.
pub fn parse(s: &str) -> Option<Version> {
    todo!()
}

/// True iff `remote` is strictly newer than `current` under SemVer 2.0
/// precedence. Either side unparseable → `false`.
pub fn is_newer(remote: &str, current: &str) -> bool {
    todo!()
}

/// `tauri.conf.json` carries a beta as `X.Y.Z-N` (its bundler rejects the
/// dotted form); the tag and the manifest carry `X.Y.Z-beta.N`. SemVer ranks
/// a numeric prerelease BELOW an alphanumeric one, so without this the running
/// beta reads its own manifest as an update. Applied in exactly one place:
/// the comparator the desktop host hands to the updater plugin.
pub fn normalize_tauri_version(s: &str) -> String {
    todo!()
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core updates::version -- --nocapture`
Expected: panics at `todo!()` (every test FAILS).

- [ ] **Step 4: Implement**

Replace the three `todo!()` bodies:

```rust
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

pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse(remote), parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

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
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core updates::version`
Expected: 11 passed.

- [ ] **Step 6: Gate and commit**

Run: `cargo check -p athenaeum-core --no-default-features && cargo check -p athenaeum-core --all-targets`
Expected: both green.

```bash
git add crates/athenaeum-core/Cargo.toml crates/athenaeum-core/src/lib.rs crates/athenaeum-core/src/updates/
git commit -m "feat(updates): core version comparison and the tauri-form normalizer

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 2: `core::updates::manifest` — the manifest, its URL, platform support, the fetch

**Files:**
- Create: `crates/athenaeum-core/src/updates/manifest.rs`
- Modify: `crates/athenaeum-core/src/updates/mod.rs` (add `pub mod manifest;`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `pub const MANIFEST_BASE_URL: &str = "https://artfrom.space/updates"`
  - `pub enum Channel { Stable, Beta }` (serde camelCase, `ts_rs::TS`), `Channel::file(&self) -> &'static str`, `Channel::from_tag(tag: &str) -> Channel`
  - `pub struct PlatformEntry { url: String, signature: String }`, `pub struct UpdateManifest { version, notes: Option<String>, pub_date: Option<String>, platforms: HashMap<String, PlatformEntry> }`, `UpdateManifest::parse(&str) -> anyhow::Result<Self>`
  - `pub struct TelemetryPing<'a> { app_version, os, arch, commit, installation_id: &'a str }`, `manifest_url(base: &str, channel: Channel, ping: &TelemetryPing) -> String`
  - `plugin_target_os(rust_os: &str) -> &'static str`
  - `platform_supported(keys: &[&str], os: &str, arch: &str, installer: Option<&str>) -> bool`
  - `async fn fetch_manifest(client: &reqwest::Client, url: &str) -> anyhow::Result<UpdateManifest>`

- [ ] **Step 1: Write the failing tests**

`crates/athenaeum-core/src/updates/manifest.rs`:

```rust
//! The update manifest (spec §3, the plugin's static format) and everything
//! needed to ask for it: the channel file, the telemetry query, the platform
//! key the running build should look for.

use anyhow::{Context, Result};
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
        todo!()
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
    todo!()
}

/// The plugin's target vocabulary spells macOS `darwin`; every other
/// `std::env::consts::OS` value is used as is.
pub fn plugin_target_os(rust_os: &str) -> &'static str {
    todo!()
}

/// Whether a manifest with `keys` can serve this build. Mirrors the plugin's
/// own lookup order — `{os}-{arch}-{installer}` first, then `{os}-{arch}` —
/// with one rule of our own in front: an install a package manager owns
/// (`deb`, `rpm`) is never served, because the fallback would hand it the
/// AppImage entry and `dpkg -i` would choke on it after a 100 MB download.
pub fn platform_supported(keys: &[&str], os: &str, arch: &str, installer: Option<&str>) -> bool {
    todo!()
}

/// GET the manifest. Every failure is an `Err` with the cause logged here —
/// the old `version.json` fetch returned `None` and lost it.
pub async fn fetch_manifest(client: &reqwest::Client, url: &str) -> Result<UpdateManifest> {
    todo!()
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core updates::manifest`
Expected: FAIL (panics at `todo!()`).

- [ ] **Step 3: Implement**

```rust
impl UpdateManifest {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("update manifest is not the expected JSON")
    }
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

pub fn plugin_target_os(rust_os: &str) -> &'static str {
    match rust_os {
        "macos" => "darwin",
        "windows" => "windows",
        _ => "linux",
    }
}

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
```

Note `plugin_target_os`'s `_ => "linux"`: the plugin itself only ever runs on the three desktop OSes, and the web host reaches `platform_supported` with `HostKind::Web`, which short-circuits to `false` in Task 3 before this function matters.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core updates::manifest`
Expected: 11 passed.

- [ ] **Step 5: Gate and commit**

Run: `cargo check -p athenaeum-core --no-default-features && cargo check -p athenaeum-core --all-targets`

```bash
git add crates/athenaeum-core/src/updates/
git commit -m "feat(updates): manifest model, telemetry URL, platform support rule, fetch

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 3: `core::updates` — `check`, `whats_new`, `release_notes`, the embedded notes, TS export

**Files:**
- Create: `crates/athenaeum-core/src/updates/notes.rs`
- Modify: `crates/athenaeum-core/src/updates/mod.rs`
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `Channel`, `UpdateCheck`, `WhatsNew` in the `models.ts` decl list)
- Regenerate: `src/types/models.ts` (via `TS_RS_WRITE=1`)

**Interfaces:**
- Consumes: Task 1 `version::is_newer`; Task 2 `manifest::*`.
- Produces:
  - `pub enum HostKind { Desktop, Web }`
  - `pub struct HostInfo<'a> { kind: HostKind, git_hash: &'a str, installer: Option<&'a str> }`
  - `pub struct UpdateCheck { current_version, latest_version, is_update_available, channel, notes, pub_date, platform_supported, download_page_url, blog_url, docker_image }` (serde camelCase + TS)
  - `pub struct WhatsNew { version, notes, blog_url }` (serde camelCase + TS)
  - `pub async fn check(ctx: &ServiceContext, host: &HostInfo<'_>) -> Result<UpdateCheck>`
  - `pub async fn check_at(ctx, host, base_url: &str) -> Result<UpdateCheck>` (what `check` calls with `MANIFEST_BASE_URL`; tests point it at a local server)
  - `pub fn whats_new(ctx: &ServiceContext) -> Result<Option<WhatsNew>>`
  - `pub fn release_notes() -> WhatsNew`
  - `pub fn installation_id(conn: &rusqlite::Connection) -> Result<String>`
  - `notes::EMBEDDED_RELEASE_NOTES: &str`, `notes::blog_url(version: &str) -> String`
  - Settings keys: `updates.check_beta`, `updates.last_seen_version`, `installation.id`.

- [ ] **Step 1: `notes.rs` with its tests**

```rust
//! The running build's own release notes (spec §2.2, §4). The release commit
//! holds `RELEASE_NOTES.md` together with the six version files, so the text
//! embedded at compile time IS the notes of the build that carries it — no
//! network, no channel question.

/// Path is relative to this file: updates/ → src/ → athenaeum-core/ → crates/ → repo root.
pub const EMBEDDED_RELEASE_NOTES: &str = include_str!("../../../../RELEASE_NOTES.md");

/// The docs site's blog slug is the tag with every dot removed
/// (`v0.6.5` → `v065`, `v0.6.5-beta.1` → `v065-beta1`) — the same rule
/// `.gitlab/ci/scripts/verify_release.sh` uses to poll the post.
pub fn blog_url(version: &str) -> String {
    let v = version.trim().trim_start_matches('v');
    format!("https://artfrom.space/blog/v{}/", v.replace('.', ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blog_slug_drops_the_dots() {
        assert_eq!(blog_url("0.6.5"), "https://artfrom.space/blog/v065/");
        assert_eq!(blog_url("0.6.5-beta.1"), "https://artfrom.space/blog/v065-beta1/");
        assert_eq!(blog_url("v0.6.5"), "https://artfrom.space/blog/v065/");
    }

    #[test]
    fn embedded_notes_are_the_release_notes_file() {
        assert!(EMBEDDED_RELEASE_NOTES.starts_with('*'), "notes start with the italic tagline");
        assert!(EMBEDDED_RELEASE_NOTES.contains("## "), "notes carry at least one section");
    }
}
```

- [ ] **Step 2: `mod.rs` — types, signatures, failing tests**

Replace `crates/athenaeum-core/src/updates/mod.rs` with:

```rust
//! In-app updates — the host-independent half (spec
//! `docs/superpowers/specs/2026-09-16-in-app-updates-design.md` §2.2):
//! manifest fetch, channel choice, SemVer comparison, release notes and the
//! once-per-version "What's new". Download/verify/install live in the
//! desktop host (`tauri-plugin-updater`); this module never touches a file.

pub mod manifest;
pub mod notes;
pub mod version;

use crate::services::ServiceContext;
use anyhow::{Context, Result};
use manifest::{Channel, TelemetryPing, UpdateManifest, MANIFEST_BASE_URL};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DOWNLOAD_PAGE_URL: &str = "https://artfrom.space/releases/download";
pub const DOCKER_IMAGE: &str = "vsharifov/athenaeum";
pub const SETTING_CHECK_BETA: &str = "updates.check_beta";
pub const SETTING_LAST_SEEN_VERSION: &str = "updates.last_seen_version";
pub const SETTING_INSTALLATION_ID: &str = "installation.id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    Desktop,
    Web,
}

/// What the host knows and core does not: which binary this is, which bundle
/// it was installed from (`nsis`/`msi`/`app`/`appimage`/`deb`/`rpm`, the
/// plugin's installer vocabulary; `None` on web or when unknown).
pub struct HostInfo<'a> {
    pub kind: HostKind,
    pub git_hash: &'a str,
    pub installer: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    /// `CARGO_PKG_VERSION`, dotted form.
    pub current_version: String,
    /// The winning manifest's version, dotted form.
    pub latest_version: String,
    pub is_update_available: bool,
    /// Which channel file won.
    pub channel: Channel,
    /// The manifest's `notes` (Markdown), when present.
    pub notes: Option<String>,
    /// RFC 3339 as published; the frontend formats it.
    pub pub_date: Option<String>,
    /// The plugin can install this on THIS build: desktop, and a manifest key
    /// for the running `<os>-<arch>[-installer]` exists.
    pub platform_supported: bool,
    pub download_page_url: String,
    pub blog_url: String,
    /// `vsharifov/athenaeum:<latest>` on the web host, `None` on desktop.
    pub docker_image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct WhatsNew {
    pub version: String,
    pub notes: String,
    pub blog_url: String,
}

/// One UUID per app database, created on first use (the adoption dashboard
/// counts distinct ids). Moved here from the desktop host so both hosts
/// report the same way.
pub fn installation_id(conn: &rusqlite::Connection) -> Result<String> {
    todo!()
}

/// The running build's own notes, any time (About's "View release notes").
pub fn release_notes() -> WhatsNew {
    todo!()
}

/// Once per version: `Some` on the first call after the version changed,
/// `None` otherwise. A fresh install (no key yet) records the version and
/// returns `None` — a new user is not greeted with a change list. The key is
/// written BEFORE returning, so a crash in the dialog cannot show it twice.
pub fn whats_new(ctx: &ServiceContext) -> Result<Option<WhatsNew>> {
    todo!()
}

pub async fn check(ctx: &ServiceContext, host: &HostInfo<'_>) -> Result<UpdateCheck> {
    check_at(ctx, host, MANIFEST_BASE_URL).await
}

/// `check` against an explicit base URL (tests serve a manifest locally).
/// Stable is always fetched; beta only when `updates.check_beta`; the newer
/// of the two wins. Both fetches failing is an error (logged inside
/// `fetch_manifest`); one failing is logged and the other answers.
pub async fn check_at(ctx: &ServiceContext, host: &HostInfo<'_>, base_url: &str) -> Result<UpdateCheck> {
    todo!()
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("Athenaeum-App")
        .build()
        .context("could not build the HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn ctx() -> (tempfile::TempDir, ServiceContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(dir.path().join("t.db"));
        (dir, ctx)
    }

    fn setting(ctx: &ServiceContext, key: &str) -> Option<String> {
        let db = ctx.db.get().unwrap();
        crate::db::get_setting(&db.conn(), key).unwrap()
    }

    fn set(ctx: &ServiceContext, key: &str, value: &str) {
        let db = ctx.db.get().unwrap();
        crate::db::set_setting(&db.conn(), key, value).unwrap();
    }

    /// A one-shot HTTP/1.1 server on 127.0.0.1: answers `routes` by path
    /// (status, body) and 404 otherwise, for `hits` requests, then exits.
    fn serve(routes: Vec<(&'static str, u16, String)>, hits: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for _ in 0..hits {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let path_only = path.split('?').next().unwrap();
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| *p == path_only)
                    .map(|(_, st, b)| (*st, b.clone()))
                    .unwrap_or((404, String::new()));
                let reason = if status == 200 { "OK" } else { "Not Found" };
                write!(
                    s,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        base
    }

    fn manifest(version: &str, keys: &[&str]) -> String {
        let platforms: Vec<String> = keys
            .iter()
            .map(|k| format!(r#""{k}": {{"url":"https://x/{k}","signature":"c2ln"}}"#))
            .collect();
        format!(
            r#"{{"version":"{version}","notes":"## New in {version}","pub_date":"2026-09-20T00:00:00Z","platforms":{{{}}}}}"#,
            platforms.join(",")
        )
    }

    const DESKTOP_MAC: HostInfo<'static> = HostInfo { kind: HostKind::Desktop, git_hash: "abc", installer: Some("app") };
    const WEB: HostInfo<'static> = HostInfo { kind: HostKind::Web, git_hash: "abc", installer: None };

    #[test]
    fn installation_id_is_created_once() {
        let (_d, ctx) = ctx();
        let db = ctx.db.get().unwrap();
        let a = installation_id(&db.conn()).unwrap();
        let b = installation_id(&db.conn()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(setting(&ctx, SETTING_INSTALLATION_ID).as_deref(), Some(a.as_str()));
    }

    #[test]
    fn release_notes_are_the_embedded_notes_for_this_version() {
        let r = release_notes();
        assert_eq!(r.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(r.notes, notes::EMBEDDED_RELEASE_NOTES);
        assert_eq!(r.blog_url, notes::blog_url(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn fresh_install_records_the_version_silently() {
        let (_d, ctx) = ctx();
        assert!(whats_new(&ctx).unwrap().is_none());
        assert_eq!(setting(&ctx, SETTING_LAST_SEEN_VERSION).as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn version_change_shows_once() {
        let (_d, ctx) = ctx();
        set(&ctx, SETTING_LAST_SEEN_VERSION, "0.0.1");
        let first = whats_new(&ctx).unwrap().expect("changed version → Some");
        assert_eq!(first.version, env!("CARGO_PKG_VERSION"));
        assert!(whats_new(&ctx).unwrap().is_none(), "second call → None");
        assert_eq!(setting(&ctx, SETTING_LAST_SEEN_VERSION).as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[tokio::test]
    async fn stable_only_by_default_and_newer_wins() {
        let (_d, ctx) = ctx();
        let base = serve(vec![("/latest.json", 200, manifest("99.0.0", &["darwin-aarch64"]))], 1);
        let r = check_at(&ctx, &DESKTOP_MAC, &base).await.unwrap();
        assert_eq!(r.current_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(r.latest_version, "99.0.0");
        assert!(r.is_update_available);
        assert_eq!(r.channel, Channel::Stable);
        assert_eq!(r.notes.as_deref(), Some("## New in 99.0.0"));
        assert_eq!(r.pub_date.as_deref(), Some("2026-09-20T00:00:00Z"));
        assert_eq!(r.download_page_url, DOWNLOAD_PAGE_URL);
        assert_eq!(r.blog_url, "https://artfrom.space/blog/v9900/");
        assert!(r.docker_image.is_none());
    }

    #[tokio::test]
    async fn beta_is_fetched_only_when_opted_in_and_newer_beta_wins() {
        let (_d, ctx) = ctx();
        set(&ctx, SETTING_CHECK_BETA, "true");
        let base = serve(
            vec![
                ("/latest.json", 200, manifest("99.0.0", &["darwin-aarch64"])),
                ("/latest-beta.json", 200, manifest("99.1.0-beta.1", &["darwin-aarch64"])),
            ],
            2,
        );
        let r = check_at(&ctx, &DESKTOP_MAC, &base).await.unwrap();
        assert_eq!(r.latest_version, "99.1.0-beta.1");
        assert_eq!(r.channel, Channel::Beta);
    }

    #[tokio::test]
    async fn stable_beats_an_older_beta() {
        let (_d, ctx) = ctx();
        set(&ctx, SETTING_CHECK_BETA, "true");
        let base = serve(
            vec![
                ("/latest.json", 200, manifest("99.1.0", &["darwin-aarch64"])),
                ("/latest-beta.json", 200, manifest("99.1.0-beta.3", &["darwin-aarch64"])),
            ],
            2,
        );
        let r = check_at(&ctx, &DESKTOP_MAC, &base).await.unwrap();
        assert_eq!(r.latest_version, "99.1.0");
        assert_eq!(r.channel, Channel::Stable);
    }

    #[tokio::test]
    async fn older_manifest_is_up_to_date() {
        let (_d, ctx) = ctx();
        let base = serve(vec![("/latest.json", 200, manifest("0.0.1", &["darwin-aarch64"]))], 1);
        let r = check_at(&ctx, &DESKTOP_MAC, &base).await.unwrap();
        assert!(!r.is_update_available);
        assert_eq!(r.latest_version, "0.0.1");
    }

    #[tokio::test]
    async fn platform_support_follows_the_host() {
        let (_d, ctx) = ctx();
        let base = serve(vec![("/latest.json", 200, manifest("99.0.0", &["linux-x86_64"]))], 3);
        let mac = check_at(&ctx, &DESKTOP_MAC, &base).await.unwrap();
        assert!(!mac.platform_supported, "no darwin key");
        let web = check_at(&ctx, &WEB, &base).await.unwrap();
        assert!(!web.platform_supported, "web is never installable");
        assert_eq!(web.docker_image.as_deref(), Some("vsharifov/athenaeum:99.0.0"));
        let deb = HostInfo { kind: HostKind::Desktop, git_hash: "abc", installer: Some("deb") };
        let d = check_at(&ctx, &deb, &base).await.unwrap();
        assert!(!d.platform_supported, "package-manager installs are refused by rule");
    }

    #[tokio::test]
    async fn one_channel_failing_does_not_hide_the_other() {
        let (_d, ctx) = ctx();
        set(&ctx, SETTING_CHECK_BETA, "true");
        let base = serve(vec![("/latest-beta.json", 200, manifest("99.0.0-beta.1", &["darwin-aarch64"]))], 2);
        let r = check_at(&ctx, &DESKTOP_MAC, &base).await.unwrap();
        assert_eq!(r.latest_version, "99.0.0-beta.1");
    }

    #[tokio::test]
    async fn both_channels_failing_is_an_error() {
        let (_d, ctx) = ctx();
        let base = serve(vec![], 1);
        let err = check_at(&ctx, &DESKTOP_MAC, &base).await.unwrap_err().to_string();
        assert!(err.contains("HTTP 404"), "{err}");
    }

    #[tokio::test]
    async fn the_request_carries_the_telemetry_query() {
        let (_d, ctx) = ctx();
        // The one-shot server ignores the query, so assert on the URL builder
        // with the same inputs `check_at` uses.
        let db = ctx.db.get().unwrap();
        let id = installation_id(&db.conn()).unwrap();
        let ping = TelemetryPing {
            app_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            commit: "abc",
            installation_id: &id,
        };
        let url = manifest::manifest_url("http://h", Channel::Stable, &ping);
        assert!(url.contains(&format!("&id={id}")));
        assert!(url.contains(&format!("?v={}&", env!("CARGO_PKG_VERSION"))));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core updates::`
Expected: the Task 1/2 tests pass, the new `mod.rs` tests FAIL at `todo!()`.

- [ ] **Step 4: Implement the four functions**

```rust
pub fn installation_id(conn: &rusqlite::Connection) -> Result<String> {
    if let Some(id) = crate::db::get_setting(conn, SETTING_INSTALLATION_ID)? {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    crate::db::set_setting(conn, SETTING_INSTALLATION_ID, &id)?;
    tracing::info!("installation id created");
    Ok(id)
}

pub fn release_notes() -> WhatsNew {
    let version = env!("CARGO_PKG_VERSION");
    WhatsNew {
        version: version.to_string(),
        notes: notes::EMBEDDED_RELEASE_NOTES.to_string(),
        blog_url: notes::blog_url(version),
    }
}

pub fn whats_new(ctx: &ServiceContext) -> Result<Option<WhatsNew>> {
    let db = ctx.db.get().context("database not initialized")?;
    let conn = db.conn();
    let current = env!("CARGO_PKG_VERSION");
    let seen = crate::db::get_setting(&conn, SETTING_LAST_SEEN_VERSION)?;
    if seen.as_deref() == Some(current) {
        return Ok(None);
    }
    crate::db::set_setting(&conn, SETTING_LAST_SEEN_VERSION, current)?;
    match seen {
        None => {
            tracing::info!(current_version = current, "first launch — release notes not shown");
            Ok(None)
        }
        Some(previous) => {
            tracing::info!(previous_version = %previous, current_version = current, "version changed — showing what's new");
            Ok(Some(release_notes()))
        }
    }
}

pub async fn check_at(ctx: &ServiceContext, host: &HostInfo<'_>, base_url: &str) -> Result<UpdateCheck> {
    let current = env!("CARGO_PKG_VERSION");
    let (id, check_beta) = {
        let db = ctx.db.get().context("database not initialized")?;
        let conn = db.conn();
        let id = installation_id(&conn)?;
        let beta = crate::db::get_setting(&conn, SETTING_CHECK_BETA)?
            .map(|v| v == "true")
            .unwrap_or(false);
        (id, beta)
    };
    let ping = TelemetryPing {
        app_version: current,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        commit: host.git_hash,
        installation_id: &id,
    };
    let client = http_client()?;

    let stable = manifest::fetch_manifest(&client, &manifest::manifest_url(base_url, Channel::Stable, &ping)).await;
    let beta = if check_beta {
        Some(manifest::fetch_manifest(&client, &manifest::manifest_url(base_url, Channel::Beta, &ping)).await)
    } else {
        None
    };

    let (channel, manifest): (Channel, UpdateManifest) = match (stable, beta) {
        (Ok(s), Some(Ok(b))) => {
            if version::is_newer(&b.version, &s.version) { (Channel::Beta, b) } else { (Channel::Stable, s) }
        }
        (Ok(s), _) => (Channel::Stable, s),
        (Err(_), Some(Ok(b))) => (Channel::Beta, b),
        (Err(e), _) => return Err(e),
    };

    let latest = manifest.version.trim().trim_start_matches('v').to_string();
    let keys: Vec<&str> = manifest.platforms.keys().map(String::as_str).collect();
    let platform_supported = host.kind == HostKind::Desktop
        && manifest::platform_supported(
            &keys,
            manifest::plugin_target_os(std::env::consts::OS),
            std::env::consts::ARCH,
            host.installer,
        );
    let is_update_available = version::is_newer(&latest, current);
    tracing::info!(
        channel = ?channel,
        current_version = current,
        latest_version = %latest,
        is_update_available,
        platform_supported,
        "update check finished"
    );
    Ok(UpdateCheck {
        current_version: current.to_string(),
        latest_version: latest.clone(),
        is_update_available,
        channel,
        notes: manifest.notes,
        pub_date: manifest.pub_date,
        platform_supported,
        download_page_url: DOWNLOAD_PAGE_URL.to_string(),
        blog_url: notes::blog_url(&latest),
        docker_image: match host.kind {
            HostKind::Web => Some(format!("{DOCKER_IMAGE}:{latest}")),
            HostKind::Desktop => None,
        },
    })
}
```

The `(Err(_), Some(Ok(b)))` arm: the stable failure was already logged by `fetch_manifest`; the beta answers. `(Err(e), _)` covers "stable failed and beta absent or failed" — the beta's own error, if any, was logged too.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core updates::`
Expected: all pass (11 + 11 + 2 + 12).

- [ ] **Step 6: Register the TS types and regenerate**

`crates/athenaeum-core/src/ts_export.rs`, in the `models.ts` `decls![ ... ]` list, after `crate::services::compute_queue::ComputeQueueEntry` (or at the end of that list if that entry is spelled differently — add at the end):

```rust
            crate::updates::manifest::Channel,
            crate::updates::UpdateCheck,
            crate::updates::WhatsNew,
```

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract && cargo test -p athenaeum-core --test ts_contract`
Expected: `src/types/models.ts` gains `Channel`, `UpdateCheck`, `WhatsNew`; the second run passes. Confirm with `grep -n "UpdateCheck\|WhatsNew\|export type Channel" src/types/models.ts`.

Note `ts_export` is gated `all(render, solver)` — the default features — so this runs in the ordinary `cargo test`.

- [ ] **Step 7: Gate and commit**

Run: `cargo check -p athenaeum-core --no-default-features && cargo check -p athenaeum-core --all-targets && cargo test -p athenaeum-core updates::`

```bash
git add crates/athenaeum-core/src/updates/ crates/athenaeum-core/src/ts_export.rs src/types/models.ts
git commit -m "feat(updates): core check, what's-new, embedded release notes, TS types

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 4: Desktop host — the updater plugin, `commands/updates.rs`, early refusals, comparator parity

**Files:**
- Modify: `crates/athenaeum-tauri/Cargo.toml` (add `tauri-plugin-updater = "2"`; REMOVE `semver = "1"`)
- Modify: `crates/athenaeum-tauri/src/lib.rs` (register the plugin in `setup`; the five commands in `generate_handler![]`; `update_in_flight` in `AppState`)
- Modify: `crates/athenaeum-tauri/src/commands/mod.rs` (`pub mod updates; pub use updates::*;`; `AppState.update_in_flight`)
- Modify: `crates/athenaeum-tauri/src/commands/core.rs` (delete `UpdateInfo`, `VersionJson`, `version_is_newer`, `fetch_version_json`, `check_for_updates` and their tests — they moved to core in Tasks 1–3)
- Create: `crates/athenaeum-tauri/src/commands/updates.rs`
- Modify: `crates/athenaeum-tauri/tauri.conf.json` (`plugins.updater` block; `updater:default` permission in the inline capability)
- Modify: `crates/athenaeum-tauri/capabilities/default.json` (`updater:default`)

**Interfaces:**
- Consumes: Task 3 `updates::{check, whats_new, release_notes, HostInfo, HostKind, UpdateCheck, WhatsNew}`, Task 2 `manifest::{Channel, MANIFEST_BASE_URL}`, Task 1 `version::{is_newer, normalize_tauri_version}`.
- Produces (commands, both hosts must match): `check_for_updates() -> UpdateCheck`, `get_whats_new() -> Option<WhatsNew>`, `get_release_notes() -> WhatsNew`, `install_update({ channel: Channel }) -> ()`, `restart_app() -> ()`. Events: `update-progress { downloaded: u64, total: Option<u64> }`, `update-ready { version: String }`.

- [ ] **Step 1: Generate a DEVELOPMENT signing key (throwaway) and add the plugin config**

The release key is the owner's (spec §3.1, Task 7 swaps the public key). For development and the rehearsal, generate a throwaway pair OUTSIDE the repo:

```bash
mkdir -p ~/.tauri && npx tauri signer generate -w ~/.tauri/athenaeum-dev.key --ci
cat ~/.tauri/athenaeum-dev.key.pub
```

`crates/athenaeum-tauri/tauri.conf.json` — add after the `"bundle"` object (top level):

```json
  "plugins": {
    "updater": {
      "pubkey": "<contents of athenaeum-dev.key.pub, one line>",
      "endpoints": ["https://artfrom.space/updates/latest.json"],
      "windows": { "installMode": "passive" }
    }
  }
```

and in `app.security.capabilities[0].permissions` add `"updater:default"`; the same in `capabilities/default.json`'s `permissions`.

Record in the commit message that the committed public key is the DEV key and is replaced by the release key in Task 7 — a build signed with the dev key is only ever served by the rehearsal's local server.

- [ ] **Step 2: Dependencies and plugin registration**

`crates/athenaeum-tauri/Cargo.toml`: replace the `semver = "1"` line with

```toml
tauri-plugin-updater = "2"
```

`crates/athenaeum-tauri/src/lib.rs` — the builder gets a `setup` hook registering the plugin (desktop only, per the plugin's README). Find the existing `.plugin(tauri_plugin_fs::init())` and add right after it:

```rust
        .setup(|app| {
            // The updater is registered in setup (the plugin's documented
            // placement); it reads `plugins.updater` from tauri.conf.json.
            app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
            Ok(())
        })
```

If `lib.rs` already has a `.setup(...)` closure, add the one `app.handle().plugin(...)` line at the top of the existing closure instead of a second `.setup`.

`crates/athenaeum-tauri/src/commands/mod.rs` — `AppState` gains

```rust
    /// One install at a time (spec §6.2): set by `install_update`, cleared by
    /// its guard on every exit path.
    pub update_in_flight: std::sync::atomic::AtomicBool,
```

and every `AppState { ... }` literal (in `lib.rs` `.manage({ ... })`) gains `update_in_flight: std::sync::atomic::AtomicBool::new(false),`. Add `pub mod updates;` and `pub use updates::*;` beside the other modules.

- [ ] **Step 3: Delete the old update code from `commands/core.rs`**

Remove `UpdateInfo`, `VersionJson`, `version_is_newer`, `fetch_version_json`, `check_for_updates` and the `mod tests` block that tests `version_is_newer` (all moved to core). Leave `initialize_database` and the rest. Remove `use super::AppState;` only if nothing else in the file uses it (it does — keep it). The `commands::check_for_updates` entry in `generate_handler![]` will now resolve to the new module's function of the same name.

- [ ] **Step 4: Write the parity test (failing) and the command module**

`crates/athenaeum-tauri/src/commands/updates.rs`:

```rust
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
    use tauri::utils::platform::BundleType;
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
```

`total` in the progress closure is `Option<u64>` per the plugin's `on_chunk: FnMut(usize, Option<u64>)`.

- [ ] **Step 5: Register the commands**

`crates/athenaeum-tauri/src/lib.rs`, in `tauri::generate_handler![ ... ]`, next to `commands::check_for_updates` (already listed) add:

```rust
            commands::get_whats_new,
            commands::get_release_notes,
            commands::install_update,
            commands::restart_app,
```

- [ ] **Step 6: Build and test**

Run: `cargo check -p athenaeum-tauri && cargo test -p athenaeum-tauri updates::`
Expected: compiles; `plugin_and_core_agree` passes. If `BundleType` variant names differ from the match above, read them with `grep -n "pub enum BundleType" -A20 ~/.cargo/registry/src/*/tauri-utils-2.*/src/platform.rs` and fix the match — keep the `_ => None` arm.

Also run `npm run tauri dev` once: the app starts, About's check still works (Task 6 rewires the UI; until then the old `UpdateInfo` fields are missing — the About page shows "Failed to check for updates" or `undefined` values, expected until Task 6). What matters here: no plugin init panic in the terminal.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-tauri/
git commit -m "feat(updates): desktop host — updater plugin, install/restart commands, early refusals

The committed updater public key is a DEVELOPMENT key; the release key
replaces it in the pipeline task (spec §3.1).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 5: Web host — `build.rs`, `routes/updates.rs`, the two `501`s

**Files:**
- Create: `crates/athenaeum-web/build.rs`
- Create: `crates/athenaeum-web/src/routes/updates.rs`
- Modify: `crates/athenaeum-web/src/routes/mod.rs` (`mod updates;`, five routes, delete the `check_for_updates` stub function and its route line)

**Interfaces:**
- Consumes: Task 3 core functions. Mirrors Task 4's five commands by name.

- [ ] **Step 1: `build.rs`**

`crates/athenaeum-web/build.rs` — the git-hash half of the Tauri crate's build script, nothing else:

```rust
//! Embeds the git commit hash as ATHENAEUM_GIT_HASH for the update check's
//! telemetry query (the desktop crate does the same in its own build.rs).
//! Docker builds have no .git → "unknown".

fn main() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ATHENAEUM_GIT_HASH={hash}");
    let git_dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../.git");
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(contents) = std::fs::read_to_string(&head) {
            if let Some(ref_path) = contents.trim().strip_prefix("ref: ") {
                let branch_ref = git_dir.join(ref_path);
                if branch_ref.exists() {
                    println!("cargo:rerun-if-changed={}", branch_ref.display());
                }
            }
        }
    }
}
```

- [ ] **Step 2: The route module with its tests**

`crates/athenaeum-web/src/routes/updates.rs`:

```rust
// Update route handlers — mirrors `athenaeum-tauri/src/commands/updates.rs`
// one-for-one. The check and the notes are core; install/restart are
// desktop-only and answer 501 here (spec §2.4): a Docker install is updated
// by pulling the image.

use athenaeum_core::updates::{self, HostInfo, HostKind, UpdateCheck, WhatsNew};
use axum::{extract::State, http::StatusCode, Json};

use crate::WebAppState;

const HOST: HostInfo<'static> = HostInfo {
    kind: HostKind::Web,
    git_hash: env!("ATHENAEUM_GIT_HASH"),
    installer: None,
};

pub const NOT_ON_WEB: &str = "updates are installed by pulling the image";

/// `POST /api/check_for_updates`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn check_for_updates(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<UpdateCheck>, (StatusCode, String)> {
    updates::check(&state.ctx, &HOST)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
}

/// `POST /api/get_whats_new`
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_whats_new(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Option<WhatsNew>>, (StatusCode, String)> {
    updates::whats_new(&state.ctx)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `POST /api/get_release_notes`
#[tracing::instrument(skip_all)]
pub async fn get_release_notes(Json(_): Json<serde_json::Value>) -> Json<WhatsNew> {
    Json(updates::release_notes())
}

/// `POST /api/install_update` — 501 on web.
#[tracing::instrument(skip_all)]
pub async fn install_update(Json(_): Json<serde_json::Value>) -> (StatusCode, String) {
    tracing::warn!("install_update called on the web host");
    (StatusCode::NOT_IMPLEMENTED, NOT_ON_WEB.to_string())
}

/// `POST /api/restart_app` — 501 on web.
#[tracing::instrument(skip_all)]
pub async fn restart_app(Json(_): Json<serde_json::Value>) -> (StatusCode, String) {
    tracing::warn!("restart_app called on the web host");
    (StatusCode::NOT_IMPLEMENTED, NOT_ON_WEB.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn install_and_restart_are_not_implemented_on_web() {
        let (s, body) = install_update(Json(serde_json::json!({}))).await;
        assert_eq!(s, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(body, NOT_ON_WEB);
        let (s, _) = restart_app(Json(serde_json::json!({}))).await;
        assert_eq!(s, StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn release_notes_are_the_embedded_notes() {
        let Json(n) = get_release_notes(Json(serde_json::json!({}))).await;
        assert_eq!(n.version, env!("CARGO_PKG_VERSION"));
        assert!(n.notes.starts_with('*'));
    }

    #[test]
    fn web_host_is_never_installable() {
        assert_eq!(HOST.kind, HostKind::Web);
        assert!(HOST.installer.is_none());
    }
}
```

- [ ] **Step 3: Register and delete the stub**

`crates/athenaeum-web/src/routes/mod.rs`: add `mod updates;` beside `mod calendar;`; delete the `.route("/api/check_for_updates", post(check_for_updates))` line in the "Category A — Desktop-only stubs" block AND the `async fn check_for_updates` stub function below; add, beside the calendar route:

```rust
        .route("/api/check_for_updates", post(updates::check_for_updates))
        .route("/api/get_whats_new", post(updates::get_whats_new))
        .route("/api/get_release_notes", post(updates::get_release_notes))
        .route("/api/install_update", post(updates::install_update))
        .route("/api/restart_app", post(updates::restart_app))
```

- [ ] **Step 4: Build and test**

Run: `cargo check -p athenaeum-web && cargo test -p athenaeum-web updates::`
Expected: green; 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-web/
git commit -m "feat(updates): web host — check/what's-new/notes routes, 501 for install/restart

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 6: Frontend — `UpdatesContext`, `UpdateDialog`, launch sequence, About, Settings

**Files:**
- Modify: `package.json` (add `react-markdown`, `remark-gfm`)
- Modify: `src/types/helpers.ts` (delete the hand-written `UpdateInfo`)
- Create: `src/contexts/UpdatesContext.tsx`
- Create: `src/components/updates/ReleaseNotes.tsx`
- Create: `src/components/updates/UpdateDialog.tsx`
- Modify: `src/hooks/useAutoUpdateCheck.ts`
- Modify: `src/components/Layout.tsx` (provider + dialog at root)
- Modify: `src/pages/About.tsx` (`UpdateSection` rewrite, `?update` query)
- Modify: `src/pages/Settings.tsx` (Updates section: drop the `isTauri` gate, reword)

**Interfaces:**
- Consumes: generated `UpdateCheck`, `WhatsNew`, `Channel` from `src/types/models.ts` (Task 3); commands from Tasks 4/5; events `update-progress { downloaded: number | null, total: number | null, finished?: boolean }`, `update-ready { version: string }`; `get_compute_queue` → `ComputeQueueEntry[]`; `useTransfers().active` (`OutboundSummary[]`).
- Produces: `useUpdates()` with `{ check, whatsNew, dialog, phase, error, runCheck(), openAvailable(), openWhatsNew(), openReleaseNotes(), close(), install(), restart() }`.

- [ ] **Step 1: Dependencies and the stale type**

```bash
npm install react-markdown@^10 remark-gfm@^4
```

`src/types/helpers.ts`: delete the `UpdateInfo` interface and its comment (lines 36–43 in the current file). Run `grep -rn "UpdateInfo" src/` — the only remaining users are `About.tsx` and `useAutoUpdateCheck.ts`, both rewritten below.

- [ ] **Step 2: `UpdatesContext`**

`src/contexts/UpdatesContext.tsx`:

```tsx
// In-app updates (spec §2.5, §5): the last check, the dialog's mode and the
// install phase, shared by the launch hook, the About page and the dialog.
// Listeners use the cancelled-flag pattern from CLAUDE.md.

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { api } from '../api';
import type { UpdateCheck, WhatsNew } from '../types/models';

export type DialogMode = 'closed' | 'available' | 'whatsNew';

export type InstallPhase =
  | { kind: 'idle' }
  | { kind: 'downloading'; downloaded: number; total: number | null }
  | { kind: 'ready'; version: string }
  | { kind: 'failed'; message: string };

interface ProgressEvent {
  downloaded: number | null;
  total: number | null;
  finished?: boolean;
}

interface UpdatesContextValue {
  /** Last `check_for_updates` result, `null` before the first check. */
  check: UpdateCheck | null;
  /** What the dialog shows in `whatsNew` mode. */
  whatsNew: WhatsNew | null;
  dialog: DialogMode;
  phase: InstallPhase;
  /** Error text of the last manual check, if any. */
  checkError: string | null;
  checking: boolean;
  runCheck: () => Promise<UpdateCheck | null>;
  openAvailable: () => void;
  /** Open `whatsNew` mode with the given notes (launch: the once-per-version result). */
  openWhatsNew: (w: WhatsNew) => void;
  /** Open `whatsNew` mode with the running build's notes (About: View release notes). */
  openReleaseNotes: () => Promise<void>;
  close: () => void;
  install: () => Promise<void>;
  restart: () => Promise<void>;
}

const UpdatesContext = createContext<UpdatesContextValue | null>(null);

export function UpdatesProvider({ children }: { children: ReactNode }) {
  const [check, setCheck] = useState<UpdateCheck | null>(null);
  const [whatsNew, setWhatsNew] = useState<WhatsNew | null>(null);
  const [dialog, setDialog] = useState<DialogMode>('closed');
  const [phase, setPhase] = useState<InstallPhase>({ kind: 'idle' });
  const [checkError, setCheckError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const checkRef = useRef<UpdateCheck | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenReady: (() => void) | undefined;
    api
      .listen<ProgressEvent>('update-progress', (p) => {
        if (cancelled) return;
        if (p.finished || p.downloaded === null) return;
        setPhase({ kind: 'downloading', downloaded: p.downloaded, total: p.total });
      })
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[updates] listen update-progress failed:', err));
    api
      .listen<{ version: string }>('update-ready', (p) => {
        if (cancelled) return;
        setPhase({ kind: 'ready', version: p.version });
      })
      .then((fn) => { if (cancelled) fn(); else unlistenReady = fn; })
      .catch((err) => console.error('[updates] listen update-ready failed:', err));
    return () => { cancelled = true; unlistenProgress?.(); unlistenReady?.(); };
  }, []);

  const runCheck = useCallback(async (): Promise<UpdateCheck | null> => {
    setChecking(true);
    setCheckError(null);
    try {
      const result = await api.invoke<UpdateCheck>('check_for_updates');
      checkRef.current = result;
      setCheck(result);
      return result;
    } catch (err) {
      const msg = typeof err === 'string' ? err : 'Failed to check for updates';
      console.error('check_for_updates:', err);
      setCheckError(msg);
      return null;
    } finally {
      setChecking(false);
    }
  }, []);

  const openAvailable = useCallback(() => setDialog('available'), []);
  const openWhatsNew = useCallback((w: WhatsNew) => { setWhatsNew(w); setDialog('whatsNew'); }, []);
  const openReleaseNotes = useCallback(async () => {
    try {
      const w = await api.invoke<WhatsNew>('get_release_notes');
      setWhatsNew(w);
      setDialog('whatsNew');
    } catch (err) {
      console.error('get_release_notes:', err);
    }
  }, []);
  const close = useCallback(() => setDialog('closed'), []);

  const install = useCallback(async () => {
    const current = checkRef.current;
    if (!current) return;
    setPhase({ kind: 'downloading', downloaded: 0, total: null });
    try {
      await api.invoke('install_update', { args: { channel: current.channel } });
      // `update-ready` sets the ready phase; on Windows the app exits before.
    } catch (err) {
      const message = typeof err === 'string' ? err : 'The update could not be installed';
      console.error('install_update:', err);
      setPhase({ kind: 'failed', message });
    }
  }, []);

  const restart = useCallback(async () => {
    try {
      await api.invoke('restart_app');
    } catch (err) {
      console.error('restart_app:', err);
      setPhase({ kind: 'failed', message: typeof err === 'string' ? err : 'Restart failed' });
    }
  }, []);

  const value = useMemo<UpdatesContextValue>(
    () => ({ check, whatsNew, dialog, phase, checkError, checking, runCheck, openAvailable, openWhatsNew, openReleaseNotes, close, install, restart }),
    [check, whatsNew, dialog, phase, checkError, checking, runCheck, openAvailable, openWhatsNew, openReleaseNotes, close, install, restart],
  );
  return <UpdatesContext.Provider value={value}>{children}</UpdatesContext.Provider>;
}

export function useUpdates(): UpdatesContextValue {
  const ctx = useContext(UpdatesContext);
  if (!ctx) throw new Error('useUpdates must be used within UpdatesProvider');
  return ctx;
}
```

The `install_update` invoke passes `{ args: { channel } }` because the Tauri command takes one struct argument named `args` (Task 4's `InstallArgs`); the web mirror ignores its body.

- [ ] **Step 3: `ReleaseNotes` — Markdown with tokens**

`src/components/updates/ReleaseNotes.tsx`:

```tsx
// Release-notes Markdown → token-styled elements. Links open outside the
// webview (opener / window.open), never as in-app navigation.

import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { openUrl } from '../../api/desktop';

export function ReleaseNotes({ markdown }: { markdown: string }) {
  return (
    <div className="text-sm text-content-secondary leading-relaxed">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => <h1 className="text-base font-semibold text-content mt-4 mb-2">{children}</h1>,
          h2: ({ children }) => <h2 className="text-sm font-semibold uppercase tracking-wider text-accent mt-5 mb-2">{children}</h2>,
          h3: ({ children }) => <h3 className="text-sm font-semibold text-content mt-3 mb-1">{children}</h3>,
          p: ({ children }) => <p className="my-2">{children}</p>,
          em: ({ children }) => <em className="text-content-muted">{children}</em>,
          strong: ({ children }) => <strong className="text-content font-semibold">{children}</strong>,
          ul: ({ children }) => <ul className="list-disc pl-5 my-2 space-y-1">{children}</ul>,
          ol: ({ children }) => <ol className="list-decimal pl-5 my-2 space-y-1">{children}</ol>,
          li: ({ children }) => <li>{children}</li>,
          code: ({ children }) => <code className="rounded bg-surface px-1 py-0.5 font-mono text-xs text-content">{children}</code>,
          pre: ({ children }) => <pre className="rounded bg-surface p-3 my-2 overflow-x-auto font-mono text-xs">{children}</pre>,
          table: ({ children }) => <table className="my-2 w-full text-xs border-collapse">{children}</table>,
          th: ({ children }) => <th className="border border-border px-2 py-1 text-left text-content">{children}</th>,
          td: ({ children }) => <td className="border border-border px-2 py-1">{children}</td>,
          a: ({ href, children }) => (
            <a
              href={href}
              className="text-accent underline hover:text-accent-hover"
              onClick={(e) => {
                e.preventDefault();
                if (href) void openUrl(href);
              }}
            >
              {children}
            </a>
          ),
        }}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}
```

- [ ] **Step 4: `UpdateDialog`**

`src/components/updates/UpdateDialog.tsx`:

```tsx
// One modal, two modes (spec §5.1): `available` (notes + install flow) and
// `whatsNew` (notes + Close). Rendered at app root by Layout.

import { useEffect, useState } from 'react';
import { Copy, Download, ExternalLink, RefreshCw, X, AlertTriangle, CheckCircle2 } from 'lucide-react';
import { api } from '../../api';
import { openUrl } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { formatTimestamp } from '../../utils/dateFormatting';
import { useUpdates } from '../../contexts/UpdatesContext';
import { useTransfers } from '../../contexts/TransfersContext';
import type { ComputeQueueEntry } from '../../types/models';
import { ReleaseNotes } from './ReleaseNotes';

const isWindows = typeof navigator !== 'undefined' && /Win/i.test(navigator.platform);

function formatBytes(n: number): string {
  if (n >= 1024 * 1024 * 1024) return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024).toFixed(0)} KB`;
}

/** Soft guard (spec §5.2): what the sidebar already knows about running work. */
function useBusyReason(open: boolean): string | null {
  const { active } = useTransfers();
  const [queue, setQueue] = useState<ComputeQueueEntry[]>([]);
  useEffect(() => {
    if (!open) return;
    api.invoke<ComputeQueueEntry[]>('get_compute_queue')
      .then(setQueue)
      .catch((err) => console.error('[UpdateDialog] get_compute_queue failed:', err));
  }, [open]);
  const running = queue.find((e) => e.state === 'running');
  if (running) return `${running.label} is in progress — restarting now will cancel it.`;
  if (active.length > 0) return `${active.length} transfer${active.length > 1 ? 's are' : ' is'} in progress — restarting now will interrupt ${active.length > 1 ? 'them' : 'it'}.`;
  return null;
}

export function UpdateDialog() {
  const { check, whatsNew, dialog, phase, close, install, restart } = useUpdates();
  const open = dialog !== 'closed';
  const busy = useBusyReason(open);
  const [copied, setCopied] = useState(false);
  if (!open) return null;

  const isAvailable = dialog === 'available';
  const version = isAvailable ? check?.latestVersion : whatsNew?.version;
  const notes = isAvailable ? check?.notes : whatsNew?.notes;
  const blogUrl = isAvailable ? check?.blogUrl : whatsNew?.blogUrl;
  if (!version) return null;

  const copyDocker = async () => {
    if (!check?.dockerImage) return;
    try {
      await navigator.clipboard.writeText(`docker pull ${check.dockerImage}`);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (err) {
      console.error('[UpdateDialog] clipboard write failed:', err);
    }
  };

  const downloading = phase.kind === 'downloading';

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50" role="dialog" aria-modal="true">
      <div className="mx-4 flex max-h-[85vh] w-full max-w-2xl flex-col rounded-lg border border-border bg-surface-elevated shadow-xl">
        <div className="flex items-start justify-between border-b border-border px-6 py-4">
          <div>
            <h2 className="text-lg font-semibold text-content">
              {isAvailable ? `Athenaeum v${version} is available` : `What's new in v${version}`}
            </h2>
            {isAvailable && check && (
              <p className="mt-1 text-xs text-content-muted">
                You have v{check.currentVersion}
                {check.pubDate ? ` · released ${formatTimestamp(check.pubDate)}` : ''}
                {check.channel === 'beta' ? ' · beta channel' : ''}
              </p>
            )}
          </div>
          {!downloading && (
            <button onClick={close} className="rounded p-1 text-content-muted hover:bg-surface-hover hover:text-content" aria-label="Close">
              <X size={18} />
            </button>
          )}
        </div>

        <div className="flex-1 overflow-y-auto px-6 py-4">
          {notes ? <ReleaseNotes markdown={notes} /> : <p className="text-sm text-content-muted">No release notes were published for this version.</p>}
          {blogUrl && (
            <button onClick={() => void openUrl(blogUrl)} className="mt-4 flex items-center gap-1 text-sm text-accent hover:underline">
              Read the full post <ExternalLink size={13} />
            </button>
          )}
        </div>

        <div className="space-y-3 border-t border-border px-6 py-4">
          {isAvailable && phase.kind === 'downloading' && (
            <div>
              <div className="mb-1 flex justify-between text-xs text-content-muted">
                <span>Downloading v{version}…</span>
                <span>
                  {formatBytes(phase.downloaded)}
                  {phase.total ? ` / ${formatBytes(phase.total)} · ${Math.floor((phase.downloaded / phase.total) * 100)} %` : ''}
                </span>
              </div>
              <div className="h-2 w-full overflow-hidden rounded bg-surface">
                <div
                  className="h-full bg-accent transition-all"
                  style={{ width: phase.total ? `${Math.min(100, (phase.downloaded / phase.total) * 100)}%` : '30%' }}
                />
              </div>
              {isWindows && <p className="mt-2 text-xs text-content-muted">Athenaeum will close when the installer starts and reopen when it is done.</p>}
            </div>
          )}
          {isAvailable && phase.kind === 'failed' && (
            <div className="flex items-start gap-2 rounded-lg border border-error/40 bg-error/10 p-3 text-sm text-error">
              <AlertTriangle size={15} className="mt-0.5 flex-shrink-0" />
              <span>{phase.message}</span>
            </div>
          )}
          {isAvailable && phase.kind === 'ready' && (
            <div className="flex items-center gap-2 rounded-lg border border-success/40 bg-success/10 p-3 text-sm text-success">
              <CheckCircle2 size={15} /> v{phase.version} is installed — restart to apply.
            </div>
          )}
          {isAvailable && busy && (phase.kind === 'ready' || (isWindows && phase.kind !== 'downloading')) && (
            <div className="flex items-start gap-2 text-sm text-warning">
              <AlertTriangle size={15} className="mt-0.5 flex-shrink-0" />
              <span>{busy}</span>
            </div>
          )}

          <div className="flex flex-wrap items-center justify-end gap-2">
            {!isAvailable && (
              <button onClick={close} className="rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">Close</button>
            )}

            {isAvailable && check && !check.platformSupported && (
              <>
                {check.dockerImage && (
                  <div className="mr-auto flex items-center gap-2">
                    <code className="rounded bg-surface px-2 py-1 font-mono text-xs text-content">docker pull {check.dockerImage}</code>
                    <button onClick={copyDocker} className="rounded p-1 text-content-muted hover:bg-surface-hover hover:text-content" title="Copy">
                      {copied ? <CheckCircle2 size={15} className="text-success" /> : <Copy size={15} />}
                    </button>
                  </div>
                )}
                <button onClick={() => void openUrl(check.downloadPageUrl)} className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">
                  Open download page <ExternalLink size={13} />
                </button>
              </>
            )}

            {isAvailable && check && check.platformSupported && isTauri && (
              <>
                {(phase.kind === 'idle' || phase.kind === 'failed') && (
                  <>
                    <button onClick={close} className="rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">Later</button>
                    {phase.kind === 'failed' && (
                      <button onClick={() => void openUrl(check.downloadPageUrl)} className="flex items-center gap-2 rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">
                        Open download page <ExternalLink size={13} />
                      </button>
                    )}
                    <button onClick={() => void install()} className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">
                      {phase.kind === 'failed' ? <RefreshCw size={15} /> : <Download size={15} />}
                      {phase.kind === 'failed' ? 'Retry' : 'Download and install'}
                    </button>
                  </>
                )}
                {phase.kind === 'ready' && (
                  <>
                    <button onClick={close} className="rounded-lg bg-surface-hover px-4 py-2 text-sm text-content hover:brightness-110">Later</button>
                    <button onClick={() => void restart()} className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm hover:bg-accent-hover">
                      <RefreshCw size={15} /> Restart now
                    </button>
                  </>
                )}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
```

`z-[70]` sits above the notification panel's `z-[60]`. `ComputeJobState`'s running value: confirm the generated union in `src/types/models.ts` spells it `'running'` (`grep -n "ComputeJobState" src/types/models.ts`); adjust the literal if it is `'Running'`.

- [ ] **Step 5: Launch sequence — `useAutoUpdateCheck`**

Replace `src/hooks/useAutoUpdateCheck.ts`:

```ts
import { useEffect, useRef } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import { useUpdates } from '../contexts/UpdatesContext';
import type { WhatsNew } from '../types/models';

/**
 * Launch sequence (spec §5.3), both hosts:
 *  1. `get_whats_new` — once per version; opens the What's-new dialog.
 *  2. unless `updates.auto_check` is off: `check_for_updates`; a newer
 *     version raises a toast + bell entry linking to /about?update.
 * Failures are console-only — offline at launch is normal, never a toast.
 */
export function useAutoUpdateCheck() {
  const { notify } = useNotifications();
  const { runCheck, openWhatsNew } = useUpdates();
  const ran = useRef(false);

  useEffect(() => {
    if (ran.current) return; // React StrictMode double-invoke guard
    ran.current = true;

    (async () => {
      try {
        const w = await api.invoke<WhatsNew | null>('get_whats_new');
        if (w) openWhatsNew(w);
      } catch (err) {
        console.error('get_whats_new:', err);
      }
      try {
        const enabled = await api.invoke<string>('get_setting', { key: 'updates.auto_check', defaultValue: 'true' });
        if (enabled.toLowerCase() !== 'true') return;
        const info = await runCheck();
        if (info?.isUpdateAvailable) {
          notify({
            title: `Update available: v${info.latestVersion}`,
            detail: info.platformSupported
              ? `You have v${info.currentVersion}. Click to read the notes and install.`
              : `You have v${info.currentVersion}. Click to read the notes.`,
            tone: 'success',
            kind: 'update',
            link: '/about?update',
            dedupeKey: `update-${info.latestVersion}`,
          });
        }
      } catch (err) {
        console.error('auto update check:', err);
      }
    })();
  }, [notify, runCheck, openWhatsNew]);
}
```

- [ ] **Step 6: Layout — provider and dialog at root**

`src/components/Layout.tsx`: import `UpdatesProvider` from `../contexts/UpdatesContext` and `UpdateDialog` from `./updates/UpdateDialog`. Wrap the tree one level inside `TransfersProvider` (the dialog reads `useTransfers`, the hook reads `useNotifications`):

```tsx
    <TransfersProvider>
    <UpdatesProvider>
    <ScanProgressProvider>
      ...
```

and close `</UpdatesProvider>` right before `</TransfersProvider>`. Next to `<NotificationPanel />` add `<UpdateDialog />`. `<AutoUpdateCheck />` stays where it is (it is inside both providers).

- [ ] **Step 7: About page**

In `src/pages/About.tsx` replace `UpdateSection` entirely and its usage:

```tsx
import { useSearchParams } from 'react-router-dom';
import { useUpdates } from '../contexts/UpdatesContext';

function UpdateSection() {
  const { check, checking, checkError, runCheck, openAvailable, openReleaseNotes } = useUpdates();
  const [params, setParams] = useSearchParams();

  // Arriving from the update toast (/about?update): check, then open the dialog.
  useEffect(() => {
    if (!params.has('update')) return;
    setParams({}, { replace: true });
    runCheck().then((r) => { if (r?.isUpdateAvailable) openAvailable(); });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <section className="rounded-lg bg-surface-elevated/60 p-6 space-y-4">
      <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">Updates</h2>
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <span className="text-content-secondary">Athenaeum v{__APP_VERSION__}</span>
        <button onClick={() => void runCheck()} disabled={checking} className="flex items-center gap-2 px-3 py-1.5 bg-accent hover:bg-accent-hover rounded-lg transition disabled:opacity-50 text-sm">
          <RefreshCw size={14} className={checking ? 'animate-spin' : ''} />
          {checking ? 'Checking…' : 'Check for updates'}
        </button>
        <button onClick={() => void openReleaseNotes()} className="text-accent hover:underline">View release notes</button>
      </div>
      {checkError && (
        <div className="flex items-start gap-2 p-3 bg-error/10 border border-error/40 rounded-lg text-sm text-error">
          <AlertCircle size={15} className="flex-shrink-0 mt-0.5" />{checkError}
        </div>
      )}
      {check && !check.isUpdateAvailable && (
        <div className="flex items-center gap-2 p-3 bg-success/10 border border-success/40 rounded-lg text-sm text-success">
          <CheckCircle2 size={15} /> You're up to date (v{check.currentVersion}).
        </div>
      )}
      {check && check.isUpdateAvailable && (
        <div className="flex items-center justify-between p-3 bg-accent/10 border border-accent/40 rounded-lg text-sm">
          <span className="flex items-center gap-2 font-semibold text-accent"><Info size={15} /> Version {check.latestVersion} is available</span>
          <button onClick={openAvailable} className="flex items-center gap-2 px-3 py-1.5 bg-accent hover:bg-accent-hover rounded-lg text-sm">
            <Download size={14} /> {check.platformSupported ? 'Install' : 'Details'}
          </button>
        </div>
      )}
    </section>
  );
}
```

Replace `{isTauri && <UpdateSection />}` with `<UpdateSection />` (web has the check now). `__APP_VERSION__`: if the project does not already define it (`grep -rn "__APP_VERSION__" vite.config.ts src/vite-env.d.ts`), show `check?.currentVersion ?? '…'` instead and drop the constant — do NOT add a Vite define for this task. Remove the now-unused `UpdateInfo` import and, if `isTauri` is no longer used in the file, its import too.

- [ ] **Step 8: Settings wording**

`src/pages/Settings.tsx`: remove the `{isTauri && (` / `)}` wrapper around the Updates section (keep the section) and change the auto-check description to:

> When enabled, Athenaeum checks for a newer version each time it starts and shows a notification if one is available — on the desktop app and in the web build alike. Disable to only check manually from the About page.

Keep `isTauri` imported if the file still uses it elsewhere (it does, for the data-locations section).

- [ ] **Step 9: Type-check, build, run**

Run: `npx tsc --noEmit && npm run build:web`
Expected: clean.

Run `npm run tauri dev`: About shows the version line and the two actions; **Check for updates** answers "up to date" (the manifest does not exist yet → the check fails with "HTTP 404" — expected until Task 7 publishes one; the error box shows it). **View release notes** opens the dialog in `whatsNew` mode with the current `RELEASE_NOTES.md`. Set `updates.last_seen_version` to `0.0.1` (`sqlite3 <dev db> "UPDATE settings SET value='0.0.1' WHERE key='updates.last_seen_version'"`), relaunch: the What's-new dialog opens once.

- [ ] **Step 10: Commit**

```bash
git add package.json package-lock.json src/
git commit -m "feat(updates): UpdateDialog, UpdatesContext, launch sequence, About and Settings

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 7: Pipeline — updater artifacts: names, build jobs (F5.1 reversed), deploy, the release key

**Files:**
- Modify: `.gitlab/ci/scripts/artifact_names.sh` (`updater_artifacts()`)
- Modify: `.gitlab/ci/scripts/test/run_tests.sh` (artifact_names section)
- Modify: `.gitlab-ci.yml` (`build:macos`, `build:windows`, `build:linux`, `deploy`)
- Modify: `crates/athenaeum-tauri/tauri.conf.json` (`bundle.createUpdaterArtifacts: true`; the RELEASE public key)

**Interfaces:**
- Produces: `updater_artifacts()` lines `product os arch ext variant subdir filename plugin_key`; `deploy` publishes `builds/<tag>/<subdir>/<filename>` and `<filename>.sig` for every line and exports `release_stage/**/*.sig` as a job artifact (Task 8 reads them).

- [ ] **Step 1: The release signing key (OWNER step — blocks nothing else in this task)**

The owner runs on their machine, once:

```bash
npx tauri signer generate -w ~/.tauri/athenaeum-release.key
```

and stores `athenaeum-release.key` + the password in 1Password (item "Athenaeum updater signing key — CANNOT BE ROTATED"), then sets GitLab project variables `TAURI_SIGNING_PRIVATE_KEY` (the file's content) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, both **Protected + Masked**. The `.pub` content replaces the dev key in `tauri.conf.json` `plugins.updater.pubkey`. Until the owner has done this, the executor keeps the dev key in the file and leaves this step's checkbox open; **Task 12 (acceptance) refuses to tag while the dev key is committed** (its Step 1 checks).

- [ ] **Step 2: Inventory — failing test first**

In `.gitlab/ci/scripts/test/run_tests.sh`, in the `-- artifact_names.sh --` section (after the existing asserts), add:

```bash
ua=$(VERSION=0.7.0 updater_artifacts)
assert_eq "updater: 5 lines" "5" "$(printf '%s\n' "$ua" | wc -l | tr -d ' ')"
assert_file_has_line "updater: macos arm64 app.tar.gz" "athenaeum macos arm64 app.tar.gz - macos athenaeum-0.7.0-macos-arm64.app.tar.gz darwin-aarch64" <(printf '%s\n' "$ua")
assert_file_has_line "updater: macos x64 app.tar.gz" "athenaeum macos x64 app.tar.gz - macos athenaeum-0.7.0-macos-x64.app.tar.gz darwin-x86_64" <(printf '%s\n' "$ua")
assert_file_has_line "updater: windows nsis" "athenaeum windows x64 exe setup windows athenaeum-0.7.0-windows-x64-setup.exe windows-x86_64-nsis" <(printf '%s\n' "$ua")
assert_file_has_line "updater: windows msi" "athenaeum windows x64 msi - windows athenaeum-0.7.0-windows-x64.msi windows-x86_64-msi" <(printf '%s\n' "$ua")
assert_file_has_line "updater: linux appimage" "athenaeum linux x64 AppImage - linux athenaeum-0.7.0-linux-x64.AppImage linux-x86_64" <(printf '%s\n' "$ua")
assert_not_contains "updater: no plain windows key" " windows-x86_64$" "$ua"
```

`assert_file_has_line` takes a file path; process substitution `<(...)` works because `run_tests.sh` is bash. Run: `.gitlab/ci/scripts/test/run_tests.sh` → the new asserts FAIL (`updater_artifacts: command not found`).

- [ ] **Step 3: Implement `updater_artifacts()`**

Append to `.gitlab/ci/scripts/artifact_names.sh` after `all_release_artifacts`:

```bash
# updater_artifacts — the files the in-app updater downloads, each published
# beside its `<filename>.sig`. One line each:
#   product os arch ext variant subdir filename plugin_key
# plugin_key is tauri-plugin-updater's manifest key ({os}-{arch}[-{installer}]).
# The two Windows installers get installer-specific keys and there is NO plain
# `windows-x86_64` key on purpose: the plugin falls back to the plain key for
# a build of unknown installer type, which must not be handed the NSIS exe.
# Not on the download page — these are not for people.
updater_artifacts() {
  : "${VERSION:?VERSION must be set (no leading v)}"
  local spec product os arch ext variant subdir key
  for spec in \
    "athenaeum macos arm64 app.tar.gz - macos darwin-aarch64" \
    "athenaeum macos x64 app.tar.gz - macos darwin-x86_64" \
    "athenaeum windows x64 exe setup windows windows-x86_64-nsis" \
    "athenaeum windows x64 msi - windows windows-x86_64-msi" \
    "athenaeum linux x64 AppImage - linux linux-x86_64"; do
    read -r product os arch ext variant subdir key <<< "$spec"
    local v="" ; [ "$variant" != "-" ] && v="$variant"
    printf '%s %s %s %s %s %s %s %s\n' "$product" "$os" "$arch" "$ext" "$variant" "$subdir" \
      "$(artifact_name "$product" "$os" "$arch" "$ext" "$v")" "$key"
  done
}
```

Run the tests: the new asserts pass, the count of passes grows by 7.

- [ ] **Step 4: `tauri.conf.json` — updater artifacts on**

In `"bundle"` add `"createUpdaterArtifacts": true,` right after `"active": true,`. (The public key: Step 1.)

- [ ] **Step 5: Build jobs**

`.gitlab-ci.yml`:

`build:macos` — replace the two `env -u APPLE_API_ISSUER -u APPLE_API_KEY -u APPLE_API_KEY_PATH npm run tauri build -- --target …` lines with plain

```yaml
    # D4 (in-app updates spec): tauri build notarizes + staples the .app
    # itself again, so the updater's .app.tar.gz carries a stapled bundle.
    # Ruling F5.1 (DMG-only notarization) is reversed by that spec; the DMG
    # loop below still notarizes the DMG a first-time user downloads.
    - npm run tauri build -- --target aarch64-apple-darwin
    - npm run tauri build -- --target x86_64-apple-darwin
```

and delete the paragraph of comment above them that explains the withholding (the one beginning `# Tauri SIGNS the .app (APPLE_SIGNING_IDENTITY) but does NOT notarize it:`). Keep the `unset APPLE_CERTIFICATE …` line and its comment — that one is about the keychain, not notarization. Add to `artifacts.paths`:

```yaml
      - target/aarch64-apple-darwin/release/bundle/macos/*.app.tar.gz
      - target/aarch64-apple-darwin/release/bundle/macos/*.app.tar.gz.sig
      - target/x86_64-apple-darwin/release/bundle/macos/*.app.tar.gz
      - target/x86_64-apple-darwin/release/bundle/macos/*.app.tar.gz.sig
```

All three build jobs get a loud check that the signing key reached the job (protected variables are absent on an unprotected tag — the bundler would then fail with a less clear message deep in the log). `build:macos` and `build:linux`, first line of `script`:

```yaml
    - '[ -n "$TAURI_SIGNING_PRIVATE_KEY" ] || { echo "FATAL: TAURI_SIGNING_PRIVATE_KEY is empty — is the tag a Protected tag?"; exit 1; }'
```

`build:windows` (PowerShell):

```yaml
    - if (-not $env:TAURI_SIGNING_PRIVATE_KEY) { Write-Error "FATAL: TAURI_SIGNING_PRIVATE_KEY is empty - is the tag a Protected tag?"; exit 1 }
```

`build:windows` artifacts add `target/release/bundle/msi/*.msi.sig` and `target/release/bundle/nsis/*.exe.sig`; `build:linux` adds `target/release/bundle/appimage/*.AppImage.sig`.

- [ ] **Step 6: `deploy`**

After the existing `cp` lines add:

```yaml
    # Updater artifacts (in-app updates spec §3.4/§3.5): the macOS app
    # archives and every signature, beside the installers they belong to.
    - cp target/aarch64-apple-darwin/release/bundle/macos/*.app.tar.gz     "release_stage/macos/$(artifact_name athenaeum macos arm64 app.tar.gz)"
    - cp target/aarch64-apple-darwin/release/bundle/macos/*.app.tar.gz.sig "release_stage/macos/$(artifact_name athenaeum macos arm64 app.tar.gz).sig"
    - cp target/x86_64-apple-darwin/release/bundle/macos/*.app.tar.gz      "release_stage/macos/$(artifact_name athenaeum macos x64 app.tar.gz)"
    - cp target/x86_64-apple-darwin/release/bundle/macos/*.app.tar.gz.sig  "release_stage/macos/$(artifact_name athenaeum macos x64 app.tar.gz).sig"
    - cp target/release/bundle/nsis/*.exe.sig                              "release_stage/windows/$(artifact_name athenaeum windows x64 exe setup).sig"
    - cp target/release/bundle/msi/*.msi.sig                               "release_stage/windows/$(artifact_name athenaeum windows x64 msi).sig"
    - cp target/release/bundle/appimage/*.AppImage.sig                     "release_stage/linux/$(artifact_name athenaeum linux x64 AppImage).sig"
    - |
      while read -r product os arch ext variant subdir filename key; do
        [ -s "release_stage/$subdir/$filename" ]     || { echo "ERROR: missing release_stage/$subdir/$filename"; exit 1; }
        [ -s "release_stage/$subdir/$filename.sig" ] || { echo "ERROR: missing release_stage/$subdir/$filename.sig"; exit 1; }
      done < <(updater_artifacts)
```

(place the `while` block right after the existing inventory check). The existing `for d in windows macos linux perseus; do ${SCP_CMD} release_stage/$d/* …` already ships them. Add to the job:

```yaml
  artifacts:
    name: "release-sigs-${CI_COMMIT_TAG}"
    paths:
      - release_stage/**/*.sig
    expire_in: 30 days
```

- [ ] **Step 7: Gate and commit**

Run: `.gitlab/ci/scripts/test/run_tests.sh` (green) and `python3 -c "import yaml,sys; yaml.safe_load(open('.gitlab-ci.yml'))"` (if PyYAML is present; else `npx --yes yaml-lint .gitlab-ci.yml`).

```bash
git add .gitlab-ci.yml .gitlab/ci/scripts/artifact_names.sh .gitlab/ci/scripts/test/run_tests.sh crates/athenaeum-tauri/tauri.conf.json
git commit -m "ci(updates): signed updater artifacts — inventory, build jobs notarize the .app again (F5.1 reversed), deploy

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 8: Pipeline — the manifest generator and `publish:updater-manifest`

**Files:**
- Create: `.gitlab/ci/scripts/gen_updater_manifest.sh`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh` (new section)
- Modify: `.gitlab-ci.yml` (new job in `publish`)

**Interfaces:**
- Consumes: `updater_artifacts()` (Task 7), `release_stage/<subdir>/<filename>.sig` from `deploy`'s artifact.
- Produces: `gen_updater_manifest.sh` — env `CI_COMMIT_TAG` (required), `STAGE_DIR` (default `release_stage`), `NOTES_PATH` (default `RELEASE_NOTES.md`), `BUILDS_BASE_URL` (default `https://artfrom.space/builds`), `PUB_DATE` (RFC 3339, default now UTC); writes the manifest JSON to stdout; exit 1 with `ERROR: missing signature <path>` when any inventory row lacks a non-empty `.sig`. Server file `updates/<tag>.json`.

- [ ] **Step 1: Failing tests**

Append to `run_tests.sh` before the final `echo` / `Passed:` lines:

```bash
echo
echo "-- gen_updater_manifest.sh --"

UM_STAGE=$(mktemp -d -t updater_stage.XXXXXX)
mkdir -p "$UM_STAGE/macos" "$UM_STAGE/windows" "$UM_STAGE/linux"
printf 'SIGMACARM\n' > "$UM_STAGE/macos/athenaeum-0.7.0-macos-arm64.app.tar.gz.sig"
printf 'SIGMACX64'   > "$UM_STAGE/macos/athenaeum-0.7.0-macos-x64.app.tar.gz.sig"
printf 'SIGNSIS'     > "$UM_STAGE/windows/athenaeum-0.7.0-windows-x64-setup.exe.sig"
printf 'SIGMSI'      > "$UM_STAGE/windows/athenaeum-0.7.0-windows-x64.msi.sig"
printf 'SIGAPPIMG'   > "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig"
UM_OUT=$(mktemp -t updater_manifest.XXXXXX)
rc=0
CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" PUB_DATE=2026-10-01T12:00:00Z \
  "$HELPERS_DIR/gen_updater_manifest.sh" > "$UM_OUT" 2>/dev/null || rc=$?
assert_eq "manifest: exits 0" "0" "$rc"
um_field() { python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$UM_OUT" "$1"; }
assert_eq "manifest: version without v" "0.7.0" "$(um_field 'd["version"]')"
assert_eq "manifest: pub_date passed through" "2026-10-01T12:00:00Z" "$(um_field 'd["pub_date"]')"
assert_eq "manifest: 5 platforms" "5" "$(um_field 'len(d["platforms"])')"
assert_eq "manifest: arm64 url" "https://artfrom.space/builds/v0.7.0/macos/athenaeum-0.7.0-macos-arm64.app.tar.gz" "$(um_field 'd["platforms"]["darwin-aarch64"]["url"]')"
assert_eq "manifest: arm64 signature trimmed" "SIGMACARM" "$(um_field 'd["platforms"]["darwin-aarch64"]["signature"]')"
assert_eq "manifest: nsis key" "https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64-setup.exe" "$(um_field 'd["platforms"]["windows-x86_64-nsis"]["url"]')"
assert_eq "manifest: msi key" "SIGMSI" "$(um_field 'd["platforms"]["windows-x86_64-msi"]["signature"]')"
assert_eq "manifest: no plain windows key" "False" "$(um_field '"windows-x86_64" in d["platforms"]')"
assert_eq "manifest: notes are the whole file" "same" "$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print("same" if d["notes"]==open(sys.argv[2],encoding="utf-8").read() else "differs")' "$UM_OUT" "$FIXTURES_DIR/release_notes_sample.md")"
assert_contains "manifest: notes keep a quoted tagline" 'A \"quoted\" tagline' "$(cat "$UM_OUT")"

rc=0
out=$(CI_COMMIT_TAG=v0.7.0-beta.2 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: beta tag exits 1 when its files are missing (stage holds 0.7.0)" "1" "$rc"
assert_contains "manifest: names the missing signature" "ERROR: missing signature $UM_STAGE/macos/athenaeum-0.7.0-beta.2-macos-arm64.app.tar.gz.sig" "$out"

rm -f "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig"
rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: one missing sig exits 1" "1" "$rc"
assert_contains "manifest: names it" "athenaeum-0.7.0-linux-x64.AppImage.sig" "$out"
: > "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig"
rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: an EMPTY sig exits 1" "1" "$rc"

rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" PUB_DATE="yesterday" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: a non-RFC3339 PUB_DATE exits 1" "1" "$rc"
rm -rf "$UM_STAGE" "$UM_OUT"
```

Run: `.gitlab/ci/scripts/test/run_tests.sh` → the section fails (script missing).

- [ ] **Step 2: The generator**

`.gitlab/ci/scripts/gen_updater_manifest.sh`:

```bash
#!/usr/bin/env bash
# The in-app updater's manifest for one tag (spec §3.6), to stdout.
# Env: CI_COMMIT_TAG (required); STAGE_DIR (default release_stage) holding
# <subdir>/<filename>.sig for every updater_artifacts row; NOTES_PATH
# (default RELEASE_NOTES.md); BUILDS_BASE_URL; PUB_DATE (RFC 3339, default now).
# A missing or empty .sig is exit 1 — a platform is never omitted: every build
# job is required, so a missing signature is a pipeline defect, and a release
# the updater cannot serve on one OS is not a release.
set -euo pipefail

: "${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/artifact_names.sh"
VERSION="${CI_COMMIT_TAG#v}"; export VERSION
STAGE_DIR="${STAGE_DIR:-release_stage}"
NOTES_PATH="${NOTES_PATH:-RELEASE_NOTES.md}"
BUILDS_BASE_URL="${BUILDS_BASE_URL:-https://artfrom.space/builds}"
PUB_DATE="${PUB_DATE:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"

[ -s "$NOTES_PATH" ] || { echo "ERROR: release notes not found at $NOTES_PATH" >&2; exit 1; }

# One line per platform: key<TAB>url<TAB>sig_path — python assembles the JSON
# (never printf-built JSON: notes carry quotes, backslashes and newlines).
ROWS=$(mktemp -t updater_rows.XXXXXX); trap 'rm -f "$ROWS"' EXIT
fail=0
while read -r product os arch ext variant subdir filename key; do
  sig="$STAGE_DIR/$subdir/$filename.sig"
  if [ ! -s "$sig" ]; then echo "ERROR: missing signature $sig" >&2; fail=1; continue; fi
  printf '%s\t%s\t%s\n' "$key" "${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/${subdir}/${filename}" "$sig" >> "$ROWS"
done < <(updater_artifacts)
[ "$fail" -eq 0 ] || exit 1

python3 - "$VERSION" "$PUB_DATE" "$NOTES_PATH" "$ROWS" <<'PY'
import datetime, json, sys
version, pub_date, notes_path, rows_path = sys.argv[1:5]
try:
    datetime.datetime.strptime(pub_date, "%Y-%m-%dT%H:%M:%SZ")
except ValueError:
    sys.exit("ERROR: PUB_DATE must be RFC 3339 UTC like 2026-10-01T12:00:00Z, got %r" % pub_date)
platforms = {}
with open(rows_path, encoding="utf-8") as f:
    for line in f:
        key, url, sig_path = line.rstrip("\n").split("\t")
        with open(sig_path, encoding="utf-8") as s:
            signature = s.read().strip()
        if not signature:
            sys.exit("ERROR: missing signature %s" % sig_path)
        platforms[key] = {"url": url, "signature": signature}
with open(notes_path, encoding="utf-8") as f:
    notes = f.read()
json.dump({"version": version, "notes": notes, "pub_date": pub_date, "platforms": platforms},
          sys.stdout, indent=2, ensure_ascii=False)
sys.stdout.write("\n")
PY
```

`chmod +x .gitlab/ci/scripts/gen_updater_manifest.sh`. Run the tests → the section passes.

- [ ] **Step 3: The job**

`.gitlab-ci.yml`, in the `publish` stage next to `docs:publish`:

```yaml
# --- Updater manifest (tags only) ---
# Writes the FROZEN per-tag manifest updates/<tag>.json (in-app updates spec
# §3.6). The channel file (latest.json / latest-beta.json) is NOT touched
# here: it moves in publish:updater-channel, stage announce, after verify —
# the same rule as builds/latest. verify:release fetches this file back and
# checks every URL and signature it names.
publish:updater-manifest:
  stage: publish
  extends: .ssh_deploy_setup
  tags: [linux]
  only: [tags]
  needs:
    - job: deploy
      artifacts: true
  variables:
    DEPLOY_PORT: "40022"
    UPDATES_PATH: "/var/www/artfrom.space/updates"
  script:
    - CI_COMMIT_TAG="${CI_COMMIT_TAG}" STAGE_DIR=release_stage .gitlab/ci/scripts/gen_updater_manifest.sh > "updater-${CI_COMMIT_TAG}.json"
    - python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "updater-${CI_COMMIT_TAG}.json"
    - ${SSH_CMD} ${DEPLOY_USER}@${DEPLOY_HOST} "mkdir -p ${UPDATES_PATH}"
    - ${SCP_CMD} "updater-${CI_COMMIT_TAG}.json" ${DEPLOY_USER}@${DEPLOY_HOST}:${UPDATES_PATH}/${CI_COMMIT_TAG}.json
  artifacts:
    paths:
      - updater-*.json
    expire_in: 30 days
```

`verify:release`'s `needs` gains `- job: publish:updater-manifest` with `artifacts: false` (Task 9 uses it).

- [ ] **Step 4: Gate and commit**

Run: `.gitlab/ci/scripts/test/run_tests.sh` → green.

```bash
git add .gitlab-ci.yml .gitlab/ci/scripts/gen_updater_manifest.sh .gitlab/ci/scripts/test/run_tests.sh
git commit -m "ci(updates): the updater manifest generator and publish:updater-manifest

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 9: Pipeline — `verify:release` checks the manifest and the signatures; `publish:updater-channel`

**Files:**
- Modify: `.gitlab/ci/scripts/verify_release.sh` (section 1b)
- Modify: `.gitlab/ci/scripts/test/fixtures/mock_bin/curl` (`updates/` arm; downloads)
- Create: `.gitlab/ci/scripts/test/fixtures/mock_bin/rsign` (mock verifier)
- Create: `.gitlab/ci/scripts/test/fixtures/updater_manifest_good.json`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh` (verify section)
- Modify: `.gitlab-ci.yml` (`verify:release` installs `rsign2`; new `publish:updater-channel`; `publish_version` comment)

**Interfaces:**
- Consumes: `updates/<tag>.json` (Task 8), `updater_artifacts()` (Task 7), `plugins.updater.pubkey` from `tauri.conf.json`.
- Produces: verify env `UPDATES_BASE_URL` (default `https://artfrom.space/updates`), `TAURI_CONF_PATH` (default `crates/athenaeum-tauri/tauri.conf.json`), `SKIP_UPDATER_CHECK=1` (tests of unrelated sections only); messages `ok: updater manifest …`, `ERROR: updater manifest …`.

- [ ] **Step 1: Mock `rsign` and the manifest fixture**

`.gitlab/ci/scripts/test/fixtures/mock_bin/rsign`:

```bash
#!/usr/bin/env bash
# Mock rsign2 for verify_release.sh tests. `rsign verify -p PUB -x SIG FILE`
# exits MOCK_RSIGN_RC (default 0); MOCK_RSIGN_FAIL_FILES (space-separated
# basenames) exit 1 regardless.
set -euo pipefail
file="${@: -1}"
for f in ${MOCK_RSIGN_FAIL_FILES:-}; do [ "$(basename "$file")" = "$f" ] && { echo "Signature verification failed" >&2; exit 1; }; done
exit "${MOCK_RSIGN_RC:-0}"
```

`chmod +x` it. Fixture `updater_manifest_good.json` — a manifest for `v0.7.0` with the five keys, URLs exactly as `updater_artifacts` spells them under `https://artfrom.space/builds/v0.7.0/…`, each `signature` the base64 of a two-line minisign text (any base64 string works for the mock, e.g. `dW50cnVzdGVkIGNvbW1lbnQ6IHRlc3QKUldUdGVzdA==`), `notes` `"n"`, `pub_date` `"2026-10-01T12:00:00Z"`.

Extend `mock_bin/curl`: in the `*artfrom.space/builds/*` arm, when NOT `--head`, write `MOCK_CURL_ARTIFACT_LENGTH` (default 5000000, but cap the mock at 4096) bytes of `x` to `$output_file` — a download the verifier then hands to `rsign` — unless the basename is in `MOCK_CURL_MISSING_ARTIFACTS` (then 404 and empty). Add a new arm ABOVE it:

```bash
  *artfrom.space/updates/*)
    # verify_release.sh's manifest fetch. MOCK_CURL_MANIFEST_HTTP (200),
    # MOCK_CURL_MANIFEST_FILE (body).
    http_code="${MOCK_CURL_MANIFEST_HTTP:-200}"
    if [ "$is_head" -eq 1 ]; then printf 'HTTP/2 %s\n' "$http_code" > "$output_file"
    elif [ -n "${MOCK_CURL_MANIFEST_FILE:-}" ] && [ -f "$MOCK_CURL_MANIFEST_FILE" ]; then cp "$MOCK_CURL_MANIFEST_FILE" "$output_file"
    else printf '{}' > "$output_file"; fi
    ;;
```

- [ ] **Step 2: Failing tests**

In `run_tests.sh`'s `-- verify_release.sh --` section: the existing `common=(...)` gains `TAURI_CONF_PATH="$FIXTURES_DIR/version_tree/crates/athenaeum-tauri/tauri.conf.json"` **only if that fixture file carries a `plugins.updater.pubkey`** — it does not today, so instead create `$FIXTURES_DIR/updater_tauri.conf.json` with `{"version":"0.7.0","plugins":{"updater":{"pubkey":"dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDAKUldUdGVzdA=="}}}` and pass `TAURI_CONF_PATH="$FIXTURES_DIR/updater_tauri.conf.json"` plus `MOCK_CURL_MANIFEST_FILE="$FIXTURES_DIR/updater_manifest_good.json"` in `common`. The existing asserts must keep passing (the good manifest + mock rsign are green by default). Add:

```bash
assert_contains "verify: updater manifest ok" "ok: updater manifest v0.7.0 — 5 platforms, every URL present, every signature verified" "$out"

rc=0
UM_BAD=$(mktemp -t um_bad.XXXXXX)
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); d["version"]="0.6.9"; json.dump(d,open(sys.argv[2],"w"))' "$FIXTURES_DIR/updater_manifest_good.json" "$UM_BAD"
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_MANIFEST_FILE="$UM_BAD" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: wrong manifest version exits 1" "1" "$rc"
assert_contains "verify: names the version mismatch" "ERROR: updater manifest version is 0.6.9, tag v0.7.0 expects 0.7.0" "$out"

rc=0
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); del d["platforms"]["linux-x86_64"]; json.dump(d,open(sys.argv[2],"w"))' "$FIXTURES_DIR/updater_manifest_good.json" "$UM_BAD"
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_MANIFEST_FILE="$UM_BAD" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: missing platform key exits 1" "1" "$rc"
assert_contains "verify: names the missing key" "ERROR: updater manifest has no entry for linux-x86_64" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_RSIGN_FAIL_FILES="athenaeum-0.7.0-linux-x64.AppImage" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: a bad signature exits 1" "1" "$rc"
assert_contains "verify: names the file whose signature failed" "ERROR: updater signature does not verify for https://artfrom.space/builds/v0.7.0/linux/athenaeum-0.7.0-linux-x64.AppImage" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_MANIFEST_HTTP=404 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: no manifest exits 1" "1" "$rc"
assert_contains "verify: names the manifest URL" "ERROR: HTTP 404 for https://artfrom.space/updates/v0.7.0.json" "$out"
rm -f "$UM_BAD"
```

Run the tests → these FAIL.

- [ ] **Step 3: Section 1b in `verify_release.sh`**

Insert after section 1 (before `# --- 2. Docker Hub`):

```bash
# --- 1b. the updater manifest: version, every platform, every URL, every signature
UPDATES_BASE_URL="${UPDATES_BASE_URL:-https://artfrom.space/updates}"
TAURI_CONF_PATH="${TAURI_CONF_PATH:-crates/athenaeum-tauri/tauri.conf.json}"
if [ "${SKIP_UPDATER_CHECK:-0}" = "1" ]; then
  echo "skipped: updater manifest check (SKIP_UPDATER_CHECK=1)"
else
  murl="${UPDATES_BASE_URL}/${CI_COMMIT_TAG}.json"
  MAN=$(mktemp -t verify_manifest.XXXXXX); PUB=$(mktemp -t verify_pub.XXXXXX); DL=$(mktemp -t verify_dl.XXXXXX); SIG=$(mktemp -t verify_sig.XXXXXX)
  trap 'rm -f "$HDR" "$BODY" "$MAN" "$PUB" "$DL" "$SIG"' EXIT
  code=$(curl --silent --show-error --output "$MAN" --write-out '%{http_code}' --max-time 60 "$murl" || echo 000)
  if [ "$code" != "200" ]; then
    echo "ERROR: HTTP $code for $murl" >&2; fail=1
  else
    mver=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$MAN" 2>/dev/null || true)
    if [ -z "$mver" ]; then echo "ERROR: updater manifest at $murl is not the expected JSON" >&2; fail=1
    elif [ "$mver" != "$VERSION" ]; then echo "ERROR: updater manifest version is $mver, tag $CI_COMMIT_TAG expects $VERSION" >&2; fail=1
    else
      # The committed public key (base64 of the minisign key file) → rsign's key file.
      python3 -c 'import base64,json,sys; sys.stdout.write(base64.b64decode(json.load(open(sys.argv[1]))["plugins"]["updater"]["pubkey"]).decode())' "$TAURI_CONF_PATH" > "$PUB"
      command -v rsign >/dev/null || { echo "ERROR: rsign (rsign2) is not installed on this runner — cargo install rsign2 --locked" >&2; exit 1; }
      mcount=0
      while read -r product os arch ext variant subdir filename key; do
        expected="${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/${subdir}/${filename}"
        entry=$(python3 -c 'import json,sys; p=json.load(open(sys.argv[1]))["platforms"].get(sys.argv[2]); print(p["url"]+"\t"+p["signature"] if p else "")' "$MAN" "$key")
        if [ -z "$entry" ]; then echo "ERROR: updater manifest has no entry for $key" >&2; fail=1; continue; fi
        IFS=$'\t' read -r url signature <<< "$entry"
        if [ "$url" != "$expected" ]; then echo "ERROR: updater manifest $key points at $url, expected $expected" >&2; fail=1; continue; fi
        code=$(curl --silent --show-error --output "$DL" --write-out '%{http_code}' --max-time 600 "$url" || echo 000)
        if [ "$code" != "200" ] || [ ! -s "$DL" ]; then echo "ERROR: HTTP $code (or empty body) for $url" >&2; fail=1; continue; fi
        printf '%s' "$signature" | python3 -c 'import base64,sys; sys.stdout.write(base64.b64decode(sys.stdin.read()).decode())' > "$SIG"
        if rsign verify -p "$PUB" -x "$SIG" "$DL" >/dev/null 2>&1; then mcount=$((mcount + 1))
        else echo "ERROR: updater signature does not verify for $url" >&2; fail=1; fi
      done < <(updater_artifacts)
      [ "$fail" -eq 0 ] && echo "ok: updater manifest v${VERSION} — ${mcount} platforms, every URL present, every signature verified"
    fi
  fi
fi
```

Section 1's `trap` line stays; the new `trap` supersedes it with the longer list. Run the tests → green (the good path prints the `ok:` line; the four failure cases name their cause).

- [ ] **Step 4: Runner tooling and the channel job**

`.gitlab-ci.yml` `verify:release` `script`, before the verify call:

```yaml
    - export PATH="$HOME/.cargo/bin:$PATH"
    - command -v rsign >/dev/null || cargo install rsign2 --locked
```

(the job runs on the macOS runner; `~/.cargo/bin` persists between jobs there, so the install happens once).

New job in `announce`:

```yaml
# The updater's channel file moves ONLY here, after both verify jobs — the
# same rule as builds/latest: a rejected build is never what the app offers.
# Idempotent (copies the frozen per-tag file), safe to re-run.
publish:updater-channel:
  stage: announce
  extends: .ssh_deploy_setup
  tags: [linux]
  only: [tags]
  needs:
    - job: verify:release
      artifacts: false
    - job: verify:macos-gatekeeper
      artifacts: false
  variables:
    DEPLOY_PORT: "40022"
    UPDATES_PATH: "/var/www/artfrom.space/updates"
  script:
    - |
      if echo "${CI_COMMIT_TAG}" | grep -q '\-beta'; then CHANNEL_FILE=latest-beta.json; else CHANNEL_FILE=latest.json; fi
      ${SSH_CMD} ${DEPLOY_USER}@${DEPLOY_HOST} "cp ${UPDATES_PATH}/${CI_COMMIT_TAG}.json ${UPDATES_PATH}/${CHANNEL_FILE}.tmp && mv -f ${UPDATES_PATH}/${CHANNEL_FILE}.tmp ${UPDATES_PATH}/${CHANNEL_FILE}"
```

Above `publish_version` add the comment:

```yaml
# LEGACY FEED — keep until v0.7.0 (in-app updates spec §3.8): installs
# ≤ 0.6.4 read only version.json / version-beta.json and would otherwise never
# learn that a version with the in-app updater exists. Goes with the legacy
# filename aliases.
```

- [ ] **Step 5: Gate and commit**

Run: `.gitlab/ci/scripts/test/run_tests.sh` → green.

```bash
git add .gitlab-ci.yml .gitlab/ci/scripts/verify_release.sh .gitlab/ci/scripts/test/
git commit -m "ci(updates): verify the manifest's URLs and signatures; the channel file moves only after verify

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

---

### Task 10: Documentation — CLAUDE.md, the release skill, the logging dictionary, the docs site, the adoption parser

**Files:**
- Modify: `CLAUDE.md` (new **In-app updates** section after **Release workflow**; the `## Tauri commands` count line: +4 commands, 253 → 257, module list gains `updates`)
- Modify: `.claude/skills/release/SKILL.md`
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary extension)
- Modify: `docs/superpowers/open-items.md` (owner smokes owed)
- Modify (other repo): `~/Documents/Projects/artfrom-space/src/content/docs/download.md` — one sentence outside the marked blocks
- Modify (other repo): `~/Documents/astronet/templates/athenaeum_version_parser.py` — `VERSION_PATHS` + channel rule

- [ ] **Step 1: CLAUDE.md**

Add after the **Release workflow** section:

```markdown
## In-app updates

Spec `docs/superpowers/specs/2026-09-16-in-app-updates-design.md`, plan
`docs/superpowers/plans/2026-09-16-in-app-updates-plan.md`. Hybrid: **core
owns the check** (`athenaeum-core/src/updates/` — `check` fetches
`https://artfrom.space/updates/latest.json` (+ `latest-beta.json` under
`updates.check_beta`) with the telemetry query `v/os/arch/commit/id` the
retired `version.json` GET used to carry, compares with `semver`, exposes the
manifest's notes; `whats_new` is once per version via
`updates.last_seen_version`, from the notes EMBEDDED at build time
(`include_str!("RELEASE_NOTES.md")` — the release commit holds notes and
versions together, so they are this build's by construction);
`release_notes` is the same text any time) and **`tauri-plugin-updater` owns
download / minisign verify / install / relaunch** (`commands/updates.rs`:
`install_update` runs the plugin with `MANIFEST_BASE_URL/<channel file>` and a
`version_comparator` that normalizes the plugin's `X.Y.Z-N` current version to
`X.Y.Z-beta.N` — the ONE place that normalization happens; `restart_app` is
`AppHandle::restart`). Web mirrors answer the check and the notes and `501`
for install/restart. **Three version forms**: Cargo dotted `0.6.5-beta.1`,
`tauri.conf.json` `0.6.5-1`, tag + manifest dotted. `platform_supported` keys
on `<os>-<arch>[-<installer>]` with the plugin's lookup order and refuses
`deb`/`rpm` installs by rule. Early refusals before any download: debug
build, install in flight, translocated/unwritable macOS bundle, non-AppImage
Linux. Events `update-progress` / `update-ready`; `UpdateDialog`
(`src/components/updates/`) in `available` / `whatsNew` modes, state in
`UpdatesContext`. **Pipeline**: `bundle.createUpdaterArtifacts` +
`TAURI_SIGNING_PRIVATE_KEY` (protected; the key CANNOT be rotated — 1Password);
`build:macos` notarizes the `.app` again (F5.1 reversed); `deploy` publishes
`.app.tar.gz` + every `.sig` beside the installers (`updater_artifacts()` in
`artifact_names.sh`, keys `darwin-aarch64`, `darwin-x86_64`,
`windows-x86_64-nsis`, `windows-x86_64-msi`, `linux-x86_64` — no plain
Windows key on purpose); `publish:updater-manifest` writes the frozen
`updates/<tag>.json`; `verify:release` fetches it back and verifies every
URL and signature with `rsign2`; `publish:updater-channel` (announce) copies
it to `latest.json`/`latest-beta.json`. `publish_version` (`version.json`)
stays until v0.7.0 for pre-updater installs.
```

Update the Tauri-commands count sentence: `257 functions across 24 modules` with a dated note (`2026-09-16: +4 updates — get_whats_new/get_release_notes/install_update/restart_app; check_for_updates moved from core to the new updates module`), and add `updates` to the module list.

- [ ] **Step 2: Release skill**

`.claude/skills/release/SKILL.md`: in the stage-by-stage list add `publish:updater-manifest` under `publish` (red = a `.sig` missing from a build artifact → the build job did not sign → check `TAURI_SIGNING_PRIVATE_KEY` reached the job; fix and retag), extend the `verify` bullet (`verify:release` also names a manifest URL or a signature that failed — a signature failure means the published file is not the one the build signed: do NOT announce, retag), add `publish:updater-channel` under `announce`. New subsection **Signing key**: where it lives, that it cannot be rotated, that local `npm run tauri build` needs `TAURI_SIGNING_PRIVATE_KEY` + `_PASSWORD` exported (`tauri dev` does not), and that the committed `pubkey` must be the RELEASE key before any tag.

- [ ] **Step 3: Logging dictionary**

Append to the logging spec's dictionary-extension paragraphs:

```markdown
**Dictionary extension (in-app updates, `core::updates` + `commands/updates.rs`, 2026-09-16):** `channel` (string, `stable` | `beta`; which manifest an event is about), `current_version` / `latest_version` (strings, dotted SemVer; the running build and the manifest's), `previous_version` (string; `whats_new`'s last-seen value), `bytes_total` (integer; the download size when the server states it). `url` is reused as is.
```

- [ ] **Step 4: Open items + docs site + adoption parser**

`docs/superpowers/open-items.md`: a new **In-app updates** subsection listing the owner smokes of spec §7.3 (two tags, five platforms, `spctl` on the updated bundle, the dashboard's new pings) and the `.deb` follow-up (a `linux-x86_64-deb` entry — the plugin supports `dpkg -i` — deferred).

`artfrom-space/src/content/docs/download.md`: one sentence above the marked Latest-Build block: "From v0.6.5 on, Athenaeum updates itself: the app tells you when a new version is out, shows the notes and installs it. The downloads below are for a first install."

`astronet/templates/athenaeum_version_parser.py`: `VERSION_PATHS = ("/version.json", "/version-beta.json", "/updates/latest.json", "/updates/latest-beta.json")` and the channel rule `channel = "beta" if parts.path.endswith("-beta.json") else "stable"`. Deploy is the owner's: `ansible-playbook deploy_version_metrics.yml` from the astronet repo (memory `feedback-deploy-astronet-1password`).

- [ ] **Step 5: Commit (this repo)**

```bash
git add CLAUDE.md .claude/skills/release/SKILL.md docs/superpowers/specs/2026-07-03-logging-overhaul-design.md docs/superpowers/open-items.md
git commit -m "docs(updates): CLAUDE.md section, release skill, logging dictionary, open items

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01K8JnnN1syE8PZZ1rLKbk6Y"
```

The two other-repo edits are committed in their own repos with their own conventions (the owner pushes/deploys).

---

### Task 11: Local rehearsal — the real gate before any tag (spec §7.2)

**Files:** none committed. Everything under `$SCRATCH` (the session scratchpad) and `~/.tauri/`.

**Interfaces:** consumes everything above.

- [ ] **Step 1: Two builds, one "installed", one "new"**

Build A = the current tree (version `0.6.4-beta.1`, tauri `0.6.4-1`), signed with the dev key, endpoint pointed at a local server at BUILD time:

```bash
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/athenaeum-dev.key)" TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
export ATHENAEUM_UPDATES_BASE_URL="http://127.0.0.1:8765"
npm run tauri build -- --config '{"plugins":{"updater":{"dangerousInsecureTransportProtocol":true,"endpoints":["http://127.0.0.1:8765/latest.json"]}}}'
cp -R target/release/bundle/macos/Athenaeum.app /Applications/Athenaeum-rehearsal.app
```

(use the target triple path `target/aarch64-apple-darwin/release/bundle/…` if the build was targeted). Build B = bump on a scratch branch, same env:

```bash
git switch -c scratch/rehearsal && scripts/release/bump.sh 0.6.5-beta.1
npm run tauri build -- --config '{"plugins":{"updater":{"dangerousInsecureTransportProtocol":true,"endpoints":["http://127.0.0.1:8765/latest.json"]}}}'
mkdir -p "$SCRATCH/serve" && cp target/release/bundle/macos/Athenaeum.app.tar.gz "$SCRATCH/serve/b.app.tar.gz"
SIG=$(cat target/release/bundle/macos/Athenaeum.app.tar.gz.sig)
ARCH_KEY=$([ "$(uname -m)" = arm64 ] && echo darwin-aarch64 || echo darwin-x86_64)
python3 - "$SIG" "$ARCH_KEY" > "$SCRATCH/serve/latest.json" <<'PY'
import json,sys
sig,key=sys.argv[1:3]
print(json.dumps({"version":"0.6.5-beta.1","notes":open("RELEASE_NOTES.md",encoding="utf-8").read(),"pub_date":"2026-09-16T12:00:00Z","platforms":{key:{"url":"http://127.0.0.1:8765/b.app.tar.gz","signature":sig}}}))
PY
git switch - && git branch -D scratch/rehearsal
(cd "$SCRATCH/serve" && python3 -m http.server 8765 &)
```

Note the beta.1 manifest is served as STABLE (`latest.json`) so `check_beta` need not be on; SemVer `0.6.5-beta.1 > 0.6.4-beta.1` holds.

- [ ] **Step 2: Walk the desktop flow**

Launch `/Applications/Athenaeum-rehearsal.app`. Expected, in order: no What's-new (fresh key → silent); toast *Update available: v0.6.5-beta.1*; bell → About → dialog with the notes, `You have v0.6.4-beta.1 · released 2026-09-16 …`; **Download and install** → progress bar reaches the archive size → *v0.6.5-beta.1 is installed — restart to apply* → **Restart now** → the app relaunches → **What's new in v0.6.5-beta.1** opens once → About says `v0.6.5-beta.1`, **Check for updates** says up to date. `codesign --verify --deep --strict /Applications/Athenaeum-rehearsal.app` passes (dev-signed; notarization is the pipeline's). Record the JSONL log lines (`update check finished`, `update download started`, `update installed`) via the `athenaeum-logs` MCP `query_logs`.

Negative checks: kill the server → **Check for updates** shows the error text; tamper one byte of `b.app.tar.gz` → **Download and install** fails with the verification message and nothing changed; a second click while downloading is impossible (button hidden) and a direct second `install_update` invoke answers "already being installed".

- [ ] **Step 3: Walk the web flow**

`ATHENAEUM_UPDATES_BASE_URL=http://127.0.0.1:8765 cargo run -p athenaeum-web` (with the frontend built for web: `npm run build:web`) against a temp DB; open the UI: the same toast; the dialog shows the notes and `docker pull vsharifov/athenaeum:0.6.5-beta.1` with a working Copy; **Open download page** opens the browser. Set `updates.auto_check` off in Settings, reload: no check. `curl -X POST localhost:<port>/api/install_update -d '{}' -H 'content-type: application/json'` → `501`.

- [ ] **Step 4: Write it down**

`docs/superpowers/research/2026-09-16-updater-rehearsal.md`: what was built, what was observed (with the log lines), what failed and how it was fixed. Any fix goes into its own task's file with a test where a test is possible. Commit the research note.

Clean up: `rm -rf /Applications/Athenaeum-rehearsal.app "$SCRATCH/serve"`, kill the server.

---

### Task 12: Acceptance on real releases — two tags (spec §7.3, owner-driven)

**Files:** `docs/superpowers/research/<date>-updater-acceptance-run.md` (created at the end).

- [ ] **Step 1: Preconditions**

`grep -c '"pubkey"' crates/athenaeum-tauri/tauri.conf.json` shows the RELEASE key (compare with the 1Password item's `.pub`; the dev key from Task 4 must be gone). GitLab has `TAURI_SIGNING_PRIVATE_KEY` + `_PASSWORD` protected + masked. The astronet parser is deployed. `.gitlab/ci/scripts/test/run_tests.sh` green, `cargo test --workspace` green, GitHub run green on `main`. Owner's word to push.

- [ ] **Step 2: Tag one — `v0.6.5-beta.1`**

Through the `release` skill. Then, by hand: install from the DMG (arm64 and x64 Macs), the NSIS exe on Windows, the AppImage on Linux, `docker pull vsharifov/athenaeum:0.6.5-beta.1`. Checks: the pipeline is green including `publish:updater-manifest` and the new `verify:release` output line `ok: updater manifest v0.6.5-beta.1 — 5 platforms, …`; `curl https://artfrom.space/updates/v0.6.5-beta.1.json` and `latest-beta.json` are identical; the adoption dashboard shows `/updates/latest-beta.json` pings within the parser's next run.

- [ ] **Step 3: Tag two — `v0.6.5-beta.2`**

Any minimal user-visible change (a release-notes-only beta is fine, like v0.6.4-beta.1 was). Through the `release` skill. On EVERY installed platform with `updates.check_beta` on: launch → toast → dialog → **Download and install** → restart → **What's new in v0.6.5-beta.2** once. macOS: `spctl --assess --type execute -v /Applications/Athenaeum.app` prints `accepted` on the UPDATED bundle, and `codesign -dv --verbose=2` shows the Developer ID. Windows: Add/Remove Programs shows ONE Athenaeum entry at 0.6.5-beta.2 (NSIS install) — and, on an MSI-installed copy, the same through the MSI. Linux: the AppImage's mtime changed, `--version`-equivalent (About) says beta.2. Docker: the beta.1 container's UI shows *available* with `docker pull vsharifov/athenaeum:0.6.5-beta.2`.

- [ ] **Step 4: Write the acceptance note and close the open items**

`docs/superpowers/research/<date>-updater-acceptance-run.md` with the per-platform results, timings (download sizes and seconds), the notarization waits added per release, anything refused. Delete the passed smokes from `docs/superpowers/open-items.md`. Commit.

---

## Self-review

**Spec coverage.** §1 goal → Tasks 6, 11, 12. §2.1–2.2 → Tasks 1–3. §2.3 → Task 4 (minus `tauri-plugin-process`, Deviations). §2.4 → Task 5. §2.5 → Task 6. §3.1 key → Task 7 Step 1 (owner) + Task 12 Step 1 check. §3.2 → Tasks 4 (plugin block) and 7 (`createUpdaterArtifacts`). §3.3 → Task 7. §3.4 → Task 7 (`updater_artifacts`). §3.5 → Task 7. §3.6 → Task 8. §3.7 → Task 9. §3.8 → Task 9 (channel job, `publish_version` kept). §3.9 → settled in Deviations, no task. §4 → Tasks 1, 3, 4 (parity test); `check_versions.sh` already asserts the tauri form (its `TAURI_VERSION` transform), so the spec's "gains the assertion" is already true — no change. §5.1–5.4 → Task 6. §6.1–6.3 → Tasks 2 (fetch errors), 4 (refusals, in-flight guard, failure messages), 6 (Retry, download page fallback). §6.4 → Tasks 8, 9. §6.5 → nothing to build. §6.6 → Task 10 (parser). §7.1 → tests inside Tasks 1–5, 7–9. §7.2 → Task 11. §7.3 → Task 12. §8 → nothing. §9 → Task 10.

**Placeholders.** The one intentional open value is the release public key (a secret the owner produces) — its handling is spelled out (dev key committed, swapped in Task 7 Step 1, gated in Task 12 Step 1). The `BundleType` match and the `ComputeJobState` literal carry an explicit verify-and-adjust instruction rather than a guess.

**Type consistency.** `UpdateCheck` fields (`currentVersion`, `latestVersion`, `isUpdateAvailable`, `channel`, `notes`, `pubDate`, `platformSupported`, `downloadPageUrl`, `blogUrl`, `dockerImage`) are used with those camelCase names in Task 6; `WhatsNew { version, notes, blogUrl }` likewise. `install_update` takes `InstallArgs { channel }` (Task 4) and is invoked as `{ args: { channel } }` (Task 6). Events `update-progress { downloaded, total, finished? }` and `update-ready { version }` match between Task 4 and Task 6. `updater_artifacts()` columns (`product os arch ext variant subdir filename key`) are read identically in Tasks 7, 8 and 9. `HostInfo { kind, git_hash, installer }` matches between Tasks 3, 4 and 5.
