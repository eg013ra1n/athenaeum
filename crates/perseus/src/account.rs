//! Perseus account pairing (task M1).
//!
//! Perseus pairs with a primary either through a hub **account** (a `perseus
//! login` device token) or a raw **pairing ticket**. This module owns:
//!
//! - [`login`] — the interactive CLI OTP flow (email → code) using the shared
//!   [`athenaeum_core::account`] hub client. The device token is written to a
//!   0600 file in `data_dir` (never the TOML), and this device registers itself
//!   as a `capture` device paired to the account's primary.
//! - [`resolve_pairing`] — the run-time resolution the [`crate::run::Agent`]
//!   uses: it feeds the account/ticket inputs into the **single** shared resolver
//!   ([`athenaeum_core::sync::pairing`]) so Perseus and the app agree on the
//!   order (account → ticket → error) and both get the hub relay map with an
//!   offline cache fallback.
//! - [`PairingCache`] — the persisted `perseus login` result + last successful
//!   resolutions (`data_dir/pairing_cache.json`), so an offline restart still
//!   resolves. The token is NOT in it (that is the 0600 [`TokenStore`] file).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use athenaeum_core::account::{DeviceKey, DeviceRole, HubClient, TokenStore};
use athenaeum_core::sharing::types::NodeId;
use athenaeum_core::sync::pairing::{self, AccountPairing};
use athenaeum_core::sync::PeerResolution;
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
    /// The paired primary's hub device id (config override, or auto-picked).
    #[serde(default)]
    pub primary_device_id: Option<String>,
    /// Last successfully resolved peer node id (64-char lowercase hex).
    #[serde(default)]
    pub peer_node_id_hex: Option<String>,
    /// Last successfully resolved relay URLs.
    #[serde(default)]
    pub relay_urls: Vec<String>,
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
pub struct ResolvedPairing {
    /// The peer to send to.
    pub peer: NodeId,
    /// The relay mode the transport should bind with.
    pub relay_mode: RelayMode,
    /// `Some` only on the dev-ticket path — the caller registers the peer's
    /// dialable address from this ticket (the account path resolves by node id).
    pub ticket: Option<String>,
}

/// Interactive `perseus login`: request an OTP for the account email, verify it,
/// store the device token (0600 file), and register this device as a `capture`
/// device paired to the account's primary. Requires an `[account]` config table.
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

    let primary_id = verify_and_register(config, &account, &client, &email, &code).await?;
    match &primary_id {
        Some(p) => println!("Signed in. Registered as a capture device paired to primary {p}."),
        None => println!(
            "Signed in, but no primary device was found. Set the primary role on your \
             main Athenaeum machine (Settings → Account), then re-run `perseus login` \
             or set [account].primary_device_id."
        ),
    }
    Ok(())
}

/// The non-interactive core of [`login`]: verify the OTP, store the token, and
/// register this device as a `capture` device paired to the account's primary.
/// Returns the resolved primary id (`None` when the account has no primary yet).
/// Split out so the hub interactions are testable without prompting for stdin.
async fn verify_and_register(
    config: &Config,
    account: &AccountConfig,
    client: &HubClient,
    email: &str,
    code: &str,
) -> Result<Option<String>> {
    // The ONE device identity: the same key file the run-time transport binds.
    let key = DeviceKey::load_or_create(&config.device_key_path())
        .context("load or create device key")?;
    let resp = client
        .verify(email, code, &key.pubkey_base64(), &device_name())
        .await
        .map_err(|e| anyhow!("verify code: {e}"))?;

    // Token → 0600 file store (headless: no OS keychain).
    token_store(config, account)
        .store(&resp.device_token)
        .context("store device token")?;

    // Resolve the primary to pair with: config override, else auto-pick the
    // account's single primary.
    let devices = client
        .list_devices(&resp.device_token)
        .await
        .map_err(|e| anyhow!("list account devices: {e}"))?;
    let primary_id = match account.primary_device_id.clone() {
        Some(id) => Some(id),
        None => auto_pick_primary(&devices)?,
    };

    // Register this device as capture, paired to the primary when one is known.
    if let Some(primary) = &primary_id {
        client
            .set_role(&resp.device_token, &resp.device_id, DeviceRole::Capture, Some(primary))
            .await
            .map_err(|e| anyhow!("register as capture device: {e}"))?;
    }

    let mut cache = PairingCache::load(&config.data_dir);
    cache.device_id = Some(resp.device_id.clone());
    cache.primary_device_id = primary_id.clone();
    cache.save(&config.data_dir);

    tracing::info!(device_id = %resp.device_id, "perseus signed in");
    Ok(primary_id)
}

