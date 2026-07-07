//! Peer + relay resolution for personal sync (task M1).
//!
//! This module is the **single documented resolver** the plan calls for: both the
//! app's capture-role sender and the headless Perseus agent decide *who to sync
//! to* (and *over which relays*) through the functions here, so the resolution
//! order lives in exactly one place.
//!
//! # Peer resolution order ([`resolve_peer`])
//!
//! 1. **Account pairing.** When signed in as a `capture` device with a paired
//!    primary ([`AccountPairing`]), the primary's current pubkey is fetched from
//!    the account device list on the hub and decoded to a [`NodeId`]. If the hub
//!    is unreachable, the **last cached resolution** is used instead (the caller
//!    persists it on every successful resolve — see the `fresh` flag). A role/
//!    peer change on the hub therefore takes effect on the *next successful
//!    refresh*, not instantly on a cached start.
//! 2. **Dev ticket.** Behind the `sync.dev_ticket_pairing` flag, the peer is
//!    derived from a pasted iroh pairing ticket. Kept for tests / offline dev.
//! 3. **Neither** → [`PeerResolution::Disabled`] with an actionable reason the
//!    host surfaces as "sync not configured".
//!
//! # Relay resolution ([`resolve_relays`])
//!
//! When signed in, the transport's relays come from the hub's `GET /relay-map`.
//! The last successful map is cached for offline starts; with nothing available
//! the transport falls back to iroh's default relays ([`relay_mode_from_urls`]
//! maps an empty list to [`RelayMode::Default`]). Relays only perform NAT
//! traversal — package *content* is end-to-end over QUIC and hash-verified — so a
//! default-relay fallback is never a confidentiality concern.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use iroh::{RelayMap, RelayMode};
use iroh_tickets::endpoint::EndpointTicket;

use crate::account::HubClient;
use crate::sharing::types::NodeId;

/// Account-pairing inputs for a signed-in `capture` device with a paired primary.
/// The token is a bearer credential — never logged (only the `peer_device_id` is).
#[derive(Clone)]
pub struct AccountPairing {
    /// Base URL of the hub this device authenticates against.
    pub hub_url: String,
    /// This device's hub bearer token.
    pub token: String,
    /// The hub device id of this capture device's paired primary.
    pub peer_device_id: String,
}

impl std::fmt::Debug for AccountPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact the token; a `{:?}` must never leak the bearer credential.
        f.debug_struct("AccountPairing")
            .field("hub_url", &self.hub_url)
            .field("token", &"<redacted>")
            .field("peer_device_id", &self.peer_device_id)
            .finish()
    }
}

/// The resolved sync peer and how it was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerResolution {
    /// Resolved from the account. `fresh = true` means it came live from the hub
    /// (the caller should persist it as the new cache); `fresh = false` means the
    /// hub was unreachable and the last cached resolution was used.
    Account { peer: NodeId, fresh: bool },
    /// Resolved from a dev-flag pairing ticket.
    Ticket { peer: NodeId },
    /// No pairing is configured (or an account peer could not be resolved and no
    /// cache exists). `reason` is an actionable, user-facing status string.
    Disabled { reason: String },
}

/// The resolved relay URLs to build the transport with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayResolution {
    /// Relay URLs to use. Empty means "fall back to iroh's default relays".
    pub urls: Vec<String>,
    /// True when freshly fetched from the hub — the caller should persist these
    /// as the new relay-map cache.
    pub fresh: bool,
}

/// Resolve the sync peer following the documented order. `dev_ticket` is the
/// pasted ticket when the `sync.dev_ticket_pairing` flag is on; `cached_peer` is
/// the last successfully resolved peer (used only when the hub is unreachable).
pub async fn resolve_peer(
    account: Option<&AccountPairing>,
    dev_ticket: Option<&str>,
    cached_peer: Option<NodeId>,
) -> PeerResolution {
    if let Some(acc) = account {
        match fetch_primary_node_id(acc).await {
            Ok(peer) => {
                tracing::info!(
                    peer_device_id = %acc.peer_device_id,
                    "resolved sync peer from account device list"
                );
                return PeerResolution::Account { peer, fresh: true };
            }
            Err(error) => {
                if let Some(peer) = cached_peer {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        peer_device_id = %acc.peer_device_id,
                        "could not resolve sync peer from hub; using last cached resolution"
                    );
                    return PeerResolution::Account { peer, fresh: false };
                }
                return PeerResolution::Disabled {
                    reason: format!(
                        "signed in as a capture device but the paired primary could not be \
                         resolved and there is no cached peer yet: {error:#}"
                    ),
                };
            }
        }
    }

    if let Some(ticket) = dev_ticket {
        return match node_id_from_ticket(ticket) {
            Ok(peer) => PeerResolution::Ticket { peer },
            Err(error) => PeerResolution::Disabled {
                reason: format!("dev pairing ticket is not a valid iroh ticket: {error:#}"),
            },
        };
    }

    PeerResolution::Disabled {
        reason: "sync is not configured: sign in and set this machine's role to capture \
                 with a paired primary, or enable dev ticket pairing"
            .to_string(),
    }
}

