# Collab Slice 3 — App Project Client (Catalog Side) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Athenaeum app learns about collaboration projects: a hub collab client with signature-verified membership snapshots, a local cache + project↔frame-set linking, the spec-§4 quality gate evaluated over linked sets, and a new Projects page (cards + Contribute/Overview tabs). NO exchange — publish/receive/moderation arrive in slice 4.

**Architecture:** New core module `crates/athenaeum-core/src/collab/` (`hub_client.rs` — reqwest client for the collab endpoints, mirroring `account::client::HubClient`'s shape and error mapping; `snapshot.rs` — ed25519 verify + parse of the hub-signed membership snapshot with TOFU-pinned hub pubkey; `gate.rs` — the pure quality-gate engine). New catalog tables (`collab_projects` poll cache incl. the raw signed snapshot for slice-4's PeerAuthorizer, `project_links`, `project_link_intents`) in `db/schema.rs` + `db/collab.rs`. Orchestration in `api/collab.rs` (refresh/list/detail/link/suggest/gate) behind thin Tauri + Axum wrappers (both backends in the same task — house law). Frontend: `Projects` page + `ProjectDetail`, `useProjects` hook, `project` notification kind, a "Publish as project" entry on `FrameSetDetail` deep-linking to the portal's `/new` with a recorded auto-link intent.

**Tech Stack:** Rust (rusqlite, reqwest 0.12, wiremock 0.6 dev, ed25519-dalek 2 — new direct dep, already in-tree via iroh), React 18 + TS (generated types via ts-rs), Tailwind design tokens.

## Global Constraints

- **Repo/branch:** athenaeum repo, new branch **`0.5.0`** cut from `0.4.0` (owner rule: version = branch). No version bumps in this slice (releases keep flowing from `0.4.0`).
- **Two backends in sync (house law):** every new Tauri command in `crates/athenaeum-tauri/src/commands/collab.rs` ships with its Axum mirror in `crates/athenaeum-web/src/routes/collab.rs` in the same commit; real logic lives in `athenaeum-core` (`api/collab.rs` handlers take `&ServiceContext`).
- **Serde boundary:** all new wire DTOs `#[serde(rename_all = "camelCase")]` + `#[derive(ts_rs::TS)]`, registered in `crates/athenaeum-core/src/ts_export.rs` `decls![]`; regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; never hand-edit `src/types/models.ts`.
- **Never swallow errors** — log to `tracing` before returning; command wrappers wear `#[tracing::instrument(skip_all, err)]`.
- **Design tokens only** in the frontend (`bg-surface`, `text-content-muted`, `bg-accent`, `text-error`, `border-border`, …); icons from `lucide-react`; notifications ONLY via `notify()` from `useNotifications()`; `api.listen` uses the cancelled-flag pattern (CLAUDE.md).
- **Snapshot contract (BINDING, from the hub README):** clients apply EVERY signature-verified snapshot (compare content, not version — device add/revoke changes content without a version bump); node lists are ordered by raw pubkey bytes (never assume base64-ASCII order); ONLY hub-fetched snapshots are cached/used. Payload schema: `{schema: 1, projectId, membershipVersion, requireApproval, issuedAt, members: [{accountId, displayName, dataRole, coordinator, nodes: [<base64 32-byte pubkey>]}]}` — the exact signed bytes arrive base64-encoded in `payload` and MUST be verified before parsing.
- **Gate metric registry (spec §4, exact keys/ops):** `fwhm_arcsec` (= `median_fwhm` px × pixel scale), `eccentricity` (= `median_eccentricity`), `stars_detected`, `not_trailed` (`FrameAnalysis.possibly_trailed` is a `bool` — fail when `true`); SNR family `median_snr`/`snr_weight`/`frame_snr` supported generically. Ops: `"lte"`, `"gte"`, `"reject_if"` (value `true` → fail when the flag metric is set). Unknown metric key → rule skipped with `tracing::warn!` (registry is extensible).
- **Pixel scale:** prefer `plate_solves.pixel_scale_arcsec`; fallback = the atan form over `frames.xpixsz`/`focallen` **without multiplying by binning** (XPIXSZ is already the binned pixel size — mirror `plate_solve/hints.rs:158-176`). No scale → precondition failure "unknown pixel scale".
- **Frame center precedence (on-target check):** `plate_solves.crval1/crval2` → `frames.ra/dec` → parsed `frames.objctra/objctdec` (`coordinates::parse_ra_sexagesimal`/`parse_dec_sexagesimal`); none → failure "no coordinates". Distance via `coordinates::angular_distance` (decimal degrees in/out).
- **Tests:** core tests use an isolated throwaway rusqlite DB (`Connection::open_in_memory` for pure db-module tests; the api-layer `test_ctx()` fixture uses a TEMPDIR FILE so the pool's multiple connections see one database) + `wiremock` for hub HTTP. Gates for the slice: `cargo build --workspace`, `cargo test -p athenaeum-core`, `cargo build -p perseus --no-default-features` (the render-gate check), `npx tsc --noEmit`.
- **Commit identity:** `eg013ra1n <vilen.sharifov@gmail.com>`, never a Claude author/co-author line.

---

### Task 1: Branch + collab hub client + snapshot verification

**Files:**
- Create: `crates/athenaeum-core/src/collab/mod.rs`, `crates/athenaeum-core/src/collab/hub_client.rs`, `crates/athenaeum-core/src/collab/snapshot.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod collab;`), `crates/athenaeum-core/Cargo.toml` (add `ed25519-dalek = "3.0.0-rc.0"` — the exact version iroh 1.0.2 already carries transitively; `"2"` would resolve to a SECOND crate version and violate the consistent-dep-versions rule), `Cargo.lock` (refreshed by the first build)
- Test: inline `#[cfg(test)]` in both new files (house style, like `account/client.rs`)

**Interfaces:**
- Consumes: `account::client` patterns (reqwest client shape, `AccountClientError` — publicly re-exported from `crate::account`), `wiremock` dev-dep (already present).
- Produces (BINDING for Tasks 4–5):
  - `pub struct CollabClient { .. }` with `pub fn new(base_url: impl Into<String>) -> Result<Self, AccountClientError>` and async methods (token passed per call, `bearer_auth`):
    - `pub async fn collab_pubkey(&self) -> Result<String, AccountClientError>` — GET `/api/v1/collab/pubkey` → the `pubkey` field (base64).
    - `pub async fn my_projects(&self, token: &str) -> Result<Vec<MyProjectWire>, AccountClientError>` — GET `/api/v1/me/projects`.
    - `pub async fn project_page(&self, id_or_slug: &str) -> Result<ProjectPageWire, AccountClientError>` — GET `/api/v1/projects/{id_or_slug}` (public).
    - `pub async fn membership_snapshot(&self, token: &str, project_id: &str) -> Result<SignedSnapshotWire, AccountClientError>` — GET `/api/v1/projects/{id}/membership`.
    - `pub async fn thresholds(&self, token: &str, project_id: &str) -> Result<ThresholdsWire, AccountClientError>` — GET `/api/v1/projects/{id}/thresholds`.
  - Wire DTOs (all `#[derive(Debug, Clone, serde::Deserialize)]` + `#[serde(rename_all = "camelCase")]`): `MyProjectWire { id, slug, title, data_role, coordinator, require_approval, pending_announcements: i64 }`, `ProjectPageWire { project: ProjectWire, members: Vec<MemberWire> }`, `ProjectWire { id, slug, title, status, require_approval, target: TargetWire }`, `TargetWire { name, ra_deg, dec_deg, radius_deg }`, `MemberWire { display_name, data_role, coordinator }`, `SignedSnapshotWire { payload, signature, pubkey }`, `ThresholdsWire { current: Option<ThresholdSetWire> }`, `ThresholdSetWire { version: i32, rules: Vec<serde_json::Value> }`.
  - `snapshot.rs`: `pub struct VerifiedSnapshot { pub membership_version: i64, pub require_approval: bool, pub members: Vec<SnapshotMember> }`, `pub struct SnapshotMember { pub account_id: String, pub display_name: String, pub data_role: String, pub coordinator: bool, pub nodes: Vec<String> }` — `SnapshotMember` derives **`Serialize` AND `Deserialize`** (Task 5 re-serializes `verified.members` into `members_json`), and `pub fn verify_and_parse(wire: &SignedSnapshotWire, pinned_pubkey_b64: &str) -> anyhow::Result<VerifiedSnapshot>` — decodes `payload`/`signature`/pinned key from base64, REQUIRES `wire.pubkey == pinned_pubkey_b64` (mismatch = hard error naming both), ed25519-verifies the signature over the EXACT payload bytes, THEN parses the payload JSON (schema must be `1`).

- [ ] **Step 1: Cut the branch**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
git checkout 0.4.0 && git checkout -b 0.5.0   # 0.4.0 has no upstream tracking — no pull
```

- [ ] **Step 2: Write the failing tests**

In `crates/athenaeum-core/src/collab/hub_client.rs` (bottom, mirroring `account/client.rs`'s wiremock style):

```rust
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
```

In `crates/athenaeum-core/src/collab/snapshot.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_fixture(key: &SigningKey, payload_json: &serde_json::Value) -> SignedSnapshotWire {
        let bytes = serde_json::to_vec(payload_json).unwrap();
        SignedSnapshotWire {
            payload: B64.encode(&bytes),
            signature: B64.encode(key.sign(&bytes).to_bytes()),
            pubkey: B64.encode(key.verifying_key().to_bytes()),
        }
    }

    fn payload() -> serde_json::Value {
        serde_json::json!({
            "schema": 1, "projectId": "p-1", "membershipVersion": 4, "requireApproval": true,
            "issuedAt": "2026-07-13T00:00:00Z",
            "members": [{"accountId": "a-1", "displayName": "Vilen", "dataRole": "send_receive",
                         "coordinator": true, "nodes": [B64.encode([7u8; 32])]}]
        })
    }

    #[test]
    fn verifies_and_parses_a_good_snapshot() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let wire = signed_fixture(&key, &payload());
        let pinned = wire.pubkey.clone();
        let snap = verify_and_parse(&wire, &pinned).unwrap();
        assert_eq!(snap.membership_version, 4);
        assert!(snap.require_approval);
        assert_eq!(snap.members[0].display_name, "Vilen");
        assert_eq!(snap.members[0].nodes.len(), 1);
    }

    #[test]
    fn rejects_tampered_payload_wrong_key_and_pin_mismatch() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let mut wire = signed_fixture(&key, &payload());
        let pinned = wire.pubkey.clone();

        // Tampered payload bytes → signature check fails.
        let mut tampered = serde_json::to_vec(&payload()).unwrap();
        tampered[10] ^= 0xFF;
        wire.payload = B64.encode(&tampered);
        assert!(verify_and_parse(&wire, &pinned).is_err(), "tampered payload must fail");

        // Signed by a DIFFERENT key but claiming the pinned pubkey → fails.
        let other = SigningKey::from_bytes(&[2u8; 32]);
        let forged = SignedSnapshotWire {
            pubkey: pinned.clone(),
            ..signed_fixture(&other, &payload())
        };
        assert!(verify_and_parse(&forged, &pinned).is_err(), "wrong key must fail");

        // Honest wire but the PIN doesn't match → hard error (never TOFU-drift).
        let honest = signed_fixture(&key, &payload());
        let other_pin = B64.encode(other.verifying_key().to_bytes());
        assert!(verify_and_parse(&honest, &other_pin).is_err(), "pin mismatch must fail");
    }

    #[test]
    fn rejects_unknown_schema() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let mut p = payload();
        p["schema"] = serde_json::json!(2);
        let wire = signed_fixture(&key, &p);
        let pinned = wire.pubkey.clone();
        assert!(verify_and_parse(&wire, &pinned).is_err(), "schema 2 must be refused");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p athenaeum-core collab 2>&1 | tail -5`
Expected: compile error — `collab` module missing.

- [ ] **Step 4: Implement**

`crates/athenaeum-core/Cargo.toml` — next to the `iroh` dependency add:

```toml
# ed25519 verification of hub-signed membership snapshots. MUST match the
# version iroh 1.0.2 already carries transitively (one crate version in the
# tree — owner rule): check `grep -A1 'name = "ed25519-dalek"' Cargo.lock`
# and pin exactly that (3.0.0-rc.0 at the time of writing).
ed25519-dalek = "3.0.0-rc.0"
```

`crates/athenaeum-core/src/collab/mod.rs`:

```rust
//! Stage-II collaboration: hub collab client, verified membership snapshots,
//! and the quality gate. Catalog-side only — the exchange layer is slice 4.

pub mod gate;
pub mod hub_client;
pub mod snapshot;
```

(`gate` lands in Task 3 — create an empty `pub mod gate;`-satisfying `gate.rs` with just the module doc comment now, or add the mod line in Task 3; choose the latter: `mod.rs` starts with only `hub_client` + `snapshot` and Task 3 appends the line.)

`crates/athenaeum-core/src/collab/hub_client.rs`:

```rust
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
```

`crates/athenaeum-core/src/collab/snapshot.rs`:

```rust
//! Verification + parsing of hub-signed membership snapshots (the
//! cross-account trust anchor, spec §2a).
//!
//! Contract (hub README): verify the signature over the EXACT transported
//! payload bytes against the PINNED hub pubkey, then parse. Clients apply
//! every verified snapshot — content is compared, not the version (device
//! add/revoke changes content without a version bump). Only hub-fetched
//! snapshots ever reach this function.

use anyhow::{bail, Context};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::hub_client::SignedSnapshotWire;

// Serialize too: Task 5 re-serializes the verified member list into the
// cache's members_json (camelCase preserved for the ProjectMemberView parse).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMember {
    pub account_id: String,
    pub display_name: String,
    pub data_role: String,
    pub coordinator: bool,
    /// Active athenaeum-device pubkeys, base64; ordered by raw pubkey bytes
    /// on the hub (NOT base64-ASCII order) — never assume sortedness here.
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotPayload {
    schema: u32,
    #[allow(dead_code)]
    project_id: String,
    membership_version: i64,
    require_approval: bool,
    #[allow(dead_code)]
    issued_at: String,
    members: Vec<SnapshotMember>,
}

#[derive(Debug, Clone)]
pub struct VerifiedSnapshot {
    pub membership_version: i64,
    pub require_approval: bool,
    pub members: Vec<SnapshotMember>,
}

