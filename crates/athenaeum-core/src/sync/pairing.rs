//! Relay + node-id resolution helpers for personal sync (Sync 2C mesh model).
//!
//! In the mesh model there is no single "primary" a device pairs to: every
//! Athenaeum install is a full peer, a Perseus agent is send-only, and a sender
//! chooses **explicit targets** (device names/ids resolved against the account
//! device list). Target → [`NodeId`] resolution therefore lives with each host
//! (the app's sender, Perseus's `account` module), decoding a device's pubkey
//! via [`node_id_from_pubkey_b64`]. What remains shared here is the relay-map
//! resolution both hosts still go through, plus the small node-id/address
//! helpers ([`node_id_from_ticket`], [`peer_addr_with_relays`]).
//!
//! # Relay resolution ([`resolve_relays`] + [`relay_mode_for`])
//!
//! When signed in, the transport's relays come from the hub's `GET /relay-map`.
//! The last successful map is cached for offline starts. What happens with
//! *nothing* resolved (empty hub map AND no cache) is gated (review finding #1):
//! [`relay_mode_for`] takes an explicit `allow_default` opt-in from the caller
//! (`sync.dev_ticket_pairing` for the app, `[account].allow_default_relays` for
//! Perseus, both dev-only and default `false`) and only THEN falls back to
//! [`RelayMode::Default`] — otherwise it is an actionable error. A production,
//! signed-in agent must never silently start riding iroh's public n0 relays just
//! because the hub's relay map is empty or unreachable; that is exactly the kind
//! of surprise infrastructure dependence a misconfigured hub could cause
//! invisibly. (Package *content* is end-to-end hash-verified regardless of which
//! relay carries it, so this gate is about avoiding surprise dependence, not
//! confidentiality — see [`crate::sharing::iroh`].)

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use iroh::{RelayMap, RelayMode};
use iroh_tickets::endpoint::EndpointTicket;

use crate::account::HubClient;
use crate::sharing::types::NodeId;

/// The resolved relay URLs to build the transport with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayResolution {
    /// Relay URLs to use. Empty means "fall back to iroh's default relays".
    pub urls: Vec<String>,
    /// True when freshly fetched from the hub — the caller should persist these
    /// as the new relay-map cache.
    pub fresh: bool,
}

