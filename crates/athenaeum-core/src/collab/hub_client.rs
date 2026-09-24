//! Collab endpoints of the athenaeum-hub (read side used by slice 3).
//!
//! Mirrors `account::client::HubClient`: base URL baked in, device token per
//! call via `bearer_auth`, `AccountClientError` for the shared 401→SignedOut
//! mapping at the api boundary. Endpoint contract: hub README "API —
//! Collaboration (Stage II)".

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
    /// Count of pending (unmoderated) frames in this project — the v3
    /// replacement for the old package-level `pendingAnnouncements`, which is
    /// no longer read.
    #[serde(default)]
    pub pending_frames: i64,
    /// This member's governance capabilities on the project (e.g.
    /// `"data.moderate"`); empty for an ordinary member.
    #[serde(default)]
    pub gov_caps: Vec<String>,
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
    /// `projects.version` — bumped by every change a device must see
    /// (membership, thresholds, dictionary, any manifest row, options).
    /// `None` on a hub that predates it.
    #[serde(default)]
    pub version: Option<i64>,
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

// ── Per-frame api (collab v3, wave 2) ────────────────────────────────────────

/// One entry of `GET /me/project-versions` — the cheap per-project version this
/// device already has cached, used to skip a manifest fetch when nothing
/// changed.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectVersionWire {
    pub project_id: String,
    pub version: i64,
}

/// One row of `GET /projects/{id}/manifest` — a published (or moderation-
/// pending) frame from ANY member, never the local file itself.
///
/// Also `Serialize`: `db::collab_frames::upsert_from_manifest` round-trips the
/// whole row into `project_frames_local.manifest_json` so the local cache can
/// answer from disk without re-fetching the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameViewWire {
    pub frame_uuid: String,
    pub publisher_account_id: String,
    pub publisher_display_name: String,
    pub own: bool,
    pub file_name: String,
    pub content_version: i32,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
    pub filter_raw: String,
    pub filter_canonical: String,
    pub channel: String,
    pub exptime_sec: f64,
    pub date_obs: Option<String>,
    #[serde(default)]
    pub meta: serde_json::Value,
    pub gate_version: i32,
    pub accepted: bool,
    pub accepted_reason: Option<String>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub manifest_version: i64,
    pub created_at: String,
    /// How many devices currently report holding this frame's content
    /// (`put_holders`). Absent on an older hub → `0`.
    #[serde(default)]
    pub holder_count: i64,
}

/// The cursor to resume a paged manifest fetch from (`next` on
/// [`ManifestPageWire`] when `has_more`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestCursorWire {
    pub since: i64,
    pub after: String,
}

/// One page of `GET /projects/{id}/manifest`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPageWire {
    pub project_version: i64,
    pub rows: Vec<FrameViewWire>,
    pub has_more: bool,
    pub next: Option<ManifestCursorWire>,
}

/// One frame of a `POST /projects/{id}/frames` batch (up to 500 per call).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameInWire {
    pub frame_uuid: String,
    pub file_name: String,
    pub blake3: String,
    pub byte_size: i64,
    pub xxh3: String,
    pub filter_raw: String,
    pub filter_canonical: String,
    pub channel: String,
    pub exptime_sec: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_obs: Option<String>,
    pub gate_version: i32,
    pub meta: serde_json::Value,
}

/// Reply to `POST /projects/{id}/frames`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceFramesWire {
    pub state: String,
    pub project_version: i64,
    pub announced: usize,
}

/// Reply to `POST /projects/{id}/frames/{uuid}/version` — a new content
/// version (re-calibration) superseding the old one.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewVersionWire {
    pub content_version: i32,
    pub project_version: i64,
}

/// One `(frame_uuid, content_version)` this device holds, for
/// `PUT /projects/{id}/holders/self`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderRefWire {
    pub frame_uuid: String,
    pub content_version: i32,
}

/// One entry of the project's filter/channel dictionary.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntryWire {
    pub canonical: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub kind: String,
}

/// One versioned dictionary set.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionarySetWire {
    pub version: i32,
    pub entries: Vec<DictionaryEntryWire>,
}

/// Reply to `GET /projects/{id}/dictionary`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryWire {
    pub current: Option<DictionarySetWire>,
}

pub struct CollabClient {
    http: reqwest::Client,
    base_url: String,
}