/// Verify the wire snapshot against the pinned hub pubkey and parse it.
/// Every failure is a hard error — a snapshot that does not verify is never
/// partially used.
pub fn verify_and_parse(
    wire: &SignedSnapshotWire,
    pinned_pubkey_b64: &str,
) -> anyhow::Result<VerifiedSnapshot> {
    if wire.pubkey != pinned_pubkey_b64 {
        bail!(
            "snapshot pubkey does not match the pinned hub key (got {}, pinned {})",
            &wire.pubkey,
            pinned_pubkey_b64
        );
    }

    let key_bytes: [u8; 32] = B64
        .decode(pinned_pubkey_b64)
        .context("pinned pubkey is not valid base64")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("pinned pubkey must decode to 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&key_bytes).context("pinned pubkey is not a valid ed25519 key")?;

    let payload = B64
        .decode(&wire.payload)
        .context("snapshot payload is not valid base64")?;
    let sig_bytes: [u8; 64] = B64
        .decode(&wire.signature)
        .context("snapshot signature is not valid base64")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("snapshot signature must decode to 64 bytes"))?;

    key.verify(&payload, &Signature::from_bytes(&sig_bytes))
        .context("snapshot signature verification failed")?;

    let parsed: SnapshotPayload =
        serde_json::from_slice(&payload).context("verified snapshot payload does not parse")?;
    if parsed.schema != 1 {
        bail!("unsupported snapshot schema {}", parsed.schema);
    }

    Ok(VerifiedSnapshot {
        membership_version: parsed.membership_version,
        require_approval: parsed.require_approval,
        members: parsed.members,
    })
}
```

`crates/athenaeum-core/src/lib.rs`: add `pub mod collab;` to the module list (the file's ordering is NOT strictly alphabetical — place it near `clustering`/`coordinates` matching the file's existing grouping).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core collab 2>&1 | tail -5` (first run without `--locked` — Cargo.lock gains the direct ed25519-dalek entry).
Expected: PASS (5 tests). Then `cargo build -p athenaeum-core` clean.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/collab/ crates/athenaeum-core/src/lib.rs crates/athenaeum-core/Cargo.toml Cargo.lock
git commit -m "feat(collab): hub collab client + verified membership snapshots (pinned-pubkey ed25519)"
```

---

### Task 2: Catalog tables + `db/collab.rs`

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (three `CREATE TABLE IF NOT EXISTS` blocks at the end of `init_db`, following the house pattern)
- Create: `crates/athenaeum-core/src/db/collab.rs`
- Modify: `crates/athenaeum-core/src/db/mod.rs` (add `pub mod collab;` + re-exports if the module follows the house re-export pattern — mirror how `light_calibrations` is exposed)
- Test: inline `#[cfg(test)]` in `db/collab.rs`

**Interfaces:**
- Consumes: `rusqlite::Connection`, existing `frames_set` table.
- Produces (BINDING for Tasks 4–6):

```sql
-- Poll cache of my collaboration projects, one row per project. Holds the RAW
-- signed snapshot (payload+signature, base64) so slice-4's project
-- PeerAuthorizer can re-verify offline. Refreshed wholesale on each poll.
CREATE TABLE IF NOT EXISTS collab_projects (
    project_id             TEXT PRIMARY KEY,
    slug                   TEXT NOT NULL,
    title                  TEXT NOT NULL,
    data_role              TEXT NOT NULL,
    is_coordinator         INTEGER NOT NULL DEFAULT 0,
    require_approval       INTEGER NOT NULL DEFAULT 0,
    pending_announcements  INTEGER NOT NULL DEFAULT 0,
    project_status         TEXT NOT NULL DEFAULT 'active',
    target_name            TEXT NOT NULL,
    target_ra_deg          REAL NOT NULL,
    target_dec_deg         REAL NOT NULL,
    target_radius_deg      REAL NOT NULL,
    membership_version     INTEGER NOT NULL,
    snapshot_payload_b64   TEXT NOT NULL,
    snapshot_signature_b64 TEXT NOT NULL,
    members_json           TEXT NOT NULL,
    thresholds_version     INTEGER,
    thresholds_rules_json  TEXT,
    fetched_at             TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Local project↔frame-set links. NEVER sent to the hub (spec §7).
CREATE TABLE IF NOT EXISTS project_links (
    project_id    TEXT    NOT NULL,
    frames_set_id INTEGER NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_id, frames_set_id),
    FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
);

-- "Publish as project" deep-link intents: when the portal /new form was
-- prefilled from this set, the next poll auto-links the newly appeared
-- project whose target matches (spec §8 "source set auto-linked").
CREATE TABLE IF NOT EXISTS project_link_intents (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    frames_set_id INTEGER NOT NULL,
    ra_deg        REAL    NOT NULL,
    dec_deg       REAL    NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
);
```

- Rust surface in `db/collab.rs` (conn = `&rusqlite::Connection`, `anyhow::Result`, house style like `db/light_calibrations.rs`):
  - `pub struct CollabProjectRow { pub project_id: String, pub slug: String, pub title: String, pub data_role: String, pub is_coordinator: bool, pub require_approval: bool, pub pending_announcements: i64, pub project_status: String, pub target_name: String, pub target_ra_deg: f64, pub target_dec_deg: f64, pub target_radius_deg: f64, pub membership_version: i64, pub snapshot_payload_b64: String, pub snapshot_signature_b64: String, pub members_json: String, pub thresholds_version: Option<i32>, pub thresholds_rules_json: Option<String>, pub fetched_at: String }`
  - `pub fn upsert_project(conn, row: &CollabProjectRow) -> Result<()>` (INSERT … ON CONFLICT(project_id) DO UPDATE of every non-PK column, `fetched_at = datetime('now')`)
  - `pub fn list_projects(conn) -> Result<Vec<CollabProjectRow>>` (ORDER BY title)
  - `pub fn get_project(conn, project_id: &str) -> Result<Option<CollabProjectRow>>`
  - `pub fn prune_projects_not_in(conn, keep_ids: &[String]) -> Result<usize>` (DELETE the cache rows whose id is not in the list; empty list deletes all)
  - `pub fn link_set(conn, project_id: &str, frames_set_id: i64) -> Result<()>` (INSERT OR IGNORE), `pub fn unlink_set(conn, project_id, frames_set_id) -> Result<usize>`, `pub fn linked_set_ids(conn, project_id) -> Result<Vec<i64>>`, `pub fn is_set_linked(conn, project_id, frames_set_id) -> Result<bool>`
  - `pub fn add_link_intent(conn, frames_set_id: i64, ra_deg: f64, dec_deg: f64) -> Result<i64>`, `pub fn list_link_intents(conn) -> Result<Vec<(i64, i64, f64, f64)>>` (`(intent_id, frames_set_id, ra, dec)`), `pub fn delete_link_intent(conn, intent_id: i64) -> Result<()>`