/// Fetch the paired primary's node id from the account device list.
async fn fetch_primary_node_id(acc: &AccountPairing) -> Result<NodeId> {
    let client = HubClient::new(&acc.hub_url).map_err(|e| anyhow!("{e}"))?;
    let devices = client
        .list_devices(&acc.token)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    let primary = devices
        .iter()
        .find(|d| d.id == acc.peer_device_id)
        .ok_or_else(|| {
            anyhow!(
                "paired primary device {} is not in the account device list",
                acc.peer_device_id
            )
        })?;
    node_id_from_pubkey_b64(&primary.pubkey)
}

/// Resolve the relay URLs for the transport. When `account` is `Some`
/// (`hub_url`, `token`), the hub's relay map is fetched; on any failure the
/// `cached` map is used; with neither available the result is empty (the caller
/// then falls back to iroh's default relays under [`relay_mode_from_urls`]).
pub async fn resolve_relays(account: Option<(&str, &str)>, cached: &[String]) -> RelayResolution {
    if let Some((hub_url, token)) = account {
        match fetch_relay_map(hub_url, token).await {
            Ok(urls) if !urls.is_empty() => {
                tracing::info!(count = urls.len(), "resolved relay map from hub");
                return RelayResolution { urls, fresh: true };
            }
            Ok(_) => tracing::warn!("hub returned an empty relay map; using cached/default relays"),
            Err(error) => tracing::warn!(
                error = %format!("{error:#}"),
                "hub relay-map unavailable; using cached/default relays"
            ),
        }
    }
    if !cached.is_empty() {
        return RelayResolution {
            urls: cached.to_vec(),
            fresh: false,
        };
    }
    RelayResolution {
        urls: Vec::new(),
        fresh: false,
    }
}

/// Fetch the hub's advertised relay URLs.
async fn fetch_relay_map(hub_url: &str, token: &str) -> Result<Vec<String>> {
    let client = HubClient::new(hub_url).map_err(|e| anyhow!("{e}"))?;
    client.relay_map(token).await.map_err(|e| anyhow!("{e}"))
}

/// Build an iroh [`RelayMode`] from resolved relay URLs. An empty list (or any
/// unparsable URL) falls back to [`RelayMode::Default`] — iroh rejects an empty
/// custom relay map, and a default relay is always a safe NAT-traversal fallback.
pub fn relay_mode_from_urls(urls: &[String]) -> RelayMode {
    if urls.is_empty() {
        return RelayMode::Default;
    }
    match RelayMap::try_from_iter(urls.iter().map(String::as_str)) {
        Ok(map) if !map.is_empty() => RelayMode::Custom(map),
        Ok(_) => RelayMode::Default,
        Err(error) => {
            tracing::warn!(%error, "invalid relay url in relay map; using default relays");
            RelayMode::Default
        }
    }
}

/// Decode a base64 device pubkey (the hub's `pubkey` field) into a [`NodeId`].
pub fn node_id_from_pubkey_b64(pubkey_b64: &str) -> Result<NodeId> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64.trim())
        .context("decode device pubkey base64")?;
    let node: NodeId = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("device pubkey is {} bytes, expected 32", bytes.len()))?;
    Ok(node)
}