fn net(e: reqwest::Error) -> AccountClientError {
    AccountClientError::Network(e.to_string())
}

/// One status classifier for every GET/POST/PUT/PATCH call this client makes
/// against the per-frame api (`get_json` and every new v3 method below —
/// the old package-api methods keep their own [`unexpected`]): `401` →
/// `Unauthorized`; `403` → `Forbidden`; a `409` whose body `{error}` is
/// EXACTLY `"collab_api_outdated"` → `CollabApiOutdated` (this client
/// predates the per-frame api); everything else → `Network`, carrying the
/// status, the call (`what`), and the hub's best-effort message.
async fn classify(status: StatusCode, resp: reqwest::Response, what: &str) -> AccountClientError {
    match status {
        StatusCode::UNAUTHORIZED => AccountClientError::Unauthorized,
        StatusCode::FORBIDDEN => AccountClientError::Forbidden,
        StatusCode::CONFLICT => {
            let msg = body_message(resp).await;
            if msg == "collab_api_outdated" {
                AccountClientError::CollabApiOutdated
            } else {
                network_status(StatusCode::CONFLICT, what, &msg)
            }
        }
        s => {
            let msg = body_message(resp).await;
            network_status(s, what, &msg)
        }
    }
}

/// Shared `Network` message builder for [`classify`]: `hub returned {status}
/// ({what})` plus `: {msg}` when the hub's best-effort message is non-empty.
fn network_status(status: StatusCode, what: &str, msg: &str) -> AccountClientError {
    if msg.is_empty() {
        AccountClientError::Network(format!("hub returned {status} ({what})"))
    } else {
        AccountClientError::Network(format!("hub returned {status} ({what}): {msg}"))
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
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<T>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode {what}: {e}")));
        }
        Err(classify(status, resp, what).await)
    }

    /// The hub's snapshot-signing pubkey (base64). Fetched once and pinned.
    pub async fn collab_pubkey(&self) -> Result<String, AccountClientError> {
        let wire: PubkeyWire = self
            .get_json("/collab/pubkey", None, "collab pubkey")
            .await?;
        Ok(wire.pubkey)
    }

    pub async fn my_projects(&self, token: &str) -> Result<Vec<MyProjectWire>, AccountClientError> {
        self.get_json("/me/projects", Some(token), "my projects")
            .await
    }

    /// Public page (no token) — target/members for the cache.
    pub async fn project_page(
        &self,
        id_or_slug: &str,
    ) -> Result<ProjectPageWire, AccountClientError> {
        self.get_json(&format!("/projects/{id_or_slug}"), None, "project page")
            .await
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

    // ── Per-frame api (collab v3, wave 2) ────────────────────────────────────

    /// `GET /me/project-versions` — every project I'm a member of, with the
    /// hub's current `version` for each, so the caller can skip a manifest
    /// fetch for a project whose cached version already matches.
    pub async fn project_versions(
        &self,
        token: &str,
    ) -> Result<Vec<ProjectVersionWire>, AccountClientError> {
        self.get_json("/me/project-versions", Some(token), "project versions")
            .await
    }

    /// `GET /projects/{id}/manifest` — one page of frame rows with
    /// `manifestVersion > since` (`since = 0` = full manifest), resumed via
    /// `after` when the previous page's `hasMore` was true. `limit` is passed
    /// through verbatim; clamping it to the hub's 1..=1000 page bound is the
    /// caller's job.
    pub async fn manifest_page(
        &self,
        token: &str,
        project_id: &str,
        since: i64,
        after: Option<&str>,
        limit: u32,
    ) -> Result<ManifestPageWire, AccountClientError> {
        let mut query: Vec<(&str, String)> =
            vec![("since", since.to_string()), ("limit", limit.to_string())];
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        let resp = self
            .http
            .get(self.url(&format!("/projects/{project_id}/manifest")))
            .bearer_auth(token)
            .query(&query)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<ManifestPageWire>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode manifest page: {e}")));
        }
        Err(classify(status, resp, "manifest page").await)
    }

    /// `POST /projects/{id}/frames` — announce a batch of frames (up to 500 per
    /// call; the hub enforces the cap). A `409 {"error":"collab_api_outdated"}`
    /// is the ONE place a stale client actually learns it is stale (this is the
    /// write path every publish goes through).
    pub async fn announce_frames(
        &self,
        token: &str,
        project_id: &str,
        frames: &[FrameInWire],
    ) -> Result<AnnounceFramesWire, AccountClientError> {
        let resp = self
            .http
            .post(self.url(&format!("/projects/{project_id}/frames")))
            .bearer_auth(token)
            .json(&serde_json::json!({ "frames": frames }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<AnnounceFramesWire>()
                .await
                .map_err(|e| AccountClientError::Network(format!("decode announce frames: {e}")));
        }
        Err(classify(status, resp, "announce frames").await)
    }

    /// `POST /projects/{id}/frames/{uuid}/version` — publish a new content
    /// version of an already-published frame (re-calibration); the hub sets
    /// `supersededBy` on the prior version.
    pub async fn new_frame_version(
        &self,
        token: &str,
        project_id: &str,
        frame_uuid: &str,
        blake3: &str,
        byte_size: i64,
        xxh3: &str,
    ) -> Result<NewVersionWire, AccountClientError> {
        let resp = self
            .http
            .post(self.url(&format!(
                "/projects/{project_id}/frames/{frame_uuid}/version"
            )))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "blake3": blake3,
                "byteSize": byte_size,
                "xxh3": xxh3,
            }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp.json::<NewVersionWire>().await.map_err(|e| {
                AccountClientError::Network(format!("decode new frame version: {e}"))
            });
        }
        Err(classify(status, resp, "new frame version").await)
    }

    /// `POST /projects/{id}/frames/{uuid}/approve` — first-publication
    /// moderation. Always sends `{"trust": trust}`; `trust` also marks the
    /// publisher trusted for future first-publications (the portal's default
    /// on approve) and can retroactively publish every OTHER pending frame
    /// from the same publisher in the same call. Returns how many frames the
    /// hub actually published as a result of this call (the hub's
    /// `{"published": N}`; without `trust` that's the one requested frame,
    /// with it, N can be greater).
    pub async fn approve_frame(
        &self,
        token: &str,
        project_id: &str,
        frame_uuid: &str,
        trust: bool,
    ) -> Result<u64, AccountClientError> {
        #[derive(Deserialize)]
        struct ApproveReply {
            published: u64,
        }
        let resp = self
            .http
            .post(self.url(&format!(
                "/projects/{project_id}/frames/{frame_uuid}/approve"
            )))
            .bearer_auth(token)
            .json(&serde_json::json!({ "trust": trust }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<ApproveReply>()
                .await
                .map(|r| r.published)
                .map_err(|e| AccountClientError::Network(format!("decode approve frame: {e}")));
        }
        Err(classify(status, resp, "approve frame").await)
    }

    /// `POST /projects/{id}/frames/{uuid}/reject` with body `{reason}` —
    /// refused once any holder other than publisher/coordinator exists (the
    /// hub enforces this; a 409 there surfaces via `Network`).
    pub async fn reject_frame(
        &self,
        token: &str,
        project_id: &str,
        frame_uuid: &str,
        reason: &str,
    ) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .post(self.url(&format!(
                "/projects/{project_id}/frames/{frame_uuid}/reject"
            )))
            .bearer_auth(token)
            .json(&serde_json::json!({ "reason": reason }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        Err(classify(status, resp, "reject frame").await)
    }

    /// `PUT /projects/{id}/holders/self` — this device's holder report for the
    /// project: `full = true` replaces the whole set, `false` applies `add`/
    /// `remove` as a delta. Always sends all three keys, even when `add`/
    /// `remove` are empty (an empty `add` under `full = true` is a real
    /// statement: "I hold nothing here any more").
    pub async fn put_holders(
        &self,
        token: &str,
        project_id: &str,
        full: bool,
        add: &[HolderRefWire],
        remove: &[String],
    ) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .put(self.url(&format!("/projects/{project_id}/holders/self")))
            .bearer_auth(token)
            .json(&serde_json::json!({ "full": full, "add": add, "remove": remove }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::NO_CONTENT || status == StatusCode::OK {
            return Ok(());
        }
        Err(classify(status, resp, "put holders").await)
    }

    /// `GET /projects/{id}/frames/{uuid}/holders` — every device currently
    /// reporting it holds this frame's content.
    pub async fn frame_holders(
        &self,
        token: &str,
        project_id: &str,
        frame_uuid: &str,
    ) -> Result<Vec<HolderWire>, AccountClientError> {
        self.get_json(
            &format!("/projects/{project_id}/frames/{frame_uuid}/holders"),
            Some(token),
            "frame holders",
        )
        .await
    }

    /// `GET /projects/{id}/dictionary` — the project's current filter/channel
    /// dictionary (canonical names + aliases the gate and the manifest both
    /// validate against).
    pub async fn dictionary(
        &self,
        token: &str,
        project_id: &str,
    ) -> Result<DictionaryWire, AccountClientError> {
        self.get_json(
            &format!("/projects/{project_id}/dictionary"),
            Some(token),
            "dictionary",
        )
        .await
    }
}

#[cfg(test)]
#[allow(deprecated)] // exercises the wave-2 Task-11-deprecated package methods
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn my_projects_decodes_and_maps_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me/projects"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "p-1", "slug": "m101", "title": "M 101", "dataRole": "send_receive",
                    "coordinator": true, "requireApproval": true, "pendingFrames": 2
                }])),
            )
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        let mine = client.my_projects("tok").await.unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].slug, "m101");
        assert!(mine[0].coordinator);
        assert_eq!(mine[0].pending_frames, 2);

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

    // ---- per-frame api (collab v3 wave 2, Task 1) ----

    /// A `409 {"error":"collab_api_outdated"}` is the hub's typed refusal of a
    /// client speaking a stale collab api — it must decode to
    /// [`AccountClientError::CollabApiOutdated`] on both a GET (`get_json`) and a
    /// POST path, not fall into the generic `Network` bucket.
    #[tokio::test]
    async fn outdated_api_409_is_typed_on_get_and_post() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me/project-versions"))
            .respond_with(
                ResponseTemplate::new(409).set_body_json(json!({"error":"collab_api_outdated"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames"))
            .respond_with(
                ResponseTemplate::new(409).set_body_json(json!({"error":"collab_api_outdated"})),
            )
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        assert!(matches!(
            c.project_versions("t").await,
            Err(AccountClientError::CollabApiOutdated)
        ));
        assert!(matches!(
            c.announce_frames("t", "p1", &[]).await,
            Err(AccountClientError::CollabApiOutdated)
        ));
    }

    /// A 409 for any OTHER reason (a stale gate version, not an outdated client)
    /// must keep surfacing the hub's message via `Network`, never get relabeled
    /// as `CollabApiOutdated`.
    #[tokio::test]
    async fn other_409_keeps_the_hub_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "error":"gate version 1 is stale, current is 2"
            })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let err = c.announce_frames("t", "p1", &[]).await.unwrap_err();
        assert!(err.to_string().contains("gate version 1 is stale"), "{err}");
    }

    /// `manifest_page` passes `since`/`after`/`limit` as query params (not a
    /// body) and decodes the paged rows, including the per-row `holderCount`.
    #[tokio::test]
    async fn manifest_page_passes_cursor_and_decodes_rows() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/manifest"))
            .and(query_param("since", "7"))
            .and(query_param("after", "u9"))
            .and(query_param("limit", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "projectVersion": 9, "hasMore": false, "next": null,
                "rows": [{ "frameUuid":"u10","publisherAccountId":"a","publisherDisplayName":"Ann","own":false,
                  "fileName":"c_x.fits","contentVersion":1,"blake3":"b".repeat(64),"byteSize":10,
                  "xxh3":"0123456789abcdef","filterRaw":"Red","filterCanonical":"R","channel":"mono",
                  "exptimeSec":300.0,"dateObs":null,"meta":{},"gateVersion":0,"accepted":true,
                  "acceptedReason":null,"state":"published","rejectReason":null,"manifestVersion":9,
                  "createdAt":"2026-09-24T00:00:00Z","holderCount":2 }]
            })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let page = c
            .manifest_page("t", "p1", 7, Some("u9"), 1000)
            .await
            .unwrap();
        assert_eq!(page.project_version, 9);
        assert_eq!(page.rows[0].filter_canonical, "R");
        assert_eq!(page.rows[0].holder_count, 2);
    }

    /// `my_projects` reads the v3 `pendingFrames`/`govCaps` fields and no longer
    /// reads the old `pendingAnnouncements` alias at all (a hub that still sends
    /// it decodes fine — the unknown field is just ignored).
    #[tokio::test]
    async fn my_projects_reads_pending_frames_and_caps_without_the_alias() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id":"p1","slug":"m31","title":"M31","dataRole":"send_receive","coordinator":false,
                "requireApproval":true,"pendingFrames":3,"govCaps":["data.moderate"]
            }])))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let p = &c.my_projects("t").await.unwrap()[0];
        assert_eq!(p.pending_frames, 3);
        assert_eq!(p.gov_caps, vec!["data.moderate".to_string()]);
    }

    /// `put_holders` always sends all three keys (`full`/`add`/`remove`), never
    /// omitting an empty `remove`.
    #[tokio::test]
    async fn put_holders_sends_full_add_remove() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/projects/p1/holders/self"))
            .and(body_json(json!({
                "full": true,
                "add": [{"frameUuid":"u1","contentVersion":2}],
                "remove": []
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        c.put_holders(
            "t",
            "p1",
            true,
            &[HolderRefWire {
                frame_uuid: "u1".into(),
                content_version: 2,
            }],
            &[],
        )
        .await
        .unwrap();
    }

    // ---- per-frame api, wire shapes pinned against the hub (fix round 1) ----

    /// `approve_frame` always sends `{"trust": trust}` and decodes the hub's
    /// `{"published": N}` reply — under `trust` the hub can publish more than
    /// the one requested frame (every other pending frame from the same
    /// publisher), so the count is NOT pinned to 1.
    #[tokio::test]
    async fn approve_frame_sends_trust_and_returns_published_count() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/u1/approve"))
            .and(header("authorization", "Bearer t"))
            .and(body_json(json!({ "trust": true })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "published": 3 })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let published = c.approve_frame("t", "p1", "u1", true).await.unwrap();
        assert_eq!(published, 3);
    }

    /// `reject_frame` sends `{"reason": reason}`; the hub's `{"state":
    /// "rejected"}` reply carries no data this client reads (`Result<(), _>`).
    #[tokio::test]
    async fn reject_frame_sends_reason_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/u1/reject"))
            .and(body_json(json!({ "reason": "bad focus" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "state": "rejected" })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        c.reject_frame("t", "p1", "u1", "bad focus").await.unwrap();
    }

    /// `new_frame_version` sends `{blake3, byteSize, xxh3}` and decodes
    /// `{contentVersion, projectVersion}`.
    #[tokio::test]
    async fn new_frame_version_sends_body_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/u1/version"))
            .and(body_json(json!({
                "blake3": "a".repeat(64), "byteSize": 100, "xxh3": "0123456789abcdef"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "contentVersion": 2, "projectVersion": 11
            })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let v = c
            .new_frame_version("t", "p1", "u1", &"a".repeat(64), 100, "0123456789abcdef")
            .await
            .unwrap();
        assert_eq!(v.content_version, 2);
        assert_eq!(v.project_version, 11);
    }

    /// `frame_holders` decodes the hub's holder list — same shape as an
    /// announcement's `holders` (pubkey/displayName/lastSeenAt/relayUrl).
    #[tokio::test]
    async fn frame_holders_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/frames/u1/holders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "pubkey": "cHVia2V5", "displayName": "Vilen",
                  "lastSeenAt": "2026-09-24T00:00:00Z",
                  "relayUrl": "https://relay.example.org/" }
            ])))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let holders = c.frame_holders("t", "p1", "u1").await.unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].display_name, "Vilen");
        assert_eq!(
            holders[0].relay_url.as_deref(),
            Some("https://relay.example.org/")
        );
    }

    /// `dictionary` decodes a present current set — ignoring the hub's extra
    /// `createdAt`/top-level `history` fields this client doesn't read — and
    /// an absent one (`current: null`).
    #[tokio::test]
    async fn dictionary_decodes_present_and_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/dictionary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "current": {
                    "version": 2,
                    "entries": [{ "canonical": "Ha", "aliases": ["H-alpha"], "kind": "narrowband" }],
                    "createdAt": "2026-09-24T00:00:00Z"
                },
                "history": []
            })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let dict = c.dictionary("t", "p1").await.unwrap();
        let current = dict.current.unwrap();
        assert_eq!(current.version, 2);
        assert_eq!(current.entries[0].canonical, "Ha");
        assert_eq!(current.entries[0].aliases, vec!["H-alpha".to_string()]);

        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/dictionary"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "current": null, "history": [] })),
            )
            .mount(&server2)
            .await;
        let c2 = CollabClient::new(server2.uri()).unwrap();
        let dict2 = c2.dictionary("t", "p1").await.unwrap();
        assert!(dict2.current.is_none());
    }
}
