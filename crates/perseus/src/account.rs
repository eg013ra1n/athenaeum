//! Perseus account sign-in + send-target resolution (Sync 2C mesh model).
//!
//! Perseus is a **send-only** capability: it signs into a hub **account** (a
//! `perseus login` device token) and sends captures to explicit **targets**
//! (account devices named by id or name), or — for offline dev/tests — derives a
//! single peer from a raw **pairing ticket**. This module owns:
//!
//! - [`login`] — the interactive CLI OTP flow (email → code) using the shared
//!   [`athenaeum_core::account`] hub client. The device token is written to a
//!   0600 file in `data_dir` (never the TOML), and this device registers itself
//!   with the [`DeviceCapability::Perseus`] capability. There is no per-account
//!   "primary" to pair to — that one-primary model is gone.
//! - [`resolve_pairing`] — the run-time resolution the [`crate::run::Agent`]
//!   uses: it resolves the first configured target to a [`NodeId`] (from the hub
//!   device list, matched by id then name, pubkey → node id) and pairs the hub
//!   relay map (with an offline cache fallback). Task 6 resolves `targets[0]`;
//!   the full multi-target send is task 7. Falling back to iroh's public default
//!   relays requires the dev-only `[account].allow_default_relays` opt-in
//!   (default `false`) — otherwise an empty/unreachable relay map with no cache
//!   is a loud, actionable error.
//! - [`PairingCache`] — the persisted `perseus login` result + last successful
//!   resolutions (`data_dir/pairing_cache.json`), so an offline restart still
//!   resolves. The token is NOT in it (that is the 0600 [`TokenStore`] file).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use athenaeum_core::account::{
    default_device_name, DeviceCapability, DeviceKey, HubClient, TokenStore,
};
use athenaeum_core::sharing::types::NodeId;
use athenaeum_core::sync::pairing;
use athenaeum_core::sync::{node_id_from_hex, node_id_hex};
use iroh::RelayMode;
use serde::{Deserialize, Serialize};

use crate::config::{AccountConfig, Config};

/// Filename of the persisted pairing cache inside `data_dir`.
const PAIRING_CACHE_FILE: &str = "pairing_cache.json";

/// Persisted result of `perseus login` plus the last successful resolutions.
/// Never holds the bearer token — that lives in the 0600 [`TokenStore`] file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCache {
    /// This device's hub device id (from login).
    #[serde(default)]
    pub device_id: Option<String>,
    /// Last successfully resolved target peer node id (64-char lowercase hex).
    /// Task 6 tracks the single `targets[0]` peer here for offline restarts.
    #[serde(default)]
    pub peer_node_id_hex: Option<String>,
    /// Last successfully resolved relay URLs.
    #[serde(default)]
    pub relay_urls: Vec<String>,
    /// node_id_hex → hub device name, refreshed whenever the device list is
    /// fetched. Display-only (history rows keep the hex as the stable key).
    #[serde(default)]
    pub device_names: HashMap<String, String>,
    /// The email this device signed in with (display-only; surfaced by
    /// [`account_status`] when the config file carries no `[account].email`).
    #[serde(default)]
    pub email: Option<String>,
}

impl PairingCache {
    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(PAIRING_CACHE_FILE)
    }

    /// Load the cache, treating any read/parse error as an empty cache (the file
    /// is a best-effort accelerator, never authoritative).
    pub fn load(data_dir: &Path) -> Self {
        match std::fs::read(Self::path(data_dir)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "pairing cache unreadable; ignoring");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Persist the cache (best-effort; a failure only weakens the next offline start).
    pub fn save(&self, data_dir: &Path) {
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(Self::path(data_dir), bytes) {
                    tracing::warn!(%error, "failed to write pairing cache");
                }
            }
            Err(error) => tracing::warn!(%error, "failed to serialize pairing cache"),
        }
    }
}

/// The resolved sync peer + relays for one agent start.
#[derive(Debug)]
pub struct ResolvedPairing {
    /// The peer to send to.
    pub peer: NodeId,
    /// The relay mode the transport should bind with.
    pub relay_mode: RelayMode,
    /// `Some` only on the dev-ticket path — the caller registers the peer's
    /// dialable address from this ticket (the account path resolves by node id).
    pub ticket: Option<String>,
    /// The raw relay URLs `relay_mode` was built from (fix-review, production
    /// bug: account-mode dial failure). On the account path (`ticket = None`)
    /// the caller must attach these to the peer's `EndpointAddr`
    /// ([`pairing::peer_addr_with_relays`]) before `add_peer` — a bare node id
    /// is undialable with `IrohTransport`'s discovery-free preset. Whatever
    /// `resolve_relays` resolved (fresh from the hub, or the offline cache
    /// fallback) ends up here, so the cached-relay offline start dials
    /// correctly too, with no extra plumbing.
    pub relay_urls: Vec<String>,
}

