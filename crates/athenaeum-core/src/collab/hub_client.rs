//! Collab endpoints of the athenaeum-hub (read side used by slice 3).
//!
//! Mirrors `account::client::HubClient`: base URL baked in, device token per
//! call via `bearer_auth`, `AccountClientError` for the shared 401→SignedOut
//! mapping at the api boundary. Endpoint contract: hub README "API —
//! Collaboration (Stage II)".

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::account::AccountClientError;
use crate::collab::live::wire::{
    BeatWire, HolderDeltaPageWire, HoldersReportReplyWire, HoldersReportWire, HoldersSnapshotWire,
    VersionInWire, VersionsReplyWire,
};

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
    /// The one device of this account that may announce NEW frames into the
    /// project (amendment A6), or `None` when nothing is bound or the bound
    /// device is revoked/retired — "no device is publishing yet", never
    /// "publishing here". Absent on a hub that predates A6 → `None`.
    #[serde(default)]
    pub publishing_device: Option<PublishingDeviceWire>,
}

/// `/me/projects` → `publishingDevice` (amendment A6).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishingDeviceWire {
    /// Base64 of the bound device's public key (P3) — the same string as
    /// `GET /devices` → `pubkey` and `own_device_id`.
    pub device_id: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Reply to `PUT /projects/{id}/publishing-device` (amendment A6).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishingDeviceSwitchWire {
    /// The caller's base64 public key — the device now bound.
    pub device_id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub project_version: i64,
    /// `false` when the caller was already bound (no bump, no event).
    pub changed: bool,
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
    /// Per-filter integration goals (`{canonical: seconds}`, spec
    /// 2026-09-29 §5.5). `None` when the hub has none set, or predates the
    /// field.
    #[serde(default)]
    pub goals: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberWire {
    pub display_name: String,
    pub data_role: String,
    pub coordinator: bool,
    /// This member's last-seen timestamp (ISO 8601), filled by the hub only
    /// for an authenticated co-member (spec 2026-09-29 §5.6/D8) — `None` on
    /// an anonymous fetch, for a member who has never been seen, or on a hub
    /// that predates the field.
    #[serde(default)]
    pub last_seen_at: Option<String>,
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

// ── Per-frame api (collab v3, wave 2) ────────────────────────────────────────

/// One row of `GET /projects/{id}/manifest` — a published (or moderation-
/// pending) frame from ANY member, never the local file itself.
///
/// Also `Serialize`: `db::collab_frames::upsert_from_manifest` round-trips the
/// whole row into `project_frames_local.manifest_json` so the local cache can
/// answer from disk without re-fetching the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameViewWire {
    pub frame_uuid: String,
    /// A dense per-project frame ordinal, assigned at announce and never
    /// reused. Absent on an older hub → `0` (only the v3 wave-2 fixtures that
    /// predate the field construct one without it).
    #[serde(default)]
    pub frame_seq: i32,
    pub publisher_account_id: String,
    pub publisher_display_name: String,
    /// The account-level `own` the hub computes (`publisherAccountId ==
    /// caller`). Since amendment A6 the app never trusts it: the manifest
    /// appliers overwrite it with the DEVICE-own derivation
    /// (`api::collab_exchange::derive_device_own`) before anything reads it.
    #[serde(default)]
    pub own: bool,
    /// Base64 public key of the device that announced (or, after an accepted
    /// fallback version, adopted) the frame — amendment A6. `None` when the
    /// hub has no recorded device (and on a hub that predates A6).
    #[serde(default)]
    pub publisher_device_id: Option<String>,
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

/// Map a `.json::<T>()` failure on an otherwise-successful response.
/// `reqwest::Error::is_decode()` is true exactly when the body was read fine
/// but didn't parse into `T` — a permanent shape mismatch that retrying can
/// never fix, so it becomes [`AccountClientError::Decode`] (never retried)
/// and is logged at `error!` here, once, at the point of failure. A body-read
/// failure (timeout, connection drop mid-body) is not a decode error and
/// stays [`AccountClientError::Network`] (retried).
fn decode_err(what: &str, e: reqwest::Error) -> AccountClientError {
    if e.is_decode() {
        tracing::error!(command = what, error = %e, "hub response body did not decode; not retrying");
        AccountClientError::Decode(format!("decode {what}: {e}"))
    } else {
        AccountClientError::Network(format!("decode {what}: {e}"))
    }
}

/// One status classifier for every GET/POST/PUT/PATCH call this client makes
/// against the per-frame api (`get_json` and every new v3 method below):
/// `401` → `Unauthorized`; `403` → `Forbidden`; `429` → `RateLimited` (spec
/// §4.6: retried); a `409`/`410` whose body `{error}` is a recognised typed
/// refusal (`collab_api_outdated`, `session_gone`, `version_conflict`) → that
/// typed variant; any other `410` → [`AccountClientError::Gone`] with the
/// hub's `error` string (the caller matches `Gone(e)` directly for the
/// specific 410 cases it handles itself — `hub_text()` does not cover it);
/// everything else → [`AccountClientError::Http`], carrying the status, the
/// call (`what`), and the hub's best-effort message.
async fn classify(status: StatusCode, resp: reqwest::Response, what: &str) -> AccountClientError {
    match status {
        StatusCode::UNAUTHORIZED => AccountClientError::Unauthorized,
        StatusCode::FORBIDDEN => AccountClientError::Forbidden,
        StatusCode::TOO_MANY_REQUESTS => AccountClientError::RateLimited,
        StatusCode::CONFLICT | StatusCode::GONE => {
            let text = resp.text().await.unwrap_or_default();
            let json: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let error = json
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match (status, error.as_str()) {
                (_, "collab_api_outdated") => AccountClientError::CollabApiOutdated,
                (_, "session_gone") => AccountClientError::SessionGone,
                (_, "version_conflict") => AccountClientError::VersionConflict {
                    content_version: json
                        .get("contentVersion")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32,
                },
                (StatusCode::CONFLICT, "publishing_device") => {
                    let (device_id, device_name) = device_of(&json);
                    AccountClientError::PublishingDevice {
                        device_id,
                        device_name,
                    }
                }
                (StatusCode::CONFLICT, "not_publishing_device") => {
                    let (device_id, device_name) = device_of(&json);
                    AccountClientError::NotPublishingDevice {
                        device_id,
                        device_name,
                    }
                }
                (StatusCode::GONE, _) => AccountClientError::Gone(error),
                _ => http_status(
                    status,
                    what,
                    if error.is_empty() {
                        text.trim()
                    } else {
                        &error
                    },
                ),
            }
        }
        s => {
            let msg = body_message(resp).await;
            http_status(s, what, &msg)
        }
    }
}

/// `(deviceId, deviceName)` of an A6 refusal body (`publishing_device` /
/// `not_publishing_device`): an absent id reads as empty, a null or absent
/// name as `None`.
fn device_of(json: &serde_json::Value) -> (String, Option<String>) {
    (
        json.get("deviceId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        json.get("deviceName")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    )
}

/// [`AccountClientError::Http`] with the wave-2 message text (was
/// `network_status`): `hub returned {status} ({what})` plus `: {msg}` when
/// the hub's best-effort message is non-empty.
fn http_status(status: StatusCode, what: &str, msg: &str) -> AccountClientError {
    let message = if msg.is_empty() {
        format!("hub returned {status} ({what})")
    } else {
        format!("hub returned {status} ({what}): {msg}")
    };
    AccountClientError::Http {
        status: status.as_u16(),
        message,
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
            return resp.json::<T>().await.map_err(|e| decode_err(what, e));
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

    /// Project page — sent WITH the device token so the hub's member-only
    /// fields (`lastSeenAt`, spec 2026-09-29 D8) are filled; `None` keeps the
    /// anonymous public view.
    pub async fn project_page(
        &self,
        id_or_slug: &str,
        token: Option<&str>,
    ) -> Result<ProjectPageWire, AccountClientError> {
        self.get_json(&format!("/projects/{id_or_slug}"), token, "project page")
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
                .map_err(|e| decode_err("manifest page", e));
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
                .map_err(|e| decode_err("announce frames", e));
        }
        Err(classify(status, resp, "announce frames").await)
    }

    /// `POST /projects/{id}/frames/{uuid}/version` — publish a new content
    /// version of an already-published frame (re-calibration); the hub sets
    /// `supersededBy` on the prior version. `expected_version` is the version
    /// this update supersedes (optimistic concurrency, hub § "Frames —
    /// versions"); a mismatch answers `409 version_conflict` →
    /// [`AccountClientError::VersionConflict`].
    pub async fn new_frame_version(
        &self,
        token: &str,
        project_id: &str,
        frame_uuid: &str,
        expected_version: i32,
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
                "expectedVersion": expected_version,
                "blake3": blake3,
                "byteSize": byte_size,
                "xxh3": xxh3,
            }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<NewVersionWire>()
                .await
                .map_err(|e| decode_err("new frame version", e));
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
                .map_err(|e| decode_err("approve frame", e));
        }
        Err(classify(status, resp, "approve frame").await)
    }

    /// `POST /projects/{id}/frames/{uuid}/reject` with body `{reason}` —
    /// refused once any holder other than publisher/coordinator exists (the
    /// hub enforces this; a 409 there surfaces via `Http`).
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

    // ── Live exchange (collab v3, wave 3) ────────────────────────────────────

    /// `GET /projects/{id}/holders/snapshot` — one REPEATABLE READ read of the
    /// project's whole holder map, and the `(epoch, holderSeq, version)`
    /// cursors it reflects.
    pub async fn holders_snapshot(
        &self,
        token: &str,
        project_id: &str,
    ) -> Result<HoldersSnapshotWire, AccountClientError> {
        self.get_json(
            &format!("/projects/{project_id}/holders/snapshot"),
            Some(token),
            "holders snapshot",
        )
        .await
    }

    /// `GET /projects/{id}/holders?since=…` — one page of holder changes
    /// since `since`, resumed via `after` when the previous page's `hasMore`
    /// was true. `epoch`, when given, asks the hub to answer `410
    /// epoch_changed` if it no longer matches. A `410` (below the floor,
    /// ahead of the cursor, or an epoch change) surfaces as
    /// [`AccountClientError::Gone`] with the hub's `error` string — the
    /// caller reloads the snapshot in every case.
    pub async fn holders_since(
        &self,
        token: &str,
        project_id: &str,
        since: i64,
        after: Option<&str>,
        epoch: Option<&str>,
    ) -> Result<HolderDeltaPageWire, AccountClientError> {
        let mut query: Vec<(&str, String)> = vec![("since", since.to_string())];
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        if let Some(epoch) = epoch {
            query.push(("epoch", epoch.to_string()));
        }
        let resp = self
            .http
            .get(self.url(&format!("/projects/{project_id}/holders")))
            .bearer_auth(token)
            .query(&query)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<HolderDeltaPageWire>()
                .await
                .map_err(|e| decode_err("holders delta", e));
        }
        Err(classify(status, resp, "holders delta").await)
    }

    /// `PUT /projects/{id}/holders/self` — this device's coalesced claim
    /// report: `full: true` replaces the whole claim set as of `report_seq`,
    /// `false` applies `add`/`remove` as a delta. `digest`/`count` describe
    /// the whole claim set AFTER this report (spec §6.3); a `digestMatch:
    /// false` reply means the caller must send one `full: true` report next.
    pub async fn report_holders(
        &self,
        token: &str,
        project_id: &str,
        body: &HoldersReportWire,
    ) -> Result<HoldersReportReplyWire, AccountClientError> {
        let resp = self
            .http
            .put(self.url(&format!("/projects/{project_id}/holders/self")))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<HoldersReportReplyWire>()
                .await
                .map_err(|e| decode_err("holders report reply", e));
        }
        Err(classify(status, resp, "report holders").await)
    }

    /// `POST /projects/{id}/frames/versions` — a batch (1..=500) of optimistic
    /// CAS version bumps in request order. Per-entry outcomes come back as
    /// `status: ok | conflict | not_found | forbidden` in
    /// [`VersionsReplyWire`]; only a whole-batch failure (bad shape, closed
    /// project, non-member) is a `Err` here.
    pub async fn frame_versions(
        &self,
        token: &str,
        project_id: &str,
        versions: &[VersionInWire],
    ) -> Result<VersionsReplyWire, AccountClientError> {
        let resp = self
            .http
            .post(self.url(&format!("/projects/{project_id}/frames/versions")))
            .bearer_auth(token)
            .json(&serde_json::json!({ "versions": versions }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<VersionsReplyWire>()
                .await
                .map_err(|e| decode_err("versions batch", e));
        }
        Err(classify(status, resp, "frame versions batch").await)
    }

    /// `PUT /projects/{id}/publishing-device` — make THIS device (the
    /// token's) the project's publishing device for its account (amendment
    /// A6). Idempotent (`changed: false` when already bound). No body is
    /// read by the hub; `{}` is sent.
    pub async fn set_publishing_device(
        &self,
        token: &str,
        project_id: &str,
    ) -> Result<PublishingDeviceSwitchWire, AccountClientError> {
        let resp = self
            .http
            .put(self.url(&format!("/projects/{project_id}/publishing-device")))
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK {
            return resp
                .json::<PublishingDeviceSwitchWire>()
                .await
                .map_err(|e| decode_err("publishing device", e));
        }
        Err(classify(status, resp, "publishing device").await)
    }

    /// `POST /me/presence` — the presence beat. Session-authenticated, no
    /// bearer (hub § "Presence beat"). `409 {"error":"session_gone"}` →
    /// [`AccountClientError::SessionGone`]: reopen the event stream at once.
    pub async fn presence_beat(&self, body: &BeatWire) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .post(self.url("/me/presence"))
            .json(body)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::NO_CONTENT || status == StatusCode::OK {
            return Ok(());
        }
        Err(classify(status, resp, "presence beat").await)
    }

    /// `DELETE /me/presence` — go offline at once. No bearer; a `204` is
    /// success even for an unknown session (hub § "Presence beat").
    pub async fn presence_leave(&self, session_id: &str) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .delete(self.url("/me/presence"))
            .json(&serde_json::json!({ "sessionId": session_id }))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::NO_CONTENT || status == StatusCode::OK {
            return Ok(());
        }
        Err(classify(status, resp, "presence leave").await)
    }

    /// The hub base URL this client is bound to (no trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Whether the caller keeps retrying a failed hub call forever, or gives up
