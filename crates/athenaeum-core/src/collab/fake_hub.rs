//! A stateful, in-process fake of the hub's collab v3 api (plan P16), for
//! tests only.
//!
//! ONE `FakeHub` can serve up to three app contexts at once: each context
//! signs in with its own device token, and the fake resolves that token to an
//! account and a device. State lives in one `Arc<Mutex<FakeHubState>>` behind
//! a single catch-all wiremock responder, so tests can both drive the hub
//! through its HTTP routes (as the app does) and reach into its state
//! directly (as a portal admin or another member would).
//!
//! The responders implement the hub rules the app depends on (hub `main`
//! 6127951, `routes/frames.rs` / `routes/holders.rs`):
//!
//! - `gateVersion` must equal the current thresholds version (0 when none):
//!   409 `gate version X is stale, current is Y`, verbatim;
//! - `filterCanonical` must be a canonical of the current dictionary (400),
//!   and a project without a dictionary refuses every announce (409);
//! - an already-announced uuid is a 409
//!   `frame {uuid} already announced; use /version to publish new content`;
//! - a batch is atomic: any refusal leaves the state untouched;
//! - birth state is `published` unless the project requires approval and
//!   the publisher is neither trusted nor a moderator;
//! - pending and rejected rows are visible only to their publisher and to
//!   moderators (`data.moderate`, which a coordinator always has);
//! - holder reports are filtered per row: `send` members may hold only their
//!   own frames, `send_receive` members and moderators any published frame,
//!   moderators also pending ones; holds on a stale content version drop;
//! - every manifest-visible change bumps the project version and stamps the
//!   touched rows' `manifestVersion` with it;
//! - the manifest is ordered by `(manifestVersion, frameUuid)` and paged with
//!   `next = {since, after}` (page size overridable for tests);
//! - a frame's `frameSeq` is a dense per-project ordinal, assigned at
//!   announce/seed, never reused (v3 wave 3: the manifest gains `frameSeq`
//!   and loses `holderCount`).
//!
//! The membership snapshot is signed with a fixed test keypair exactly as the
//! hub signs it, so the app's TOFU pin + `verify_and_parse` run for real.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::collab::filters::DictionaryEntry;
use crate::collab::hub_client::FrameViewWire;

/// The hub's holder freshness window (`HOLDER_FRESH_SQL`).
pub const HOLDER_FRESH: Duration = Duration::from_secs(75 * 60);
/// The hub's manifest page cap.
pub const MANIFEST_PAGE: usize = 1000;
/// The hub's announce batch cap.
const MAX_BATCH: usize = 500;
/// The hub's `meta` size cap.
const MAX_META_BYTES: usize = 8192;

/// One signed-in device: the account it belongs to and its node pubkey.
#[derive(Debug, Clone)]
pub struct FakeAccount {
    pub account_id: String,
    pub display: String,
    /// base64 of the device's 32-byte node id.
    pub device_pubkey_b64: String,
    pub relay_url: Option<String>,
}

/// One project membership.
#[derive(Debug, Clone)]
pub struct FakeMember {
    pub account_id: String,
    /// `send` | `send_receive`.
    pub data_role: String,
    pub coordinator: bool,
    /// Governance caps as the hub stores them (`/me/projects` returns them
    /// raw; a coordinator implicitly has every cap).
    pub gov_caps: Vec<String>,
    pub trusted: bool,
}

impl FakeMember {
    /// The hub's `Member::has_cap`: a coordinator has every cap.
    pub fn has_cap(&self, cap: &str) -> bool {
        self.coordinator || self.gov_caps.iter().any(|c| c == cap)
    }
}

/// One project's hub-side state.
pub struct FakeProject {
    pub slug: String,
    pub title: String,
    /// `active` | `closed`.
    pub status: String,
    pub version: i64,
    pub membership_version: i64,
    /// 0 = the project has no thresholds (`current: null`).
    pub thresholds_version: i32,
    pub thresholds_rules: Vec<Value>,
    /// 0 = the project has no dictionary (`current: null`).
    pub dictionary_version: i32,
    pub dictionary: Vec<DictionaryEntry>,
    /// Keyed by frame uuid. `own` is computed per viewer at response time;
    /// the stored value is ignored.
    pub frames: BTreeMap<String, FrameViewWire>,
    /// frame uuid → device pubkey (base64) → (content version, reported at).
    pub holders: HashMap<String, HashMap<String, (i32, Instant)>>,
    pub members: Vec<FakeMember>,
    pub require_approval: bool,
    /// The hub's dense per-project frame ordinal (§ "Identifiers and
    /// encodings"): assigned at announce/seed, never reused.
    pub next_frame_seq: i32,
}

impl FakeProject {
    fn member(&self, account_id: &str) -> Option<&FakeMember> {
        self.members.iter().find(|m| m.account_id == account_id)
    }

    /// `version += 1`; returns the new version.
    pub fn bump(&mut self) -> i64 {
        self.version += 1;
        self.version
    }

    /// The next frame ordinal, dense and never reused.
    fn next_seq(&mut self) -> i32 {
        let seq = self.next_frame_seq;
        self.next_frame_seq += 1;
        seq
    }
}