- [ ] **Step 1: Write the failing test** (inline in `db/collab.rs`; the house test pattern opens an in-memory DB via `crate::db::schema::init_db`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn sample_row(id: &str) -> CollabProjectRow {
        CollabProjectRow {
            project_id: id.to_string(),
            slug: format!("{id}-slug"),
            title: format!("Project {id}"),
            data_role: "send_receive".into(),
            is_coordinator: true,
            require_approval: false,
            pending_announcements: 0,
            project_status: "active".into(),
            target_name: "M101".into(),
            target_ra_deg: 210.8,
            target_dec_deg: 54.35,
            target_radius_deg: 1.5,
            membership_version: 1,
            snapshot_payload_b64: "cGF5bG9hZA==".into(),
            snapshot_signature_b64: "c2ln".into(),
            members_json: "[]".into(),
            thresholds_version: Some(1),
            thresholds_rules_json: Some("[]".into()),
            fetched_at: String::new(), // set by SQL
        }
    }

    #[test]
    fn cache_upsert_list_prune_roundtrip() {
        let conn = test_conn();
        upsert_project(&conn, &sample_row("p-1")).unwrap();
        upsert_project(&conn, &sample_row("p-2")).unwrap();

        // Upsert updates in place (no duplicate rows).
        let mut updated = sample_row("p-1");
        updated.title = "Renamed".into();
        updated.membership_version = 5;
        upsert_project(&conn, &updated).unwrap();

        let all = list_projects(&conn).unwrap();
        assert_eq!(all.len(), 2);
        let p1 = get_project(&conn, "p-1").unwrap().unwrap();
        assert_eq!(p1.title, "Renamed");
        assert_eq!(p1.membership_version, 5);
        assert!(!p1.fetched_at.is_empty());

        // Prune keeps only the listed ids.
        let removed = prune_projects_not_in(&conn, &["p-2".to_string()]).unwrap();
        assert_eq!(removed, 1);
        assert!(get_project(&conn, "p-1").unwrap().is_none());
    }

    #[test]
    fn links_and_intents_respect_fk_cascade() {
        let conn = test_conn();
        // A real frames_set row for the FK.
        conn.execute("INSERT INTO frames_set (name) VALUES ('S1')", []).unwrap();
        let set_id = conn.last_insert_rowid();

        link_set(&conn, "p-1", set_id).unwrap();
        link_set(&conn, "p-1", set_id).unwrap(); // idempotent
        assert!(is_set_linked(&conn, "p-1", set_id).unwrap());
        assert_eq!(linked_set_ids(&conn, "p-1").unwrap(), vec![set_id]);

        let intent = add_link_intent(&conn, set_id, 210.8, 54.35).unwrap();
        assert_eq!(list_link_intents(&conn).unwrap().len(), 1);
        delete_link_intent(&conn, intent).unwrap();
        assert!(list_link_intents(&conn).unwrap().is_empty());

        // Deleting the set cascades the link away.
        add_link_intent(&conn, set_id, 1.0, 2.0).unwrap();
        conn.execute("DELETE FROM frames_set WHERE id = ?1", [set_id]).unwrap();
        assert!(linked_set_ids(&conn, "p-1").unwrap().is_empty());
        assert!(list_link_intents(&conn).unwrap().is_empty());

        assert_eq!(unlink_set(&conn, "p-1", set_id).unwrap(), 0, "already gone");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core db::collab 2>&1 | tail -3` → compile error (module missing).

- [ ] **Step 3: Implement** — the three CREATE TABLE blocks appended inside `init_db` (verbatim SQL above; place after the newest existing block, with a `-- Stage II collaboration (slice 3)` banner comment) and `db/collab.rs` with the exact signatures from Interfaces. Implementation notes: `upsert_project` lists every column explicitly in both INSERT and `DO UPDATE SET`; row mapper via a shared `const SELECT_COLS: &str` + `fn row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<CollabProjectRow>` (mirror `db/light_calibrations.rs`); `prune_projects_not_in` builds a parameterized `NOT IN (?, ?, …)` list (empty → `DELETE FROM collab_projects` unconditionally); booleans stored as INTEGER 0/1.

- [ ] **Step 4: Run to verify pass** — `cargo test -p athenaeum-core db::collab` → PASS (2 tests); then `cargo test -p athenaeum-core 2>&1 | grep "test result" | tail -3` — pre-existing suites stay green (schema change is additive).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/db/collab.rs crates/athenaeum-core/src/db/mod.rs
git commit -m "feat(collab): catalog cache tables — collab_projects, project_links, link intents"
```

---

### Task 3: Quality-gate engine (`collab/gate.rs`)

**Files:**
- Create: `crates/athenaeum-core/src/collab/gate.rs`
- Modify: `crates/athenaeum-core/src/collab/mod.rs` (add `pub mod gate;`)
- Test: inline `#[cfg(test)]` (spec §11: each precondition + each rule + unit conversion + the binning gotcha)

**Interfaces:**
- Consumes: `crate::models::FrameAnalysis`, `crate::db::light_calibrations::LightCalStatus`, `crate::coordinates::angular_distance`.
- Produces (BINDING for Task 5 and the wire):

```rust
pub struct ProjectTarget { pub ra_deg: f64, pub dec_deg: f64, pub radius_deg: f64 }

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdRuleView {
    pub metric_key: String,
    pub op: String,
    /// The repo's ts-rs feature set has NO `serde-json-impl`, so
    /// `serde_json::Value` cannot derive `TS` — override the emitted TS type
    /// (the hub validates rule values to number|bool, so this is exact).
    #[ts(type = "number | boolean")]
    pub value: serde_json::Value,
}

pub struct GateFrameInput {
    pub frame_id: i64,
    pub filename: String,
    /// Resolved center, decimal degrees (precedence handled by the caller).
    pub center: Option<(f64, f64)>,
    pub pixel_scale_arcsec: Option<f64>,
    pub cal_status: LightCalStatus,
    pub analysis: Option<FrameAnalysis>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameGateRow {
    pub frame_id: i64,
    pub filename: String,
    pub fwhm_arcsec: Option<f64>,
    pub eccentricity: Option<f64>,
    pub stars_detected: Option<i64>,
    pub trailed: Option<bool>,
    pub publishable: bool,
    /// Human-readable failure reasons, empty when publishable
    /// (e.g. `FWHM 3.4″ > 3.0″`, `not calibrated (Stale)`, `no analysis`,
    /// `unknown pixel scale`, `outside target radius (2.1° > 1.5°)`).
    pub failures: Vec<String>,
}

pub fn evaluate_frame(input: &GateFrameInput, target: &ProjectTarget, rules: &[ThresholdRuleView]) -> FrameGateRow
```

- Semantics (spec §4, exact): **Layer 1 preconditions, always on, evaluated in order and ALL recorded** (a frame can carry several failures): (1) `cal_status == LightCalStatus::Calibrated` else `not calibrated (<Status>)`; (2) `analysis.is_some()` else `no analysis`; (3) `pixel_scale_arcsec.is_some()` else `unknown pixel scale`; (4) `center` within `target.radius_deg` via `angular_distance` else `outside target radius (X° > Y°)` (no center → `no coordinates`). **Layer 2 rules run only when their inputs exist** (no analysis → rules that need analysis are moot, the layer-1 failure already blocks): metric resolution — `fwhm_arcsec` = `analysis.median_fwhm? × pixel_scale?` (needs both), `eccentricity` = `analysis.median_eccentricity?`, `stars_detected` = `analysis.stars_detected? as f64`, `not_trailed` handled specially (op `reject_if`, value `true` → fail when `analysis.possibly_trailed != 0` with message `frame appears trailed`), `median_snr`/`snr_weight`/`frame_snr` from the same-named analysis fields. Ops: `lte` → fail when `metric > value` (`FWHM 3.40″ > 3.00″` style message using 2 decimals + the right unit — `″` only for fwhm_arcsec), `gte` → fail when `metric < value` (`stars 120 < 150`). Unknown metric key or non-numeric value for lte/gte → `tracing::warn!(metric_key, "unknown gate rule skipped")`, rule skipped. Metric present in the row's echo fields regardless of rules (`fwhm_arcsec`, `eccentricity`, `stars_detected`, `trailed`).

- [ ] **Step 1: Write the failing tests** (fixture helpers keep them short):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::light_calibrations::LightCalStatus;
    use crate::models::FrameAnalysis;

    /// `FrameAnalysis` fields are NOT `Option` (see `models.rs`): `median_fwhm:
    /// f64`, `stars_detected: i64`, `possibly_trailed: bool`, … Only
    /// `median_beta`/`quality_score`/`config_hash` are optional. Build the full
    /// literal — there is no `Default` impl to lean on.
    fn analysis(fwhm_px: f64, ecc: f64, stars: i64, trailed: bool) -> FrameAnalysis {
        FrameAnalysis {
            id: None,
            frame_id: 1,
            file_id: 1,
            stars_detected: stars,
            median_fwhm: fwhm_px,
            median_eccentricity: ecc,
            median_snr: 10.0,
            median_hfr: 2.0,
            frame_snr: 10.0,
            snr_weight: 1.0,
            psf_signal: 100.0,
            background: 10.0,
            noise: 1.0,
            detection_threshold: 5.0,
            width: 6248,
            height: 4176,
            source_channels: 1,
            trail_r_squared: 0.0,
            possibly_trailed: trailed,
            median_beta: None,
            quality_score: None,
            config_hash: None,
            analyzed_at: "2026-07-13T00:00:00Z".to_string(),
        }
    }

    fn input(analysis_opt: Option<FrameAnalysis>) -> GateFrameInput {
        GateFrameInput {
            frame_id: 1,
            filename: "L_0001.fits".into(),
            center: Some((210.8, 54.35)),
            pixel_scale_arcsec: Some(2.0),
            cal_status: LightCalStatus::Calibrated,
            analysis: analysis_opt,
        }
    }

    fn target() -> ProjectTarget {
        ProjectTarget { ra_deg: 210.8, dec_deg: 54.35, radius_deg: 1.5 }
    }

    fn rules() -> Vec<ThresholdRuleView> {
        serde_json::from_value(serde_json::json!([
            {"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0},
            {"metricKey": "eccentricity", "op": "lte", "value": 0.6},
            {"metricKey": "stars_detected", "op": "gte", "value": 150},
            {"metricKey": "not_trailed", "op": "reject_if", "value": true}
        ]))
        .unwrap()
    }

    #[test]
    fn passing_frame_is_publishable_with_converted_units() {
        // 1.2 px × 2.0 ″/px = 2.4″ ≤ 3.0″ — the unit conversion is the point.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 400, false))), &target(), &rules());
        assert!(row.publishable, "failures: {:?}", row.failures);
        assert_eq!(row.fwhm_arcsec, Some(2.4));
        assert_eq!(row.trailed, Some(false));
    }

    #[test]
    fn each_precondition_fails_with_its_reason() {
        // Not calibrated.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.cal_status = LightCalStatus::Stale;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(!row.publishable);
        assert!(row.failures.iter().any(|f| f.contains("not calibrated")), "{:?}", row.failures);

        // No analysis.
        let row = evaluate_frame(&input(None), &target(), &rules());
        assert!(row.failures.iter().any(|f| f == "no analysis"));

        // Unknown pixel scale.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.pixel_scale_arcsec = None;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("unknown pixel scale")));

        // Off target (2° away > 1.5° radius) and no coordinates.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.center = Some((210.8, 56.35));
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("outside target radius")));
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.center = None;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row.failures.iter().any(|f| f == "no coordinates"));
    }

    #[test]
    fn each_rule_fails_with_its_reason() {
        // FWHM: 2.0 px × 2.0 = 4.0″ > 3.0″.
        let row = evaluate_frame(&input(Some(analysis(2.0, 0.4, 400, false))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("FWHM") && f.contains("3.00")), "{:?}", row.failures);

        // Eccentricity 0.7 > 0.6.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.7, 400, false))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.to_lowercase().contains("eccentricity")));

        // Stars 120 < 150.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 120, false))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("120") && f.contains("150")));

        // Trailed.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 400, true))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("trailed")));
        assert_eq!(row.trailed, Some(true));
    }

    #[test]
    fn unknown_metric_is_skipped_not_fatal() {
        let mut r = rules();
        r.push(
            serde_json::from_value(serde_json::json!(
                {"metricKey": "made_up_metric", "op": "lte", "value": 1.0}
            ))
            .unwrap(),
        );
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 400, false))), &target(), &r);
        assert!(row.publishable, "unknown metric must not block: {:?}", row.failures);
    }

    #[test]
    fn snr_family_rules_apply_generically() {
        let mut a = analysis(1.2, 0.4, 400, false);
        a.median_snr = 4.0;
        let r: Vec<ThresholdRuleView> = serde_json::from_value(serde_json::json!([
            {"metricKey": "median_snr", "op": "gte", "value": 5.0}
        ]))
        .unwrap();
        let row = evaluate_frame(&input(Some(a)), &target(), &r);
        assert!(!row.publishable);
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core collab::gate 2>&1 | tail -3` → compile error.

- [ ] **Step 3: Implement `evaluate_frame`** per the Interfaces semantics. Skeleton:

```rust
pub fn evaluate_frame(
    input: &GateFrameInput,
    target: &ProjectTarget,
    rules: &[ThresholdRuleView],
) -> FrameGateRow {
    let mut failures: Vec<String> = Vec::new();

    if input.cal_status != LightCalStatus::Calibrated {
        failures.push(format!("not calibrated ({:?})", input.cal_status));
    }
    let analysis = input.analysis.as_ref();
    if analysis.is_none() {
        failures.push("no analysis".to_string());
    }
    if input.pixel_scale_arcsec.is_none() {
        failures.push("unknown pixel scale".to_string());
    }
    match input.center {
        Some((ra, dec)) => {
            let d = crate::coordinates::angular_distance(ra, dec, target.ra_deg, target.dec_deg);
            if d > target.radius_deg {
                failures.push(format!(
                    "outside target radius ({d:.1}° > {:.1}°)",
                    target.radius_deg
                ));
            }
        }
        None => failures.push("no coordinates".to_string()),
    }

    // NOTE: FrameAnalysis fields are NOT Option (models.rs) — the Option-ness
    // here comes from "is there an analysis row at all" and "do we know the
    // pixel scale", nothing else.
    let fwhm_arcsec = match (analysis, input.pixel_scale_arcsec) {
        (Some(a), Some(scale)) => Some(a.median_fwhm * scale),
        _ => None,
    };
    let eccentricity = analysis.map(|a| a.median_eccentricity);
    let stars_detected = analysis.map(|a| a.stars_detected);
    let trailed = analysis.map(|a| a.possibly_trailed);

    for rule in rules {
        match rule.metric_key.as_str() {
            "not_trailed" => {
                if rule.op == "reject_if" && rule.value == serde_json::json!(true) {
                    if trailed == Some(true) {
                        failures.push("frame appears trailed".to_string());
                    }
                } else {
                    tracing::warn!(metric_key = %rule.metric_key, op = %rule.op, "unknown gate rule skipped");
                }
            }
            key => {
                let metric: Option<f64> = match key {
                    "fwhm_arcsec" => fwhm_arcsec,
                    "eccentricity" => eccentricity,
                    "stars_detected" => stars_detected.map(|s| s as f64),
                    "median_snr" => analysis.map(|a| a.median_snr),
                    "snr_weight" => analysis.map(|a| a.snr_weight),
                    "frame_snr" => analysis.map(|a| a.frame_snr),
                    _ => {
                        tracing::warn!(metric_key = %rule.metric_key, "unknown gate rule skipped");
                        continue;
                    }
                };
                let Some(limit) = rule.value.as_f64() else {
                    tracing::warn!(metric_key = %rule.metric_key, "non-numeric gate rule value skipped");
                    continue;
                };
                let Some(metric) = metric else { continue }; // layer-1 already recorded the blocker
                let (label, unit) = match key {
                    "fwhm_arcsec" => ("FWHM", "″"),
                    "eccentricity" => ("eccentricity", ""),
                    "stars_detected" => ("stars", ""),
                    other => (other, ""),
                };
                match rule.op.as_str() {
                    "lte" if metric > limit => {
                        failures.push(format!("{label} {metric:.2}{unit} > {limit:.2}{unit}"))
                    }
                    "gte" if metric < limit => {
                        failures.push(format!("{label} {metric:.0} < {limit:.0}"))
                    }
                    "lte" | "gte" => {}
                    other => {
                        tracing::warn!(metric_key = %rule.metric_key, op = %other, "unknown gate op skipped")
                    }
                }
            }
        }
    }

    FrameGateRow {
        frame_id: input.frame_id,
        filename: input.filename.clone(),
        fwhm_arcsec,
        eccentricity,
        stars_detected,
        trailed,
        publishable: failures.is_empty(),
        failures,
    }
}
```

(Plus the type definitions from Interfaces; `LightCalStatus` needs `PartialEq` — it derives it already for `derive_status` tests; if not, add `#[derive(PartialEq)]` there.)

- [ ] **Step 4: Run to verify pass** — `cargo test -p athenaeum-core collab::gate` → PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/collab/
git commit -m "feat(collab): quality-gate engine — spec §4 preconditions + threshold registry"
```

---

### Task 4: Local orchestration — linking, suggestions, gate report, portal deep link (`api/collab.rs` part 1)

**Files:**
- Create: `crates/athenaeum-core/src/api/collab.rs`
- Modify: `crates/athenaeum-core/src/api/mod.rs` (add `#[cfg(feature = "render")] pub mod collab;` — `api::lights` is render-gated (`api/mod.rs:22-23`) and this module imports from it; an UNGATED `api::collab` breaks `cargo build -p perseus --no-default-features`, which is a routine gate in this repo. The `crate::collab` core module and `db::collab` stay ungated.)
- Modify: `crates/athenaeum-core/src/api/lights.rs` (extract `frame_cal_status` — see below)
- Test: inline `#[cfg(test)]` in `api/collab.rs`

**Interfaces:**
- Consumes: Task 2 `db::collab::*`, Task 3 `collab::gate::*`, `coordinates::{angular_distance, parse_ra_sexagesimal, parse_dec_sexagesimal}`, `db::analysis::get_frame_analyses_by_ids`, `api::db(ctx)`, `ApiError`.
- **Refactor consumed from `api/lights.rs` — read this carefully, it encodes a policy decision.** The readiness path (`get_light_calibration_readiness`, `api/lights.rs:239-337`) resolves the per-frame current `CalibrationLink`s from the catalog and calls `db::light_calibrations::derive_status` — but its `flat_norm`/`flat_norm_mode`/`params` "wanted" arguments are **caller-supplied by the frontend dialog on every path, NOT settings**. The collab gate has no dialog, so it uses the **self-consistency policy**: the "wanted" values are read back from the frame's OWN `light_calibrations` row (what was actually applied). Under this policy `derive_status` still catches everything the gate cares about — link changes, master rebuilds, engine-version bumps — while never marking a frame Stale merely because the user's dialog preferences differ from what they calibrated with. Concretely:
  1. EXTRACT the per-frame current-links resolution from the readiness path into `pub(crate) fn current_calibration_links(conn: &rusqlite::Connection, frame_id: i64) -> anyhow::Result<Vec<CalibrationLink>>` in `api/lights.rs`; make readiness call it (behavior-identical — the existing lights tests pin it).
  2. ADD `pub(crate) fn frame_cal_status(conn: &rusqlite::Connection, frame_id: i64) -> anyhow::Result<LightCalStatus>` in `api/lights.rs`: fetch the row via `db::light_calibrations::get_light_calibration_for_frame` (`None` → `Ok(LightCalStatus::NotCalibrated)`), then call `derive_status` with the current links from (1) and the ROW'S OWN stored flat-norm flag/mode/params as the wanted values (read the `LightCalRow` fields — the row stores what was applied; that is what makes `derive_status`'s param-mismatch checks vacuous by construction while keeping the staleness checks live).
  Import only `frame_cal_status` from `api/collab.rs`. One copy of the links resolution, one of the status policy.
- Produces (BINDING for Tasks 5–6; all response DTOs `#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]` + `#[serde(rename_all = "camelCase")]`):
  - `pub struct LinkSuggestion { pub frames_set_id: i64, pub name: Option<String>, pub light_count: i64, pub distance_deg: Option<f64>, pub within_radius: bool, pub already_linked: bool }`
  - `pub struct LinkedSetView { pub frames_set_id: i64, pub name: Option<String>, pub light_count: i64, pub distance_deg: Option<f64>, pub within_radius: bool }`
  - `pub struct GateReport { pub project_id: String, pub total: i64, pub publishable: i64, pub rows: Vec<crate::collab::gate::FrameGateRow> }`
  - `pub struct ProjectSetMatch { pub project_id: String, pub project_title: String, pub project_slug: String }`
  - `pub struct PortalNewProjectLink { pub url: String }`
  - Functions:
    - `pub fn link_frame_set(ctx: &ServiceContext, project_id: &str, frames_set_id: i64) -> Result<(), ApiError>` — NotFound when the project isn't in the cache or the set doesn't exist; idempotent.
    - `pub fn unlink_frame_set(ctx: &ServiceContext, project_id: &str, frames_set_id: i64) -> Result<(), ApiError>`
    - `pub fn list_link_suggestions(ctx: &ServiceContext, project_id: &str) -> Result<Vec<LinkSuggestion>, ApiError>` — every non-archived `frames_set`, ranked: within-radius first, then ascending distance, unparseable-center sets last (distance `None`).
    - `pub fn evaluate_project_gate(ctx: &ServiceContext, project_id: &str) -> Result<GateReport, ApiError>` — union of LIGHT frames across linked sets (dedup by frame id), per-frame inputs assembled per the Global-Constraints precedence, `gate::evaluate_frame` per row.
    - `pub fn record_project_link_intent(ctx: &ServiceContext, frames_set_id: i64) -> Result<PortalNewProjectLink, ApiError>` — parses the set's `objctra`/`objctdec` (Invalid with reason when absent/unparseable), stores the intent, returns the portal URL `<hub_url>/new?object=<name>&ra=<deg>&dec=<deg>&radius=1.5` built with `reqwest::Url` query_pairs (never string-concat).
    - `pub fn find_matching_projects(conn: &rusqlite::Connection, ra_deg: f64, dec_deg: f64, frames_set_id: i64) -> anyhow::Result<Vec<ProjectSetMatch>>` — cached projects whose target radius contains the point AND that aren't already linked to this set (the Task-6 hook; plain `anyhow` so both thin layers can call it with just a conn).
