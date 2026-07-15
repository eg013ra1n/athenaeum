//! App account layer (task B4): the hub client, the one shared device identity,
//! and OS-keychain token storage. Headless-compatible (ungated) — depends only
//! on `settings`, `sharing::iroh` (for the device key), and `reqwest`.
//!
//! - [`client`] — [`HubClient`], the reqwest client for the athenaeum-hub
//!   account API, with typed [`AccountClientError`]s.
//! - [`keys`] — [`DeviceKey`], load-or-create of the SHARED iroh device key
//!   (the same file the sync transport binds; spec D-5, one identity).
//! - [`token_store`] — [`TokenStore`], the device token in the OS keychain with
//!   a 0600 file fallback. Tokens never touch logs or the DB.
//!
//! The command handlers that compose these live in [`crate::api::account`].

use serde::{Deserialize, Serialize};

pub mod client;
pub mod keys;
pub mod naming;
pub mod token_store;

pub use client::{AccountClientError, HubClient, VerifyResponse};
pub use keys::DeviceKey;
pub use naming::default_device_name;
pub use token_store::TokenStore;

/// What a device *is* in the mesh sync model (Sync 2C). Every Athenaeum install
/// is a full peer (receives + sends); a Perseus capture agent is send-only. This
/// replaces the old primary/capture role one-primary topology.
///
/// Serialized lowercase on the wire — matches the hub JSON
/// (`"athenaeum"`/`"perseus"`) and the frontend string union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum DeviceCapability {
    Athenaeum,
    Perseus,
}

impl DeviceCapability {
    /// The lowercase wire string (`"athenaeum"` / `"perseus"`).
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceCapability::Athenaeum => "athenaeum",
            DeviceCapability::Perseus => "perseus",
        }
    }

    /// Parse the lowercase wire string; unknown / empty → `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "athenaeum" => Some(Self::Athenaeum),
            "perseus" => Some(Self::Perseus),
            _ => None,
        }
    }
}

impl Default for DeviceCapability {
    fn default() -> Self {
        DeviceCapability::Athenaeum
    }
}

/// A device's self-reported dialable endpoint address (finding H1, iroh
/// hardening T7). Each device PUTs its current `{homeRelayUrl, directAddrs}` to
/// the hub (`PUT /devices/self/address`, T5); the hub stamps `reportedAt` and
/// returns the whole thing on `GET /devices` as `endpointAddr`. A dialer uses
/// the peer's REAL relay (not a guess from our own relay set) as its dial hint.
///
/// Lenient by construction: every field is `#[serde(default)]`, so a device that
/// has never reported, or an older hub that omits `reportedAt`, still
/// deserializes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct EndpointAddrReport {
    /// The device's home relay url (`None` when it has no home relay yet).
    #[serde(default)]
    pub home_relay_url: Option<String>,
    /// The device's direct (IP) socket addresses, as strings — used only for a
    /// SAME-ACCOUNT dial (never handed to a cross-account collaborator; S1).
    #[serde(default)]
    pub direct_addrs: Vec<String>,
    /// When the hub last stamped this report (RFC3339). Diagnostics only; the
    /// hub owns it, so the app treats it as read-only and optional.
    #[serde(default)]
    pub reported_at: Option<String>,
}

/// One device registered under the account (from `GET /devices`). Field casing
/// matches the hub JSON verbatim so this doubles as the client decode type.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountDevice {
    pub id: String,
    pub name: String,
    /// Base64 of the device's 32-byte ed25519 public key (== its node id).
    pub pubkey: String,
    /// What this device is in the mesh (full peer vs send-only agent). Absent on
    /// older hub payloads → defaults to [`DeviceCapability::Athenaeum`].
    #[serde(default)]
    pub capability: DeviceCapability,
    pub created_at: String,
    pub last_seen_at: Option<String>,
    /// This device's self-reported dialable endpoint address (finding H1, T7).
    /// Absent on older hub payloads (or a device that never reported) → `None`,
    /// in which case every dial path falls back to the our-relay-map hint exactly
    /// as it did before this field existed.
    #[serde(default)]
    pub endpoint_addr: Option<EndpointAddrReport>,
}

