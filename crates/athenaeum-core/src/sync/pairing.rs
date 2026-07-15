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

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use iroh::{RelayMap, RelayMode};
use iroh_tickets::endpoint::EndpointTicket;

use crate::account::{EndpointAddrReport, HubClient};
use crate::sharing::iroh::node::SharedIrohNode;
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

/// Construct the dialable [`iroh::EndpointAddr`] for a peer, preferring the
/// peer's OWN hub-reported address (finding H1, iroh hardening T7) over the old
/// guess-from-our-own-relay-set hint ([`peer_addr_with_relays`]).
///
/// - **`reported` present** (the peer has PUT an `endpointAddr` the hub served
///   us): the peer's real `home_relay_url` AND `our_relays` are BOTH attached to
///   the one `EndpointAddr` (the "merged-hint form" — iroh accepts multiple relay
///   urls in one addr and tries them, so if the peer's own relay is momentarily
///   unreachable the dial can still find it via a shared relay, without any
///   per-call-site failure-retry plumbing). iroh stores the addrs in a sorted set,
///   so there is no wire ordering to rely on; both are simply present, de-duped.
///   Its `direct_addrs` are
///   attached **only when `!cross_account`** (S1): a same-account device may take
///   a LAN/direct shortcut, but a cross-account collaborator (a collab holder)
///   must never receive another account's private addresses — the helper enforces
///   this regardless of what the hub served, so a hub bug can't leak them.
/// - **`reported` absent** (an older hub, or a device that never reported): falls
///   back to exactly [`peer_addr_with_relays`] — the pre-T7 behavior, byte for
///   byte, so nothing regresses.
///
/// Unparsable relay urls / direct addrs are logged and skipped (never fail the
/// whole resolution); duplicates across the reported relay + `our_relays` are
/// de-duplicated so the merged hint carries each relay once.
pub fn peer_dial_addr(
    peer: NodeId,
    reported: Option<&EndpointAddrReport>,
    our_relays: &[String],
    cross_account: bool,
) -> Result<iroh::EndpointAddr> {
    let Some(reported) = reported else {
        return peer_addr_with_relays(peer, our_relays);
    };
    let id = iroh::EndpointId::from_bytes(&peer).map_err(|e| anyhow!("invalid peer node id: {e}"))?;
    let mut addr = iroh::EndpointAddr::new(id);

    // Reported relay first, then our-map relays appended (merged-hint form).
    let mut relay_urls: Vec<String> = Vec::new();
    if let Some(url) = &reported.home_relay_url {
        relay_urls.push(url.clone());
    }
    for url in our_relays {
        if !relay_urls.contains(url) {
            relay_urls.push(url.clone());
        }
    }
    for url in &relay_urls {
        match url.parse::<iroh::RelayUrl>() {
            Ok(relay_url) => addr = addr.with_relay_url(relay_url),
            Err(error) => {
                tracing::warn!(%url, %error, "invalid relay url in peer address hint; skipping")
            }
        }
    }

    // Direct addresses: SAME-ACCOUNT only (S1). Never leak a cross-account peer's
    // private/LAN addresses even if the hub erroneously served them.
    if !cross_account {
        for da in &reported.direct_addrs {
            match da.parse::<std::net::SocketAddr>() {
                Ok(sa) => addr = addr.with_ip_addr(sa),
                Err(error) => {
                    tracing::warn!(%da, %error, "invalid direct addr in peer report; skipping")
                }
            }
        }
    }
    Ok(addr)
}

/// Poll interval — and debounce window — of the endpoint-address reporter (T7).
/// A change observed at most once per interval is reported, so a flapping relay
/// can't spam the hub.
const ADDRESS_REPORT_INTERVAL: Duration = Duration::from_secs(30);

/// Extract the reportable `(home_relay_url, direct_addrs)` from a node's current
/// endpoint address. `home_relay_url` is the first relay url (iroh has at most
/// one home relay in practice); `direct_addrs` are the IP socket addresses, as
/// strings the hub round-trips back to peers verbatim.
pub fn endpoint_addr_report_parts(addr: &iroh::EndpointAddr) -> (Option<String>, Vec<String>) {
    let home_relay_url = addr.relay_urls().next().map(|u| u.to_string());
    let direct_addrs = addr.ip_addrs().map(|a| a.to_string()).collect();
    (home_relay_url, direct_addrs)
}