- Shared internal helpers this task also defines (used again by Task 5): `fn set_center(conn, frames_set_id) -> Option<(f64, f64)>` (parse `frames_set.objctra/objctdec` via the sexagesimal parsers, warn-and-None on parse failure), `fn light_count(conn, frames_set_id) -> Result<i64>` and `fn union_light_frames(conn, set_ids: &[i64]) -> Result<Vec<(i64, String)>>` — the `load_light_members` join from `api/lights.rs:220` generalized to `ino.frames_set_id IN (…)` with `SELECT DISTINCT`, plus `fn frame_gate_inputs(conn: &rusqlite::Connection, frames: &[(i64, String)]) -> anyhow::Result<Vec<GateFrameInput>>` (conn-only — the self-consistency cal-status policy needs no settings) which batch-reads: `plate_solves` (`SELECT frame_id, pixel_scale_arcsec, crval1, crval2 FROM plate_solves WHERE frame_id IN (…)`), `frames` (`SELECT id, ra, dec, objctra, objctdec, xpixsz, focallen FROM frames WHERE id IN (…)`), analyses via `get_frame_analyses_by_ids`, cal status via `frame_cal_status(conn, frame_id)` — then resolves center precedence (crval → ra/dec → parsed strings) and scale precedence (plate-solve → `((xpixsz/1000)/focallen).atan().to_degrees()*3600` when both present and focallen > 0 — NO binning multiply).

- [ ] **Step 1: Write the failing tests** (inline; construct the test `ServiceContext` the same way the existing `api/sync.rs` tests do — search `crates/athenaeum-core/src/api/sync.rs` for its `#[cfg(test)]` ServiceContext/Database construction and reuse that exact fixture shape):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::CollabProjectRow;

    /// Cached project fixture: target M101 (210.8, +54.35), radius 1.5°, one
    /// threshold rule (reject trailed frames).
    fn cached_project(conn: &rusqlite::Connection) {
        crate::db::collab::upsert_project(
            conn,
            &CollabProjectRow {
                project_id: "p-1".into(),
                slug: "m101".into(),
                title: "M 101".into(),
                data_role: "send_receive".into(),
                is_coordinator: true,
                require_approval: false,
                pending_announcements: 0,
                project_status: "active".into(),
                target_name: "M101".into(),
                target_ra_deg: 210.8,
                target_dec_deg: 54.35,
                target_radius_deg: 1.5,
                membership_version: 1,
                snapshot_payload_b64: "e30=".into(),
                snapshot_signature_b64: "e30=".into(),
                members_json: "[]".into(),
                thresholds_version: Some(1),
                thresholds_rules_json: Some(
                    r#"[{"metricKey":"not_trailed","op":"reject_if","value":true}]"#.into(),
                ),
                fetched_at: String::new(), // filled by SQL
            },
        )
        .unwrap();
    }

    /// A frames_set whose center is (`objctra`, `objctdec`), holding `lights`
    /// LIGHT frames through the imaging_nights → sessions → session_members
    /// chain. Every frame gets a frame_analysis row; frame index 1 (the second)
    /// is flagged trailed. Returns (set_id, frame_ids).
    fn seed_set(
        conn: &rusqlite::Connection,
        name: &str,
        objctra: &str,
        objctdec: &str,
        ra_deg: f64,
        dec_deg: f64,
        lights: usize,
    ) -> (i64, Vec<i64>) {
        conn.execute(
            "INSERT INTO frames_set (name, objctra, objctdec) VALUES (?1, ?2, ?3)",
            rusqlite::params![name, objctra, objctdec],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) \
             VALUES (?1, '2026-07-01T20:00:00Z', '2026-07-02T03:00:00Z')",
            [set_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'ASI2600MM')",
            [night_id],
        )
        .unwrap();
        let session_id = conn.last_insert_rowid();

        let mut frame_ids = Vec::new();
        for i in 0..lights {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) \
                 VALUES (?1, ?2, 1000, '2026-07-01T21:00:00Z', 'FITS')",
                rusqlite::params![
                    format!("/data/{name}/L_{i:04}.fits"),
                    format!("L_{i:04}.fits")
                ],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();

            // xpixsz (µm, already binned) + focallen (mm) give the header
            // pixel-scale fallback: (3.76/1000 / 1000).atan() ≈ 0.776″/px.
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, object, instrume, ra, dec, xpixsz, focallen, exptime, filter) \
                 VALUES (?1, 'Light', 'M101', 'ASI2600MM', ?2, ?3, 3.76, 1000.0, 300.0, 'L')",
                rusqlite::params![file_id, ra_deg, dec_deg],
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();

            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![session_id, frame_id],
            )
            .unwrap();

            // Every NOT NULL column of frame_analysis must be provided.
            conn.execute(
                "INSERT INTO frame_analysis \
                 (frame_id, file_id, stars_detected, median_fwhm, median_eccentricity, median_snr, \
                  median_hfr, frame_snr, snr_weight, psf_signal, background, noise, \
                  detection_threshold, width, height, source_channels, trail_r_squared, possibly_trailed) \
                 VALUES (?1, ?2, 400, 2.0, 0.4, 10.0, 2.0, 10.0, 1.0, 100.0, 10.0, 1.0, 5.0, \
                         6248, 4176, 1, 0.0, ?3)",
                rusqlite::params![frame_id, file_id, if i == 1 { 1 } else { 0 }],
            )
            .unwrap();

            frame_ids.push(frame_id);
        }
        (set_id, frame_ids)
    }

    #[test]
    fn gate_report_covers_union_of_linked_sets() {
        let (_tmp, ctx) = test_ctx(); // (TempDir, ServiceContext) — see the note below
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 2)
        };

        // An uncached project id is a NotFound, not a panic.
        assert!(matches!(
            evaluate_project_gate(&ctx, "nope"),
            Err(crate::api::ApiError::NotFound(_))
        ));

        // Nothing linked yet → no candidates.
        assert_eq!(evaluate_project_gate(&ctx, "p-1").unwrap().total, 0);

        link_frame_set(&ctx, "p-1", set_id).unwrap();
        link_frame_set(&ctx, "p-1", set_id).unwrap(); // idempotent

        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 2, "both LIGHT frames are candidates");
        assert_eq!(report.rows.len(), 2);
        assert_eq!(report.publishable, 0, "no light_calibrations rows → not calibrated");

        // Frame 0: blocked only by calibration. Frame 1: calibration + trailed.
        let row0 = report.rows.iter().find(|r| r.frame_id == frames[0]).unwrap();
        assert!(row0.failures.iter().any(|f| f.contains("not calibrated")), "{:?}", row0.failures);
        assert!(!row0.failures.iter().any(|f| f.contains("trailed")));
        assert_eq!(row0.stars_detected, Some(400));
        // 2.0 px × ~0.776 ″/px (header fallback, no binning multiply).
        let scale = ((3.76f64 / 1000.0) / 1000.0).atan().to_degrees() * 3600.0;
        assert!((row0.fwhm_arcsec.unwrap() - 2.0 * scale).abs() < 1e-6);

        let row1 = report.rows.iter().find(|r| r.frame_id == frames[1]).unwrap();
        assert!(row1.failures.iter().any(|f| f.contains("trailed")), "{:?}", row1.failures);

        unlink_frame_set(&ctx, "p-1", set_id).unwrap();
        assert_eq!(evaluate_project_gate(&ctx, "p-1").unwrap().total, 0);
    }

    #[test]
    fn suggestions_rank_by_distance_and_flag_linked() {
        let (_tmp, ctx) = test_ctx();
        let (near, far) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            let (near, _) = seed_set(&conn, "On target", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            // ~5° south of the target — outside the 1.5° radius.
            let (far, _) = seed_set(&conn, "Far away", "14:03:12", "+49:21:00", 210.8, 49.35, 1);
            (near, far)
        };

        let suggestions = list_link_suggestions(&ctx, "p-1").unwrap();
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].frames_set_id, near, "within-radius set ranks first");
        assert!(suggestions[0].within_radius);
        assert_eq!(suggestions[0].light_count, 1);
        assert!(!suggestions[0].already_linked);

        assert_eq!(suggestions[1].frames_set_id, far);
        assert!(!suggestions[1].within_radius);
        assert!(suggestions[1].distance_deg.unwrap() > 4.0);

        link_frame_set(&ctx, "p-1", near).unwrap();
        let suggestions = list_link_suggestions(&ctx, "p-1").unwrap();
        assert!(suggestions[0].already_linked);
    }

    #[test]
    fn intent_builds_portal_url_and_persists() {
        let (_tmp, ctx) = test_ctx();
        let (with_center, no_center) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            let (with_center, _) =
                seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            conn.execute("INSERT INTO frames_set (name) VALUES ('No center')", [])
                .unwrap();
            (with_center, conn.last_insert_rowid())
        };

        let link = record_project_link_intent(&ctx, with_center).unwrap();
        assert!(link.url.contains("/new?"), "portal deep link: {}", link.url);
        assert!(link.url.contains("object=M101+Set") || link.url.contains("object=M101%20Set"));
        assert!(link.url.contains("ra=210.8"));
        assert!(link.url.starts_with("http"), "must be a plain web URL");

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let intents = crate::db::collab::list_link_intents(&conn).unwrap();
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].1, with_center);
        }

        assert!(matches!(
            record_project_link_intent(&ctx, no_center),
            Err(crate::api::ApiError::Invalid(_))
        ));
    }

    #[test]
    fn find_matching_projects_excludes_linked() {
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        cached_project(&conn);
        let (set_id, _) = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);

        let matches = find_matching_projects(&conn, 210.8, 54.35, set_id).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].project_id, "p-1");

        // A point far outside the radius matches nothing.
        assert!(find_matching_projects(&conn, 10.0, 10.0, set_id).unwrap().is_empty());

        // Once linked, the project stops being suggested for that set.
        crate::db::collab::link_set(&conn, "p-1", set_id).unwrap();
        assert!(find_matching_projects(&conn, 210.8, 54.35, set_id).unwrap().is_empty());
    }
}
```

**`test_ctx()`** — the `ServiceContext` fixture. Do NOT invent one: copy the helper from `crates/athenaeum-core/src/api/sync.rs:961-1005` (`fn test_ctx() -> (tempfile::TempDir, ServiceContext)` — a TEMPDIR-FILE-backed `Database`, not `:memory:`; that matters because the pool hands out multiple connections). Keep the tuple return and bind `let (_tmp, ctx) = test_ctx();` so the tempdir lives for the test's duration. It gives a context whose `api::db(&ctx)` returns a `Database` with `init_db` applied and whose `settings` resolve `ACCOUNT_HUB_URL` to the default (the intent test asserts the URL starts with `http`).

**Note on `conn` lifetimes:** `crate::api::db(&ctx)?.conn()` hands out a pooled connection — take it in a short scope (as the tests above do with `{ … }` blocks) so the `api::*` calls that open their own connection do not deadlock on a single-connection pool.

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core api::collab 2>&1 | tail -3` → compile error.

- [ ] **Step 3: Implement** `api/collab.rs` per Interfaces. Key implementation fragments:

Union lights (generalizes `api/lights.rs::load_light_members`):

```rust
fn union_light_frames(conn: &rusqlite::Connection, set_ids: &[i64]) -> anyhow::Result<Vec<(i64, String)>> {
    if set_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; set_ids.len()].join(",");
    let sql = format!(
        "SELECT DISTINCT sm.frame_id, fi.filename \
         FROM session_members sm \
         JOIN sessions s ON s.id = sm.session_id \
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id \
         JOIN frames f ON f.id = sm.frame_id \
         JOIN files fi ON fi.id = f.file_id \
         WHERE ino.frames_set_id IN ({placeholders}) AND f.imagetyp = 'Light' \
         ORDER BY sm.frame_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(set_ids.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
```

`evaluate_project_gate` skeleton:

```rust
pub fn evaluate_project_gate(ctx: &ServiceContext, project_id: &str) -> Result<GateReport, ApiError> {
    let db = crate::api::db(ctx)?;
    let conn = db.conn();
    let project = crate::db::collab::get_project(&conn, project_id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {project_id} is not cached — refresh first")))?;

    let target = crate::collab::gate::ProjectTarget {
        ra_deg: project.target_ra_deg,
        dec_deg: project.target_dec_deg,
        radius_deg: project.target_radius_deg,
    };
    let rules: Vec<crate::collab::gate::ThresholdRuleView> = match &project.thresholds_rules_json {
        Some(json) => serde_json::from_str(json).map_err(|e| {
            tracing::warn!(project_id, error = %e, "cached threshold rules do not parse — gating on preconditions only");
            e
        }).unwrap_or_default(),
        None => Vec::new(),
    };

    let set_ids = crate::db::collab::linked_set_ids(&conn, project_id).map_err(internal)?;
    let frames = union_light_frames(&conn, &set_ids).map_err(internal)?;
    let inputs = frame_gate_inputs(&conn, &frames).map_err(internal)?;

    let rows: Vec<_> = inputs
        .iter()
        .map(|i| crate::collab::gate::evaluate_frame(i, &target, &rules))
        .collect();
    let publishable = rows.iter().filter(|r| r.publishable).count() as i64;
    Ok(GateReport { project_id: project_id.to_string(), total: rows.len() as i64, publishable, rows })
}
```

(`fn internal(e: anyhow::Error) -> ApiError` — the module-local mapper; follow the house `ApiError::Internal` conversion used across `api/*.rs`.)