/// Resolve the peer + relays for an agent start, via the shared resolver.
///
/// Order (task M1): account pairing (signed-in capture device → primary from the
/// hub device list, with the cached peer as the offline fallback) → dev ticket →
/// error. Successful resolutions refresh the offline cache.
pub async fn resolve_pairing(config: &Config) -> Result<ResolvedPairing> {
    let mut cache = PairingCache::load(&config.data_dir);

    let account_inputs = build_account_pairing(config, &mut cache).await?;
    let dev_ticket = config
        .pairing_ticket
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let cached_peer = cache
        .peer_node_id_hex
        .as_deref()
        .and_then(node_id_from_hex);

    let resolution = pairing::resolve_peer(account_inputs.as_ref(), dev_ticket, cached_peer).await;

    // Relays: hub map when signed in, cached map otherwise, iroh default as a
    // last resort. Cache a fresh map for the next offline start.
    let relay_account = account_inputs
        .as_ref()
        .map(|a| (a.hub_url.as_str(), a.token.as_str()));
    let relays = pairing::resolve_relays(relay_account, &cache.relay_urls).await;
    if relays.fresh {
        cache.relay_urls = relays.urls.clone();
    }
    let relay_mode = pairing::relay_mode_from_urls(&relays.urls);

    let resolved = match resolution {
        PeerResolution::Account { peer, fresh } => {
            if fresh {
                cache.peer_node_id_hex = Some(node_id_hex(&peer));
            }
            ResolvedPairing { peer, relay_mode, ticket: None }
        }
        PeerResolution::Ticket { peer } => ResolvedPairing {
            peer,
            relay_mode,
            ticket: dev_ticket.map(str::to_string),
        },
        PeerResolution::Disabled { reason } => {
            cache.save(&config.data_dir);
            bail!("cannot resolve a sync peer — {reason}");
        }
    };
    cache.save(&config.data_dir);
    Ok(resolved)
}

/// Build the account-pairing inputs, or `None` when Perseus is not signed in
/// (no `[account]` table or no stored token). Resolves the paired primary id
/// from config → cache → auto-pick (a hub call, cached on success).
async fn build_account_pairing(
    config: &Config,
    cache: &mut PairingCache,
) -> Result<Option<AccountPairing>> {
    let Some(account) = &config.account else {
        return Ok(None);
    };
    let Some(token) = token_store(config, account)
        .load()
        .context("load device token")?
    else {
        return Ok(None);
    };

    let peer_device_id = match account
        .primary_device_id
        .clone()
        .or_else(|| cache.primary_device_id.clone())
    {
        Some(id) => id,
        None => {
            // No configured/cached primary: auto-pick from the hub. If the hub is
            // unreachable here we can't proceed — an actionable error is better
            // than a silent no-peer start.
            let client = HubClient::new(&account.hub_url).map_err(|e| anyhow!("{e}"))?;
            let devices = client.list_devices(&token).await.map_err(|e| {
                anyhow!(
                    "could not auto-pick the primary device (set [account].primary_device_id, \
                     or run once while the hub is reachable): {e}"
                )
            })?;
            let id = auto_pick_primary(&devices)?
                .ok_or_else(|| anyhow!("no primary device in the account — set the primary role on your main machine first"))?;
            cache.primary_device_id = Some(id.clone());
            id
        }
    };

    Ok(Some(AccountPairing {
        hub_url: account.hub_url.clone(),
        token,
        peer_device_id,
    }))
}