/// Derive a peer's [`NodeId`] from an iroh pairing ticket (an `EndpointTicket`).
pub fn node_id_from_ticket(ticket: &str) -> Result<NodeId> {
    let ticket: EndpointTicket = ticket
        .parse()
        .context("parse pairing ticket as an iroh endpoint ticket")?;
    Ok(*ticket.endpoint_addr().id.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A 32-byte pubkey with a recognizable pattern, plus its base64 the hub
    /// would return and the `NodeId` it must decode to.
    fn sample_primary_pubkey() -> ([u8; 32], String) {
        let raw = [7u8; 32];
        (raw, STANDARD.encode(raw))
    }

    fn devices_body(primary_id: &str, primary_pubkey_b64: &str) -> serde_json::Value {
        serde_json::json!([
            {
                "id": "capture-1", "name": "Mini PC", "pubkey": "b3RoZXI=",
                "role": "capture", "peerDeviceId": primary_id,
                "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null
            },
            {
                "id": primary_id, "name": "Studio Mac", "pubkey": primary_pubkey_b64,
                "role": "primary", "peerDeviceId": null,
                "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null
            }
        ])
    }

    /// A valid iroh pairing ticket string + the node id it encodes, built from a
    /// real (relay-disabled, in-memory) transport so the parse is exercised.
    async fn sample_ticket() -> (String, NodeId) {
        use crate::sharing::iroh::{random_secret, BlobStore, IrohTransport};
        use crate::sharing::SharingTransport;

        let transport = IrohTransport::new(random_secret(), RelayMode::Disabled, BlobStore::Memory)
            .await
            .unwrap();
        let info = transport.start().await.unwrap();
        let node = transport.node_id();
        transport.shutdown().await;
        (info.pairing_ticket, node)
    }

    #[tokio::test]
    async fn account_path_wins_over_dev_ticket() {
        let (raw, pubkey_b64) = sample_primary_pubkey();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(devices_body("primary-1", &pubkey_b64)))
            .mount(&server)
            .await;

        let (ticket, _ticket_node) = sample_ticket().await;
        let account = AccountPairing {
            hub_url: server.uri(),
            token: "tok".into(),
            peer_device_id: "primary-1".into(),
        };

        // Both account AND a dev ticket present: the account path must win, live.
        let res = resolve_peer(Some(&account), Some(&ticket), None).await;
        assert_eq!(
            res,
            PeerResolution::Account { peer: raw, fresh: true },
            "account resolution must win over the dev ticket and decode the primary pubkey"
        );
    }

    #[tokio::test]
    async fn hub_down_uses_cached_peer() {
        // A server with no /devices mock returns 404 → the client errors → the
        // resolver must fall back to the cached peer rather than fail.
        let server = MockServer::start().await;
        let cached = [42u8; 32];
        let account = AccountPairing {
            hub_url: server.uri(),
            token: "tok".into(),
            peer_device_id: "primary-1".into(),
        };

        let res = resolve_peer(Some(&account), None, Some(cached)).await;
        assert_eq!(
            res,
            PeerResolution::Account { peer: cached, fresh: false },
            "an unreachable hub must fall back to the last cached resolution (not fresh)"
        );
    }

    #[tokio::test]
    async fn hub_down_no_cache_is_disabled() {
        let server = MockServer::start().await;
        let account = AccountPairing {
            hub_url: server.uri(),
            token: "tok".into(),
            peer_device_id: "primary-1".into(),
        };
        let res = resolve_peer(Some(&account), None, None).await;
        assert!(
            matches!(res, PeerResolution::Disabled { .. }),
            "no fresh resolution and no cache must disable sync, got {res:?}"
        );
    }

    #[tokio::test]
    async fn dev_ticket_fallback_when_no_account() {
        let (ticket, ticket_node) = sample_ticket().await;
        let res = resolve_peer(None, Some(&ticket), None).await;
        assert_eq!(
            res,
            PeerResolution::Ticket { peer: ticket_node },
            "with no account the dev ticket path resolves the ticket's node id"
        );
    }

    #[tokio::test]
    async fn nothing_configured_is_disabled() {
        let res = resolve_peer(None, None, None).await;
        assert!(
            matches!(res, PeerResolution::Disabled { .. }),
            "no account and no ticket must disable sync, got {res:?}"
        );
    }

    #[tokio::test]
    async fn relay_map_fetched_from_hub_then_cached_round_trips_offline() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/relay-map"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "relays": ["https://relay1.example.org", "https://relay2.example.org"]
            })))
            .mount(&server)
            .await;

        // Signed in → fetched fresh from the hub.
        let fresh = resolve_relays(Some((server.uri().as_str(), "tok")), &[]).await;
        assert_eq!(
            fresh.urls,
            vec![
                "https://relay1.example.org".to_string(),
                "https://relay2.example.org".to_string()
            ]
        );
        assert!(fresh.fresh, "a hub-sourced relay map is fresh (caller persists it)");

        // Offline start (no account) → the cached map is used, not fresh.
        let offline = resolve_relays(None, &fresh.urls).await;
        assert_eq!(offline.urls, fresh.urls, "cached relay map round-trips offline");
        assert!(!offline.fresh);

        // Hub down (404) but a cache exists → cache is used.
        let down = MockServer::start().await;
        let cached_fallback = resolve_relays(Some((down.uri().as_str(), "tok")), &fresh.urls).await;
        assert_eq!(cached_fallback.urls, fresh.urls);
        assert!(!cached_fallback.fresh);
    }

    #[test]
    fn relay_mode_maps_empty_to_default_and_urls_to_custom() {
        assert!(matches!(relay_mode_from_urls(&[]), RelayMode::Default));
        let custom = relay_mode_from_urls(&["https://relay1.example.org".to_string()]);
        assert!(matches!(custom, RelayMode::Custom(_)), "a real relay url yields a custom map");
        // A garbage url can't parse → safe fallback to default relays.
        assert!(matches!(relay_mode_from_urls(&["not a url".to_string()]), RelayMode::Default));
    }

    #[test]
    fn pubkey_b64_round_trips_to_node_id() {
        let (raw, b64) = sample_primary_pubkey();
        assert_eq!(node_id_from_pubkey_b64(&b64).unwrap(), raw);
        assert!(node_id_from_pubkey_b64("short").is_err(), "wrong length must error");
    }

    #[test]
    fn account_pairing_debug_redacts_token() {
        let acc = AccountPairing {
            hub_url: "https://h".into(),
            token: "super-secret-token".into(),
            peer_device_id: "p".into(),
        };
        let dbg = format!("{acc:?}");
        assert!(!dbg.contains("super-secret-token"), "token leaked into Debug: {dbg}");
    }
}