Suggestions:

```rust
pub fn list_link_suggestions(ctx: &ServiceContext, project_id: &str) -> Result<Vec<LinkSuggestion>, ApiError> {
    let db = crate::api::db(ctx)?;
    let conn = db.conn();
    let project = crate::db::collab::get_project(&conn, project_id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {project_id} is not cached")))?;

    let mut out = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id, name FROM frames_set WHERE is_archived = 0 ORDER BY id DESC")
        .map_err(|e| internal(e.into()))?;
    let sets = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
        .map_err(|e| internal(e.into()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| internal(e.into()))?;

    for (set_id, name) in sets {
        let center = set_center(&conn, set_id);
        let distance_deg = center.map(|(ra, dec)| {
            crate::coordinates::angular_distance(ra, dec, project.target_ra_deg, project.target_dec_deg)
        });
        out.push(LinkSuggestion {
            frames_set_id: set_id,
            name,
            light_count: light_count(&conn, set_id).map_err(internal)?,
            within_radius: distance_deg.map(|d| d <= project.target_radius_deg).unwrap_or(false),
            already_linked: crate::db::collab::is_set_linked(&conn, project_id, set_id).map_err(internal)?,
            distance_deg,
        });
    }
    out.sort_by(|a, b| {
        b.within_radius
            .cmp(&a.within_radius)
            .then_with(|| {
                a.distance_deg
                    .unwrap_or(f64::MAX)
                    .partial_cmp(&b.distance_deg.unwrap_or(f64::MAX))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    Ok(out)
}
```

`find_matching_projects` (Task-6 hook, plain conn):

```rust
pub fn find_matching_projects(
    conn: &rusqlite::Connection,
    ra_deg: f64,
    dec_deg: f64,
    frames_set_id: i64,
) -> anyhow::Result<Vec<ProjectSetMatch>> {
    let mut out = Vec::new();
    for p in crate::db::collab::list_projects(conn)? {
        let d = crate::coordinates::angular_distance(ra_deg, dec_deg, p.target_ra_deg, p.target_dec_deg);
        if d <= p.target_radius_deg && !crate::db::collab::is_set_linked(conn, &p.project_id, frames_set_id)? {
            out.push(ProjectSetMatch {
                project_id: p.project_id,
                project_title: p.title,
                project_slug: p.slug,
            });
        }
    }
    Ok(out)
}
```

`record_project_link_intent`: read `frames_set` name/objctra/objctdec; parse via `parse_ra_sexagesimal`/`parse_dec_sexagesimal` (Invalid: `"the set has no usable center coordinates"`); `db::collab::add_link_intent`; hub URL via `ctx.settings.get_with_precedence(&conn, crate::settings::keys::ACCOUNT_HUB_URL, crate::settings::defaults::ACCOUNT_HUB_URL)`; URL built with `reqwest::Url::parse(&hub_url)` + `set_path("/new")` + `query_pairs_mut().append_pair("object", name).append_pair("ra", &format!("{ra:.4}")).append_pair("dec", &format!("{dec:.4}")).append_pair("radius", "1.5")`.

- [ ] **Step 4: Run to verify pass** — `cargo test -p athenaeum-core api::collab` → PASS (4 tests); `cargo test -p athenaeum-core api::lights 2>&1 | tail -3` — the readiness extraction must keep the lights suites green.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/api/collab.rs crates/athenaeum-core/src/api/mod.rs crates/athenaeum-core/src/api/lights.rs
git commit -m "feat(collab): linking + ranked suggestions + gate report + portal deep-link intents"
```

---

### Task 5: Hub refresh + cards + detail (`api/collab.rs` part 2)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs`
- Test: inline `#[cfg(test)]` additions (wiremock)

**Interfaces:**
- Consumes: Task 1 `CollabClient`/`verify_and_parse`, Task 2 cache CRUD, Task 4 helpers (`evaluate_project_gate`, `set_center`, `light_count`, `linked_set_ids`), `api::account::hub_credentials(ctx) -> Result<Option<(String, String)>, ApiError>` (exists — account.rs:351).
- Produces (BINDING for Task 6; DTOs serde camelCase + ts_rs):
  - `pub struct ProjectCard { pub project_id: String, pub slug: String, pub title: String, pub data_role: String, pub coordinator: bool, pub require_approval: bool, pub pending_announcements: i64, pub project_status: String, pub target_name: String, pub target_ra_deg: f64, pub target_dec_deg: f64, pub target_radius_deg: f64, pub membership_version: i64, pub linked_sets: i64, pub candidates: i64, pub publishable: i64, pub fetched_at: String }`
  - `pub struct ProjectMemberView { pub display_name: String, pub data_role: String, pub coordinator: bool }` (serde `Deserialize` too — parsed back from `members_json`)
  - `pub struct ProjectDetail { pub card: ProjectCard, pub members: Vec<ProjectMemberView>, pub thresholds_version: Option<i32>, pub thresholds: Vec<crate::collab::gate::ThresholdRuleView>, pub links: Vec<LinkedSetView>, pub portal_base: String }`
  - `pub fn list_projects(ctx: &ServiceContext) -> Result<Vec<ProjectCard>, ApiError>` — cache only, instant; counts via `evaluate_project_gate` + `linked_set_ids`.
  - `pub fn get_project_detail(ctx: &ServiceContext, project_id: &str) -> Result<ProjectDetail, ApiError>`.
  - `pub async fn refresh_projects(ctx: &ServiceContext) -> Result<Vec<ProjectCard>, ApiError>` — the poll: signed-out → `ApiError::SignedOut("Sign in to use collaboration projects.".into())`; TOFU-pin the snapshot pubkey per hub host (settings key `collab.snapshot_pubkey.<host>` via `db::get_setting`/`db::set_setting`; first fetch stores with `tracing::info!`; a later `verify_and_parse` pin mismatch fails that project with `tracing::error!` and keeps the stale cache row); per-project fetch (`project_page` + `membership_snapshot` + `thresholds`) with per-project error isolation (`warn!` + keep stale row + continue); upsert fresh rows; `prune_projects_not_in(keep = fetched ids ∪ ids whose fetch failed-but-are-still-mine)`; then process link intents: for every intent × PROJECT THAT IS NEW THIS REFRESH (id not in the pre-refresh cache) with `angular_distance(intent, target) ≤ 0.1°` → `link_set` + `delete_link_intent` + `tracing::info!(project_id, frames_set_id, "auto-linked source set from portal deep-link intent")`; finally return `list_projects(ctx)`.

- [ ] **Step 1: Write the failing test** (wiremock end-to-end refresh):

```rust
    #[tokio::test]
    async fn refresh_populates_cache_verifies_snapshot_and_auto_links_intent() {
        // 1. ServiceContext fixture with in-memory catalog; set ACCOUNT_HUB_URL
        //    to the mock server URI; store a device token the way
        //    api::account::hub_credentials reads it (see that fn — use the
        //    file-based TokenStore fixture the account api tests use).
        // 2. Seed one frames_set with objctra/objctdec ≈ (210.8, 54.35) and an
        //    intent via record_project_link_intent (mock hub not needed for that).
        // 3. Mock hub: GET /api/v1/collab/pubkey (key K), GET /api/v1/me/projects
        //    (one project p-1, slug m101), GET /api/v1/projects/p-1 (target
        //    210.8/54.35 r1.5), GET /api/v1/projects/p-1/membership — a REAL
        //    signed snapshot built with ed25519 SigningKey K (reuse the Task-1
        //    signed_fixture technique), GET /api/v1/projects/p-1/thresholds.
        // 4. refresh_projects(ctx).await → one ProjectCard; assert:
        //    - db::collab::get_project("p-1") populated with membership_version
        //      + members_json parsed back to the fixture member;
        //    - the settings pin `collab.snapshot_pubkey.<host>` now stores K;
        //    - the intent auto-linked: linked_set_ids("p-1") == [set];
        //      intents list now empty; card.linked_sets == 1.
        // 5. Second refresh with the membership endpoint now signed by a
        //    DIFFERENT key → the p-1 row keeps its previous fetched_at content
        //    (stale kept, error logged), refresh still returns Ok.
    }
```

**Note to the implementer:** the comment block above is the test's REQUIRED behavior — write it as real code in this step (the mocks and the signing fixture are fully specified by Task 1's tests; the ServiceContext fixture is `test_ctx()` from Task 4; store the device token with `api::account::store_token_for_test` (`api/account.rs:366`, `#[cfg(test)] pub(crate)` — it writes through the same `resolve_config`/token-store path that `hub_credentials` reads)).

- [ ] **Step 2: Run to verify failure** — `cargo test -p athenaeum-core api::collab::tests::refresh 2>&1 | tail -3` → compile error (`refresh_projects` missing).

- [ ] **Step 3: Implement** `list_projects` / `get_project_detail` / `refresh_projects` per Interfaces. `refresh_projects` core loop sketch:

```rust
pub async fn refresh_projects(ctx: &ServiceContext) -> Result<Vec<ProjectCard>, ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut("Sign in to use collaboration projects.".into()));
    };
    let client = crate::collab::hub_client::CollabClient::new(&hub_url).map_err(client_err)?;

    let pinned = pinned_pubkey(ctx, &hub_url, &client).await?; // TOFU: get_setting or fetch+set_setting
    let mine = client.my_projects(&token).await.map_err(client_err)?;

    let db = crate::api::db(ctx)?;
    let previous_ids: std::collections::HashSet<String> = {
        let conn = db.conn();
        crate::db::collab::list_projects(&conn)
            .map_err(internal)?
            .into_iter()
            .map(|p| p.project_id)
            .collect()
    };

    let mut keep: Vec<String> = Vec::new();
    let mut new_targets: Vec<(String, f64, f64)> = Vec::new();
    for p in &mine {
        keep.push(p.id.clone());
        match fetch_one_project(&client, &token, &pinned, p).await {
            Ok(row) => {
                let conn = db.conn();
                if !previous_ids.contains(&row.project_id) {
                    new_targets.push((row.project_id.clone(), row.target_ra_deg, row.target_dec_deg));
                }
                crate::db::collab::upsert_project(&conn, &row).map_err(internal)?;
            }
            Err(err) => {
                // Keep the stale cache row; the project stays in `keep`.
                tracing::warn!(project_id = %p.id, error = %format!("{err:#}"), "project refresh failed — keeping cached state");
            }
        }
    }
    {
        let conn = db.conn();
        crate::db::collab::prune_projects_not_in(&conn, &keep).map_err(internal)?;
        // Auto-link deep-link intents against projects that appeared this refresh.
        for (intent_id, set_id, ra, dec) in crate::db::collab::list_link_intents(&conn).map_err(internal)? {
            if let Some((project_id, ..)) = new_targets
                .iter()
                .find(|(_, tra, tdec)| crate::coordinates::angular_distance(ra, dec, *tra, *tdec) <= 0.1)
            {
                crate::db::collab::link_set(&conn, project_id, set_id).map_err(internal)?;
                crate::db::collab::delete_link_intent(&conn, intent_id).map_err(internal)?;
                tracing::info!(%project_id, frames_set_id = set_id, "auto-linked source set from portal deep-link intent");
            }
        }
    }
    list_projects(ctx)
}
```

