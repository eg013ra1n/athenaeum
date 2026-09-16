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
    if let Some(id) = crate::db::get_setting(conn, SETTING_INSTALLATION_ID)? {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    crate::db::set_setting(conn, SETTING_INSTALLATION_ID, &id)?;
    tracing::info!("installation id created");
    Ok(id)
}

/// The running build's own notes, any time (About's "View release notes").
pub fn release_notes() -> WhatsNew {
    let version = env!("CARGO_PKG_VERSION");
    WhatsNew {
        version: version.to_string(),
        notes: notes::EMBEDDED_RELEASE_NOTES.to_string(),
        blog_url: notes::blog_url(version),
    }
}

/// Once per version: `Some` on the first call after the version changed,
/// `None` otherwise. A fresh install (no key yet) records the version and
/// returns `None` — a new user is not greeted with a change list. The key is
/// written BEFORE returning, so a crash in the dialog cannot show it twice.
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

pub async fn check(ctx: &ServiceContext, host: &HostInfo<'_>) -> Result<UpdateCheck> {
    check_at(ctx, host, MANIFEST_BASE_URL).await
}

/// `check` against an explicit base URL (tests serve a manifest locally).
/// Stable is always fetched; beta only when `updates.check_beta`; the newer
/// of the two wins. Both fetches failing is an error (logged inside
/// `fetch_manifest`); one failing is logged and the other answers.
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
            r###"{{"version":"{version}","notes":"## New in {version}","pub_date":"2026-09-20T00:00:00Z","platforms":{{{}}}}}"###,
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