/// Snapshot of this device's account state, resolvable offline from persisted
/// settings + local token presence.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    /// Whether a device token is present locally.
    pub signed_in: bool,
    /// Signed-in email (display only); `None` when signed out.
    pub email: Option<String>,
    /// This device's hub-assigned id; `None` when signed out.
    pub device_id: Option<String>,
    /// This device's capability. The app is always a full peer
    /// ([`DeviceCapability::Athenaeum`]).
    pub capability: DeviceCapability,
    /// The hub this device authenticates against.
    pub hub_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_capability_str_roundtrip() {
        assert_eq!(DeviceCapability::Athenaeum.as_str(), "athenaeum");
        assert_eq!(DeviceCapability::Perseus.as_str(), "perseus");
        assert_eq!(DeviceCapability::parse("perseus"), Some(DeviceCapability::Perseus));
        assert_eq!(DeviceCapability::parse("bogus"), None);
        assert_eq!(DeviceCapability::default(), DeviceCapability::Athenaeum);
    }

    #[test]
    fn account_device_decodes_capability_and_defaults_athenaeum() {
        let with: AccountDevice = serde_json::from_value(serde_json::json!({
            "id":"d1","name":"Studio Mac","pubkey":"AAAA","capability":"perseus","createdAt":"t"
        }))
        .unwrap();
        assert_eq!(with.capability, DeviceCapability::Perseus);
        let without: AccountDevice = serde_json::from_value(serde_json::json!({
            "id":"d2","name":"Laptop","pubkey":"BBBB","createdAt":"t"
        }))
        .unwrap();
        assert_eq!(without.capability, DeviceCapability::Athenaeum); // missing → default
    }

    /// T7 old-hub compat: a `GET /devices` payload with NO `endpointAddr` key
    /// (every hub before T5) still decodes — the field defaults to `None`, so
    /// every dial path falls back to the our-relay hint exactly as before.
    #[test]
    fn account_device_endpoint_addr_absent_defaults_none() {
        let without: AccountDevice = serde_json::from_value(serde_json::json!({
            "id":"d1","name":"Studio Mac","pubkey":"AAAA","createdAt":"t"
        }))
        .unwrap();
        assert_eq!(without.endpoint_addr, None, "missing endpointAddr → None (old-hub compat)");
    }

    /// A present `endpointAddr` decodes its relay + direct addrs, and a report
    /// that omits `reportedAt`/`directAddrs` is still accepted (lenient fields).
    #[test]
    fn endpoint_addr_report_decodes_and_is_lenient() {
        let dev: AccountDevice = serde_json::from_value(serde_json::json!({
            "id":"d1","name":"Studio","pubkey":"AAAA","createdAt":"t",
            "endpointAddr": {
                "homeRelayUrl": "https://relay1.example.org/",
                "directAddrs": ["192.168.1.5:1234"],
                "reportedAt": "2026-07-14T00:00:00Z"
            }
        }))
        .unwrap();
        let rep = dev.endpoint_addr.expect("endpointAddr present");
        assert_eq!(rep.home_relay_url.as_deref(), Some("https://relay1.example.org/"));
        assert_eq!(rep.direct_addrs, vec!["192.168.1.5:1234".to_string()]);

        // Only a relay, nothing else — lenient defaults fill the rest.
        let bare: EndpointAddrReport =
            serde_json::from_value(serde_json::json!({ "homeRelayUrl": "https://r/" })).unwrap();
        assert_eq!(bare.home_relay_url.as_deref(), Some("https://r/"));
        assert!(bare.direct_addrs.is_empty());
        assert_eq!(bare.reported_at, None);
    }
}