`fetch_one_project` assembles the `CollabProjectRow` from `project_page` + `membership_snapshot` (verified via `crate::collab::snapshot::verify_and_parse` — the row stores BOTH the raw wire payload/signature AND `members_json = serde_json::to_string(&verified.members)`) + `thresholds`. `client_err` maps `AccountClientError::Unauthorized → ApiError::SignedOut` (mirror `api/account.rs::map_client_err`'s mapping; reuse it if it is `pub(crate)`, else a local copy with a comment naming the origin).

- [ ] **Step 4: Run to verify pass** — `cargo test -p athenaeum-core api::collab` → PASS (5 tests, incl. the async refresh test).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/api/collab.rs
git commit -m "feat(collab): poll refresh with TOFU-pinned verified snapshots, cards, detail"
```

---

### Task 6: Command wiring (Tauri + Axum), ts_export, set-match hooks

**Files:**
- Create: `crates/athenaeum-tauri/src/commands/collab.rs`, `crates/athenaeum-web/src/routes/collab.rs`
- Modify: `crates/athenaeum-tauri/src/commands/mod.rs` (declare + re-export), `crates/athenaeum-tauri/src/lib.rs` (invoke_handler), `crates/athenaeum-web/src/routes/mod.rs` (routes), `crates/athenaeum-core/src/ts_export.rs` (decls)
- Modify: `crates/athenaeum-tauri/src/commands/frame_sets.rs` + the web mirror of `auto_generate_frame_sets` (set-match hook, both layers)
- Test: existing `ts_contract` test (regen) + build gates

**Interfaces:**
- Consumes: Tasks 4–5 api fns + DTOs.
- Produces — seven commands, IDENTICAL names/surfaces on both backends:

| command | args (camelCase) | returns |
| ---- | ---- | ---- |
| `list_collab_projects` | — | `Vec<ProjectCard>` |
| `refresh_collab_projects` | — | `Vec<ProjectCard>` |
| `get_collab_project_detail` | `projectId: String` | `ProjectDetail` |
| `evaluate_collab_gate` | `projectId: String` | `GateReport` |
| `list_collab_link_suggestions` | `projectId: String` | `Vec<LinkSuggestion>` |
| `set_collab_link` | `projectId: String, framesSetId: i64, linked: bool` | `()` (calls link/unlink) |
| `create_collab_link_intent` | `framesSetId: i64` | `PortalNewProjectLink` |

- Discrete event `project-set-match` with payload `ProjectSetMatchEvent { frames_set_id: i64, set_name: Option<String>, matches: Vec<ProjectSetMatch> }` (serde camelCase + ts_rs, defined in `api/collab.rs`), emitted AFTER `auto_generate_frame_sets` persists each new set whose center matches a cached project (via `api::collab::find_matching_projects`) — in BOTH thin layers. **Neither layer currently holds an emitter in `auto_generate_frame_sets`** — construct one: the Tauri command gains an `app: tauri::AppHandle` parameter and builds `TauriProgressEmitter(app.clone())` (pattern: `commands/files.rs:234`; the struct is `TauriProgressEmitter(pub AppHandle)` at `tauri_events.rs:5`); the web mirror builds `SseProgressEmitter::new(state.event_tx.clone())` (pattern: `routes/masters.rs:118`). Both then call `athenaeum_core::events::emit_event(&emitter, "project-set-match", &event)`.

- [ ] **Step 1: Tauri wrappers** (`commands/collab.rs`, house style — thin, `#[tracing::instrument(skip_all, err)]`):

```rust
//! Collaboration-project commands — thin wrappers over `athenaeum_core::api::collab`.

use athenaeum_core::api::collab as api;
use athenaeum_core::api::collab::{GateReport, LinkSuggestion, PortalNewProjectLink, ProjectCard, ProjectDetail};
use tauri::State;

use super::AppState; // AppState lives in commands/mod.rs and is NOT re-exported at the crate root

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_projects(state: State<'_, AppState>) -> Result<Vec<ProjectCard>, String> {
    api::list_projects(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn refresh_collab_projects(state: State<'_, AppState>) -> Result<Vec<ProjectCard>, String> {
    api::refresh_projects(&state.ctx).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_collab_project_detail(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectDetail, String> {
    api::get_project_detail(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn evaluate_collab_gate(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<GateReport, String> {
    api::evaluate_project_gate(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_link_suggestions(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<LinkSuggestion>, String> {
    api::list_link_suggestions(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_link(
    state: State<'_, AppState>,
    project_id: String,
    frames_set_id: i64,
    linked: bool,
) -> Result<(), String> {
    if linked {
        api::link_frame_set(&state.ctx, &project_id, frames_set_id).map_err(|e| e.to_string())
    } else {
        api::unlink_frame_set(&state.ctx, &project_id, frames_set_id).map_err(|e| e.to_string())
    }
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn create_collab_link_intent(
    state: State<'_, AppState>,
    frames_set_id: i64,
) -> Result<PortalNewProjectLink, String> {
    api::record_project_link_intent(&state.ctx, frames_set_id).map_err(|e| e.to_string())
}
```

Register `pub mod collab;` (+ any `pub use collab::*;` if `commands/mod.rs` follows that pattern) and add the seven `commands::<name>,` lines to `tauri::generate_handler![…]` in `crates/athenaeum-tauri/src/lib.rs` (alongside the sync/account block).

- [ ] **Step 2: Axum mirrors** (`routes/collab.rs`) — same seven surfaces; args via `#[derive(Deserialize)] #[serde(rename_all = "camelCase")]` structs; errors via the house `crate::routes::api_err` (maps `ApiError::SignedOut` → 401):

```rust
//! Web mirrors of the collab commands (one-for-one with commands/collab.rs).

use athenaeum_core::api::collab as api;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::routes::api_err;
use crate::WebAppState; // the web crate's state type — there is no `AppState` in athenaeum-web

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdArgs {
    project_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLinkArgs {
    project_id: String,
    frames_set_id: i64,
    linked: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentArgs {
    frames_set_id: i64,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_projects(State(state): State<WebAppState>) -> Result<Json<Vec<api::ProjectCard>>, (axum::http::StatusCode, String)> {
    api::list_projects(&state.ctx).map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn refresh_collab_projects(State(state): State<WebAppState>) -> Result<Json<Vec<api::ProjectCard>>, (axum::http::StatusCode, String)> {
    api::refresh_projects(&state.ctx).await.map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_project_detail(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::ProjectDetail>, (axum::http::StatusCode, String)> {
    api::get_project_detail(&state.ctx, &args.project_id).map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn evaluate_collab_gate(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::GateReport>, (axum::http::StatusCode, String)> {
    api::evaluate_project_gate(&state.ctx, &args.project_id).map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_link_suggestions(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<api::LinkSuggestion>>, (axum::http::StatusCode, String)> {
    api::list_link_suggestions(&state.ctx, &args.project_id).map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_link(
    State(state): State<WebAppState>,
    Json(args): Json<SetLinkArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    let r = if args.linked {
        api::link_frame_set(&state.ctx, &args.project_id, args.frames_set_id)
    } else {
        api::unlink_frame_set(&state.ctx, &args.project_id, args.frames_set_id)
    };
    r.map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn create_collab_link_intent(
    State(state): State<WebAppState>,
    Json(args): Json<IntentArgs>,
) -> Result<Json<api::PortalNewProjectLink>, (axum::http::StatusCode, String)> {
    api::record_project_link_intent(&state.ctx, args.frames_set_id).map(Json).map_err(api_err)
}
```

Register in `routes/mod.rs::build_router` (seven `.route("/api/<command_name>", post(collab::<fn>))` lines next to the sync block; match the house path convention exactly — inspect a neighboring registration). NOTE: if the house convention exposes zero-arg commands as POST with empty body, keep POST for all seven for uniformity.

- [ ] **Step 3: Set-match hooks.** In BOTH `auto_generate_frame_sets` layers (`crates/athenaeum-tauri/src/commands/frame_sets.rs` post-persist loop ~line 89-101, and its web mirror), after each `db::create_frames_set(...)` succeeds with the new `set_id` + `metadata` (which carries `objctra`/`objctdec`):

```rust
    // Collaboration: suggest linking a new set whose center falls inside one
    // of my projects' target radius (spec §7 join-first-shoot-later; never
    // auto-link — the notification is a suggestion).
    if let (Ok(ra), Ok(dec)) = (
        athenaeum_core::coordinates::parse_ra_sexagesimal(&metadata.objctra),
        athenaeum_core::coordinates::parse_dec_sexagesimal(&metadata.objctdec),
    ) {
        match athenaeum_core::api::collab::find_matching_projects(&conn, ra, dec, set_id) {
            Ok(matches) if !matches.is_empty() => {
                athenaeum_core::events::emit_event(
                    &emitter,
                    "project-set-match",
                    &athenaeum_core::api::collab::ProjectSetMatchEvent {
                        frames_set_id: set_id,
                        set_name: metadata_name.clone(),
                        matches,
                    },
                );
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(set_id, error = %format!("{err:#}"), "project match check failed"),
        }
    }
```

(Adapt the exact variable names — `metadata`, the emitter handle, the set name — to each layer's local scope; the fields/behavior are binding. If `metadata.objctra` is `Option<String>`, guard with `if let Some(..)` first.)

Define in `api/collab.rs`:

```rust
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSetMatchEvent {
    pub frames_set_id: i64,
    pub set_name: Option<String>,
    pub matches: Vec<ProjectSetMatch>,
}
```

- [ ] **Step 4: ts_export.** In `crates/athenaeum-core/src/ts_export.rs`, add to the `models.ts` `decls![]` list: `crate::api::collab::ProjectCard, crate::api::collab::ProjectDetail, crate::api::collab::ProjectMemberView, crate::api::collab::LinkedSetView, crate::api::collab::LinkSuggestion, crate::api::collab::GateReport, crate::api::collab::ProjectSetMatch, crate::api::collab::ProjectSetMatchEvent, crate::api::collab::PortalNewProjectLink, crate::collab::gate::FrameGateRow, crate::collab::gate::ThresholdRuleView` (every DTO must `#[derive(ts_rs::TS)]` — added in Tasks 3–5). Regenerate: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.

- [ ] **Step 5: Gates + commit**

Run: `cargo build --workspace 2>&1 | tail -2` (clean, zero warnings) and `cargo test -p athenaeum-core --test ts_contract` (drift test green after regen).

```bash
git add crates/athenaeum-tauri/src/commands/ crates/athenaeum-tauri/src/lib.rs \
        crates/athenaeum-web/src/routes/ crates/athenaeum-core/src/ts_export.rs \
        crates/athenaeum-core/src/api/collab.rs src/types/models.ts
git commit -m "feat(collab): commands + web mirrors + ts types + project-set-match hooks"
```

---

### Task 7: Frontend — Projects page, hook, nav, notification kind

**Files:**
- Create: `src/hooks/useProjects.ts`, `src/hooks/useProjectMatches.ts`, `src/pages/Projects.tsx`
- Modify: `src/App.tsx` (routes `/projects`, `/projects/:id`), `src/components/Layout.tsx` (nav item + mount `useProjectMatches`), `src/contexts/NotificationContext.tsx` (add `'project'` to `NotificationKind`), `src/components/NotificationPanel.tsx` (icon map entry)

**Interfaces:**
- Consumes: generated types from `src/types/models.ts` (`ProjectCard`, `ProjectSetMatchEvent`, …), `api` object, `notify()`.
- Produces: `useProjects(): { projects: ProjectCard[]; loading: boolean; refreshing: boolean; signedOut: boolean; refresh: () => Promise<void> }` — `list_collab_projects` on mount (instant cache), then `refresh_collab_projects` (and every 5 min while mounted); a `SignedOut`-shaped error (`message` contains `Sign in`) sets `signedOut` instead of surfacing an error. Route `/projects` renders the cards; `/projects/:id` is Task 8's detail. Nav item "Projects" with the `Users` lucide icon, placed after the Objects entry.

- [ ] **Step 1: `useProjects.ts`**

```ts
import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { ProjectCard } from '../types/models';

const REFRESH_INTERVAL_MS = 5 * 60 * 1000;

/** Cached-first project list: instant cache render, then a hub refresh on
 * mount and every 5 minutes while the page is open (spec §2 poll cadence). */
export function useProjects() {
  const [projects, setProjects] = useState<ProjectCard[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [signedOut, setSignedOut] = useState(false);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const fresh = await api.invoke<ProjectCard[]>('refresh_collab_projects');
      if (mounted.current) {
        setProjects(fresh);
        setSignedOut(false);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes('Sign in')) {
        if (mounted.current) setSignedOut(true);
      } else {
        console.error('[projects] refresh failed:', err);
      }
    } finally {
      if (mounted.current) setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    (async () => {
      try {
        const cached = await api.invoke<ProjectCard[]>('list_collab_projects');
        if (mounted.current) setProjects(cached);
      } catch (err) {
        console.error('[projects] cached list failed:', err);
      } finally {
        if (mounted.current) setLoading(false);
      }
      void refresh();
    })();
    const timer = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => {
      mounted.current = false;
      clearInterval(timer);
    };
  }, [refresh]);

  return { projects, loading, refreshing, signedOut, refresh };
}
```

- [ ] **Step 2: `useProjectMatches.ts`** (mounted once in `Layout` — global listener, cancelled-flag pattern, discrete notification):

```ts
import { useEffect } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type { ProjectSetMatchEvent } from '../types/models';

/** Join-first-shoot-later (spec §7): a freshly clustered set whose center falls
 * inside one of my projects' targets raises a discrete suggestion. Never
 * auto-links. */
export function useProjectMatches() {
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<ProjectSetMatchEvent>('project-set-match', (p) => {
        if (cancelled) return;
        const first = p.matches[0];
        if (!first) return;
        notify({
          title: `Frame set matches project ${first.projectTitle}`,
          detail: `${p.setName ?? `Set #${p.framesSetId}`} lies within the project target — link it from the Projects page.`,
          kind: 'project',
          tone: 'info',
          link: '/projects',
          dedupeKey: `project-match-${first.projectId}-${p.framesSetId}`,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[projects] match listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);
}
```

(Field names come from the generated `ProjectSetMatchEvent` — serde camelCase → `framesSetId`, `setName`, `matches[].projectTitle`. Verify against the regenerated `models.ts` and adjust if the generator emitted different casing.)

- [ ] **Step 3: `Projects.tsx`** (cards; tokens only):

```tsx
import { RefreshCw, Target, Users } from 'lucide-react';
import { Link } from 'react-router-dom';
import { useProjects } from '../hooks/useProjects';

export default function Projects() {
  const { projects, loading, refreshing, signedOut, refresh } = useProjects();

  if (loading) return <p className="p-6 text-content-muted">Loading projects…</p>;

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center gap-3">
        <Users size={20} className="text-content-secondary" />
        <h1 className="text-lg font-semibold text-content">Projects</h1>
        <button
          onClick={() => void refresh()}
          disabled={refreshing}
          className="ml-auto inline-flex items-center gap-2 rounded-lg border border-border px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover disabled:opacity-50"
        >
          <RefreshCw size={14} className={refreshing ? 'animate-spin' : ''} />
          Refresh
        </button>
      </div>

      {signedOut && (
        <p className="text-sm text-content-muted">
          Sign in (Settings → Account) to see your collaboration projects.
        </p>
      )}
      {!signedOut && projects.length === 0 && (
        <p className="text-sm text-content-muted">
          No projects yet — browse and join on the portal, or publish a frame set as a project.
        </p>
      )}

      <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
        {projects.map((p) => (
          <Link
            key={p.projectId}
            to={`/projects/${p.projectId}`}
            className="rounded-lg border border-border bg-surface p-4 hover:bg-surface-hover"
          >
            <div className="flex items-center gap-2">
              <span className="font-medium text-content">{p.title}</span>
              {p.coordinator && (
                <span className="rounded bg-accent/20 px-1.5 py-0.5 text-xs text-accent">coordinator</span>
              )}
              {p.projectStatus === 'closed' && (
                <span className="rounded bg-surface-hover px-1.5 py-0.5 text-xs text-content-muted">closed</span>
              )}
            </div>
            <p className="mt-1 flex items-center gap-1 text-xs text-content-muted">
              <Target size={12} /> {p.targetName} · r {p.targetRadiusDeg.toFixed(1)}°
            </p>
            <p className="mt-2 text-sm text-content-secondary">
              {p.publishable} publishable of {p.candidates}
              {p.linkedSets === 0 ? ' — link an object to start' : ` · ${p.linkedSets} linked set${p.linkedSets === 1 ? '' : 's'}`}
            </p>
            {p.coordinator && p.pendingAnnouncements > 0 && (
              <p className="mt-1 text-xs text-warning">
                {p.pendingAnnouncements} contribution{p.pendingAnnouncements === 1 ? '' : 's'} awaiting approval
              </p>
            )}
          </Link>
        ))}
      </div>
    </div>
  );
}
```

(If the token set has no `text-warning`, use the established warning token from the codebase — grep an existing usage, e.g. in the calibration views, and match it.)

- [ ] **Step 4: Wiring.** `App.tsx`: `import Projects from './pages/Projects';` + `<Route path="projects" element={<Projects />} />` (and Task 8 adds `projects/:id`). `Layout.tsx`: add `{ to: '/projects', icon: Users, label: 'Projects' }` to `navItems` after the Objects entry, and call `useProjectMatches()` inside the Layout component body. `NotificationContext.tsx`: extend the `NotificationKind` union with `'project'`. `NotificationPanel.tsx`: add `project: Users` (lucide `Users`) to `KIND_ICON`.

- [ ] **Step 5: Gates + commit**

Run: `npx tsc --noEmit` (clean) and `npm run build 2>&1 | tail -2` if the repo's standard check includes it (`tsc` is the house gate).

```bash
git add src/hooks/useProjects.ts src/hooks/useProjectMatches.ts src/pages/Projects.tsx \
        src/App.tsx src/components/Layout.tsx src/contexts/NotificationContext.tsx src/components/NotificationPanel.tsx
git commit -m "feat(ui): Projects page — cards, cached-first refresh, project notification kind"
```

---


### Task 8: Frontend — Project detail (Contribute + Overview tabs), linking UI, "Publish as project" entry

**Files:**
- Create: `src/utils/externalUrl.ts`, `src/pages/ProjectDetail.tsx`, `src/components/collab/LinkObjectDialog.tsx`
- Modify: `src/App.tsx` (`projects/:id` route), `src/pages/FrameSetDetail.tsx` (a "Publish as project" action)

**Interfaces:**
- Consumes: generated types (`ProjectDetail`, `GateReport`, `FrameGateRow`, `LinkSuggestion`, `PortalNewProjectLink`), `api`, `openUrl` from `src/api/desktop.ts` (portal opens in the browser — Tauri plugin-opener / web `window.open`), `notify()` from `useNotifications()`.
- Produces: `/projects/:id` detail with two tabs (**Contribute**: linked-object chips + link dialog + gate candidate table with per-rule reasons + a disabled "Publish N passing frames" button, `title="Sending arrives with the exchange update"`; **Overview**: members, thresholds read-only, target line, "Manage on portal" opening `<portalBase>/p/<slug>/admin` for coordinators else `/p/<slug>`). `FrameSetDetail` gains a "Publish as project" button → `create_collab_link_intent` → `openUrl(result.url)`.

- [ ] **Step 1: The shared external-URL guard** (S3 — used by BOTH call sites in this task)

Create `src/utils/externalUrl.ts`:

```ts
/**
 * Only ever hand http(s) URLs to the OS browser. `openUrl` reaches
 * plugin-opener on desktop, so a `javascript:`/`file:`/`data:` URL would be a
 * real vector — and every candidate here traces back to the settings-configurable
 * hub URL (portalBase, or the deep link minted by `create_collab_link_intent`).
 * Returns the normalized URL, or null when it is not a plain web address.
 */
export function safeExternalUrl(raw: string): string | null {
  try {
    const u = new URL(raw);
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.toString() : null;
  } catch {
    return null;
  }
}
```

- [ ] **Step 2: `LinkObjectDialog.tsx`** — the ranked link picker

Create `src/components/collab/LinkObjectDialog.tsx`:

```tsx
import { useCallback, useEffect, useState } from 'react';
import { Check, Link2, X } from 'lucide-react';
import { api } from '../../api';
import type { LinkSuggestion } from '../../types/models';

/**
 * Link/unlink frame sets to a project (spec §7 — linking is explicit, never
 * automatic). Suggestions are ranked by the backend: within-radius first, then
 * ascending distance from the project target.
 */
export default function LinkObjectDialog({
  projectId,
  onClose,
  onChanged,
}: {
  projectId: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [suggestions, setSuggestions] = useState<LinkSuggestion[]>([]);
  const [busy, setBusy] = useState<number | null>(null);

  const load = useCallback(async () => {
    try {
      setSuggestions(
        await api.invoke<LinkSuggestion[]>('list_collab_link_suggestions', { projectId }),
      );
    } catch (err) {
      console.error('[projects] link suggestions failed:', err);
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (s: LinkSuggestion) => {
    setBusy(s.framesSetId);
    try {
      await api.invoke('set_collab_link', {
        projectId,
        framesSetId: s.framesSetId,
        linked: !s.alreadyLinked,
      });
      await load();
      onChanged();
    } catch (err) {
      console.error('[projects] link toggle failed:', err);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        className="max-h-[80vh] w-[34rem] overflow-auto rounded-lg border border-border bg-surface p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center gap-2">
          <Link2 size={16} className="text-content-secondary" />
          <h2 className="font-medium text-content">Link an object</h2>
          <button
            onClick={onClose}
            className="ml-auto text-content-muted transition-colors hover:text-content"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>
        <p className="mb-3 text-xs text-content-muted">
          Frame sets nearest the project target come first. Linking is a catalog-only
          choice — each frame is still checked against the project&apos;s quality gate.
        </p>
        <ul className="space-y-1">
          {suggestions.map((s) => (
            <li
              key={s.framesSetId}
              className="flex items-center gap-2 rounded border border-border px-3 py-2 text-sm"
            >
              <span className="truncate text-content">{s.name ?? `Set #${s.framesSetId}`}</span>
              <span className="flex-shrink-0 text-xs text-content-muted">
                {s.lightCount} lights
              </span>
              {s.withinRadius ? (
                <span className="flex-shrink-0 rounded bg-accent/20 px-1.5 py-0.5 text-xs text-accent">
                  on target
                </span>
              ) : s.distanceDeg != null ? (
                <span className="flex-shrink-0 text-xs text-content-muted">
                  {s.distanceDeg.toFixed(1)}° away
                </span>
              ) : (
                <span className="flex-shrink-0 text-xs text-content-muted">no center</span>
              )}
              <button
                onClick={() => void toggle(s)}
                disabled={busy === s.framesSetId}
                className={`ml-auto flex-shrink-0 inline-flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors disabled:opacity-50 ${
                  s.alreadyLinked
                    ? 'border border-border text-content-secondary hover:bg-surface-hover'
                    : 'bg-accent text-surface hover:bg-accent-hover'
                }`}
              >
                {s.alreadyLinked ? (
                  <>
                    <Check size={12} /> Linked
                  </>
                ) : (
                  'Link'
                )}
              </button>
            </li>
          ))}
          {suggestions.length === 0 && (
            <li className="py-2 text-sm text-content-muted">No frame sets to link yet.</li>
          )}
        </ul>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: `ProjectDetail.tsx`** — Contribute + Overview tabs