/// Interactive `perseus login`: request an OTP for the account email, verify it,
/// store the device token (0600 file), and register this device with the
/// [`DeviceCapability::Perseus`] capability. Requires an `[account]` config table.
pub async fn login(config: &Config) -> Result<()> {
    let account = config.account.clone().ok_or_else(|| {
        anyhow!("`perseus login` requires an [account] table in the config (hub_url/email)")
    })?;
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("create data dir {}", config.data_dir.display()))?;

    let email = match account.email.as_deref().map(str::trim) {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => prompt("Account email: ")?,
    };
    if email.is_empty() {
        bail!("email is required");
    }

    let client = HubClient::new(&account.hub_url).map_err(|e| anyhow!("{e}"))?;
    client
        .request_otp(&email)
        .await
        .map_err(|e| anyhow!("request one-time code: {e}"))?;
    // Human CLI output (documented zero-print exemption for the perseus binary).
    println!("A one-time code was sent to {email}.");
    let code = prompt("Enter the code: ")?;
    if code.is_empty() {
        bail!("code is required");
    }

    verify_and_register(config, &account, &client, &email, &code).await?;
    println!(
        "Signed in as a Perseus capture device. Add the devices to send to in the \
         config `targets` list (by device name or id)."
    );
    Ok(())
}

/// The non-interactive core of [`login`]: verify the OTP, store the token, and
/// register this device with the [`DeviceCapability::Perseus`] capability.
/// Split out so the hub interactions are testable without prompting for stdin.
async fn verify_and_register(
    config: &Config,
    account: &AccountConfig,
    client: &HubClient,
    email: &str,
    code: &str,
) -> Result<()> {
    // The ONE device identity: the same key file the run-time transport binds.
    let key = DeviceKey::load_or_create(&config.device_key_path())
        .context("load or create device key")?;
    let resp = client
        .verify(
            email,
            code,
            &key.pubkey_base64(),
            &device_name(config, &key),
            DeviceCapability::Perseus,
        )
        .await
        .map_err(|e| anyhow!("verify code: {e}"))?;

    // Token → 0600 file store (headless: no OS keychain).
    token_store(config, account)
        .store(&resp.device_token)
        .context("store device token")?;

    let mut cache = PairingCache::load(&config.data_dir);
    cache.device_id = Some(resp.device_id.clone());
    cache.email = Some(email.to_string());
    // Best-effort: refresh the friendly-name cache history rows resolve peer hex
    // against. There is no primary to pick, so a device-list failure here must
    // NOT fail sign-in — the token is already stored.
    match client.list_devices(&resp.device_token).await {
        Ok(devices) => cache.device_names = device_names_from(&devices),
        Err(error) => tracing::warn!(%error, "could not refresh device names after sign-in"),
    }
    cache.save(&config.data_dir);

    tracing::info!(device_id = %resp.device_id, "perseus signed in");
    Ok(())
}

/// Is a hub device token stored for this config's account? (`false` when there
/// is no `[account]` table at all.) The supervisor (Task 3) gates the sync
/// engine on this.
pub fn token_present(config: &Config) -> bool {
    match &config.account {
        Some(a) => matches!(token_store(config, a).load(), Ok(Some(_))),
        None => false,
    }
}

/// Ask the hub to email a one-time code. Requires an `[account]` table (hub_url).
/// The non-interactive counterpart of the OTP request inside [`login`], for the
/// web page's email→OTP sign-in.
pub async fn request_code(config: &Config, email: &str) -> Result<()> {
    let account = config.account.clone().ok_or_else(|| {
        anyhow!("sign-in requires an [account] table in the config (hub_url)")
    })?;
    let email = email.trim();
    if email.is_empty() {
        bail!("email is required");
    }
    let client = HubClient::new(&account.hub_url).map_err(|e| anyhow!("{e}"))?;
    client
        .request_otp(email)
        .await
        .map_err(|e| anyhow!("request one-time code: {e}"))?;
    tracing::info!("one-time code requested");
    Ok(())
}