/// Everything the fake hub knows.
pub struct FakeHubState {
    pub projects: HashMap<String, FakeProject>,
    /// device token → the account + device it authenticates.
    pub tokens: HashMap<String, FakeAccount>,
    /// Manifest page size (the hub's is [`MANIFEST_PAGE`]); tests lower it
    /// to exercise `next` paging.
    pub page_size: usize,
    /// Request paths (suffix match) answered with a 500 — a hub fault
    /// injected by a test.
    pub failing: HashSet<String>,
    /// One-shot state changes applied just before the next request whose
    /// path ends with the suffix is answered (a change landing "between" two
    /// app calls).
    pub triggers: Vec<(String, StateChange)>,
}

/// A test's one-shot change to the hub's state.
pub type StateChange = Box<dyn FnOnce(&mut FakeHubState) + Send>;

impl FakeHubState {
    /// device pubkey (base64) → account id.
    fn device_accounts(&self) -> HashMap<String, String> {
        self.tokens
            .values()
            .map(|a| (a.device_pubkey_b64.clone(), a.account_id.clone()))
            .collect()
    }

    fn display_of(&self, account_id: &str) -> String {
        self.tokens
            .values()
            .find(|a| a.account_id == account_id)
            .map(|a| a.display.clone())
            .unwrap_or_else(|| "former member".to_string())
    }

    /// Every device pubkey of an account, sorted (the snapshot's `nodes`).
    fn devices_of(&self, account_id: &str) -> Vec<String> {
        let mut devices: Vec<String> = self
            .tokens
            .values()
            .filter(|a| a.account_id == account_id)
            .map(|a| a.device_pubkey_b64.clone())
            .collect();
        devices.sort();
        devices.dedup();
        devices
    }
}

/// The fake hub: a wiremock server plus the state its one responder serves.
pub struct FakeHub {
    pub server: MockServer,
    pub state: Arc<Mutex<FakeHubState>>,
    key: SigningKey,
}

/// The standard dictionary every new fake project starts with (version 1),
/// so a publish test does not trip the hub's "no dictionary" refusal.
pub fn default_dictionary() -> Vec<DictionaryEntry> {
    let entry = |canonical: &str, aliases: &[&str], kind: &str| DictionaryEntry {
        canonical: canonical.to_string(),
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        kind: kind.to_string(),
    };
    vec![
        entry("L", &["Lum", "Luminance"], "broadband"),
        entry("R", &["Red"], "broadband"),
        entry("G", &["Green"], "broadband"),
        entry("B", &["Blue"], "broadband"),
        entry("Ha", &["H-alpha", "Halpha"], "narrowband"),
        entry("OIII", &["O3"], "narrowband"),
        entry("SII", &["S2"], "narrowband"),
    ]
}

