//! Collab endpoints of the athenaeum-hub (read side used by slice 3).
//!
//! Mirrors `account::client::HubClient`: base URL baked in, device token per
//! call via `bearer_auth`, `AccountClientError` for the shared 401→SignedOut
//! mapping at the api boundary. Endpoint contract: hub README "API —
//! Collaboration (Stage II)".

use std::collections::HashSet;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::account::AccountClientError;

const HTTP_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyProjectWire {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub data_role: String,
    pub coordinator: bool,
    pub require_approval: bool,
    pub pending_announcements: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetWire {
    pub name: String,
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWire {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub require_approval: bool,
    pub target: TargetWire,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberWire {
    pub display_name: String,
    pub data_role: String,
    pub coordinator: bool,
}

/// Public project page — only the fields slice 3 consumes; unknown fields
/// (packages, progress) are ignored by serde.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPageWire {
    pub project: ProjectWire,
    pub members: Vec<MemberWire>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedSnapshotWire {
    pub payload: String,
    pub signature: String,
    pub pubkey: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdSetWire {
    pub version: i32,
    pub rules: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdsWire {
    pub current: Option<ThresholdSetWire>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PubkeyWire {
    pubkey: String,
}

/// Body of `POST /projects/{id}/announcements`. Serialized camelCase;
/// `aggregate_stats` (incl. `manifestXxh3`) passes through untouched.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceRequest {
    /// Package uuid string.
    pub package_id: String,
    /// Root hash, exactly 64 hex chars (hub-validated).
    pub root_hash: String,
    pub byte_size: i64,
    pub frame_count: i32,
    pub aggregate_stats: serde_json::Value,
    /// Announcement ids this one supersedes. `announce` pre-dedupes (the hub
    /// rejects duplicates) and preserves first-seen order.
    pub supersedes: Vec<String>,
}

/// `{id, state}` reply shared by announce/approve/reject. `state` is one of
/// `pending`/`published`/`rejected`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceResponse {
    pub id: String,
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderWire {
    /// base64-encoded 32-byte peer pubkey.
    pub pubkey: String,
    pub display_name: String,
    pub last_seen_at: Option<String>,
    /// The holder's self-reported home relay url (finding H1, T7). CROSS-ACCOUNT
    /// by nature (a holder may be in a different account), so the hub serves the
    /// relay ONLY — never direct addrs (S1). Absent on an older hub → `None`, in
    /// which case the download falls back to our own resolved relay set.
    #[serde(default)]
    pub relay_url: Option<String>,
}

/// One announcement row from `GET /projects/{id}/announcements`. Unknown
/// (future) fields are ignored; `aggregate_stats`/`supersedes`/`holders`
/// default when the hub omits them.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementWire {
    pub id: String,
    pub package_id: String,
    pub publisher_display_name: String,
    pub own: bool,
    pub root_hash: String,
    pub byte_size: i64,
    pub frame_count: i32,
    #[serde(default)]
    pub aggregate_stats: serde_json::Value,
    #[serde(default)]
    pub supersedes: Vec<String>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
    #[serde(default)]
    pub holders: Vec<HolderWire>,
}

pub struct CollabClient {
    http: reqwest::Client,
    base_url: String,
}

fn net(e: reqwest::Error) -> AccountClientError {
    AccountClientError::Network(e.to_string())
}

/// A non-OK/401 status becomes `Network` carrying the status plus the hub's
/// best-effort `{error}` message (empty body → status only). Mirrors
/// `account::client::unexpected`.
async fn unexpected(status: StatusCode, resp: reqwest::Response) -> AccountClientError {
    let msg = body_message(resp).await;
    if msg.is_empty() {
        AccountClientError::Network(format!("hub returned {status}"))
    } else {
        AccountClientError::Network(format!("hub returned {status}: {msg}"))
    }
}

/// Best-effort human message from an error body: an `{error}`/`{message}`/
/// `{detail}` JSON field if present, else the trimmed raw text. Never panics on
/// an empty or non-JSON body.
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

impl CollabClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, AccountClientError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(net)?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base_url, path)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
        what: &str,
    ) -> Result<T, AccountClientError> {
        let mut req = self.http.get(self.url(path));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.map_err(net)?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<T>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode {what}: {e}"))),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            s => Err(AccountClientError::Network(format!(
                "unexpected status {s} fetching {what}"
            ))),
        }
    }

    /// The hub's snapshot-signing pubkey (base64). Fetched once and pinned.
    pub async fn collab_pubkey(&self) -> Result<String, AccountClientError> {
        let wire: PubkeyWire = self.get_json("/collab/pubkey", None, "collab pubkey").await?;
        Ok(wire.pubkey)
    }

    pub async fn my_projects(&self, token: &str) -> Result<Vec<MyProjectWire>, AccountClientError> {
        self.get_json("/me/projects", Some(token), "my projects").await
    }

    /// Public page (no token) — target/members for the cache.
    pub async fn project_page(&self, id_or_slug: &str) -> Result<ProjectPageWire, AccountClientError> {
        self.get_json(&format!("/projects/{id_or_slug}"), None, "project page").await
    }

    pub async fn membership_snapshot(
        &self,
        token: &str,
        project_id: &str,
    ) -> Result<SignedSnapshotWire, AccountClientError> {
        self.get_json(
            &format!("/projects/{project_id}/membership"),
            Some(token),
            "membership snapshot",
        )
        .await
    }

    pub async fn thresholds(
        &self,
        token: &str,
        project_id: &str,
    ) -> Result<ThresholdsWire, AccountClientError> {
        self.get_json(
            &format!("/projects/{project_id}/thresholds"),
            Some(token),
            "thresholds",
        )
        .await
    }

    /// `POST /projects/{id}/announcements` — publish a package announcement.
    /// `supersedes` is deduped (order-preserving) before send since the hub
    /// rejects duplicates. 200 → `{id, state}`; 401 → `Unauthorized`; a 409
    /// (closed project or globally-duplicate packageId) and any other status
    /// surface the hub's `{error}` message in the `Network` arm.
    pub async fn announce(
        &self,
        token: &str,
        project_id: &str,
        req: &AnnounceRequest,
    ) -> Result<AnnounceResponse, AccountClientError> {
        let mut body = req.clone();
        let mut seen = HashSet::new();
        body.supersedes.retain(|id| seen.insert(id.clone()));
        let resp = self
            .http
            .post(self.url(&format!("/projects/{project_id}/announcements")))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<AnnounceResponse>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode announce: {e}"))),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `GET /projects/{id}/announcements` — every announcement visible to the
    /// caller, each carrying its current `holders`.
    pub async fn list_announcements(
        &self,
        token: &str,
        project_id: &str,
    ) -> Result<Vec<AnnouncementWire>, AccountClientError> {
        self.get_json(
            &format!("/projects/{project_id}/announcements"),
            Some(token),
            "announcements",
        )
        .await
    }

    /// `POST /announcements/{id}/approve`. 409 when the announcement is no
    /// longer pending → surfaced via `Network`.
    pub async fn approve_announcement(
        &self,
        token: &str,
        announcement_id: &str,
    ) -> Result<AnnounceResponse, AccountClientError> {
        self.decide(
            &format!("/announcements/{announcement_id}/approve"),
            token,
            None,
            "approve",
        )
        .await
    }

    /// `POST /announcements/{id}/reject` with body `{reason}`. The hub enforces
    /// the reason's 1..=500 BYTE bound; the client carries the string verbatim.
    pub async fn reject_announcement(
        &self,
        token: &str,
        announcement_id: &str,
        reason: &str,
    ) -> Result<AnnounceResponse, AccountClientError> {
        self.decide(
            &format!("/announcements/{announcement_id}/reject"),
            token,
            Some(serde_json::json!({ "reason": reason })),
            "reject",
        )
        .await
    }

    /// Shared approve/reject POST: optional JSON body, `{id, state}` reply.
    async fn decide(
        &self,
        path: &str,
        token: &str,
        body: Option<serde_json::Value>,
        what: &str,
    ) -> Result<AnnounceResponse, AccountClientError> {
        let mut req = self.http.post(self.url(path)).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(net)?;
        match resp.status() {
            StatusCode::OK => resp
                .json::<AnnounceResponse>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode {what}: {e}"))),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            s => Err(unexpected(s, resp).await),
        }
    }

    /// `POST /announcements/{id}/have` — report that this device holds the
    /// package's blob. 204 → Ok. A non-device (portal-session) token is a 400
    /// `{error}`, distinct from 401/403, surfaced via `Network`.
    pub async fn report_have(
        &self,
        token: &str,
        announcement_id: &str,
    ) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .post(self.url(&format!("/announcements/{announcement_id}/have")))
            .bearer_auth(token)
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            StatusCode::NO_CONTENT | StatusCode::OK => Ok(()),
            StatusCode::UNAUTHORIZED => Err(AccountClientError::Unauthorized),
            s => Err(unexpected(s, resp).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_announce_req() -> AnnounceRequest {
        AnnounceRequest {
            package_id: "pkg-1".into(),
            root_hash: "a".repeat(64),
            byte_size: 1,
            frame_count: 1,
            aggregate_stats: serde_json::json!({ "manifestXxh3": "abcd" }),
            supersedes: vec![],
        }
    }

    #[tokio::test]
    async fn my_projects_decodes_and_maps_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me/projects"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": "p-1", "slug": "m101", "title": "M 101", "dataRole": "send_receive",
                "coordinator": true, "requireApproval": true, "pendingAnnouncements": 2
            }])))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let mine = client.my_projects("tok").await.unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].slug, "m101");
        assert!(mine[0].coordinator);
        assert_eq!(mine[0].pending_announcements, 2);

        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me/projects"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server2)
            .await;
        let client2 = CollabClient::new(server2.uri()).unwrap();
        assert!(matches!(
            client2.my_projects("tok").await,
            Err(crate::account::AccountClientError::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn project_page_and_thresholds_decode() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/m101"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": {"id": "p-1", "slug": "m101", "title": "M 101", "status": "active",
                            "requireApproval": false,
                            "target": {"name": "M101", "raDeg": 210.8, "decDeg": 54.35, "radiusDeg": 1.5}},
                "members": [{"displayName": "Vilen", "dataRole": "send_receive", "coordinator": true}],
                "packages": [], "progress": {"totalFrames": 0, "integrationSecondsByFilter": {}, "perMember": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/thresholds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "current": {"version": 3, "rules": [{"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0}],
                            "createdAt": "2026-07-13T00:00:00Z"},
                "history": []
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let page = client.project_page("m101").await.unwrap();
        assert_eq!(page.project.target.radius_deg, 1.5);
        assert_eq!(page.members[0].display_name, "Vilen");
        let th = client.thresholds("tok", "p-1").await.unwrap();
        assert_eq!(th.current.unwrap().version, 3);
    }

    // ---- announcements / decide / have (slice-4 task 2) ----

    /// Happy path: the request body is serialized camelCase, `aggregateStats`
    /// (incl. `manifestXxh3`) passes through untouched, `supersedes` rides along,
    /// and the `{id, state}` response decodes.
    #[tokio::test]
    async fn announce_sends_camelcase_body_and_decodes() {
        let server = MockServer::start().await;
        let root_hash = "a".repeat(64);
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(serde_json::json!({
                "packageId": "pkg-1",
                "rootHash": root_hash.clone(),
                "byteSize": 123456,
                "frameCount": 42,
                "aggregateStats": { "manifestXxh3": "deadbeefcafef00d", "fwhmMedian": 2.1 },
                "supersedes": ["ann-1", "ann-2"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "ann-9", "state": "pending"
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let req = AnnounceRequest {
            package_id: "pkg-1".into(),
            root_hash,
            byte_size: 123456,
            frame_count: 42,
            aggregate_stats: serde_json::json!({ "manifestXxh3": "deadbeefcafef00d", "fwhmMedian": 2.1 }),
            supersedes: vec!["ann-1".into(), "ann-2".into()],
        };
        let resp = client.announce("tok", "p-1", &req).await.unwrap();
        assert_eq!(resp.id, "ann-9");
        assert_eq!(resp.state, "pending");
    }

    /// The hub rejects duplicate `supersedes` ids, so the client pre-dedupes
    /// (order-preserving) before send. The mock matches ONLY the deduped list;
    /// a non-deduped body would miss it and 404.
    #[tokio::test]
    async fn announce_dedupes_supersedes_before_send() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .and(body_json(serde_json::json!({
                "packageId": "pkg-1",
                "rootHash": "b".repeat(64),
                "byteSize": 1,
                "frameCount": 1,
                "aggregateStats": {},
                "supersedes": ["ann-1", "ann-2", "ann-3"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "x", "state": "published"
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let req = AnnounceRequest {
            package_id: "pkg-1".into(),
            root_hash: "b".repeat(64),
            byte_size: 1,
            frame_count: 1,
            aggregate_stats: serde_json::json!({}),
            supersedes: vec![
                "ann-1".into(),
                "ann-2".into(),
                "ann-1".into(),
                "ann-3".into(),
                "ann-2".into(),
            ],
        };
        let resp = client.announce("tok", "p-1", &req).await.unwrap();
        assert_eq!(resp.state, "published");
    }

    /// A closed project and a globally-duplicate packageId both come back 409
    /// with an `{error}` body; each surfaces its distinct message (best-effort
    /// into the `Network` arm), never a swallowed/panicking decode.
    #[tokio::test]
    async fn announce_409_closed_and_duplicate_surface_distinct_messages() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "project is closed"
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let err = client.announce("tok", "p-1", &sample_announce_req()).await.unwrap_err();
        match err {
            AccountClientError::Network(msg) => assert!(msg.contains("closed"), "closed msg: {msg}"),
            other => panic!("expected Network for 409, got {other:?}"),
        }

        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "package already announced"
            })))
            .mount(&server2)
            .await;
        let client2 = CollabClient::new(server2.uri()).unwrap();
        let err2 = client2.announce("tok", "p-1", &sample_announce_req()).await.unwrap_err();
        match err2 {
            AccountClientError::Network(msg) => {
                assert!(msg.contains("already announced"), "dup msg: {msg}")
            }
            other => panic!("expected Network for 409, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn announce_401_maps_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let err = client.announce("tok", "p-1", &sample_announce_req()).await.unwrap_err();
        assert!(
            matches!(err, AccountClientError::Unauthorized),
            "401 must map to Unauthorized, got {err:?}"
        );
    }

    /// The list decodes holders, honors serde defaults for the announcement that
    /// omits `holders`/`supersedes`/`aggregateStats`, and ignores unknown
    /// (future) fields rather than failing the whole decode.
    #[tokio::test]
    async fn list_announcements_decodes_holders_and_tolerates_unknown_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "ann-1", "packageId": "pkg-1", "publisherDisplayName": "Vilen",
                    "own": true, "rootHash": "a".repeat(64), "byteSize": 100, "frameCount": 10,
                    "aggregateStats": { "manifestXxh3": "abcd" },
                    "supersedes": ["ann-0"],
                    "state": "published", "rejectReason": null,
                    "createdAt": "2026-07-13T00:00:00Z", "decidedAt": "2026-07-13T01:00:00Z",
                    "holders": [
                        { "pubkey": "cHVia2V5", "displayName": "Vilen", "lastSeenAt": "2026-07-13T02:00:00Z", "relayUrl": "https://holder-relay.example.org/" },
                        { "pubkey": "cHVia2V5Mg", "displayName": "Remote", "lastSeenAt": null }
                    ],
                    "someFutureField": { "nested": 1 }
                },
                {
                    "id": "ann-2", "packageId": "pkg-2", "publisherDisplayName": "Remote",
                    "own": false, "rootHash": "c".repeat(64), "byteSize": 200, "frameCount": 20,
                    "state": "pending", "rejectReason": null,
                    "createdAt": "2026-07-13T00:00:00Z", "decidedAt": null
                }
            ])))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let anns = client.list_announcements("tok", "p-1").await.unwrap();
        assert_eq!(anns.len(), 2);
        assert!(anns[0].own);
        assert_eq!(anns[0].holders.len(), 2);
        assert_eq!(anns[0].holders[0].display_name, "Vilen");
        // T7: the holder's self-reported relay url decodes (fed into the download
        // dial hint); a holder that reports no relay defaults to `None`.
        assert_eq!(anns[0].holders[0].relay_url.as_deref(), Some("https://holder-relay.example.org/"));
        assert_eq!(anns[0].holders[1].relay_url, None);
        assert_eq!(anns[0].holders[1].last_seen_at, None);
        assert_eq!(anns[0].supersedes, vec!["ann-0".to_string()]);
        assert_eq!(anns[0].aggregate_stats["manifestXxh3"], "abcd");
        // second announcement omits holders/supersedes/aggregateStats → defaults
        assert!(anns[1].holders.is_empty());
        assert!(anns[1].supersedes.is_empty());
        assert!(anns[1].aggregate_stats.is_null());
        assert_eq!(anns[1].reject_reason, None);
        assert_eq!(anns[1].decided_at, None);
    }

    #[tokio::test]
    async fn approve_returns_state() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/ann-1/approve"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "ann-1", "state": "published"
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let resp = client.approve_announcement("tok", "ann-1").await.unwrap();
        assert_eq!(resp.id, "ann-1");
        assert_eq!(resp.state, "published");
    }

    /// Reject posts `{reason}` verbatim (hub enforces the 1..=500 BYTE bound;
    /// the client just carries the string) and decodes the `{id, state}` reply.
    #[tokio::test]
    async fn reject_sends_reason_body_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/ann-1/reject"))
            .and(body_json(serde_json::json!({ "reason": "FWHM too high" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "ann-1", "state": "rejected"
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let resp = client
            .reject_announcement("tok", "ann-1", "FWHM too high")
            .await
            .unwrap();
        assert_eq!(resp.state, "rejected");
    }

    /// A decide 403 with no body must NOT panic on the empty decode — it maps to
    /// the `Network` arm (there is no dedicated forbidden variant).
    #[tokio::test]
    async fn decide_403_bodyless_maps_to_network_without_panic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/ann-1/approve"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let err = client.approve_announcement("tok", "ann-1").await.unwrap_err();
        assert!(
            matches!(err, AccountClientError::Network(_)),
            "403 body-less must map to Network, got {err:?}"
        );
    }

    #[tokio::test]
    async fn report_have_204_returns_ok() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/ann-1/have"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        client.report_have("tok", "ann-1").await.unwrap();
    }

    /// `have` with a portal-session (non-device) token is a 400 `{error}` on the
    /// hub — distinct from 401/403 — and must surface as `Network` with the
    /// message, never `Unauthorized`.
    #[tokio::test]
    async fn report_have_400_non_device_maps_to_network_not_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/ann-1/have"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "device token required"
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let err = client.report_have("tok", "ann-1").await.unwrap_err();
        match err {
            AccountClientError::Network(msg) => {
                assert!(msg.contains("device token required"), "message surfaced: {msg}")
            }
            other => panic!("expected Network for 400 (not Unauthorized), got {other:?}"),
        }
    }
}