/// Non-interactive sign-in (the web page's path): verify the OTP, store the
/// token, register with the Perseus capability. Same core as `perseus login`,
/// without the stdin prompts.
pub async fn web_sign_in(config: &Config, email: &str, code: &str) -> Result<()> {
    let account = config.account.clone().ok_or_else(|| {
        anyhow!("sign-in requires an [account] table in the config (hub_url)")
    })?;
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("create data dir {}", config.data_dir.display()))?;
    let email = email.trim();
    let code = code.trim();
    if email.is_empty() {
        bail!("email is required");
    }
    if code.is_empty() {
        bail!("code is required");
    }
    let client = HubClient::new(&account.hub_url).map_err(|e| anyhow!("{e}"))?;
    verify_and_register(config, &account, &client, email, code).await
}

/// Delete the stored token and reset the pairing cache. Idempotent; the engine
/// is stopped by the supervisor (which the caller wakes), not here.
pub fn sign_out(config: &Config) -> Result<()> {
    if let Some(account) = &config.account {
        token_store(config, account).delete().context("delete device token")?;
    }
    PairingCache::default().save(&config.data_dir);
    tracing::info!("signed out");
    Ok(())
}

/// A snapshot of the signed-in state for the Account UI (Task 5). All identity
/// fields are display-only; `signed_in` is the authoritative gate (a stored
/// token), everything else is best-effort from the config + pairing cache.
#[derive(Debug, Clone)]
pub struct AccountStatus {
    /// A hub device token is stored (the sync engine may run).
    pub signed_in: bool,
    /// The signed-in email: config `[account].email` first, else the cache.
    pub email: Option<String>,
    /// The account hub base URL (`None` when there is no `[account]` table).
    pub hub_url: Option<String>,
    /// This device's hub device id (from the last sign-in).
    pub device_id: Option<String>,
}

/// Build the [`AccountStatus`] snapshot from the config + pairing cache. Pure
/// (no hub calls) so the web page can poll it cheaply.
pub fn account_status(config: &Config) -> AccountStatus {
    let cache = PairingCache::load(&config.data_dir);
    AccountStatus {
        signed_in: token_present(config),
        email: config.account.as_ref().and_then(|a| a.email.clone()).or(cache.email),
        hub_url: config.account.as_ref().map(|a| a.hub_url.clone()),
        device_id: cache.device_id,
    }
}

/// Resolve the peer + relays for an agent start.
///
/// Order (Sync 2C): account send target (signed in → resolve `targets[0]` from
/// the hub device list, with the cached peer as the offline fallback) → dev
/// ticket → error. Task 6 resolves the FIRST configured target only; the full
/// multi-target send is task 7. A successful account resolution refreshes the
/// offline cache (`peer_node_id_hex` + relay map).
///
/// The peer is resolved FIRST and a failure bails immediately, before the relay
/// map is resolved — there is no point spending a second hub round trip on
/// relays when there is nothing to sync to yet.
pub async fn resolve_pairing(config: &Config) -> Result<ResolvedPairing> {
    let mut cache = PairingCache::load(&config.data_dir);

    // Signed in? (an [account] table AND a stored device token.)
    let account_token = match &config.account {
        Some(account) => token_store(config, account)
            .load()
            .context("load device token")?
            .map(|token| (account, token)),
        None => None,
    };
    let dev_ticket = config
        .pairing_ticket
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());

    // Resolve the peer: account target[0] when signed in, else the dev ticket.
    // `relay_account` carries the (hub_url, token) the relay map is fetched with
    // — `None` on the ticket path (an account-less ticket run is dev/test).
    let (peer, ticket, relay_account) = if let Some((account, token)) = &account_token {
        let peer = resolve_target_peer(config, account, token, &mut cache).await?;
        (peer, None, Some((account.hub_url.clone(), token.clone())))
    } else if let Some(t) = dev_ticket {
        let peer = crate::run::peer_node_id_from_ticket(t)
            .context("derive the sync peer from the dev pairing ticket")?;
        (peer, Some(t.to_string()), None)
    } else {
        bail!(
            "cannot resolve a send target — sign in and add at least one entry to \
             `targets`, or set a dev pairing_ticket"
        );
    };

    // Relays: hub map when signed in, cached map otherwise. Falling back to
    // iroh's public default relays beyond that requires the dev-only opt-in
    // (review finding #1) — only relevant when actually signed in (an
    // account-less ticket run is inherently dev/test already).
    let relay_account_ref = relay_account.as_ref().map(|(u, t)| (u.as_str(), t.as_str()));
    let relays = pairing::resolve_relays(relay_account_ref, &cache.relay_urls).await;
    if relays.fresh {
        cache.relay_urls = relays.urls.clone();
    }
    let allow_default_relays = if relay_account.is_some() {
        config.account.as_ref().is_some_and(|a| a.allow_default_relays)
    } else {
        true
    };
    let relay_mode = match pairing::relay_mode_for(&relays.urls, allow_default_relays) {
        Ok(mode) => mode,
        Err(reason) => {
            cache.save(&config.data_dir);
            bail!("cannot start the sync transport — {reason}");
        }
    };

    cache.save(&config.data_dir);
    Ok(ResolvedPairing {
        peer,
        relay_mode,
        ticket,
        relay_urls: relays.urls.clone(),
    })
}