/// A deterministic lowercase-hex digest of `seed`, `len` chars long (the
/// fake's stand-in blake3/xxh3 for frames it seeds itself).
fn hex_of(seed: &str, len: usize) -> String {
    let full = blake3::hash(seed.as_bytes()).to_hex().to_string();
    full[..len].to_string()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl FakeHub {
    /// Start the server and mount the one catch-all responder.
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let state = Arc::new(Mutex::new(FakeHubState {
            projects: HashMap::new(),
            tokens: HashMap::new(),
            page_size: MANIFEST_PAGE,
            failing: HashSet::new(),
            triggers: Vec::new(),
        }));
        let key = SigningKey::from_bytes(&[7u8; 32]);
        Mock::given(any())
            .respond_with(FakeResponder {
                state: Arc::clone(&state),
                key: key.clone(),
            })
            .mount(&server)
            .await;
        FakeHub { server, state, key }
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    /// The snapshot-signing pubkey (base64) — what `/collab/pubkey` serves.
    pub fn pubkey_b64(&self) -> String {
        B64.encode(self.key.verifying_key().to_bytes())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeHubState> {
        self.state.lock().expect("fake hub state poisoned")
    }

    /// Register a device token for an account.
    pub fn add_account(
        &self,
        token: &str,
        account_id: &str,
        display: &str,
        device_pubkey_b64: &str,
        relay_url: Option<&str>,
    ) {
        self.lock().tokens.insert(
            token.to_string(),
            FakeAccount {
                account_id: account_id.to_string(),
                display: display.to_string(),
                device_pubkey_b64: device_pubkey_b64.to_string(),
                relay_url: relay_url.map(str::to_string),
            },
        );
    }

    /// Create an active project at version 1 with the [`default_dictionary`]
    /// (version 1) and no thresholds. `members` = `(account, dataRole,
    /// coordinator)`.
    pub fn add_project(
        &self,
        id: &str,
        slug: &str,
        members: &[(&str, &str, bool)],
        require_approval: bool,
    ) {
        let members = members
            .iter()
            .map(|(account, role, coordinator)| FakeMember {
                account_id: account.to_string(),
                data_role: role.to_string(),
                coordinator: *coordinator,
                gov_caps: Vec::new(),
                trusted: false,
            })
            .collect();
        self.lock().projects.insert(
            id.to_string(),
            FakeProject {
                slug: slug.to_string(),
                title: slug.to_uppercase(),
                status: "active".to_string(),
                version: 1,
                membership_version: 1,
                thresholds_version: 0,
                thresholds_rules: Vec::new(),
                dictionary_version: 1,
                dictionary: default_dictionary(),
                frames: BTreeMap::new(),
                holders: HashMap::new(),
                members,
                require_approval,
                next_frame_seq: 1,
            },
        );
    }

    fn with_project<R>(&self, project_id: &str, f: impl FnOnce(&mut FakeProject) -> R) -> R {
        let mut st = self.lock();
        let p = st
            .projects
            .get_mut(project_id)
            .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
        f(p)
    }

    /// `version += 1` (any change a device must see).
    pub fn bump(&self, project_id: &str) {
        self.with_project(project_id, |p| {
            p.bump();
        });
    }

    /// Lower (or restore) the manifest page size.
    pub fn set_page_size(&self, n: usize) {
        self.lock().page_size = n.clamp(1, MANIFEST_PAGE);
    }

    /// Answer every request whose path ends with `suffix` with a 500
    /// (`on = true`), or stop doing so.
    pub fn set_failing(&self, suffix: &str, on: bool) {
        let mut st = self.lock();
        if on {
            st.failing.insert(suffix.to_string());
        } else {
            st.failing.remove(suffix);
        }
    }

    /// Apply `change` just before the next request whose path ends with
    /// `suffix` is answered (once).
    pub fn before_next(
        &self,
        suffix: &str,
        change: impl FnOnce(&mut FakeHubState) + Send + 'static,
    ) {
        self.lock()
            .triggers
            .push((suffix.to_string(), Box::new(change)));
    }

    /// Add (or re-add) a member. Bumps both versions, as the hub does.
    pub fn add_member(
        &self,
        project_id: &str,
        account_id: &str,
        data_role: &str,
        coordinator: bool,
    ) {
        self.with_project(project_id, |p| {
            p.members.retain(|m| m.account_id != account_id);
            p.members.push(FakeMember {
                account_id: account_id.to_string(),
                data_role: data_role.to_string(),
                coordinator,
                gov_caps: Vec::new(),
                trusted: false,
            });
            p.membership_version += 1;
            p.bump();
        });
    }

    /// Replace a member's governance caps. Bumps, as the hub does.
    pub fn set_caps(&self, project_id: &str, account_id: &str, caps: &[&str]) {
        self.with_project(project_id, |p| {
            let m = p
                .members
                .iter_mut()
                .find(|m| m.account_id == account_id)
                .unwrap_or_else(|| panic!("fake hub: {account_id} is not a member"));
            m.gov_caps = caps.iter().map(|c| c.to_string()).collect();
            p.bump();
        });
    }

    /// Remove a member (they leave or are removed). Bumps both versions.
    pub fn remove_member(&self, project_id: &str, account_id: &str) {
        self.with_project(project_id, |p| {
            p.members.retain(|m| m.account_id != account_id);
            p.membership_version += 1;
            p.bump();
        });
    }

    /// Publish a new thresholds version (with no rules). Bumps.
    pub fn set_thresholds_version(&self, project_id: &str, version: i32) {
        self.with_project(project_id, |p| {
            p.thresholds_version = version;
            p.bump();
        });
    }

    /// Publish a new dictionary version. Bumps.
    pub fn set_dictionary(&self, project_id: &str, version: i32, entries: Vec<DictionaryEntry>) {
        self.with_project(project_id, |p| {
            p.dictionary_version = version;
            p.dictionary = entries;
            p.bump();
        });
    }

    /// Insert frames straight into the hub as `publisher_account` in one
    /// batch (one bump), bypassing the announce rules — how a test stands in
    /// for another member's app. Every device of the publisher holds them at
    /// content version 1. Defaults: `<uuid>.fits`, filter `L`, mono, 300 s,
    /// 1000 bytes, hashes derived from the uuid, the current gate version.
    pub fn seed_frames(
        &self,
        project_id: &str,
        publisher_account: &str,
        uuids: &[&str],
        state: &str,
    ) {
        let mut st = self.lock();
        let display = st.display_of(publisher_account);
        let devices = st.devices_of(publisher_account);
        let p = st
            .projects
            .get_mut(project_id)
            .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
        let version = p.bump();
        for uuid in uuids {
            let seq = p.next_seq();
            let view = FrameViewWire {
                frame_uuid: uuid.to_string(),
                frame_seq: seq,
                publisher_account_id: publisher_account.to_string(),
                publisher_display_name: display.clone(),
                own: false,
                file_name: format!("{uuid}.fits"),
                content_version: 1,
                blake3: hex_of(uuid, 64),
                byte_size: 1000,
                xxh3: hex_of(&format!("x{uuid}"), 16),
                filter_raw: "L".to_string(),
                filter_canonical: "L".to_string(),
                channel: "mono".to_string(),
                exptime_sec: 300.0,
                date_obs: None,
                meta: json!({}),
                gate_version: p.thresholds_version,
                accepted: true,
                accepted_reason: None,
                state: state.to_string(),
                reject_reason: None,
                manifest_version: version,
                created_at: now_rfc3339(),
            };
            p.frames.insert(uuid.to_string(), view);
            let holds = p.holders.entry(uuid.to_string()).or_default();
            for d in &devices {
                holds.insert(d.clone(), (1, Instant::now()));
            }
        }
    }

    /// Mutate one frame as a manifest edit: bumps, stamps its
    /// `manifestVersion`, then applies `f`.
    pub fn update_frame(&self, project_id: &str, uuid: &str, f: impl FnOnce(&mut FrameViewWire)) {
        self.with_project(project_id, |p| {
            let version = p.bump();
            let frame = p
                .frames
                .get_mut(uuid)
                .unwrap_or_else(|| panic!("fake hub: no frame {uuid}"));
            frame.manifest_version = version;
            f(frame);
        });
    }

    /// Exclude (`false`, with a reason) or restore (`true`) a frame. Bumps.
    pub fn set_accepted(&self, project_id: &str, uuid: &str, accepted: bool, reason: Option<&str>) {
        self.update_frame(project_id, uuid, |f| {
            f.accepted = accepted;
            f.accepted_reason = if accepted {
                None
            } else {
                reason.map(str::to_string)
            };
        });
    }

    /// One frame as the hub stores it (`own = false`).
    pub fn frame(&self, project_id: &str, uuid: &str) -> Option<FrameViewWire> {
        let st = self.lock();
        let p = st.projects.get(project_id)?;
        let f = p.frames.get(uuid)?;
        let mut v = f.clone();
        v.own = false;
        Some(v)
    }

    /// Device pubkeys (base64) that freshly hold the frame's CURRENT content,
    /// sorted.
    pub fn holders_of(&self, project_id: &str, uuid: &str) -> Vec<String> {
        let st = self.lock();
        let dev_acct = st.device_accounts();
        let Some(p) = st.projects.get(project_id) else {
            return Vec::new();
        };
        let Some(f) = p.frames.get(uuid) else {
            return Vec::new();
        };
        let mut out = fresh_holders(p, &dev_acct, f);
        out.sort();
        out
    }
}

/// Fresh holders of `f`'s current content whose device belongs to a member.
fn fresh_holders(
    p: &FakeProject,
    dev_acct: &HashMap<String, String>,
    f: &FrameViewWire,
) -> Vec<String> {
    let Some(holds) = p.holders.get(&f.frame_uuid) else {
        return Vec::new();
    };
    holds
        .iter()
        .filter(|(dev, (ver, at))| {
            *ver == f.content_version
                && at.elapsed() <= HOLDER_FRESH
                && dev_acct
                    .get(*dev)
                    .is_some_and(|acct| p.member(acct).is_some())
        })
        .map(|(dev, _)| dev.clone())
        .collect()
}

// ── The responder ────────────────────────────────────────────────────────────

struct FakeResponder {
    state: Arc<Mutex<FakeHubState>>,
    key: SigningKey,
}

impl Respond for FakeResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let mut st = self.state.lock().expect("fake hub state poisoned");
        route(&mut st, &self.key, req)
    }
}

