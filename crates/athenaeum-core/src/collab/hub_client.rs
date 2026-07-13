//! Collab endpoints of the athenaeum-hub (read side used by slice 3).
//!
//! Mirrors `account::client::HubClient`: base URL baked in, device token per
//! call via `bearer_auth`, `AccountClientError` for the shared 401→SignedOut
//! mapping at the api boundary. Endpoint contract: hub README "API —
//! Collaboration (Stage II)".

use reqwest::StatusCode;
use serde::Deserialize;

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

pub struct CollabClient {
    http: reqwest::Client,
    base_url: String,
}

fn net(e: reqwest::Error) -> AccountClientError {
    AccountClientError::Network(e.to_string())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
}