/// Resolve the first configured send target to its peer [`NodeId`].
///
/// The target string is matched against the account device list by **id first,
/// then name**, and the matched device's pubkey is decoded to a node id. A
/// Perseus capability device is refused as a target (capture agents are
/// send-only and cannot receive). When the hub is unreachable, the last cached
/// peer is used as an offline fallback; with no cache this is an actionable
/// error rather than a silent no-peer start. On a live resolution the cache's
/// `peer_node_id_hex` + friendly-name map are refreshed.
///
/// Task 6 resolves only `targets[0]`; the multi-target send loop is task 7.
async fn resolve_target_peer(
    config: &Config,
    account: &AccountConfig,
    token: &str,
    cache: &mut PairingCache,
) -> Result<NodeId> {
    let target = config
        .targets
        .iter()
        .map(|t| t.trim())
        .find(|t| !t.is_empty())
        .ok_or_else(|| {
            anyhow!("no send target configured — add at least one device to `targets`")
        })?;

    let client = HubClient::new(&account.hub_url).map_err(|e| anyhow!("{e}"))?;
    match client.list_devices(token).await {
        Ok(devices) => {
            cache.device_names = device_names_from(&devices);
            // Match by id first, then by name (either spelling is a valid target).
            let device = devices
                .iter()
                .find(|d| d.id == target)
                .or_else(|| devices.iter().find(|d| d.name == target))
                .ok_or_else(|| {
                    anyhow!(
                        "send target '{target}' was not found in your account device list — \
                         check the device name or id"
                    )
                })?;
            if device.capability == DeviceCapability::Perseus {
                bail!(
                    "send target '{target}' is a Perseus capture agent — capture agents are \
                     send-only and cannot receive frames; choose a full Athenaeum device"
                );
            }
            let peer = pairing::node_id_from_pubkey_b64(&device.pubkey)
                .with_context(|| format!("decode pubkey for send target '{target}'"))?;
            cache.peer_node_id_hex = Some(node_id_hex(&peer));
            tracing::info!(target = %target, "resolved send target from account device list");
            Ok(peer)
        }
        Err(error) => {
            // Hub unreachable: fall back to the last cached target peer, if any.
            if let Some(peer) = cache
                .peer_node_id_hex
                .as_deref()
                .and_then(|s| node_id_from_hex(s).ok())
            {
                tracing::warn!(
                    target = %target,
                    %error,
                    "could not reach the hub to resolve the send target; using last cached peer"
                );
                return Ok(peer);
            }
            bail!(
                "could not reach the hub to resolve send target '{target}', and there is no \
                 cached peer yet: {error}"
            );
        }
    }
}

/// Build the node_id_hex → device-name map from a hub device list, for the
/// pairing cache's display-only [`PairingCache::device_names`]. A device whose
/// pubkey can't be decoded is skipped (the UI just falls back to short hex for
/// it) — never fatal to the caller.
fn device_names_from(devices: &[athenaeum_core::account::AccountDevice]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for d in devices {
        if let Ok(id) = pairing::node_id_from_pubkey_b64(&d.pubkey) {
            map.insert(node_id_hex(&id), d.name.clone());
        }
    }
    map
}