/// Resolve the relay URLs for the transport. When `account` is `Some`
/// (`hub_url`, `token`), the hub's relay map is fetched; on any failure OR an
/// empty hub answer, the `cached` map is used instead (still `fresh: false` —
/// only a live, non-empty hub answer is fresh); with neither available the
/// result is empty. What an empty result means for the transport (refuse vs.
/// fall back to iroh's defaults) is [`relay_mode_for`]'s call, not this
/// function's — this layer only resolves *what* the relay list is.
pub async fn resolve_relays(account: Option<(&str, &str)>, cached: &[String]) -> RelayResolution {
    if let Some((hub_url, token)) = account {
        match fetch_relay_map(hub_url, token).await {
            Ok(urls) if !urls.is_empty() => {
                tracing::info!(count = urls.len(), "resolved relay map from hub");
                return RelayResolution { urls, fresh: true };
            }
            Ok(_) => tracing::warn!("hub returned an empty relay map; falling back to the cache, if any"),
            Err(error) => tracing::warn!(
                error = %format!("{error:#}"),
                "hub relay-map unavailable; falling back to the cache, if any"
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

/// Build an iroh [`RelayMode`] from resolved relay URLs, honoring the
/// "iroh's public default relays are dev-only" gate (review finding #1).
///
/// `allow_default` is the caller's **explicit, dev-only opt-in**
/// (`sync.dev_ticket_pairing` for the app, `[account].allow_default_relays` for
/// Perseus — both default `false`). With a non-empty, parseable relay list this
/// is irrelevant (always [`RelayMode::Custom`]); with nothing usable resolved
/// (empty list, or every URL unparsable):
/// - `allow_default = true` → [`RelayMode::Default`] (a logged, deliberate dev
///   fallback).
/// - `allow_default = false` → an actionable `Err` — a signed-in production
///   agent must not silently start riding iroh's public n0 relays just because
///   the hub's relay map came back empty/unreachable with no cache; that is a
///   hub misconfiguration that deserves a loud failure, not silent public
///   infrastructure dependence.
pub fn relay_mode_for(urls: &[String], allow_default: bool) -> Result<RelayMode, String> {
    if urls.is_empty() {
        return default_or_refuse(
            allow_default,
            "no relays were resolved (the hub returned none, or is unreachable, and no cached \
             relay map exists)",
        );
    }
    match RelayMap::try_from_iter(urls.iter().map(String::as_str)) {
        Ok(map) if !map.is_empty() => Ok(RelayMode::Custom(map)),
        Ok(_) => default_or_refuse(allow_default, "the resolved relay map was empty after parsing"),
        Err(error) => {
            tracing::warn!(%error, "invalid relay url in relay map");
            default_or_refuse(allow_default, &format!("invalid relay url in the relay map: {error}"))
        }
    }
}

/// Shared tail of [`relay_mode_for`]'s two "nothing usable" branches: allow the
/// dev-only default-relay fallback, or refuse with an actionable message.
fn default_or_refuse(allow_default: bool, why: &str) -> Result<RelayMode, String> {
    if allow_default {
        tracing::warn!(
            reason = %why,
            "no usable relay map; falling back to iroh's public default relays (dev-only opt-in)"
        );
        return Ok(RelayMode::Default);
    }
    Err(format!(
        "{why}; refusing to fall back to iroh's public default relays in production — check the \
         hub's relay configuration, or enable the dev-only opt-in (sync.dev_ticket_pairing / \
         [account].allow_default_relays) to proceed anyway"
    ))
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

/// Construct the dialable [`iroh::EndpointAddr`] for an account-resolved peer:
/// the bare node id plus the SAME relay URL(s) resolved for our own endpoint
/// (fix-review, production bug: account-mode dial failure).
///
/// `IrohTransport` binds with `presets::Minimal` — no discovery services, by
/// design (task A5) — so a bare-node-id `EndpointAddr` (no relay, no direct
/// addresses) is undialable: `endpoint.connect()` fails instantly with "No
/// addressing information available". The dev-ticket path never hit this
/// because an `EndpointTicket` embeds its holder's addresses directly; account
/// pairing only ever resolves a peer's *identity* (its pubkey from the hub's
/// device list), never its address. Devices on the same hub account share the
/// same published relay set (`GET /relay-map`), so attaching OUR OWN resolved
/// relay URL(s) to the peer's address is the correct, minimal dial hint — no
/// separate address-exchange channel needed. This is also why the cached-relay
/// offline path (`SYNC_CACHED_RELAYS` / Perseus's `pairing_cache.json`) still
/// dials correctly: callers pass through whatever `resolve_relays` resolved,
/// fresh or cached, so the cache fallback composes for free.
///
/// An unparsable URL is logged and skipped (never fails the whole
/// resolution — the caller may still have other usable relays); an empty
/// `relay_urls` yields a bare `EndpointAddr` (same as before this fix — still
/// undialable, but no worse, and every real caller has a non-empty resolved
/// list by the time it gets here).
pub fn peer_addr_with_relays(peer: NodeId, relay_urls: &[String]) -> Result<iroh::EndpointAddr> {
    let id = iroh::EndpointId::from_bytes(&peer).map_err(|e| anyhow!("invalid peer node id: {e}"))?;
    let mut addr = iroh::EndpointAddr::new(id);
    for url in relay_urls {
        match url.parse::<iroh::RelayUrl>() {
            Ok(relay_url) => addr = addr.with_relay_url(relay_url),
            Err(error) => {
                tracing::warn!(%url, %error, "invalid relay url in peer address hint; skipping")
            }
        }
    }
    Ok(addr)
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
    fn relay_mode_for_urls_is_always_custom_regardless_of_optin() {
        let urls = vec!["https://relay1.example.org".to_string()];
        assert!(
            matches!(relay_mode_for(&urls, false), Ok(RelayMode::Custom(_))),
            "a real relay url yields a custom map even without the opt-in"
        );
        assert!(matches!(relay_mode_for(&urls, true), Ok(RelayMode::Custom(_))));
    }

    /// Review finding #1: with nothing usable resolved (empty list) and no
    /// dev-only opt-in, falling back to iroh's public default relays must be
    /// REFUSED (an actionable `Err`), not silently allowed.
    #[test]
    fn relay_mode_for_empty_without_optin_is_refused() {
        let err = relay_mode_for(&[], false).expect_err("no opt-in must refuse the default fallback");
        assert!(err.contains("refusing"), "error should explain the refusal: {err}");
    }

    /// Review finding #1: the SAME empty input with the dev-only opt-in set is
    /// allowed to fall back to `RelayMode::Default`.
    #[test]
    fn relay_mode_for_empty_with_optin_allows_default() {
        assert!(matches!(relay_mode_for(&[], true), Ok(RelayMode::Default)));
    }

    /// An unparsable URL is treated the same as "nothing usable" — gated by the
    /// same opt-in, never a silent default.
    #[test]
    fn relay_mode_for_unparsable_url_is_gated_the_same_way() {
        let bad = vec!["not a url".to_string()];
        assert!(relay_mode_for(&bad, false).is_err(), "unparsable url without opt-in must refuse");
        assert!(matches!(relay_mode_for(&bad, true), Ok(RelayMode::Default)));
    }

    /// Review finding #1 composed end to end: signed-in resolution where the
    /// hub answers with an explicitly empty relay list (`{"relays": []}`).
    /// - no cache, no opt-in → refused.
    /// - no cache, WITH opt-in → default allowed.
    /// - a cache present → the cache is used regardless of the opt-in (cached
    ///   relays are not "riding public infrastructure blind", they are a
    ///   previously-good hub answer).
    #[tokio::test]
    async fn signed_in_empty_hub_relay_map_gating_matrix() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/relay-map"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "relays": [] })))
            .mount(&server)
            .await;
        let uri = server.uri();
        let account = Some((uri.as_str(), "tok"));

        // No cache, no opt-in → refused.
        let res = resolve_relays(account, &[]).await;
        assert!(res.urls.is_empty());
        assert!(!res.fresh);
        assert!(
            relay_mode_for(&res.urls, false).is_err(),
            "an empty hub map with no cache and no opt-in must be refused"
        );

        // No cache, WITH opt-in → default allowed.
        assert!(matches!(relay_mode_for(&res.urls, true), Ok(RelayMode::Default)));

        // A cache present → used, not fresh, and usable without the opt-in.
        let cached = vec!["https://relay-cache.example.org".to_string()];
        let res_cached = resolve_relays(account, &cached).await;
        assert_eq!(res_cached.urls, cached, "an empty hub map falls back to the cache");
        assert!(!res_cached.fresh);
        assert!(
            matches!(relay_mode_for(&res_cached.urls, false), Ok(RelayMode::Custom(_))),
            "a cached relay map must be usable without the dev-only opt-in"
        );
    }

    #[test]
    fn pubkey_b64_round_trips_to_node_id() {
        let (raw, b64) = sample_primary_pubkey();
        assert_eq!(node_id_from_pubkey_b64(&b64).unwrap(), raw);
        assert!(node_id_from_pubkey_b64("short").is_err(), "wrong length must error");
    }

    // ── peer_addr_with_relays (fix-review: account-mode dial failure) ───────
    //
    // Production bug: account-mode pairing resolved the peer to a bare node id
    // and handed it straight to the transport, which — bound with no discovery
    // services (`presets::Minimal`) — could not dial it:
    // "connect sync control channel: No addressing information available: All
    // address lookup services failed or produced no results". The negative
    // test pinning that exact failure lives in
    // `sharing::iroh::tests::bare_node_id_without_a_peer_address_is_undialable`
    // (needs two real endpoints); these test the pure address-construction seam.

    /// A valid ed25519 public key's raw bytes for a test peer — NOT an arbitrary
    /// byte pattern. `EndpointId::from_bytes` (used internally by
    /// `peer_addr_with_relays`) validates the bytes decode to a real curve
    /// point; a hand-picked pattern like `[7u8; 32]` fails that validation
    /// (the crate's OTHER pairing tests get away with such patterns only
    /// because they never construct a real `PublicKey`/`EndpointId` from them —
    /// they just compare raw bytes for equality).
    fn valid_peer_bytes() -> [u8; 32] {
        *iroh::SecretKey::generate().public().as_bytes()
    }

    /// A URL's canonical round-tripped form (what `RelayUrl::to_string()`
    /// actually produces) — `url::Url` normalizes a bare-authority URL like
    /// `https://relay1.example.org` to `https://relay1.example.org/` (trailing
    /// slash). Tests compare against this, not the raw input string.
    fn normalized(url: &str) -> String {
        url.parse::<iroh::RelayUrl>().unwrap().to_string()
    }

    /// Required test #1: a resolved relay list `[X]` produces an `EndpointAddr`
    /// whose `relay_urls()` contains exactly `X`, with the peer's identity
    /// preserved.
    #[test]
    fn peer_addr_with_relays_attaches_the_resolved_relay_url() {
        let peer = valid_peer_bytes();
        let addr = peer_addr_with_relays(peer, &["https://relay1.example.org".to_string()]).unwrap();

        assert_eq!(addr.id.as_bytes(), &peer, "the peer's identity is preserved");
        let urls: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
        assert_eq!(
            urls,
            vec![normalized("https://relay1.example.org")],
            "the resolved relay url must be attached as a dial hint"
        );
    }

    /// Multiple resolved relays all get attached (iroh's `EndpointAddr` supports
    /// more than one; "attach all" per the fix-review guidance).
    #[test]
    fn peer_addr_with_relays_attaches_multiple_urls() {
        let peer = valid_peer_bytes();
        let urls_in = vec![
            "https://relay1.example.org".to_string(),
            "https://relay2.example.org".to_string(),
        ];
        let addr = peer_addr_with_relays(peer, &urls_in).unwrap();
        let mut urls_out: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
        urls_out.sort();
        let mut expected: Vec<String> = urls_in.iter().map(|u| normalized(u)).collect();
        expected.sort();
        assert_eq!(urls_out, expected, "every resolved relay url is attached");
    }

    /// An unparsable relay URL is logged and skipped — never fails the whole
    /// resolution — while any other, valid URL in the same list is still
    /// attached.
    #[test]
    fn peer_addr_with_relays_skips_invalid_urls_but_keeps_valid_ones() {
        let peer = valid_peer_bytes();
        let addr = peer_addr_with_relays(
            peer,
            &["not a url".to_string(), "https://relay2.example.org".to_string()],
        )
        .unwrap();
        let urls: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
        assert_eq!(urls, vec![normalized("https://relay2.example.org")]);
    }

    /// An empty relay list yields a bare `EndpointAddr` — the same
    /// (undialable-without-discovery) shape as before this fix. Documents that
    /// the fix cannot manufacture connectivity out of nothing; it only stops
    /// throwing away a relay hint the caller already resolved.
    #[test]
    fn peer_addr_with_relays_empty_list_yields_bare_addr() {
        let peer = valid_peer_bytes();
        let addr = peer_addr_with_relays(peer, &[]).unwrap();
        assert!(addr.is_empty(), "no relay urls -> bare addr, same as pre-fix");
    }
}
