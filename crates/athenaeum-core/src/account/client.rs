//! HTTP client for the athenaeum-hub account API (task B4).
//!
//! One method per endpoint of the B2/B3 contract, each mapping the HTTP status
//! onto a typed [`AccountClientError`] the api layer turns into an actionable
//! message. The device token is passed as a `Bearer` header and is **never**
//! placed in a URL, log, or error.
//!
//! Endpoint contract (base = `account.hub_url`):
//! - `POST /api/v1/auth/otp {email}` → 204 (429 rate-limited)
//! - `POST /api/v1/auth/verify {email, code, devicePubkey, deviceName, deviceCapability}`
//!   → 200 `{deviceToken, deviceId}` | 400 (malformed pubkey) | 401 (wrong/
//!   expired code) | 409 (pubkey already owned by a DIFFERENT account —
//!   same-account re-verify/re-sign-in is a 200 on the hub, not a conflict)
//! - `GET  /api/v1/devices` → `[AccountDevice]`
//! - `POST /api/v1/devices/{id}/revoke` → 204
//! - `PATCH /api/v1/devices/{id} {name}` → 200 | 400 | 409
//! - `GET  /api/v1/relay-map` → `{relays: [url,...]}`

use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;

use super::{AccountDevice, DeviceCapability};

/// Typed outcome of a failed hub call. Carries no secrets.
#[derive(Debug)]
pub enum AccountClientError {
    /// 429 — too many requests.
    RateLimited,
    /// 401 — signed out / device revoked / bad token.
    Unauthorized,
    /// 409 — a second primary device was rejected (message from the hub).
    SecondPrimary(String),
    /// 409 on `/auth/verify` — the device pubkey already belongs to a
    /// DIFFERENT account's device (cross-account theft guard). Same-account
    /// re-verify (re-sign-in) no longer 409s on the hub — see the module docs.
    DeviceConflict(String),
    /// 400 on the role endpoint — peer validation failed (message from the hub).
    PeerValidation(String),
    /// 409 on `PATCH /devices/{id}` — the requested name collides with another
    /// active device in the account (`UNIQUE(account_id, lower(name))`). Carries
    /// no message: the api layer maps it to a fixed, actionable string.
    DuplicateName,
    /// 400 elsewhere — malformed request (e.g. bad pubkey, bad code shape).
    BadRequest(String),
    /// Transport / unexpected-status / decode failure.
    Network(String),
}

impl std::fmt::Display for AccountClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountClientError::RateLimited => {
                f.write_str("too many requests — wait a minute and try again")
            }
            AccountClientError::Unauthorized => {
                f.write_str("signed out or device revoked; sign in again")
            }
            AccountClientError::DuplicateName => {
                f.write_str("name already in use by another device")
            }
            AccountClientError::SecondPrimary(m)
            | AccountClientError::DeviceConflict(m)
            | AccountClientError::PeerValidation(m)
            | AccountClientError::BadRequest(m)
            | AccountClientError::Network(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for AccountClientError {}

/// Successful `/auth/verify` payload.
///
/// `Debug` is hand-implemented (not derived) to redact `device_token` — a live
/// bearer credential that must never appear in an ad-hoc `{:?}` log/assert.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    pub device_token: String,
    pub device_id: String,
}

impl std::fmt::Debug for VerifyResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyResponse")
            .field("device_token", &"<redacted>")
            .field("device_id", &self.device_id)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RelayMapResponse {
    #[serde(default)]
    relays: Vec<String>,
}

/// Reqwest-backed client bound to one hub base URL.
pub struct HubClient {
    http: reqwest::Client,
    base_url: String,
}