/// Pick the account's single `primary` device id: `Some(id)` for exactly one,
/// `Ok(None)` for none, an error for more than one (ambiguous — must be pinned).
fn auto_pick_primary(
    devices: &[athenaeum_core::account::AccountDevice],
) -> Result<Option<String>> {
    let primaries: Vec<&athenaeum_core::account::AccountDevice> = devices
        .iter()
        .filter(|d| d.role == Some(DeviceRole::Primary))
        .collect();
    match primaries.as_slice() {
        [only] => Ok(Some(only.id.clone())),
        [] => Ok(None),
        _ => bail!(
            "the account has more than one primary device — set [account].primary_device_id \
             to choose which one this capture node pairs with"
        ),
    }
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

/// Best-effort device name for the hub device list.
fn device_name() -> String {
    for var in ["ATHENAEUM_DEVICE_NAME", "HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "Perseus".to_string()
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

/// Lowercase-hex (64 char) rendering of a node id.
fn node_id_hex(id: &NodeId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse the 64-char lowercase-hex node id form; `None` on any malformed input.
fn node_id_from_hex(s: &str) -> Option<NodeId> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
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
        assert_eq!(node_id_from_hex(&hex), Some(id));
        assert_eq!(node_id_from_hex("too-short"), None);
    }

    #[test]
    fn pairing_cache_round_trips_relay_map() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = PairingCache::load(dir.path());
        assert_eq!(cache, PairingCache::default(), "missing file loads an empty cache");

        cache.device_id = Some("dev-1".into());
        cache.primary_device_id = Some("primary-1".into());
        cache.peer_node_id_hex = Some(node_id_hex(&[3u8; 32]));
        cache.relay_urls = vec!["https://relay1.example.org".into()];
        cache.save(dir.path());

        let reloaded = PairingCache::load(dir.path());
        assert_eq!(reloaded, cache, "the cache round-trips through disk");
        assert_eq!(reloaded.relay_urls, vec!["https://relay1.example.org".to_string()]);
    }

    use crate::config::{Config, Mode, RetentionConfig};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A minimal `[account]`-based Config over a fresh data dir. `capture_dir`
    /// need not exist here — these tests never call `validate()`, only the
    /// account helpers.
    fn account_config(data_dir: &Path, hub_url: &str, primary: Option<&str>) -> Config {
        Config {
            capture_dir: data_dir.join("capture"),
            data_dir: data_dir.to_path_buf(),
            pairing_ticket: None,
            account: Some(AccountConfig {
                hub_url: hub_url.to_string(),
                email: Some("me@example.com".into()),
                primary_device_id: primary.map(str::to_string),
            }),
            mode: Mode::Auto,
            retention: RetentionConfig::default(),
            stability_secs: 1,
            poll_interval_secs: 1,
        }
    }

    /// Task M1: the non-interactive login core verifies the code, stores the
    /// token (0600 file), auto-picks the account's single primary, and registers
    /// this device as a `capture` device paired to it.
    #[tokio::test]
    async fn login_verify_and_register_stores_token_and_registers_capture() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceToken": "tok-secret-xyz",
                "deviceId": "capture-dev",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "primary-1", "name": "Studio", "pubkey": "cHVia2V5",
                    "role": "primary", "peerDeviceId": null,
                    "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null
                }
            ])))
            .mount(&server)
            .await;
        // The role call MUST arrive as capture paired to primary-1 (body matcher);
        // if it doesn't match, the mock 404s and the flow errors — proving the
        // capture registration happened with the right arguments.
        Mock::given(method("POST"))
            .and(path("/api/v1/devices/capture-dev/role"))
            .and(body_partial_json(serde_json::json!({
                "role": "capture", "peerDeviceId": "primary-1"
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let account = AccountConfig {
            hub_url: server.uri(),
            email: Some("me@example.com".into()),
            primary_device_id: None, // exercise auto-pick
        };
        let config = account_config(tmp.path(), &server.uri(), None);
        let client = HubClient::new(&account.hub_url).unwrap();

        let primary = verify_and_register(&config, &account, &client, "me@example.com", "123456")
            .await
            .expect("login core succeeds");
        assert_eq!(primary.as_deref(), Some("primary-1"), "auto-picked the single primary");

        // Token landed in the 0600 file store.
        assert_eq!(
            token_store(&config, &account).load().unwrap().as_deref(),
            Some("tok-secret-xyz"),
            "the device token must be stored"
        );
        // Cache recorded the login result.
        let cache = PairingCache::load(tmp.path());
        assert_eq!(cache.device_id.as_deref(), Some("capture-dev"));
        assert_eq!(cache.primary_device_id.as_deref(), Some("primary-1"));
    }

    /// Task M1: run-time resolution picks the primary from the hub device list,
    /// decodes its pubkey to the peer node id, resolves the relay map, and caches
    /// both for the next offline start.
    #[tokio::test]
    async fn resolve_pairing_resolves_peer_and_relays_from_account() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        // A real device key gives us a pubkey the resolver must decode back to a
        // node id — no hand-rolled base64.
        let primary_key = DeviceKey::load_or_create_in(&tmp.path().join("primary")).unwrap();
        let primary_pubkey = primary_key.pubkey_base64();
        let primary_node = primary_key.node_id();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "primary-1", "name": "Studio", "pubkey": primary_pubkey,
                    "role": "primary", "peerDeviceId": null,
                    "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null
                }
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

        let config = account_config(tmp.path(), &server.uri(), Some("primary-1"));
        // Pre-store a token → the resolver treats this as signed in.
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        let resolved = resolve_pairing(&config).await.expect("resolution succeeds");
        assert_eq!(resolved.peer, primary_node, "peer decodes from the primary's pubkey");
        assert!(resolved.ticket.is_none(), "account path resolves by node id, not a ticket");
        assert!(
            matches!(resolved.relay_mode, RelayMode::Custom(_)),
            "the hub relay map yields a custom relay mode"
        );

        // Offline cache was refreshed with both the peer and the relay map.
        let cache = PairingCache::load(tmp.path());
        assert_eq!(cache.peer_node_id_hex, Some(node_id_hex(&primary_node)));
        assert_eq!(cache.relay_urls, vec!["https://relay1.example.org".to_string()]);
    }

    /// Task M1: with the hub unreachable, resolution falls back to the cached
    /// peer + relay map (offline start) rather than failing.
    #[tokio::test]
    async fn resolve_pairing_offline_uses_cache() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        // A hub that errors every call (no mounts → 404).
        let down = MockServer::start().await;

        let config = account_config(tmp.path(), &down.uri(), Some("primary-1"));
        token_store(&config, config.account.as_ref().unwrap())
            .store("tok-signed-in")
            .unwrap();

        // Seed the cache as a prior successful resolution.
        let cached_node = [11u8; 32];
        let mut cache = PairingCache::default();
        cache.primary_device_id = Some("primary-1".into());
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

    #[test]
    fn auto_pick_primary_handles_zero_one_many() {
        use athenaeum_core::account::AccountDevice;
        let dev = |id: &str, role: Option<DeviceRole>| AccountDevice {
            id: id.into(),
            name: id.into(),
            pubkey: "cHVia2V5".into(),
            role,
            peer_device_id: None,
            created_at: "2026-07-01T00:00:00Z".into(),
            last_seen_at: None,
        };
        assert_eq!(auto_pick_primary(&[]).unwrap(), None);
        assert_eq!(
            auto_pick_primary(&[dev("p", Some(DeviceRole::Primary)), dev("c", Some(DeviceRole::Capture))])
                .unwrap(),
            Some("p".to_string())
        );
        assert!(
            auto_pick_primary(&[
                dev("p1", Some(DeviceRole::Primary)),
                dev("p2", Some(DeviceRole::Primary))
            ])
            .is_err(),
            "two primaries is ambiguous and must error"
        );
    }
}