Create `src/pages/ProjectDetail.tsx`:

```tsx
import { useCallback, useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { ExternalLink, Plus, Target } from 'lucide-react';
import { api } from '../api';
import { openUrl } from '../api/desktop';
import { safeExternalUrl } from '../utils/externalUrl';
import { useNotifications } from '../contexts/NotificationContext';
import LinkObjectDialog from '../components/collab/LinkObjectDialog';
import type { FrameGateRow, GateReport, ProjectDetail as Detail } from '../types/models';

export default function ProjectDetail() {
  const { id } = useParams();
  const { notify } = useNotifications();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [gate, setGate] = useState<GateReport | null>(null);
  const [tab, setTab] = useState<'contribute' | 'overview'>('contribute');
  const [linkOpen, setLinkOpen] = useState(false);
  const [missing, setMissing] = useState(false);

  const load = useCallback(async () => {
    if (!id) return;
    setMissing(false);
    try {
      // The detail comes from the local cache of VERIFIED snapshots (core owns
      // verification); the gate is evaluated locally over the linked sets.
      const [d, g] = await Promise.all([
        api.invoke<Detail>('get_collab_project_detail', { projectId: id }),
        api.invoke<GateReport>('evaluate_collab_gate', { projectId: id }),
      ]);
      setDetail(d);
      setGate(g);
    } catch (err) {
      console.error('[projects] detail load failed:', err);
      setMissing(true);
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  const openPortal = async (path: string) => {
    if (!detail) return;
    const candidate = `${detail.portalBase}${path}`;
    const safe = safeExternalUrl(candidate);
    if (!safe) {
      console.error('[projects] refused non-http(s) portal url:', candidate);
      notify({
        title: 'Could not open the portal',
        detail: 'The configured hub address is not a valid web address.',
        kind: 'project',
        tone: 'warning',
      });
      return;
    }
    await openUrl(safe);
  };

  if (missing)
    return (
      <p className="p-6 text-sm text-content-muted">
        This project is not in your local list — refresh the Projects page.
      </p>
    );
  if (!detail) return <p className="p-6 text-content-muted">Loading…</p>;

  const c = detail.card;
  const portalPath = c.coordinator ? `/p/${c.slug}/admin` : `/p/${c.slug}`;

  return (
    <div className="space-y-4 p-6">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="truncate text-lg font-semibold text-content">{c.title}</h1>
        <span className="flex items-center gap-1 text-xs text-content-muted">
          <Target size={12} /> {c.targetName} · r {c.targetRadiusDeg.toFixed(1)}°
        </span>
        {c.coordinator && (
          <span className="rounded bg-accent/20 px-1.5 py-0.5 text-xs text-accent">coordinator</span>
        )}
        <button
          onClick={() => void openPortal(portalPath)}
          className="ml-auto inline-flex items-center gap-1 text-sm text-content-secondary transition-colors hover:text-content"
        >
          Manage on portal <ExternalLink size={13} />
        </button>
      </div>

      <div className="flex gap-1 border-b border-border">
        {(['contribute', 'overview'] as const).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`px-4 py-2 text-sm capitalize transition-colors ${
              tab === t
                ? 'border-b-2 border-accent font-medium text-content'
                : 'text-content-muted hover:text-content-secondary'
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      {tab === 'contribute' ? (
        <div className="space-y-4">
          <div className="flex items-center gap-3">
            <span className="text-sm font-medium text-content">Linked objects</span>
            <button
              onClick={() => setLinkOpen(true)}
              className="inline-flex items-center gap-1 rounded border border-border px-2 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover"
            >
              <Plus size={12} /> Link an object
            </button>
          </div>

          {detail.links.length === 0 ? (
            <p className="text-sm text-content-muted">Link an object to start.</p>
          ) : (
            <ul className="flex flex-wrap gap-2">
              {detail.links.map((l) => (
                <li
                  key={l.framesSetId}
                  className="rounded border border-border px-2 py-1 text-xs text-content-secondary"
                >
                  <span className="break-words">{l.name ?? `Set #${l.framesSetId}`}</span> ·{' '}
                  {l.lightCount} lights
                  {l.withinRadius ? ' · on target' : ''}
                </li>
              ))}
            </ul>
          )}

          <GateTable gate={gate} />

          <button
            disabled
            title="Sending arrives with the exchange update"
            className="cursor-not-allowed rounded bg-surface-hover px-4 py-2 text-sm text-content-muted"
          >
            Publish {gate?.publishable ?? 0} passing frames (coming soon)
          </button>
        </div>
      ) : (
        <div className="space-y-4 text-sm">
          <section>
            <h2 className="mb-1 font-medium text-content">Members</h2>
            <ul className="text-content-secondary">
              {detail.members.map((m, i) => (
                <li key={`${m.displayName}-${i}`} className="break-words">
                  {m.displayName} — {m.coordinator ? 'coordinator' : m.dataRole}
                </li>
              ))}
            </ul>
          </section>

          <section>
            <h2 className="mb-1 font-medium text-content">
              Quality thresholds
              {detail.thresholdsVersion != null ? ` (v${detail.thresholdsVersion})` : ''}
            </h2>
            {detail.thresholds.length === 0 ? (
              <p className="text-content-muted">No thresholds set.</p>
            ) : (
              <ul className="text-content-secondary">
                {detail.thresholds.map((r, i) => (
                  <li key={`${r.metricKey}-${i}`} className="break-words">
                    {r.op === 'reject_if'
                      ? `${r.metricKey} — reject when ${String(r.value)}`
                      : `${r.metricKey} ${r.op === 'lte' ? '≤' : r.op === 'gte' ? '≥' : r.op} ${String(r.value)}`}
                  </li>
                ))}
              </ul>
            )}
            <p className="mt-1 text-xs text-content-muted">
              Thresholds are set by the coordinator on the portal. Changes are prospective —
              already-published frames stay published.
            </p>
          </section>
        </div>
      )}

      {linkOpen && id && (
        <LinkObjectDialog
          projectId={id}
          onClose={() => setLinkOpen(false)}
          onChanged={() => void load()}
        />
      )}
    </div>
  );
}