fn ok(v: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(v)
}

fn empty(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code)
}

fn error(code: u16, msg: impl Into<String>) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({ "error": msg.into() }))
}

fn not_found_project() -> ResponseTemplate {
    error(404, "project not found")
}

fn bearer(req: &Request) -> Option<String> {
    let h = req.headers.get("authorization")?.to_str().ok()?;
    h.strip_prefix("Bearer ").map(str::to_string)
}

fn query(req: &Request, key: &str) -> Option<String> {
    req.url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn route(st: &mut FakeHubState, key: &SigningKey, req: &Request) -> ResponseTemplate {
    let path = req.url.path().to_string();
    let Some(rest) = path.strip_prefix("/api/v1") else {
        return empty(404);
    };
    if let Some(i) = st
        .triggers
        .iter()
        .position(|(suffix, _)| path.ends_with(suffix.as_str()))
    {
        let (_, change) = st.triggers.remove(i);
        change(st);
    }
    if st
        .failing
        .iter()
        .any(|suffix| path.ends_with(suffix.as_str()))
    {
        return error(500, "injected fault");
    }
    let segs: Vec<&str> = rest.trim_matches('/').split('/').collect();
    let method = req.method.as_str().to_string();

    // Public routes and the retired package api.
    match (method.as_str(), segs.as_slice()) {
        ("GET", ["collab", "pubkey"]) => {
            return ok(json!({ "pubkey": B64.encode(key.verifying_key().to_bytes()) }))
        }
        ("GET", ["projects", pid]) => return project_page(st, pid),
        (_, ["announcements", ..])
        | (_, ["projects", _, "announcements", ..])
        | (_, ["projects", _, "have"]) => return error(409, "collab_api_outdated"),
        _ => {}
    }

    let Some(acct) = bearer(req).and_then(|t| st.tokens.get(&t).cloned()) else {
        return empty(401);
    };
    match (method.as_str(), segs.as_slice()) {
        ("GET", ["me", "project-versions"]) => my_project_versions(st, &acct),
        ("GET", ["me", "projects"]) => my_projects(st, &acct),
        ("GET", ["projects", pid, "membership"]) => membership(st, key, &acct, pid),
        ("GET", ["projects", pid, "thresholds"]) => thresholds(st, &acct, pid),
        ("GET", ["projects", pid, "dictionary"]) => dictionary(st, &acct, pid),
        ("GET", ["projects", pid, "manifest"]) => manifest(st, &acct, pid, req),
        ("POST", ["projects", pid, "frames"]) => announce(st, &acct, pid, req),
        ("POST", ["projects", pid, "frames", uuid, "version"]) => {
            new_version(st, &acct, pid, uuid, req)
        }
        ("POST", ["projects", pid, "frames", uuid, "approve"]) => {
            approve(st, &acct, pid, uuid, req)
        }
        ("POST", ["projects", pid, "frames", uuid, "reject"]) => reject(st, &acct, pid, uuid, req),
        ("PUT", ["projects", pid, "holders", "self"]) => put_holders(st, &acct, pid, req),
        ("GET", ["projects", pid, "frames", uuid, "holders"]) => {
            frame_holders(st, &acct, pid, uuid)
        }
        _ => empty(404),
    }
}

fn project_page(st: &FakeHubState, pid: &str) -> ResponseTemplate {
    let Some(p) = st.projects.get(pid) else {
        return not_found_project();
    };
    let members: Vec<Value> = p
        .members
        .iter()
        .map(|m| {
            json!({
                "displayName": st.display_of(&m.account_id),
                "dataRole": m.data_role,
                "coordinator": m.coordinator,
            })
        })
        .collect();
    ok(json!({
        "project": {
            "id": pid,
            "slug": p.slug,
            "title": p.title,
            "status": p.status,
            "requireApproval": p.require_approval,
            "target": {"name": "M31", "raDeg": 10.68, "decDeg": 41.27, "radiusDeg": 1.5},
            "version": p.version,
        },
        "members": members,
    }))
}

fn my_project_versions(st: &FakeHubState, acct: &FakeAccount) -> ResponseTemplate {
    let mut out: Vec<(String, i64)> = st
        .projects
        .iter()
        .filter(|(_, p)| p.member(&acct.account_id).is_some())
        .map(|(id, p)| (id.clone(), p.version))
        .collect();
    out.sort();
    ok(Value::Array(
        out.into_iter()
            .map(|(id, v)| json!({ "projectId": id, "version": v }))
            .collect(),
    ))
}

fn my_projects(st: &FakeHubState, acct: &FakeAccount) -> ResponseTemplate {
    let mut ids: Vec<&String> = st.projects.keys().collect();
    ids.sort();
    let mut out = Vec::new();
    for id in ids {
        let p = &st.projects[id];
        let Some(m) = p.member(&acct.account_id) else {
            continue;
        };
        let pending = if m.has_cap("data.moderate") {
            p.frames.values().filter(|f| f.state == "pending").count()
        } else {
            0
        };
        out.push(json!({
            "id": id,
            "slug": p.slug,
            "title": p.title,
            "dataRole": m.data_role,
            "coordinator": m.coordinator,
            "requireApproval": p.require_approval,
            "pendingFrames": pending,
            "pendingAnnouncements": pending,
            "govCaps": m.gov_caps,
        }));
    }
    ok(Value::Array(out))
}

/// The project and the caller's membership, or the hub's refusal (404 for an
/// unknown project, a body-less 403 for a non-member).
fn member_of<'a>(
    st: &'a FakeHubState,
    acct: &FakeAccount,
    pid: &str,
) -> Result<(&'a FakeProject, FakeMember), ResponseTemplate> {
    let p = st.projects.get(pid).ok_or_else(not_found_project)?;
    let m = p
        .member(&acct.account_id)
        .cloned()
        .ok_or_else(|| empty(403))?;
    Ok((p, m))
}

