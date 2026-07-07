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
pub mod token_store;

pub use client::{AccountClientError, HubClient, VerifyResponse};
pub use keys::DeviceKey;
pub use token_store::TokenStore;

/// A device's role in the account. `primary` receives; `capture` sends to its
/// paired primary. Absent (`None`) means "registered but unassigned".
///
/// Serialized lowercase on the wire — matches both the hub JSON
/// (`"primary"`/`"capture"`) and the frontend string union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum DeviceRole {
    Primary,
    Capture,
}

impl DeviceRole {
    /// The lowercase wire string (`"primary"` / `"capture"`).
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceRole::Primary => "primary",
            DeviceRole::Capture => "capture",
        }
    }

    /// Parse the lowercase wire string; unknown / empty → `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "primary" => Some(DeviceRole::Primary),
            "capture" => Some(DeviceRole::Capture),
            _ => None,
        }
    }
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
    pub role: Option<DeviceRole>,
    /// The paired primary for a `capture` device (else `None`).
    pub peer_device_id: Option<String>,
    pub created_at: String,
    pub last_seen_at: Option<String>,
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
    /// This device's role; `None` when unassigned or signed out.
    pub role: Option<DeviceRole>,
    /// The hub this device authenticates against.
    pub hub_url: String,
}