/// The 0600 file-backed token store for this hub (headless: no OS keychain). The
/// account/file discriminator is the hub host so multiple hubs coexist.
fn token_store(config: &Config, account: &AccountConfig) -> TokenStore {
    let host = hub_host(&account.hub_url);
    let sanitized: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    TokenStore::file_only(host, config.data_dir.join(format!("token_{sanitized}")))
}

/// Extract the host portion of a hub URL for token-file scoping (no `reqwest::Url`
/// dependency here — a minimal split is enough for a filename discriminator).
fn hub_host(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    // Strip any userinfo, then any port.
    let host = host_port.rsplit('@').next().unwrap_or(host_port);
    host.split(':').next().unwrap_or(host).to_string()
}

/// The device name registered with the hub: the explicit `config.device_name`
/// override, else the machine hostname (falling back to a `perseus-<short-id>`
/// label from the device node id when the hostname is unavailable — see
/// [`default_device_name`]).
fn device_name(config: &Config, key: &DeviceKey) -> String {
    config
        .device_name
        .clone()
        .unwrap_or_else(|| default_device_name("perseus", &key.node_id_hex()))
}

/// Read one trimmed line from stdin after printing `label` (login prompts).
fn prompt(label: &str) -> Result<String> {
    use std::io::{self, BufRead};
    print!("{label}");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .context("read from stdin")?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_host_extracts_bare_host() {
        assert_eq!(hub_host("https://projects.artfrom.space"), "projects.artfrom.space");
        assert_eq!(hub_host("https://user@host.example:8443/path"), "host.example");
        assert_eq!(hub_host("http://127.0.0.1:1234"), "127.0.0.1");
    }

    #[test]
    fn node_id_hex_round_trips() {
        let id = [9u8; 32];
        let hex = node_id_hex(&id);
        assert_eq!(hex.len(), 64);
        assert_eq!(node_id_from_hex(&hex).unwrap(), id);
        assert!(node_id_from_hex("too-short").is_err());
    }

    #[test]
    fn pairing_cache_round_trips_relay_map() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = PairingCache::load(dir.path());
        assert_eq!(cache, PairingCache::default(), "missing file loads an empty cache");

        cache.device_id = Some("dev-1".into());
        cache.peer_node_id_hex = Some(node_id_hex(&[3u8; 32]));
        cache.relay_urls = vec!["https://relay1.example.org".into()];
        cache.save(dir.path());

        let reloaded = PairingCache::load(dir.path());
        assert_eq!(reloaded, cache, "the cache round-trips through disk");
        assert_eq!(reloaded.relay_urls, vec!["https://relay1.example.org".to_string()]);
    }

    /// Task 11: the new `device_names` map is `#[serde(default)]`, so (a) an
    /// old cache file written before this field existed still parses (→ empty
    /// map, never an error), and (b) a saved map round-trips through disk.
    #[test]
    fn pairing_cache_device_names_roundtrip_and_backcompat() {
        // (a) Old-format JSON without `device_names` parses to an empty map.
        let old = r#"{"device_id":"d","primary_device_id":"p","peer_node_id_hex":"ab","relay_urls":[]}"#;
        let c: PairingCache = serde_json::from_str(old).unwrap();
        assert!(c.device_names.is_empty(), "a pre-field cache file loads an empty map");

        // (b) Save with names → load returns them.
        let dir = tempfile::tempdir().unwrap();
        let mut cache = PairingCache::default();
        cache.device_names.insert("aa".repeat(32), "Studio".into());
        cache.device_names.insert("bb".repeat(32), "Laptop".into());
        cache.save(dir.path());

        let reloaded = PairingCache::load(dir.path());
        assert_eq!(reloaded.device_names.get(&"aa".repeat(32)).map(String::as_str), Some("Studio"));
        assert_eq!(reloaded.device_names.get(&"bb".repeat(32)).map(String::as_str), Some("Laptop"));
    }

    use crate::config::{Config, Mode, RetentionConfig};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A minimal `[account]`-based Config over a fresh data dir with the given
    /// send `targets`. `capture_dir` need not exist here — these tests never call
    /// `validate()`, only the account helpers. `allow_default_relays` defaults to
    /// `false` (production default); callers that need the dev-only opt-in use
    /// [`account_config_allowing_default_relays`].
    fn account_config(data_dir: &Path, hub_url: &str, targets: &[&str]) -> Config {
        account_config_with(data_dir, hub_url, targets, false)
    }

    /// As [`account_config`], but with the dev-only relay opt-in set — for the
    /// gating-matrix tests (review finding #1).
    fn account_config_allowing_default_relays(data_dir: &Path, hub_url: &str, targets: &[&str]) -> Config {
        account_config_with(data_dir, hub_url, targets, true)
    }

    fn account_config_with(
        data_dir: &Path,
        hub_url: &str,
        targets: &[&str],
        allow_default_relays: bool,
    ) -> Config {
        Config {
            capture_dir: Some(data_dir.join("capture")),
            capture_dirs: Vec::new(),
            data_dir: data_dir.to_path_buf(),
            pairing_ticket: None,
            account: Some(AccountConfig {
                hub_url: hub_url.to_string(),
                email: Some("me@example.com".into()),
                allow_default_relays,
            }),
            targets: targets.iter().map(|s| s.to_string()).collect(),
            device_name: None,
            mode: Mode::Auto,
            retention: RetentionConfig::default(),
            stability_secs: 1,
            poll_interval_secs: 1,
            web_bind: String::new(),
            web_token: None,
        }
    }

    /// One account device with the given fields (mesh `capability`; a real pubkey
    /// so `resolve_target_peer` can decode it to a node id).
    fn device_json(id: &str, name: &str, pubkey: &str, capability: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": name, "pubkey": pubkey, "capability": capability,
            "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null
        })
    }

    /// Sync 2C: the non-interactive login core verifies the code — sending the
    /// `perseus` capability (body matcher) — stores the token (0600 file), and
    /// caches the signed-in identity. No role/primary pairing happens.
    #[tokio::test]
    async fn verify_and_register_registers_as_perseus_and_stores_token() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let server = MockServer::start().await;

        // The verify body MUST carry deviceCapability = "perseus"; a mismatch
        // 404s and the flow errors, proving Perseus registers send-only.
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .and(body_partial_json(serde_json::json!({ "deviceCapability": "perseus" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceToken": "tok-secret-xyz",
                "deviceId": "perseus-dev",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", "cHVia2V5", "athenaeum")
            ])))
            .mount(&server)
            .await;

        let account = AccountConfig {
            hub_url: server.uri(),
            email: Some("me@example.com".into()),
            allow_default_relays: false,
        };
        let config = account_config(tmp.path(), &server.uri(), &["Studio"]);
        let client = HubClient::new(&account.hub_url).unwrap();

        verify_and_register(&config, &account, &client, "me@example.com", "123456")
            .await
            .expect("login core succeeds");

        // Token landed in the 0600 file store.
        assert_eq!(
            token_store(&config, &account).load().unwrap().as_deref(),
            Some("tok-secret-xyz"),
            "the device token must be stored"
        );
        // Cache recorded the sign-in identity + friendly-name map.
        let cache = PairingCache::load(tmp.path());
        assert_eq!(cache.device_id.as_deref(), Some("perseus-dev"));
        assert_eq!(cache.email.as_deref(), Some("me@example.com"));
    }

    /// Sync 2C: the non-interactive web sign-in core stores the token and caches
    /// the signed-in identity (email) so the Account UI can show who is signed
    /// in, driven without a stdin prompt.
    #[tokio::test]
    async fn web_sign_in_stores_token_and_caches_identity() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceToken": "tok-secret-xyz",
                "deviceId": "perseus-dev",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", "cHVia2V5", "athenaeum")
            ])))
            .mount(&server)
            .await;

        // The web sign-in path carries no email in the config file — the email
        // comes from the sign-in form and is cached; account_status must surface
        // it via the cache fallback.
        let mut config = account_config(tmp.path(), &server.uri(), &["Studio"]);
        config.account.as_mut().unwrap().email = None;

        web_sign_in(&config, "user@example.com", "123456")
            .await
            .expect("web sign-in succeeds");

        assert!(token_present(&config), "the device token must be stored");
        let cache = PairingCache::load(&config.data_dir);
        assert_eq!(cache.device_id.as_deref(), Some("perseus-dev"));
        assert_eq!(cache.email.as_deref(), Some("user@example.com"));

        let status = account_status(&config);
        assert!(status.signed_in);
        assert_eq!(status.email.as_deref(), Some("user@example.com"));
        assert_eq!(status.device_id.as_deref(), Some("perseus-dev"));
    }

    /// request_code asks the hub to email a one-time code — exactly one POST to
    /// the OTP endpoint `HubClient::request_otp` uses.
    #[tokio::test]
    async fn request_code_hits_hub_otp_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/otp"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let config = account_config(tmp.path(), &server.uri(), &["Studio"]);
        request_code(&config, "user@example.com").await.unwrap();
        // MockServer verifies the `.expect(1)` on drop.
    }

    /// sign_out deletes the stored token and resets the pairing cache, and a
    /// second call is a no-op (idempotent — the engine is stopped by the
    /// supervisor, not here).
    #[tokio::test]
    async fn sign_out_clears_token_and_cache_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceToken": "tok-secret-xyz",
                "deviceId": "perseus-dev",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", "cHVia2V5", "athenaeum")
            ])))
            .mount(&server)
            .await;

        let config = account_config(tmp.path(), &server.uri(), &["Studio"]);
        web_sign_in(&config, "user@example.com", "123456")
            .await
            .expect("web sign-in succeeds");
        assert!(token_present(&config), "signed in before sign-out");

        sign_out(&config).unwrap();
        assert!(!token_present(&config), "the token is gone after sign-out");
        assert_eq!(
            PairingCache::load(&config.data_dir),
            PairingCache::default(),
            "the pairing cache is reset to default"
        );
        sign_out(&config).unwrap(); // second call must not error
    }

    /// Sync 2C: run-time resolution matches `targets[0]` against the hub device
    /// list (by name here), decodes the device pubkey to the peer node id,
    /// resolves the relay map, and caches both for the next offline start.
    #[tokio::test]
    async fn resolve_pairing_resolves_target_peer_and_relays() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        // A real device key gives us a pubkey the resolver must decode back to a
        // node id — no hand-rolled base64.
        let target_key = DeviceKey::load_or_create_in(&tmp.path().join("target")).unwrap();
        let target_pubkey = target_key.pubkey_base64();
        let target_node = target_key.node_id();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", &target_pubkey, "athenaeum")
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/relay-map"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "relays": ["https://relay1.example.org"]
            })))
            .mount(&server)
            .await;

        // Match by name.
        let config = account_config(tmp.path(), &server.uri(), &["Studio"]);
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        let resolved = resolve_pairing(&config).await.expect("resolution succeeds");
        assert_eq!(resolved.peer, target_node, "peer decodes from the target's pubkey");
        assert!(resolved.ticket.is_none(), "account path resolves by node id, not a ticket");
        assert!(
            matches!(resolved.relay_mode, RelayMode::Custom(_)),
            "the hub relay map yields a custom relay mode"
        );

        // Offline cache was refreshed with both the peer and the relay map.
        let cache = PairingCache::load(tmp.path());
        assert_eq!(cache.peer_node_id_hex, Some(node_id_hex(&target_node)));
        assert_eq!(cache.relay_urls, vec!["https://relay1.example.org".to_string()]);
    }

    /// Sync 2C: a target given by device **id** (not name) also resolves.
    #[tokio::test]
    async fn resolve_pairing_matches_target_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let target_key = DeviceKey::load_or_create_in(&tmp.path().join("target")).unwrap();
        let target_node = target_key.node_id();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", &target_key.pubkey_base64(), "athenaeum")
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/relay-map"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "relays": ["https://relay1.example.org"]
            })))
            .mount(&server)
            .await;

        let config = account_config(tmp.path(), &server.uri(), &["studio-1"]); // by id
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        let resolved = resolve_pairing(&config).await.expect("resolution by id succeeds");
        assert_eq!(resolved.peer, target_node, "matched the target by its device id");
    }

    /// Sync 2C: a `perseus` capability device is refused as a target — capture
    /// agents are send-only and cannot receive frames.
    #[tokio::test]
    async fn resolve_pairing_rejects_perseus_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let other_key = DeviceKey::load_or_create_in(&tmp.path().join("other")).unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("cam-1", "CaptureCam", &other_key.pubkey_base64(), "perseus")
            ])))
            .mount(&server)
            .await;

        let config = account_config(tmp.path(), &server.uri(), &["CaptureCam"]);
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        let err = resolve_pairing(&config)
            .await
            .expect_err("a perseus target must be refused");
        assert!(
            format!("{err:#}").contains("send-only"),
            "error should explain a capture agent cannot receive: {err:#}"
        );
    }

    /// Sync 2C: a target that is not in the account device list is an actionable
    /// error (no silent no-peer start).
    #[tokio::test]
    async fn resolve_pairing_unknown_target_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", "cHVia2V5", "athenaeum")
            ])))
            .mount(&server)
            .await;

        let config = account_config(tmp.path(), &server.uri(), &["Nonexistent"]);
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        let err = resolve_pairing(&config)
            .await
            .expect_err("an unknown target must error");
        assert!(
            format!("{err:#}").contains("was not found"),
            "error should say the target was not found: {err:#}"
        );
    }

    /// Sync 2C: with the hub unreachable, resolution falls back to the cached
    /// target peer + relay map (offline start) rather than failing.
    #[tokio::test]
    async fn resolve_pairing_offline_uses_cache() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        // A hub that errors every call (no mounts → 404).
        let down = MockServer::start().await;

        let config = account_config(tmp.path(), &down.uri(), &["Studio"]);
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        // Seed the cache as a prior successful resolution.
        let cached_node = [11u8; 32];
        let mut cache = PairingCache::default();
        cache.peer_node_id_hex = Some(node_id_hex(&cached_node));
        cache.relay_urls = vec!["https://relay-cached.example.org".into()];
        cache.save(tmp.path());

        let resolved = resolve_pairing(&config).await.expect("offline resolution uses cache");
        assert_eq!(resolved.peer, cached_node, "an unreachable hub falls back to the cached peer");
        assert!(
            matches!(resolved.relay_mode, RelayMode::Custom(_)),
            "the cached relay map is used offline"
        );
    }

    /// Review finding #1: signed in, the hub's relay map is explicitly empty,
    /// and there is no cached relay map — without the dev-only opt-in this must
    /// be an actionable error, NOT a silent fallback to iroh's public relays.
    #[tokio::test]
    async fn resolve_pairing_empty_relay_map_no_optin_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let target_key = DeviceKey::load_or_create_in(&tmp.path().join("target")).unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", &target_key.pubkey_base64(), "athenaeum")
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/relay-map"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "relays": [] })))
            .mount(&server)
            .await;

        let config = account_config(tmp.path(), &server.uri(), &["Studio"]); // allow_default_relays: false
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        let err = resolve_pairing(&config)
            .await
            .expect_err("an empty hub relay map with no cache and no opt-in must be refused");
        assert!(
            format!("{err:#}").contains("refusing"),
            "error should explain the refusal: {err:#}"
        );
    }

    /// Review finding #1: the SAME empty-relay-map scenario, but with
    /// `[account].allow_default_relays = true` — the dev-only opt-in allows
    /// falling back to iroh's public default relays.
    #[tokio::test]
    async fn resolve_pairing_empty_relay_map_with_optin_uses_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let target_key = DeviceKey::load_or_create_in(&tmp.path().join("target")).unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                device_json("studio-1", "Studio", &target_key.pubkey_base64(), "athenaeum")
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/relay-map"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "relays": [] })))
            .mount(&server)
            .await;

        let config = account_config_allowing_default_relays(tmp.path(), &server.uri(), &["Studio"]);
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        let resolved = resolve_pairing(&config)
            .await
            .expect("the dev-only opt-in must allow the default-relay fallback");
        assert!(matches!(resolved.relay_mode, RelayMode::Default));
    }

    /// Sync 2C: the dev-ticket route still resolves a peer with no account — the
    /// offline dev/test path is unchanged by the targets migration.
    #[tokio::test]
    async fn resolve_pairing_dev_ticket_without_account() {
        use athenaeum_core::sharing::iroh::{random_secret, BlobStore, IrohTransport};
        use athenaeum_core::sharing::SharingTransport;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();

        // A real endpoint ticket (relay-disabled, in-memory) the resolver parses.
        let transport = IrohTransport::new(random_secret(), RelayMode::Disabled, BlobStore::Memory)
            .await
            .unwrap();
        let info = transport.start().await.unwrap();
        let ticket_node = transport.node_id();
        transport.shutdown().await;

        let mut config = account_config(tmp.path(), "http://127.0.0.1:1", &[]);
        config.account = None;
        config.pairing_ticket = Some(info.pairing_ticket);

        let resolved = resolve_pairing(&config).await.expect("dev ticket resolves a peer");
        assert_eq!(resolved.peer, ticket_node, "peer comes from the pairing ticket");
        assert!(resolved.ticket.is_some(), "the ticket is carried through for add_peer_ticket");
    }
}