fn membership(
    st: &FakeHubState,
    key: &SigningKey,
    acct: &FakeAccount,
    pid: &str,
) -> ResponseTemplate {
    let (p, _) = match member_of(st, acct, pid) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let members: Vec<Value> = p
        .members
        .iter()
        .map(|m| {
            json!({
                "accountId": m.account_id,
                "displayName": st.display_of(&m.account_id),
                "dataRole": m.data_role,
                "coordinator": m.coordinator,
                "nodes": st.devices_of(&m.account_id),
            })
        })
        .collect();
    let payload = json!({
        "schema": 1,
        "projectId": pid,
        "membershipVersion": p.membership_version,
        "requireApproval": p.require_approval,
        "issuedAt": now_rfc3339(),
        "members": members,
    });
    let bytes = serde_json::to_vec(&payload).expect("snapshot payload serializes");
    ok(json!({
        "payload": B64.encode(&bytes),
        "signature": B64.encode(key.sign(&bytes).to_bytes()),
        "pubkey": B64.encode(key.verifying_key().to_bytes()),
    }))
}

fn thresholds(st: &FakeHubState, acct: &FakeAccount, pid: &str) -> ResponseTemplate {
    let (p, _) = match member_of(st, acct, pid) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let current = (p.thresholds_version > 0).then(|| {
        json!({
            "version": p.thresholds_version,
            "rules": p.thresholds_rules,
            "createdAt": now_rfc3339(),
        })
    });
    ok(json!({ "current": current, "history": [] }))
}