impl HubClient {
    /// Build a client for `base_url` (trailing slashes trimmed).
    pub fn new(base_url: impl Into<String>) -> Result<Self, AccountClientError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| AccountClientError::Network(e.to_string()))?;
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Ok(Self { http, base_url })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base_url, path)
    }

    /// `POST /auth/otp` — request an email OTP. 204 → Ok; 429 → RateLimited.
    pub async fn request_otp(&self, email: &str) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .post(self.url("/auth/otp"))
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::NO_CONTENT | StatusCode::OK => Ok(()),
            StatusCode::TOO_MANY_REQUESTS => Err(AccountClientError::RateLimited),
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `POST /auth/verify` — exchange the OTP + device pubkey for a device token.
    pub async fn verify(
        &self,
        email: &str,
        code: &str,
        device_pubkey_b64: &str,
        device_name: &str,
        capability: DeviceCapability,
    ) -> Result<VerifyResponse, AccountClientError> {
        let resp = self
            .http
            .post(self.url("/auth/verify"))
            .json(&serde_json::json!({
                "email": email,
                "code": code,
                "devicePubkey": device_pubkey_b64,
                "deviceName": device_name,
                "deviceCapability": capability.as_str(),
            }))
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<VerifyResponse>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode verify response: {e}"))),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            StatusCode::BAD_REQUEST => Err(AccountClientError::BadRequest(body_message(resp).await)),
            StatusCode::CONFLICT => Err(AccountClientError::DeviceConflict(body_message(resp).await)),
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `GET /devices` — the account's registered devices.
    pub async fn list_devices(
        &self,
        token: &str,
    ) -> Result<Vec<AccountDevice>, AccountClientError> {
        let resp = self
            .http
            .get(self.url("/devices"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<Vec<AccountDevice>>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode devices: {e}"))),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `POST /devices/{id}/revoke`.
    pub async fn revoke_device(
        &self,
        token: &str,
        device_id: &str,
    ) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .post(self.url(&format!("/devices/{device_id}/revoke")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::NO_CONTENT | StatusCode::OK => Ok(()),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            StatusCode::NOT_FOUND => {
                Err(AccountClientError::BadRequest("no such device".into()))
            }
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `PATCH /devices/{id}` — rename a device. Body `{ "name": name }`. 409 →
    /// [`AccountClientError::DuplicateName`] (the name collides with another
    /// active device in the account); 401 → [`AccountClientError::Unauthorized`].
    pub async fn rename_device(
        &self,
        token: &str,
        device_id: &str,
        name: &str,
    ) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .patch(self.url(&format!("/devices/{device_id}")))
            .bearer_auth(token)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::CONFLICT => Err(AccountClientError::DuplicateName),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            StatusCode::BAD_REQUEST => Err(AccountClientError::BadRequest(body_message(resp).await)),
            StatusCode::NOT_FOUND => Err(AccountClientError::BadRequest("no such device".into())),
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `PUT /devices/self/address` — report this device's dialable endpoint
    /// address (finding H1, iroh hardening T7). Body `{homeRelayUrl, directAddrs}`
    /// (camelCase; the hub stamps `reportedAt`). A `200`/`204` is success; `401`
    /// maps to [`AccountClientError::Unauthorized`] like every other authed call.
    /// `home_relay_url` is `None` when the device has no home relay yet; the hub
    /// serializes that as JSON `null`.
    pub async fn put_device_address(
        &self,
        token: &str,
        home_relay_url: Option<&str>,
        direct_addrs: &[String],
    ) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .put(self.url("/devices/self/address"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "homeRelayUrl": home_relay_url,
                "directAddrs": direct_addrs,
            }))
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `GET /relay-map` — the hub's advertised relay URLs.
    pub async fn relay_map(&self, token: &str) -> Result<Vec<String>, AccountClientError> {
        let resp = self
            .http
            .get(self.url("/relay-map"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<RelayMapResponse>()
                .await
                .map(|r| r.relays)
                .map_err(|e| AccountClientError::Network(format!("decode relay-map: {e}"))),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            s => Err(unexpected(s, resp).await),
        }
    }
}

/// Map a reqwest transport error to `Network`. Reqwest's Display never contains
/// request headers (where the bearer token lives), so this is token-safe.
fn net(e: reqwest::Error) -> AccountClientError {
    AccountClientError::Network(e.to_string())
}

/// An unexpected status becomes `Network` with the status + body for context.
async fn unexpected(status: StatusCode, resp: reqwest::Response) -> AccountClientError {
    let msg = body_message(resp).await;
    if msg.is_empty() {
        AccountClientError::Network(format!("hub returned {status}"))
    } else {
        AccountClientError::Network(format!("hub returned {status}: {msg}"))
    }
}

/// Best-effort human message from an error response body: an `{error}` /
/// `{message}` JSON field if present, else the trimmed raw text.
async fn body_message(resp: reqwest::Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for key in ["error", "message", "detail"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sign_in_flow_stores_token() {
        use crate::account::token_store::TokenStore;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/otp"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceToken": "tok-secret-123",
                "deviceId": "dev-1",
            })))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        client.request_otp("a@b.com").await.unwrap();
        let resp = client
            .verify("a@b.com", "123456", "cHVia2V5", "test-device", DeviceCapability::Athenaeum)
            .await
            .unwrap();
        assert_eq!(resp.device_token, "tok-secret-123");
        assert_eq!(resp.device_id, "dev-1");

        // The returned token round-trips through the (file-backed) store — the
        // exact step the sign-in command performs.
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::file_only("host", dir.path().join("tok"));
        store.store(&resp.device_token).unwrap();
        assert_eq!(store.load().unwrap().as_deref(), Some("tok-secret-123"));
    }

    #[tokio::test]
    async fn revoked_token_maps_to_signed_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        let err = client.list_devices("stale-token").await.unwrap_err();
        assert!(
            matches!(err, AccountClientError::Unauthorized),
            "401 must map to Unauthorized (→ SignedOut at the api boundary), got {err:?}"
        );
    }

    #[tokio::test]
    async fn rate_limit_maps_to_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/otp"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        let err = client.request_otp("a@b.com").await.unwrap_err();
        assert!(
            matches!(err, AccountClientError::RateLimited),
            "429 must map to RateLimited, got {err:?}"
        );
    }

    /// Fix (B4 review): the hub still 409s `/auth/verify` when the pubkey
    /// belongs to a DIFFERENT account's device (cross-account theft guard —
    /// same-account re-verify now 200s on the hub side instead). The client
    /// must surface that as a distinct typed error, not the generic `Network`
    /// bucket, so the api layer can give an actionable message.
    #[tokio::test]
    async fn verify_409_maps_to_device_conflict() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "device public key already registered",
            })))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        let err = client
            .verify("a@b.com", "123456", "cHVia2V5", "test-device", DeviceCapability::Athenaeum)
            .await
            .unwrap_err();
        match err {
            AccountClientError::DeviceConflict(msg) => {
                assert!(msg.contains("already registered"), "message surfaced: {msg}");
            }
            other => panic!("expected DeviceConflict, got {other:?}"),
        }
    }

    /// `PATCH /devices/{id}` renames a device: a 204 (or 200) is success.
    #[tokio::test]
    async fn rename_device_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/devices/dev-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        client.rename_device("tok", "dev-1", "Observatory Mac").await.unwrap();
    }

    /// Sync 2C: a name that collides with another active device in the account
    /// (hub `UNIQUE(account_id, lower(name))`) returns 409, which the client
    /// must surface as the typed [`AccountClientError::DuplicateName`] — not the
    /// generic `Network` bucket — so the api layer can suggest a suffix.
    #[tokio::test]
    async fn rename_device_409_maps_to_duplicate_name() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/devices/dev-1"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "a device named \"Observatory Mac\" already exists",
            })))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        let err = client.rename_device("tok", "dev-1", "Observatory Mac").await.unwrap_err();
        assert!(
            matches!(err, AccountClientError::DuplicateName),
            "409 on rename must map to DuplicateName, got {err:?}"
        );
    }

    /// A 401 on rename maps to `Unauthorized` (→ SignedOut at the api boundary,
    /// which also clears the local session), like every other authed call.
    #[tokio::test]
    async fn rename_device_401_maps_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/devices/dev-1"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        let err = client.rename_device("stale", "dev-1", "New Name").await.unwrap_err();
        assert!(
            matches!(err, AccountClientError::Unauthorized),
            "401 on rename must map to Unauthorized, got {err:?}"
        );
    }

    /// Fix (B4 review, minor #4): `VerifyResponse`'s `Debug` must never print
    /// the bearer token — it is a live credential, and `Debug` output ends up
    /// in ad-hoc `{:?}` logging/asserts far more easily than `Display`.
    #[test]
    fn verify_response_debug_redacts_token() {
        let resp = VerifyResponse {
            device_token: "super-secret-live-token".to_string(),
            device_id: "dev-1".to_string(),
        };
        let debug = format!("{resp:?}");
        assert!(!debug.contains("super-secret-live-token"), "token leaked into Debug: {debug}");
        assert!(debug.contains("dev-1"), "device_id should still be visible: {debug}");
    }

    /// T7: `PUT /devices/self/address` reports this device's endpoint address —
    /// the exact `{homeRelayUrl, directAddrs}` body the hub expects — and a 204
    /// (or 200) is success. A `null` `homeRelayUrl` (no home relay yet) is valid.
    #[tokio::test]
    async fn put_device_address_reports_relay_and_direct_addrs() {
        use wiremock::matchers::body_partial_json;

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/devices/self/address"))
            .and(body_partial_json(serde_json::json!({
                "homeRelayUrl": "https://relay1.example.org/",
                "directAddrs": ["192.168.1.5:1234"],
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        client
            .put_device_address(
                "tok",
                Some("https://relay1.example.org/"),
                &["192.168.1.5:1234".to_string()],
            )
            .await
            .expect("address report succeeds on 204");
        // MockServer verifies `.expect(1)` (and the body matcher) on drop.
    }

    /// A 401 on the address report maps to `Unauthorized` (→ SignedOut at the api
    /// boundary), like every other authed call — never the generic `Network`.
    #[tokio::test]
    async fn put_device_address_401_maps_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/devices/self/address"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        let err = client.put_device_address("stale", None, &[]).await.unwrap_err();
        assert!(
            matches!(err, AccountClientError::Unauthorized),
            "401 on address report must map to Unauthorized, got {err:?}"
        );
    }

    /// `GET /devices` decodes the mesh `capability` field, and a payload missing
    /// it (older hub) defaults to `athenaeum` (see `AccountDevice::capability`'s
    /// `#[serde(default)]`).
    #[tokio::test]
    async fn list_devices_parses_capability_and_defaults() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "dev-1", "name": "Studio Mac", "pubkey": "cHVia2V5",
                    "capability": "athenaeum",
                    "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": "2026-07-06T00:00:00Z",
                    "endpointAddr": {
                        "homeRelayUrl": "https://relay1.example.org/",
                        "directAddrs": ["192.168.1.5:1234"],
                        "reportedAt": "2026-07-14T00:00:00Z"
                    }
                },
                {
                    "id": "dev-2", "name": "Mini PC", "pubkey": "cHVia2V5Mg",
                    "capability": "perseus",
                    "createdAt": "2026-07-02T00:00:00Z", "lastSeenAt": null
                },
                {
                    "id": "dev-3", "name": "New", "pubkey": "cHVia2V5Mw",
                    "createdAt": "2026-07-03T00:00:00Z", "lastSeenAt": null
                }
            ])))
            .mount(&server)
            .await;

        let client = HubClient::new(server.uri()).unwrap();
        let devices = client.list_devices("tok").await.unwrap();
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].capability, DeviceCapability::Athenaeum);
        assert_eq!(devices[1].capability, DeviceCapability::Perseus);
        assert_eq!(devices[2].capability, DeviceCapability::Athenaeum); // missing → default
        assert_eq!(devices[2].last_seen_at, None);
        // T7: the first device carries a self-reported endpoint address; the
        // others (no `endpointAddr` key) default to `None` — old-hub compat.
        let rep = devices[0].endpoint_addr.as_ref().expect("dev-1 reported an address");
        assert_eq!(rep.home_relay_url.as_deref(), Some("https://relay1.example.org/"));
        assert_eq!(rep.direct_addrs, vec!["192.168.1.5:1234".to_string()]);
        assert_eq!(devices[1].endpoint_addr, None, "a device that never reported → None");
        assert_eq!(devices[2].endpoint_addr, None);
    }
}