/** Candidate frames of the linked sets with their per-rule gate verdict. */
function GateTable({ gate }: { gate: GateReport | null }) {
  if (!gate) return null;
  if (gate.total === 0)
    return (
      <p className="text-sm text-content-muted">
        No candidate frames yet — link an object that has LIGHT frames.
      </p>
    );

  return (
    <div className="overflow-x-auto">
      <p className="mb-1 text-sm text-content-secondary">
        {gate.publishable} publishable of {gate.total}
      </p>
      <table className="w-full text-left text-xs">
        <thead className="text-content-muted">
          <tr>
            <th className="py-1 pr-3 font-normal">Frame</th>
            <th className="pr-3 font-normal">FWHM″</th>
            <th className="pr-3 font-normal">Ecc</th>
            <th className="pr-3 font-normal">Stars</th>
            <th className="font-normal">Gate</th>
          </tr>
        </thead>
        <tbody>
          {gate.rows.map((r: FrameGateRow) => (
            <tr key={r.frameId} className="border-t border-border/50">
              <td className="max-w-[16rem] truncate py-1 pr-3 text-content">{r.filename}</td>
              <td className="pr-3 text-content-secondary">
                {r.fwhmArcsec != null ? r.fwhmArcsec.toFixed(2) : '—'}
              </td>
              <td className="pr-3 text-content-secondary">
                {r.eccentricity != null ? r.eccentricity.toFixed(2) : '—'}
              </td>
              <td className="pr-3 text-content-secondary">{r.starsDetected ?? '—'}</td>
              <td
                className={r.publishable ? 'text-success' : 'text-error'}
                title={r.publishable ? undefined : r.failures.join('; ')}
              >
                {r.publishable ? '✓ publishable' : (r.failures[0] ?? 'not publishable')}
                {!r.publishable && r.failures.length > 1 ? ` (+${r.failures.length - 1})` : ''}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
```

**Token check:** `text-success` / `text-error` / `bg-accent` / `border-border` / `bg-surface-hover` must exist in the design-token set — grep an existing table or badge (e.g. `src/components/CalibrationSetTable.tsx`) and swap in the established names if any differ. Never introduce a raw color.

**Field-casing check:** `fwhmArcsec`, `starsDetected`, `framesSetId`, `portalBase`, `thresholdsVersion`, `alreadyLinked`, `withinRadius`, `distanceDeg`, `lightCount` come from the ts-rs regeneration in Task 6 — read `src/types/models.ts` and match exactly (do not hand-edit the generated file).

- [ ] **Step 4: `FrameSetDetail.tsx` — "Publish as project" action**

In `src/pages/FrameSetDetail.tsx`, add the handler next to the file's existing set-level actions (imports: `api` from `../api`, `openUrl` from `../api/desktop`, `safeExternalUrl` from `../utils/externalUrl`, `useNotifications`, a lucide icon such as `Users`):

```tsx
  const publishAsProject = async () => {
    try {
      // Core mints the deep link (percent-encoded, Url-built) and records an
      // intent so the next poll auto-links this set to the project the portal
      // creates from it (spec §8).
      const { url } = await api.invoke<PortalNewProjectLink>('create_collab_link_intent', {
        framesSetId: frameSetId,
      });
      const safe = safeExternalUrl(url);
      if (!safe) {
        console.error('[projects] refused non-http(s) intent url:', url);
        notify({
          title: 'Could not open the portal',
          detail: 'The configured hub address is not a valid web address.',
          kind: 'project',
          tone: 'warning',
        });
        return;
      }
      await openUrl(safe);
    } catch (err) {
      console.error('[projects] publish-as-project failed:', err);
      notify({
        title: 'Could not start project creation',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
      });
    }
  };
```

Render it with the file's existing button styling:

```tsx
  <button
    onClick={() => void publishAsProject()}
    className="inline-flex items-center gap-2 rounded-lg border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover"
  >
    <Users size={14} />
    Publish as project
  </button>
```

Wire `frameSetId` to the id variable already in scope in that page (read the file — it is the route param / the loaded set's `id`), and import the `PortalNewProjectLink` type from `../types/models`. A set with no usable center coordinates fails in core with an `Invalid` error whose message reaches the user through the `catch` → notification.

- [ ] **Step 5: Route + gates + commit**

`src/App.tsx`: `import ProjectDetail from './pages/ProjectDetail';` and add `<Route path="projects/:id" element={<ProjectDetail />} />` as a sibling of the `projects` route from Task 7.

Run: `npx tsc --noEmit`
Expected: clean (0 errors).

Manual pass against the **Security requirements** checklist below (the reviewer will repeat it): no `dangerouslySetInnerHTML`; every `openUrl` guarded by `safeExternalUrl`; no email/accountId/node-pubkey rendered; the publish button disabled; no direct hub `fetch`.

```bash
git add src/utils/externalUrl.ts src/components/collab/LinkObjectDialog.tsx \
        src/pages/ProjectDetail.tsx src/pages/FrameSetDetail.tsx src/App.tsx
git commit -m "feat(ui): project detail — Contribute/Overview tabs, link dialog, guarded portal deep links"
```

---

## Security requirements (bind Tasks 7 and 8)

These are **normative for the two frontend tasks** — the reviewer gates on them. The through-line: everything the app renders here originates *outside this machine* (hub JSON, other accounts' display names, coordinator-authored project text), so the frontend treats hub data as untrusted input and keeps every trust decision in core.

### S1. Trust boundary: the frontend never verifies, never decides

The hub-signed membership snapshot is verified **in core only** (`collab::snapshot::verify_and_parse`, Task 1) against the TOFU-pinned key, and only verified content is cached (Task 5). Tasks 7–8 therefore:

- **Never** call the hub directly (no `fetch`, no URL of the hub assembled in TS beyond `portalBase` deep links) — every read goes through the seven commands from Task 6.
- **Never** re-derive membership/permissions in TS. `card.coordinator`, `detail.members`, `require_approval` are *display* values already derived from a verified snapshot; the app must not grant itself anything based on them (there is nothing to grant in slice 3 — the only capability-shaped UI, "Publish", is disabled).
- Treat a project missing from the cache as "not mine": the detail route for an unknown id renders "Project not found — refresh" rather than falling back to a hub fetch.

### S2. Rendering untrusted strings (XSS class)

Project titles, member display names, target names, threshold `metric_key`/`op`, and gate failure strings all come from other accounts or the hub.

- **Render as text only.** `dangerouslySetInnerHTML` is forbidden anywhere under `src/pages/Projects.tsx`, `src/pages/ProjectDetail.tsx`, `src/components/collab/**` (React escapes by default — do not defeat it).
- Never build a DOM node, a `style` string, or a `title`/`href` attribute by concatenating hub strings without the rules in S3.
- Long/hostile strings must not break layout: apply `truncate`/`break-words` on title, display-name, filename, and failure-reason cells (a 5 000-char description is within the hub's own caps and will arrive intact).
- Gate failure reasons are produced by core (Task 3) from numbers + fixed labels — safe by construction; the *rule* fields (`metricKey`, `op`) in the Overview tab are hub-authored and get the same text-only treatment.

### S3. Opening external URLs (`openUrl`) — validate scheme + host before opening

`openUrl` reaches the OS browser (Tauri `plugin-opener`) — a `javascript:`, `file:`, or `data:` URL here is a real vector, and `portalBase`/the intent `url` both trace back to a settings-configurable hub URL.

- Add a single guard used by BOTH call sites (`ProjectDetail`'s "Manage on portal" and `FrameSetDetail`'s "Publish as project"), e.g. `src/utils/externalUrl.ts`:

```ts
/** Only ever hand http(s) URLs to the OS browser. Hub-derived strings
 * (portalBase, deep-link intents) are configurable and must not be able to
 * smuggle a javascript:/file:/data: scheme into plugin-opener. */
export function safeExternalUrl(raw: string): string | null {
  try {
    const u = new URL(raw);
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.toString() : null;
  } catch {
    return null;
  }
}
```

- Both call sites: `const safe = safeExternalUrl(candidate); if (!safe) { console.error('[projects] refused non-http(s) url:', candidate); notify({ title: 'Could not open the portal', detail: 'The configured hub URL is not a valid web address.', kind: 'project', tone: 'warning' }); return; } await openUrl(safe);`
- The **portal deep link is assembled in core** (Task 4 `record_project_link_intent`, `reqwest::Url` + `query_pairs_mut` — no string concat, so the set name is percent-encoded); TS must not rebuild it. `ProjectDetail` composes only `portalBase + '/p/' + slug` (+ `/admin`) — `slug` is hub-generated `[a-z0-9-]`, but it still passes through `safeExternalUrl` as one URL.
- Never put a token, an account id, or an email in an outbound URL. The intent link carries object name + coordinates only.

### S4. Cross-account data hygiene (spec §2a)

- The app displays **display names only** — never emails, never `accountId`s. `ProjectMemberView` (Task 5) deliberately carries no account id; do not add one to the wire "for keys" (use the index or `displayName` as the React key).
- Node pubkeys from the snapshot are **not** rendered in slice 3 (they exist in the cache for slice 4's authorizer). Don't surface them in the Overview tab.
- `pendingAnnouncements` is coordinator-gated **on the hub** (`/me/projects`); the UI shows it as-is and must not attempt to fetch or infer pending data for non-coordinators.

### S5. Notifications and deep links

- `useProjectMatches` notifies with `link: '/projects'` — an **in-app route only**. Never pass a hub-supplied string as a `notify({ link })` (the notification link is navigated with the app router; an external URL there would be a second, unguarded `openUrl`-shaped path).
- Keep the `dedupeKey` (`project-match-<projectId>-<framesSetId>`) so a re-cluster of the same set cannot spam the panel.
- The listener uses the mandatory cancelled-flag `api.listen` pattern (CLAUDE.md) — a leaked second listener would double every suggestion.

### S6. Errors and logging

- Never swallow: every `catch` in these two tasks logs with `console.error('[projects] …', err)` before deciding what to show.
- Surface hub-authored error text to the user only through `notify({ detail })` (plain text), never into an `href`, `title`, or markup.
- Do not log the device token, the hub URL with credentials, or raw snapshot bytes. The commands never return them; keep it that way if you add fields.

### S7. Command-surface discipline

- The seven commands from Task 6 are the entire attack surface of these tasks. Do not add a frontend-only shortcut that passes a *path*, *URL*, or *SQL fragment* into a command — the only accepted argument types are `projectId: string` (opaque hub id), `framesSetId: number`, `linked: boolean`.
- `set_collab_link` is the only mutating command reachable from Tasks 7–8; it is idempotent and scoped to the local catalog. Nothing in slice 3 sends data off-machine (the exchange lands in slice 4) — if a step you are writing appears to transmit frames, you have drifted out of scope: stop and re-read §13.

**Reviewer checklist for Tasks 7–8:** no `dangerouslySetInnerHTML`; every `openUrl` call preceded by `safeExternalUrl`; no email/accountId/pubkey rendered; `notify({ link })` is in-app only; cancelled-flag listener; no direct hub `fetch`; publish button disabled.

---

## Post-plan checklist (not tasks — verification/ops notes)

- **Slice gates (run before the whole-branch review):** `cargo build --workspace` (clean, zero warnings), `cargo test -p athenaeum-core` (all suites; the new `collab`/`db::collab`/`api::collab` tests included), **`cargo build -p perseus --no-default-features`** (proves the `#[cfg(feature = "render")]` gate on `api::collab` holds), `cargo test -p athenaeum-core --test ts_contract` (no type drift after the Task-6 regen), `npx tsc --noEmit` (0 errors).
- **Live verification — no deploy needed.** The collab hub already runs on `test-hub.artfrom.space` (slices 1+2), and debug builds default there. `npm run tauri dev`, sign in (Settings → Account), then:
  1. Join a project on the portal → it appears on the Projects page within one poll (or immediately via Refresh).
  2. Link a frame set → the Contribute gate table fills with per-frame verdicts and reasons.
  3. Re-cluster a set whose center sits inside a project target → the `project-set-match` notification fires (suggestion only — nothing is auto-linked).
  4. "Publish as project" on a frame set → the portal `/new` opens prefilled from the catalog; submit → the next poll auto-links that source set to the new project.
  5. Coordinator "Manage on portal" opens `/p/<slug>/admin`; a member gets `/p/<slug>`.
  - Web-mode smoke (the two-backend rule is only real if it runs): `cargo run -p athenaeum-web` + `npm run dev:web`, repeat steps 1–2.
- **Nothing leaves the machine in this slice.** No publish, no announce, no P2P — the exchange is slice 4. The `collab_projects.snapshot_payload_b64` / `snapshot_signature_b64` columns are cached now precisely so slice 4's project `PeerAuthorizer` can re-verify offline.
- **Slice-4 carry (unchanged from the slice-1/2 reviews):** apply every signature-verified snapshot (compare content, not version); node lists are ordered by raw pubkey bytes, never base64-ASCII; only hub-fetched snapshots may feed the authorizer; `supersedes` carries announcement ids.
- **No version bump, no deploy, no merge in this slice** — app-only work on branch `0.5.0`; releases keep flowing from `0.4.0`.
- **Poll seam (deliberate):** the 5-minute refresh interval lives in `useProjects` and runs only while the Projects page is mounted. Slice 4's project `PeerAuthorizer` needs an APP-LEVEL poll (serving decisions can't wait for the user to open a page) — plan that promotion there; the cache/refresh split in `api::collab` already supports it.

## Self-review notes (already applied)

1. **Spec coverage (§4 / §7 / §13-slice-3):** verified snapshots ✓ (T1), membership cache + `project_links` ✓ (T2/T5), quality gate with all four preconditions + the metric registry + the arcsec conversion + the XPIXSZ-binning gotcha ✓ (T3 + Global Constraints), Projects page cards ✓ (T7), Contribute/Overview tabs ✓ (T8), project↔object linking with a ranked picker ✓ (T4/T8), join-first-shoot-later suggestion ✓ (T6 hook + T7 listener), "Publish as project" prefill + auto-link intent ✓ (T4/T5/T8), coordinator "Manage on portal" deep link ✓ (T8). **Deliberately NOT in slice 3** (per §13): publish/announce/receive/moderation and the Receive tab (all need the exchange → slice 4); member administration from the app (the portal owns it, §5a). The disabled "Publish N passing frames" button is the seam marker. **Two deliberate §7 trims:** the link picker ranks by distance only (the spec's OBJECT-name-similarity secondary signal is dropped — distance alone is decisive for real captures, and the name is displayed for the human to judge); the join-first-shoot-later hook fires on set CREATION (`auto_generate_frame_sets`) only, not on later set growth via merge/find-new-images (a set that grows into a target is rare and self-corrects on the next full re-cluster) — revisit both only if real usage asks.
2. **Type consistency:** each DTO is defined once (`ProjectCard`, `ProjectDetail`, `ProjectMemberView`, `LinkedSetView`, `LinkSuggestion`, `GateReport`, `FrameGateRow`, `ThresholdRuleView`, `ProjectSetMatch`, `ProjectSetMatchEvent`, `PortalNewProjectLink` — T3–T5), exported once (T6 `ts_export`), consumed under the same names in T7/T8. Command names are identical across Tauri, Axum, and the frontend: `list_collab_projects`, `refresh_collab_projects`, `get_collab_project_detail`, `evaluate_collab_gate`, `list_collab_link_suggestions`, `set_collab_link`, `create_collab_link_intent`. `current_calibration_links` + `frame_cal_status(conn, frame_id)` exist in exactly one copy each (extracted/added in `api/lights.rs`, T4), and `frame_cal_status` encodes the self-consistency policy (wanted = the row's own stored flat-norm/params) explicitly.
3. **Deliberate choices:** cache-first UI with a 5-minute poll (spec §2 cadence, not a live socket); TOFU pin of the hub's snapshot key per host (matches the hub's own "clients pin the pubkey" contract); per-project error isolation on refresh (one unreachable project never blanks the list); deep-link intents auto-link only against projects that are NEW in that refresh and within 0.1° (an intent can never re-link a set the user has since unlinked); the gate is a pure function (fully unit-testable with no hub, no DB); portal deep links are minted in core with `Url` + `query_pairs_mut` and re-validated in the UI by `safeExternalUrl` (defense in depth, S3).