fn dictionary(st: &FakeHubState, acct: &FakeAccount, pid: &str) -> ResponseTemplate {
    let (p, _) = match member_of(st, acct, pid) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let current = (p.dictionary_version > 0).then(|| {
        json!({
            "version": p.dictionary_version,
            "entries": p.dictionary,
            "createdAt": now_rfc3339(),
        })
    });
    ok(json!({ "current": current, "history": [] }))
}

/// One row as `viewer` sees it: `own` filled in.
fn view_for(f: &FrameViewWire, viewer: &str) -> FrameViewWire {
    let mut v = f.clone();
    v.own = f.publisher_account_id == viewer;
    v
}

fn visible(f: &FrameViewWire, viewer: &str, moderator: bool) -> bool {
    f.state == "published" || f.publisher_account_id == viewer || moderator
}

fn manifest(st: &FakeHubState, acct: &FakeAccount, pid: &str, req: &Request) -> ResponseTemplate {
    let (p, m) = match member_of(st, acct, pid) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let moderator = m.has_cap("data.moderate");
    let since: i64 = query(req, "since")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let after = query(req, "after");
    let limit = query(req, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(MANIFEST_PAGE)
        .clamp(1, MANIFEST_PAGE)
        .min(st.page_size);

    let mut rows: Vec<&FrameViewWire> = p
        .frames
        .values()
        .filter(|f| visible(f, &acct.account_id, moderator))
        .filter(|f| match &after {
            Some(after) => (f.manifest_version, f.frame_uuid.as_str()) > (since, after.as_str()),
            None => f.manifest_version > since,
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.manifest_version, &a.frame_uuid).cmp(&(b.manifest_version, &b.frame_uuid))
    });
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next = if has_more {
        rows.last()
            .map(|r| json!({ "since": r.manifest_version, "after": r.frame_uuid }))
    } else {
        None
    };
    let rows: Vec<FrameViewWire> = rows
        .into_iter()
        .map(|f| view_for(f, &acct.account_id))
        .collect();
    ok(json!({
        "projectVersion": p.version,
        "rows": rows,
        "hasMore": has_more,
        "next": next,
    }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameIn {
    frame_uuid: String,
    file_name: String,
    blake3: String,
    byte_size: i64,
    xxh3: String,
    filter_raw: String,
    filter_canonical: String,
    channel: String,
    exptime_sec: f64,
    date_obs: Option<String>,
    gate_version: i32,
    meta: Value,
}

#[derive(serde::Deserialize)]
struct AnnounceBody {
    frames: Vec<FrameIn>,
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The hub's `fileName` rule.
fn file_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.trim() == name
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '\0') || c.is_control())
}

fn validate_frame(f: &FrameIn) -> Result<(), String> {
    if !file_name_ok(&f.file_name) {
        return Err(format!("{}: invalid fileName", f.file_name));
    }
    if !is_lower_hex(&f.blake3, 64) {
        return Err(format!(
            "{}: blake3 must be 64 lowercase hex chars",
            f.file_name
        ));
    }
    if !is_lower_hex(&f.xxh3, 16) {
        return Err(format!(
            "{}: xxh3 must be 16 lowercase hex chars",
            f.file_name
        ));
    }
    if f.byte_size <= 0 {
        return Err(format!("{}: byteSize must be > 0", f.file_name));
    }
    if !matches!(
        f.channel.as_str(),
        "mono" | "osc" | "osc-r" | "osc-g" | "osc-b"
    ) {
        return Err(format!("{}: unknown channel {:?}", f.file_name, f.channel));
    }
    if !(f.exptime_sec > 0.0 && f.exptime_sec <= 86400.0) {
        return Err(format!("{}: exptimeSec must be in (0, 86400]", f.file_name));
    }
    if f.filter_raw.trim().is_empty() || f.filter_raw.trim().len() > 80 {
        return Err(format!("{}: filterRaw must be 1..=80 chars", f.file_name));
    }
    let meta_len = serde_json::to_vec(&f.meta)
        .map(|b| b.len())
        .unwrap_or(usize::MAX);
    if !f.meta.is_object() || meta_len > MAX_META_BYTES {
        return Err(format!(
            "{}: meta must be an object of at most {MAX_META_BYTES} bytes",
            f.file_name
        ));
    }
    Ok(())
}

fn announce(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    req: &Request,
) -> ResponseTemplate {
    let display = st.display_of(&acct.account_id);
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    let Some(member) = p.member(&acct.account_id).cloned() else {
        return empty(403);
    };
    let body: AnnounceBody = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad announce body: {e}")),
    };
    if body.frames.is_empty() || body.frames.len() > MAX_BATCH {
        return error(400, format!("frames must contain 1..={MAX_BATCH} items"));
    }
    let mut seen = HashSet::new();
    for f in &body.frames {
        if let Err(msg) = validate_frame(f) {
            return error(400, msg);
        }
        if !seen.insert(f.frame_uuid.clone()) {
            return error(
                400,
                format!("duplicate frameUuid in batch: {}", f.frame_uuid),
            );
        }
    }
    if p.status != "active" {
        return error(409, "project is closed");
    }
    let gate = p.thresholds_version;
    if p.dictionary_version == 0 || p.dictionary.is_empty() {
        return error(409, "project has no filter dictionary");
    }
    for f in &body.frames {
        if f.gate_version != gate {
            return error(
                409,
                format!(
                    "gate version {} is stale, current is {gate}",
                    f.gate_version
                ),
            );
        }
        if !p
            .dictionary
            .iter()
            .any(|d| d.canonical == f.filter_canonical)
        {
            return error(
                400,
                format!(
                    "{}: filter {:?} is not in the project dictionary",
                    f.file_name, f.filter_canonical
                ),
            );
        }
    }
    if let Some(dup) = body
        .frames
        .iter()
        .find(|f| p.frames.contains_key(&f.frame_uuid))
    {
        return error(
            409,
            format!(
                "frame {} already announced; use /version to publish new content",
                dup.frame_uuid
            ),
        );
    }

    let state = if !p.require_approval || member.trusted || member.has_cap("data.moderate") {
        "published"
    } else {
        "pending"
    };
    let version = p.bump();
    let n = body.frames.len();
    for f in body.frames {
        p.holders
            .entry(f.frame_uuid.clone())
            .or_default()
            .insert(acct.device_pubkey_b64.clone(), (1, Instant::now()));
        let seq = p.next_seq();
        p.frames.insert(
            f.frame_uuid.clone(),
            FrameViewWire {
                frame_uuid: f.frame_uuid,
                frame_seq: seq,
                publisher_account_id: acct.account_id.clone(),
                publisher_display_name: display.clone(),
                own: false,
                file_name: f.file_name,
                content_version: 1,
                blake3: f.blake3,
                byte_size: f.byte_size,
                xxh3: f.xxh3,
                filter_raw: f.filter_raw.trim().to_string(),
                filter_canonical: f.filter_canonical,
                channel: f.channel,
                exptime_sec: f.exptime_sec,
                date_obs: f.date_obs,
                meta: f.meta,
                gate_version: f.gate_version,
                accepted: true,
                accepted_reason: None,
                state: state.to_string(),
                reject_reason: None,
                manifest_version: version,
                created_at: now_rfc3339(),
            },
        );
    }
    ok(json!({ "state": state, "projectVersion": version, "announced": n }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewVersionBody {
    blake3: String,
    byte_size: i64,
    xxh3: String,
}

fn new_version(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    uuid: &str,
    req: &Request,
) -> ResponseTemplate {
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    if p.member(&acct.account_id).is_none() {
        return empty(403);
    }
    let body: NewVersionBody = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad version body: {e}")),
    };
    if !is_lower_hex(&body.blake3, 64) {
        return error(400, "blake3 must be 64 lowercase hex chars");
    }
    if !is_lower_hex(&body.xxh3, 16) {
        return error(400, "xxh3 must be 16 lowercase hex chars");
    }
    if body.byte_size <= 0 {
        return error(400, "byteSize must be > 0");
    }
    let Some(publisher) = p.frames.get(uuid).map(|f| f.publisher_account_id.clone()) else {
        return error(404, "frame not found");
    };
    if publisher != acct.account_id {
        return empty(403);
    }
    if p.status != "active" {
        return error(409, "project is closed");
    }
    let version = p.bump();
    let f = p.frames.get_mut(uuid).expect("checked above");
    f.content_version += 1;
    f.blake3 = body.blake3;
    f.byte_size = body.byte_size;
    f.xxh3 = body.xxh3;
    f.manifest_version = version;
    let next = f.content_version;
    // The fresh upload is the only copy that exists right now.
    let holds = p.holders.entry(uuid.to_string()).or_default();
    holds.clear();
    holds.insert(acct.device_pubkey_b64.clone(), (next, Instant::now()));
    ok(json!({ "contentVersion": next, "projectVersion": version }))
}

#[derive(serde::Deserialize)]
struct ApproveBody {
    #[serde(default)]
    trust: bool,
}

fn approve(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    uuid: &str,
    req: &Request,
) -> ResponseTemplate {
    let body: ApproveBody = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad approve body: {e}")),
    };
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    if !p
        .member(&acct.account_id)
        .is_some_and(|m| m.has_cap("data.moderate"))
    {
        return empty(403);
    }
    let Some((publisher, state)) = p
        .frames
        .get(uuid)
        .map(|f| (f.publisher_account_id.clone(), f.state.clone()))
    else {
        return error(404, "frame not found");
    };
    if state != "pending" {
        return error(409, "frame is not pending");
    }
    let version = p.bump();
    let published = if body.trust {
        if let Some(m) = p.members.iter_mut().find(|m| m.account_id == publisher) {
            m.trusted = true;
        }
        let mut n = 0;
        for f in p.frames.values_mut() {
            if f.publisher_account_id == publisher && f.state == "pending" {
                f.state = "published".to_string();
                f.manifest_version = version;
                n += 1;
            }
        }
        n
    } else {
        let f = p.frames.get_mut(uuid).expect("checked above");
        f.state = "published".to_string();
        f.manifest_version = version;
        1
    };
    ok(json!({ "published": published }))
}

#[derive(serde::Deserialize)]
struct RejectBody {
    reason: String,
}

fn reject(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    uuid: &str,
    req: &Request,
) -> ResponseTemplate {
    let body: RejectBody = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad reject body: {e}")),
    };
    let reason = body.reason.trim().to_string();
    if reason.is_empty() || reason.chars().count() > 500 {
        return error(400, "reason must be 1..=500 chars");
    }
    let dev_acct = st.device_accounts();
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    if !p
        .member(&acct.account_id)
        .is_some_and(|m| m.has_cap("data.moderate"))
    {
        return empty(403);
    }
    let Some(f) = p.frames.get(uuid).cloned() else {
        return error(404, "frame not found");
    };
    if f.state != "pending" {
        return error(409, "frame is not pending");
    }
    // R13: a fresh holder outside the publisher and the moderators blocks it.
    let foreign = p
        .holders
        .get(uuid)
        .map(|holds| {
            holds.iter().any(|(dev, (_, at))| {
                at.elapsed() <= HOLDER_FRESH
                    && match dev_acct.get(dev) {
                        Some(a) if *a == f.publisher_account_id => false,
                        Some(a) => !p.member(a).is_some_and(|m| m.has_cap("data.moderate")),
                        None => true,
                    }
            })
        })
        .unwrap_or(false);
    if foreign {
        return error(
            409,
            "frame is already held by other members; exclude it instead",
        );
    }
    let version = p.bump();
    let frame = p.frames.get_mut(uuid).expect("checked above");
    frame.state = "rejected".to_string();
    frame.reject_reason = Some(reason);
    frame.manifest_version = version;
    p.holders.remove(uuid);
    ok(json!({ "state": "rejected" }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeldFrame {
    frame_uuid: String,
    content_version: i32,
}

#[derive(serde::Deserialize)]
struct HoldersBody {
    #[serde(default)]
    full: bool,
    #[serde(default)]
    add: Vec<HeldFrame>,
    #[serde(default)]
    remove: Vec<String>,
}

fn put_holders(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    req: &Request,
) -> ResponseTemplate {
    let body: HoldersBody = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad holders body: {e}")),
    };
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    let Some(member) = p.member(&acct.account_id).cloned() else {
        return empty(403);
    };
    if body.add.len() > 10_000 || body.remove.len() > 10_000 {
        return error(400, "add/remove must each contain at most 10000 items");
    }
    if body.full && !body.remove.is_empty() {
        return error(400, "remove must be empty when full is true");
    }
    let mut seen = HashSet::new();
    for f in &body.add {
        if !seen.insert(f.frame_uuid.clone()) {
            return error(
                400,
                format!("duplicate frameUuid in batch: {}", f.frame_uuid),
            );
        }
    }
    if let Some(both) = body
        .add
        .iter()
        .find(|f| body.remove.contains(&f.frame_uuid))
    {
        return error(
            400,
            format!(
                "frameUuid {} present in both add and remove",
                both.frame_uuid
            ),
        );
    }
    let moderator = member.has_cap("data.moderate");
    let any_frame = member.data_role == "send_receive" || moderator;
    let device = acct.device_pubkey_b64.clone();
    // Per-row permission filter; a row that fails is silently dropped.
    for a in &body.add {
        let Some(f) = p.frames.get(&a.frame_uuid) else {
            continue;
        };
        let mine = f.publisher_account_id == acct.account_id;
        let state_ok = f.state == "published" || (f.state == "pending" && (moderator || mine));
        if a.content_version == f.content_version && state_ok && (any_frame || mine) {
            p.holders
                .entry(a.frame_uuid.clone())
                .or_default()
                .insert(device.clone(), (a.content_version, Instant::now()));
        }
    }
    if body.full {
        let keep: HashSet<&str> = body.add.iter().map(|a| a.frame_uuid.as_str()).collect();
        for (uuid, holds) in p.holders.iter_mut() {
            if !keep.contains(uuid.as_str()) {
                holds.remove(&device);
            }
        }
    } else {
        for uuid in &body.remove {
            if let Some(holds) = p.holders.get_mut(uuid) {
                holds.remove(&device);
            }
        }
    }
    empty(204)
}

fn frame_holders(st: &FakeHubState, acct: &FakeAccount, pid: &str, uuid: &str) -> ResponseTemplate {
    let dev_acct = st.device_accounts();
    let (p, m) = match member_of(st, acct, pid) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Some(f) = p.frames.get(uuid) else {
        return error(404, "frame not found");
    };
    if !visible(f, &acct.account_id, m.has_cap("data.moderate")) {
        return error(404, "frame not found");
    }
    let mut devices = fresh_holders(p, &dev_acct, f);
    devices.sort();
    let out: Vec<Value> = devices
        .into_iter()
        .map(|dev| {
            let a = st.tokens.values().find(|a| a.device_pubkey_b64 == dev);
            json!({
                "pubkey": dev,
                "displayName": a.map(|a| a.display.clone()).unwrap_or_default(),
                "lastSeenAt": Value::Null,
                "relayUrl": a.and_then(|a| a.relay_url.clone()),
            })
        })
        .collect();
    ok(Value::Array(out))
}