/// Spawn the fire-and-forget endpoint-address reporter (finding H1, T7).
///
/// Every [`ADDRESS_REPORT_INTERVAL`] it snapshots the node's current
/// `endpoint_addr()` and — whenever the `(home_relay_url, direct_addrs)` tuple
/// has **changed** since the last successful report AND is non-empty — PUTs it to
/// the hub. This is the simplest honest mechanism for "on bind + on change": a
/// 30s poll naturally reports the first settled address shortly after bind and
/// re-reports whenever the relay/direct set shifts, without rebuilding the
/// path/relay watcher plumbing. A report failure is `warn!`-logged and retried on
/// the next change (the last-reported snapshot is only advanced on success). The
/// task holds a [`std::sync::Weak`] to the node, so it exits on its own once the
/// node is dropped (host shutdown) — it can never block or outlive transport.
pub fn spawn_endpoint_address_reporter(
    node: Arc<SharedIrohNode>,
    hub_url: String,
    token: String,
) -> tokio::task::JoinHandle<()> {
    let weak = Arc::downgrade(&node);
    drop(node);
    tokio::spawn(async move {
        let client = match HubClient::new(&hub_url) {
            Ok(c) => c,
            Err(error) => {
                tracing::warn!(%error, "endpoint-address reporter: hub client build failed; not reporting");
                return;
            }
        };
        let mut last: Option<(Option<String>, Vec<String>)> = None;
        loop {
            // Snapshot without holding the node across the await/sleep.
            let current = match weak.upgrade() {
                Some(node) => endpoint_addr_report_parts(&node.endpoint_addr()),
                None => return, // node gone (host shutdown) → stop reporting
            };
            let (relay, direct) = &current;
            let has_addr = relay.is_some() || !direct.is_empty();
            if has_addr && last.as_ref() != Some(&current) {
                match client.put_device_address(&token, relay.as_deref(), direct).await {
                    Ok(()) => {
                        tracing::info!(
                            relay_url = relay.as_deref().unwrap_or("none"),
                            direct = direct.len(),
                            "reported endpoint address to hub"
                        );
                        last = Some(current);
                    }
                    Err(error) => tracing::warn!(
                        %error,
                        "endpoint-address report failed; will retry on the next change"
                    ),
                }
            }
            tokio::time::sleep(ADDRESS_REPORT_INTERVAL).await;
        }
    })
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

    // ── peer_dial_addr (finding H1, T7): dial with the peer's REAL address ──────

    /// A same-account peer that reported its own relay: the dial hint carries the
    /// peer's REPORTED relay FIRST, then our-map relays appended (merged-hint
    /// form), and — same account — its direct addresses are attached too.
    #[test]
    fn peer_dial_addr_prefers_reported_relay_and_keeps_direct_same_account() {
        let peer = valid_peer_bytes();
        let reported = EndpointAddrReport {
            home_relay_url: Some("https://peer-relay.example.org".to_string()),
            direct_addrs: vec!["192.168.1.9:5000".to_string()],
            reported_at: None,
        };
        let addr = peer_dial_addr(peer, Some(&reported), &["https://our-relay.example.org".to_string()], false)
            .unwrap();

        // Both the reported relay AND our-map relay are attached (merged-hint
        // form). `EndpointAddr` stores addrs in a BTreeSet, so the wire order is
        // set-sorted, not insertion order — what matters is that iroh has BOTH
        // relays to try; compare as sorted sets.
        let mut relays: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
        relays.sort();
        let mut expected = vec![
            normalized("https://peer-relay.example.org"),
            normalized("https://our-relay.example.org"),
        ];
        expected.sort();
        assert_eq!(relays, expected, "reported relay AND our relay both attached (merged-hint form)");
        let ips: Vec<String> = addr.ip_addrs().map(|a| a.to_string()).collect();
        assert_eq!(ips, vec!["192.168.1.9:5000".to_string()], "same-account keeps direct addrs");
    }

    /// S1: a CROSS-ACCOUNT peer (a collab holder) must NEVER receive direct
    /// addresses — even when the hub erroneously served some. Only the relay hint
    /// survives; the helper strips the direct addrs regardless of the input.
    #[test]
    fn peer_dial_addr_cross_account_strips_direct_addrs() {
        let peer = valid_peer_bytes();
        let reported = EndpointAddrReport {
            home_relay_url: Some("https://holder-relay.example.org".to_string()),
            // A hostile/buggy hub served direct addrs; they must be dropped.
            direct_addrs: vec!["10.0.0.5:5000".to_string(), "192.168.1.9:5000".to_string()],
            reported_at: None,
        };
        let addr = peer_dial_addr(peer, Some(&reported), &[], true).unwrap();

        assert_eq!(addr.ip_addrs().count(), 0, "cross-account dial must carry NO direct addrs (S1)");
        let relays: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
        assert_eq!(
            relays,
            vec![normalized("https://holder-relay.example.org")],
            "cross-account still gets the relay hint"
        );
    }

    /// `reported` absent (older hub / a device that never reported): falls back to
    /// exactly `peer_addr_with_relays` — the pre-T7 our-relay hint, unchanged.
    #[test]
    fn peer_dial_addr_reported_absent_falls_back_to_our_relays() {
        let peer = valid_peer_bytes();
        let our = vec!["https://our-relay.example.org".to_string()];
        let via_dial = peer_dial_addr(peer, None, &our, false).unwrap();
        let via_legacy = peer_addr_with_relays(peer, &our).unwrap();
        assert_eq!(via_dial, via_legacy, "no report → identical to the legacy hint");
    }

    /// The report extractor pulls the home relay + direct IP addrs out of an
    /// `EndpointAddr` as strings the hub round-trips (round-trip through
    /// `peer_dial_addr` proves the shapes match).
    #[test]
    fn endpoint_addr_report_parts_extracts_relay_and_direct() {
        let peer = valid_peer_bytes();
        let reported = EndpointAddrReport {
            home_relay_url: Some("https://relay1.example.org".to_string()),
            direct_addrs: vec!["192.168.1.5:1234".to_string()],
            reported_at: None,
        };
        let addr = peer_dial_addr(peer, Some(&reported), &[], false).unwrap();
        let (relay, direct) = endpoint_addr_report_parts(&addr);
        assert_eq!(relay.as_deref(), Some(normalized("https://relay1.example.org").as_str()));
        assert_eq!(direct, vec!["192.168.1.5:1234".to_string()]);
    }
}