/// after a bounded number of attempts (spec §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryPolicy {
    /// Owned by the live session: retries until it succeeds or the future is
    /// dropped (the session closing cancels it).
    Background,
    /// A user-triggered action: at most [`INTERACTIVE_ATTEMPTS`] attempts,
    /// so a foreground click doesn't hang indefinitely.
    Interactive,
}

pub const INTERACTIVE_ATTEMPTS: u32 = 3;

/// Transport errors, 5xx and 429 retry (spec §4.6); 401/403/409/410 never do
/// — they are refusals no amount of retrying fixes. Nor does
/// [`AccountClientError::Decode`] — a response body that didn't parse into
/// the expected type is a permanent shape mismatch, not a transient failure.
pub fn is_retryable(e: &AccountClientError) -> bool {
    match e {
        AccountClientError::Network(_) | AccountClientError::RateLimited => true,
        AccountClientError::Http { status, .. } => *status >= 500,
        AccountClientError::Decode(_) => false,
        _ => false,
    }
}

/// Run `op` with full-jitter back-off (spec §4.6) until it succeeds, hits a
/// non-retryable error, or (under [`RetryPolicy::Interactive`]) exhausts
/// [`INTERACTIVE_ATTEMPTS`]. A [`crate::collab::live::backoff::reset_all`]
/// while sleeping restarts the back-off from attempt 0 (Sync now, P26).
pub async fn with_retry<T, F, Fut>(
    what: &'static str,
    policy: RetryPolicy,
    mut op: F,
) -> Result<T, AccountClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, AccountClientError>>,
{
    use crate::collab::live::backoff::{reset_signal, sleep_or_reset, Backoff};
    let mut backoff = Backoff::new();
    let mut reset = reset_signal();
    loop {
        match op().await {
            Ok(v) => {
                if backoff.attempt() > 0 {
                    tracing::info!(
                        command = what,
                        attempt = backoff.attempt(),
                        "hub request recovered"
                    );
                }
                return Ok(v);
            }
            Err(e) if is_retryable(&e) => {
                if policy == RetryPolicy::Interactive
                    && backoff.attempt() + 1 >= INTERACTIVE_ATTEMPTS
                {
                    tracing::warn!(command = what, attempt = backoff.attempt() + 1, error = %e, "hub request failed; giving up");
                    return Err(e);
                }
                let delay = backoff.next_delay();
                if backoff.attempt() == 1 {
                    tracing::warn!(command = what, error = %e, retry_in_ms = delay.as_millis() as u64, "hub request failed; retrying");
                } else {
                    tracing::debug!(command = what, attempt = backoff.attempt(), error = %e, retry_in_ms = delay.as_millis() as u64, "hub request failed again; retrying");
                }
                if sleep_or_reset(delay, &mut reset).await {
                    backoff.reset();
                }
            }
            Err(e) => return Err(e),
        }
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
        let page = client.project_page("m101", None).await.unwrap();
        assert_eq!(page.project.target.radius_deg, 1.5);
        assert_eq!(page.members[0].display_name, "Vilen");
        let th = client.thresholds("tok", "p-1").await.unwrap();
        assert_eq!(th.current.unwrap().version, 3);
    }

    #[tokio::test]
    async fn project_page_sends_the_device_token_and_decodes_goals_and_last_seen() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": {"id": "p1", "slug": "m31", "title": "M31", "status": "active",
                            "requireApproval": false,
                            "target": {"name": "M31", "raDeg": 10.68, "decDeg": 41.27, "radiusDeg": 1.5},
                            "version": 3, "goals": {"Ha": 216000}},
                "members": [{"displayName": "Anna", "dataRole": "send", "coordinator": false,
                             "lastSeenAt": "2026-09-27T08:30:00Z"},
                            {"displayName": "Bo", "dataRole": "send", "coordinator": true, "lastSeenAt": null}]
            })))
            .mount(&server)
            .await;
        let client = CollabClient::new(&server.uri()).unwrap();
        let page = client.project_page("p1", Some("tok")).await.unwrap();
        assert_eq!(page.project.goals, Some(serde_json::json!({"Ha": 216000})));
        assert_eq!(
            page.members[0].last_seen_at.as_deref(),
            Some("2026-09-27T08:30:00Z")
        );
        assert_eq!(page.members[1].last_seen_at, None);
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
            .and(path("/api/v1/me/projects"))
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
            c.my_projects("t").await,
            Err(AccountClientError::CollabApiOutdated)
        ));
        assert!(matches!(
            c.announce_frames("t", "p1", &[]).await,
            Err(AccountClientError::CollabApiOutdated)
        ));
    }

    /// A 409 for any OTHER reason (a stale gate version, not an outdated client)
    /// must keep surfacing the hub's message via `Http`, never get relabeled
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
    /// body) and decodes the paged rows, including the per-row `frameSeq`
    /// (v3: the manifest gains `frameSeq`, loses `holderCount`).
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
                "rows": [{ "frameUuid":"u10","frameSeq":10,"publisherAccountId":"a","publisherDisplayName":"Ann","own":false,
                  "fileName":"c_x.fits","contentVersion":1,"blake3":"b".repeat(64),"byteSize":10,
                  "xxh3":"0123456789abcdef","filterRaw":"Red","filterCanonical":"R","channel":"mono",
                  "exptimeSec":300.0,"dateObs":null,"meta":{},"gateVersion":0,"accepted":true,
                  "acceptedReason":null,"state":"published","rejectReason":null,"manifestVersion":9,
                  "createdAt":"2026-09-24T00:00:00Z" }]
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
        assert_eq!(page.rows[0].frame_seq, 10);
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

    /// `new_frame_version` sends `{expectedVersion, blake3, byteSize, xxh3}`
    /// and decodes `{contentVersion, projectVersion}`.
    #[tokio::test]
    async fn new_frame_version_sends_body_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/u1/version"))
            .and(body_json(json!({
                "expectedVersion": 1, "blake3": "a".repeat(64), "byteSize": 100, "xxh3": "0123456789abcdef"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "contentVersion": 2, "projectVersion": 11
            })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let v = c
            .new_frame_version("t", "p1", "u1", 1, &"a".repeat(64), 100, "0123456789abcdef")
            .await
            .unwrap();
        assert_eq!(v.content_version, 2);
        assert_eq!(v.project_version, 11);
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

    // ---- live exchange (collab v3 wave 3, Task 2) ----

    use crate::collab::live::wire::{expand_runs, ClaimWire, VersionStatus};

    #[tokio::test]
    async fn statuses_are_typed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/me/presence"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"session_gone"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/u1/version"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_json(json!({"error":"version_conflict","contentVersion":3})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/holders"))
            .respond_with(
                ResponseTemplate::new(410)
                    .set_body_json(json!({"error":"holders_below_floor","floor":9,"holderSeq":20})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/holders/snapshot"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":"db down"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me/projects"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let beat = BeatWire {
            session_id: "0".repeat(32),
            serving: Default::default(),
            relay_url: None,
        };
        assert!(matches!(
            c.presence_beat(&beat).await,
            Err(AccountClientError::SessionGone)
        ));
        assert!(matches!(
            c.new_frame_version("t", "p1", "u1", 1, &"b".repeat(64), 10, &"0".repeat(16))
                .await,
            Err(AccountClientError::VersionConflict { content_version: 3 })
        ));
        assert!(matches!(
            c.holders_since("t", "p1", 3, None, None).await,
            Err(AccountClientError::Gone(ref e)) if e == "holders_below_floor"
        ));
        let err = c.holders_snapshot("t", "p1").await.unwrap_err();
        assert!(matches!(err, AccountClientError::Http { status: 503, .. }));
        assert!(is_retryable(&err));
        assert!(err.to_string().contains("db down"), "{err}");
        // 429 → RateLimited, and it IS retried (spec §4.6).
        let err = c.my_projects("t").await.unwrap_err();
        assert!(matches!(err, AccountClientError::RateLimited));
        assert!(is_retryable(&err));
    }

    #[tokio::test]
    async fn report_holders_sends_the_v3_body_and_decodes_the_reply() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/projects/p1/holders/self"))
            .and(body_json(json!({"reportSeq":120,"full":false,
                "add":[{"uuid":"u1","contentVersion":1}],"remove":["u2"],
                "digest":"d173abce7c5386289c657a8a697518d8","count":2})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "holderSeq":18,"digestMatch":true,"nextFlushMs":1000,"refused":["u9"]})))
            .expect(1)
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let body = HoldersReportWire {
            report_seq: 120,
            full: false,
            add: vec![ClaimWire {
                uuid: "u1".into(),
                content_version: 1,
            }],
            remove: vec!["u2".into()],
            digest: "d173abce7c5386289c657a8a697518d8".into(),
            count: 2,
        };
        let r = c.report_holders("t", "p1", &body).await.unwrap();
        assert_eq!(
            (r.holder_seq, r.digest_match, r.next_flush_ms),
            (18, true, 1000)
        );
        assert_eq!(r.refused, vec!["u9".to_string()]);
    }

    #[tokio::test]
    async fn snapshot_runs_expand_and_versions_batch_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/holders/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "epoch":"e1","holderSeq":17,"version":42,
                "frames":[{"seq":1,"uuid":"u1","contentVersion":2}],
                "devices":[{"device":"AAA=","displayName":"Anna","relayUrl":null,
                            "claims":[[1,3,1],[5,1,2],[6,1,1]]}]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "projectVersion":43,"results":[{"uuid":"u1","status":"ok","contentVersion":2},
                                               {"uuid":"u2","status":"conflict","contentVersion":5}]})))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let s = c.holders_snapshot("t", "p1").await.unwrap();
        let claims: Vec<_> = expand_runs(&s.devices[0].claims).collect();
        assert_eq!(claims, vec![(1, 1), (2, 1), (3, 1), (5, 2), (6, 1)]);
        let v = c.frame_versions("t", "p1", &[]).await.unwrap();
        assert_eq!(v.results[1].status, VersionStatus::Conflict);
        assert_eq!(v.results[1].content_version, 5);
    }

    #[tokio::test]
    async fn interactive_retry_gives_up_after_three_attempts_and_background_recovers() {
        // A concurrent `reset_all` (another test) would restart this retry's
        // back-off and let it run past three attempts.
        let _serial = crate::collab::live::backoff::RESET_ALL_TEST_SERIAL
            .lock()
            .await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/holders/snapshot"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(4)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/holders/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "epoch":"e","holderSeq":0,"version":1,"frames":[],"devices":[]})))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let e = with_retry("snapshot", RetryPolicy::Interactive, || {
            c.holders_snapshot("t", "p1")
        })
        .await;
        assert!(matches!(
            e,
            Err(AccountClientError::Http { status: 503, .. })
        ));
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            INTERACTIVE_ATTEMPTS as usize,
            "the interactive policy stops after exactly three attempts"
        );
        // one 503 left, then 200: the background policy retries through it
        let ok = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            with_retry("snapshot", RetryPolicy::Background, || {
                c.holders_snapshot("t", "p1")
            }),
        )
        .await
        .unwrap();
        assert!(ok.is_ok());
    }

    /// A 200 whose body doesn't decode into the expected type is a permanent
    /// shape mismatch, not a transient failure: it classifies as `Decode`
    /// (never `Network`), `is_retryable` is `false`, and `with_retry` under
    /// `RetryPolicy::Background` returns on the first attempt instead of
    /// looping forever. The mock's `.expect(2)` (one direct call, one through
    /// `with_retry`) fails the test if a retry were attempted.
    #[tokio::test]
    async fn a_malformed_200_body_is_a_decode_error_and_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/holders/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .expect(2)
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let err = c.holders_snapshot("t", "p1").await.unwrap_err();
        assert!(matches!(err, AccountClientError::Decode(_)), "{err:?}");
        assert!(!is_retryable(&err));
        let e = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            with_retry("snapshot", RetryPolicy::Background, || {
                c.holders_snapshot("t", "p1")
            }),
        )
        .await
        .expect("with_retry must return immediately, not loop")
        .unwrap_err();
        assert!(matches!(e, AccountClientError::Decode(_)), "{e:?}");
    }

    #[tokio::test]
    async fn frame_view_reads_frame_seq_without_holder_count_or_own() {
        let v: FrameViewWire = serde_json::from_value(json!({
            "frameUuid":"u1","frameSeq":12,"publisherAccountId":"a","publisherDisplayName":"Ann",
            "fileName":"c_x.fits","contentVersion":1,"blake3":"b","byteSize":10,"xxh3":"x",
            "filterRaw":"Red","filterCanonical":"R","channel":"mono","exptimeSec":300.0,
            "dateObs":null,"meta":{},"gateVersion":0,"accepted":true,"acceptedReason":null,
            "state":"published","rejectReason":null,"manifestVersion":9,"createdAt":"2026-09-25T00:00:00Z"}))
        .unwrap();
        assert_eq!((v.frame_seq, v.own), (12, false));
        assert_eq!(v.publisher_device_id, None, "absent → unknown device");
    }

    // ---- amendment A6: one publishing device per (project, account) ----

    /// The manifest row names its device (`null` → unknown) and serializes
    /// it back as `publisherDeviceId: null`, never dropped (the local cache
    /// round-trips the whole row).
    #[test]
    fn frame_view_carries_the_publisher_device() {
        let row = |device: serde_json::Value| -> FrameViewWire {
            serde_json::from_value(json!({
                "frameUuid":"u1","frameSeq":1,"publisherAccountId":"a","publisherDisplayName":"Ann",
                "own":true,"publisherDeviceId":device,
                "fileName":"c_x.fits","contentVersion":1,"blake3":"b","byteSize":10,"xxh3":"x",
                "filterRaw":"Red","filterCanonical":"R","channel":"mono","exptimeSec":300.0,
                "meta":{},"gateVersion":0,"accepted":true,"state":"published",
                "manifestVersion":9,"createdAt":"2026-09-25T00:00:00Z"}))
            .unwrap()
        };
        assert_eq!(
            row(json!("QUJD")).publisher_device_id.as_deref(),
            Some("QUJD")
        );
        let unknown = row(serde_json::Value::Null);
        assert_eq!(unknown.publisher_device_id, None);
        let back = serde_json::to_value(&unknown).unwrap();
        assert_eq!(back["publisherDeviceId"], serde_json::Value::Null);
        assert!(back.as_object().unwrap().contains_key("publisherDeviceId"));
    }

    /// `/me/projects` → `publishingDevice {deviceId, name}` or `null`, and
    /// absent on an older hub.
    #[tokio::test]
    async fn my_projects_reads_the_publishing_device() {
        let server = MockServer::start().await;
        let base = json!({"id":"p1","slug":"s","title":"T","dataRole":"send","coordinator":false,
            "requireApproval":false});
        let mut bound = base.clone();
        bound["id"] = json!("p1");
        bound["publishingDevice"] = json!({"deviceId":"QUJD","name":"Obs PC"});
        let mut unbound = base.clone();
        unbound["id"] = json!("p2");
        unbound["publishingDevice"] = serde_json::Value::Null;
        let mut older = base;
        older["id"] = json!("p3");
        Mock::given(method("GET"))
            .and(path("/api/v1/me/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([bound, unbound, older])))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let mine = c.my_projects("t").await.unwrap();
        assert_eq!(
            mine[0].publishing_device,
            Some(PublishingDeviceWire {
                device_id: "QUJD".into(),
                name: Some("Obs PC".into())
            })
        );
        assert_eq!(mine[1].publishing_device, None);
        assert_eq!(mine[2].publishing_device, None);
    }

    /// The two A6 409s are typed, carry the device, are never retried, and
    /// map to the stable `collab_publishing_device:` /
    /// `collab_not_publishing_device:` prefixes.
    #[tokio::test]
    async fn publishing_device_refusals_are_typed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "error":"publishing_device","deviceId":"QUJD","deviceName":"Obs PC"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/u1/version"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "error":"not_publishing_device","deviceId":"QUJD","deviceName":null})))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let err = c.announce_frames("t", "p1", &[]).await.unwrap_err();
        assert!(
            matches!(&err, AccountClientError::PublishingDevice { device_id, device_name }
                if device_id == "QUJD" && device_name.as_deref() == Some("Obs PC")),
            "{err:?}"
        );
        assert!(!is_retryable(&err));
        assert_eq!(err.to_string(), "collab_publishing_device:Obs PC");
        let err = c
            .new_frame_version("t", "p1", "u1", 1, &"b".repeat(64), 10, &"0".repeat(16))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AccountClientError::NotPublishingDevice { device_id, device_name: None }
                if device_id == "QUJD"),
            "{err:?}"
        );
        assert!(!is_retryable(&err));
        assert_eq!(
            err.to_string(),
            "collab_not_publishing_device:another device of this account"
        );
    }

    /// `PUT /projects/{id}/publishing-device` sends `{}` and decodes the
    /// binding.
    #[tokio::test]
    async fn set_publishing_device_puts_and_decodes_the_binding() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/projects/p1/publishing-device"))
            .and(body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "deviceId":"QUJD","name":null,"projectVersion":43,"changed":true})))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let reply = c.set_publishing_device("t", "p1").await.unwrap();
        assert_eq!(
            reply,
            PublishingDeviceSwitchWire {
                device_id: "QUJD".into(),
                name: None,
                project_version: 43,
                changed: true
            }
        );
    }

    /// A batch reply with `not_publishing_device` — and with a status this
    /// build has never heard of — still decodes as a whole.
    #[tokio::test]
    async fn a_versions_reply_with_new_statuses_still_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/projects/p1/frames/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "projectVersion": 7,
                "results": [
                    {"uuid":"u1","status":"ok","contentVersion":2},
                    {"uuid":"u2","status":"not_publishing_device","contentVersion":0},
                    {"uuid":"u3","status":"some_future_status","contentVersion":0}
                ]
            })))
            .mount(&server)
            .await;
        let c = CollabClient::new(server.uri()).unwrap();
        let reply = c.frame_versions("t", "p1", &[]).await.unwrap();
        let statuses: Vec<VersionStatus> = reply.results.iter().map(|r| r.status).collect();
        assert_eq!(
            statuses,
            vec![
                VersionStatus::Ok,
                VersionStatus::NotPublishingDevice,
                VersionStatus::Unknown
            ]
        );
    }
}
