//! A stateful, in-process fake of the hub's collab v3 live-exchange api (plan
//! P16, P32, § Hub contract), for tests only.
//!
//! ONE `FakeHub` can serve up to three app contexts at once: each context
//! signs in with its own device token, and the fake resolves that token to an
//! account and a device. State lives in one `Arc<Mutex<FakeHubState>>`,
//! reachable two ways: a wiremock catch-all responder answers every REST call
//! exactly as the hub does, and a small axum front sits in front of it,
//! terminating the SSE event stream and the presence beat itself (proxying
//! everything else straight through to wiremock). Tests can both drive the
//! hub through its HTTP routes (as the app does) and reach into its state
//! directly (as a portal admin or another member would).
//!
//! The responders implement the hub rules the app depends on (hub `main`
//! 6127951, `routes/frames.rs` / `routes/holders.rs`, amended for the v3
//! live-exchange wire contract):
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
//! - a claim report is filtered per row: `send` members may hold only their
//!   own frames, `send_receive` members and moderators any published frame,
//!   moderators also pending ones;
//! - every manifest-visible change bumps the project version and stamps the
//!   touched rows' `manifestVersion` with it, and every visible claim change
//!   bumps the project's `holderSeq` once per commit;
//! - the manifest is ordered by `(manifestVersion, frameUuid)` and paged with
//!   `next = {since, after}` (page size overridable for tests);
//! - a frame's `frameSeq` is a dense per-project ordinal, assigned at
//!   announce/seed, never reused.
//!
//! The membership snapshot is signed with a fixed test keypair exactly as the
//! hub signs it, so the app's TOFU pin + `verify_and_parse` run for real.
//!
//! Simplifications versus the real hub (each is deliberate, not a gap this
//! task forgot): no coalescing windows on the event feed (one event per
//! commit, always a contiguous `prev`); no flap damping on presence; no 30 s
//! warm-up (`hello.projects[*].presence` is exact from the first event, never
//! `[]`); the 60 s `versions` vector is only ever sent on demand via
//! [`FakeHub::send_versions`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
use crate::collab::live::digest::{uuid_for_digest, ClaimDigest};

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

/// One account-device registry row (`GET /devices` / `POST
/// /devices/{id}/revoke`, T7) — `id` is the map key in
/// [`FakeHubState::devices`], distinct from `pubkey` (the marker/claim
/// identity `FakeAccount` carries).
#[derive(Debug, Clone)]
pub struct FakeDeviceRow {
    pub account_id: String,
    pub pubkey: String,
    pub name: String,
    pub created_at: String,
    pub last_seen_at: Option<String>,
    pub retired: bool,
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

/// One stored claim row: the hub's per-`(device, frame)` state.
#[derive(Debug, Clone, Copy)]
pub struct FakeClaim {
    pub content_version: i32,
    pub report_seq: i64,
    pub removed: bool,
    /// `holderSeq` at which this claim's visible state (`content_version`,
    /// `removed`) last changed — what `holders?since=` filters on.
    pub changed_seq: i64,
}

/// Timings the presence ticker uses. Defaults match the hub's; tests shorten
/// them so a smoke doesn't wait 40 real seconds.
#[derive(Debug, Clone, Copy)]
pub struct FakeTimings {
    pub keepalive: Duration,
    pub grace: Duration,
    pub silence: Duration,
}

impl Default for FakeTimings {
    fn default() -> Self {
        FakeTimings {
            keepalive: Duration::from_secs(20),
            grace: Duration::from_secs(10),
            silence: Duration::from_secs(40),
        }
    }
}

/// One connected device's presence session, opened by the event stream and
/// updated by the presence beat.
pub struct FakeSession {
    pub device: String,
    pub account_id: String,
    /// project id → serving flag, as last reported by a presence beat.
    pub serving: BTreeMap<String, bool>,
    pub relay_url: Option<String>,
    pub last_beat: Instant,
    /// Set once this session's SSE stream ends; `None` while it is live. The
    /// presence ticker expires a session `timings.grace` after this is set,
    /// or `timings.silence` after `last_beat` while it stays `None`.
    pub detached_at: Option<Instant>,
    pub kill: tokio::sync::watch::Sender<bool>,
}

/// One event-feed message, dispatched to every open stream the target
/// (project or account) reaches.
#[derive(Clone, Debug)]
pub struct FeedMsg {
    pub project_id: Option<String>,
    pub account_id: Option<String>,
    pub name: &'static str,
    pub data: String,
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
    /// Keyed by `(device pubkey, frame uuid)` — the hub's per-frame claim
    /// model (replaces the old 75-minute holder freshness map).
    pub claims: BTreeMap<(String, String), FakeClaim>,
    /// Bumped once per commit that visibly changes any claim.
    pub holder_seq: i64,
    /// Never advanced by this fake (no route retires old deltas); kept so
    /// `holders?since=` can enforce the floor check.
    pub holder_floor: i64,
    pub members: Vec<FakeMember>,
    pub require_approval: bool,
    /// The hub's dense per-project frame ordinal: assigned at announce/seed,
    /// never reused.
    pub next_frame_seq: i32,
    /// The highest `reportSeq` ever stored per device, tracked independently
    /// of individual claim rows so an implicit claim (announce/version) can
    /// be stamped correctly even after a report that touched no claim of its
    /// own (an empty digest-check report still advances this).
    report_hwm: HashMap<String, i64>,
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

    /// The device's highest stored `reportSeq` in this project (0 if none) —
    /// what an implicit claim (announce/version) is stamped with.
    fn highest_report_seq(&self, device: &str) -> i64 {
        self.report_hwm.get(device).copied().unwrap_or(0)
    }

    /// Raise the device's stored high-water mark to at least `report_seq`.
    fn raise_report_hwm(&mut self, device: &str, report_seq: i64) {
        let e = self.report_hwm.entry(device.to_string()).or_insert(0);
        if report_seq > *e {
            *e = report_seq;
        }
    }

    /// Write one CLIENT-REPORTED claim under the report_seq ordering guard.
    /// Returns `None` when the write was blocked (a stale/duplicate report,
    /// nothing written — the caller must NOT count this row toward the
    /// device's report-seq high-water mark), or `Some(changed)` when it went
    /// through, `changed` true iff the claim's visible state
    /// (content_version, removed) is now different.
    fn write_claim(
        &mut self,
        device: &str,
        uuid: &str,
        cv: i32,
        removed: bool,
        report_seq: i64,
    ) -> Option<bool> {
        let key = (device.to_string(), uuid.to_string());
        if let Some(c) = self.claims.get(&key) {
            if c.report_seq > report_seq || (c.report_seq == report_seq && report_seq != 0) {
                return None;
            }
        }
        let changed = self.claims.get(&key).map_or(!removed, |c| {
            c.content_version != cv || c.removed != removed
        });
        let changed_seq = if changed {
            self.holder_seq + 1
        } else {
            self.claims.get(&key).map_or(0, |c| c.changed_seq)
        };
        self.claims.insert(
            key,
            FakeClaim {
                content_version: cv,
                report_seq,
                removed,
                changed_seq,
            },
        );
        Some(changed)
    }

    /// Write an implicit, HUB-GENERATED claim (announce, a version bump,
    /// `seed_frames` standing in for another member's app): ALWAYS applies,
    /// never blocked by the report_seq ordering guard — that guard exists
    /// only for client-submitted reports (real hub `plan_hub_claims`,
    /// `claims/plan.rs`). Stamped with the device's own highest stored
    /// report_seq (read-only: this never advances the high-water mark
    /// itself). Returns true iff the claim's visible state changed.
    fn write_hub_claim(&mut self, device: &str, uuid: &str, cv: i32) -> bool {
        let report_seq = self.highest_report_seq(device);
        let key = (device.to_string(), uuid.to_string());
        let changed = self
            .claims
            .get(&key)
            .map_or(true, |c| c.content_version != cv || c.removed);
        let changed_seq = if changed {
            self.holder_seq + 1
        } else {
            self.claims.get(&key).map_or(0, |c| c.changed_seq)
        };
        self.claims.insert(
            key,
            FakeClaim {
                content_version: cv,
                report_seq,
                removed: false,
                changed_seq,
            },
        );
        changed
    }

    /// The order-independent digest of `device`'s current non-removed claims.
    fn digest_of(&self, device: &str) -> ClaimDigest {
        let mut d = ClaimDigest::default();
        for ((dev, uuid), c) in &self.claims {
            if dev == device && !c.removed {
                d.add(&uuid_for_digest(uuid), c.content_version as u32);
            }
        }
        d
    }
}

/// Everything the fake hub knows.
pub struct FakeHubState {
    pub projects: HashMap<String, FakeProject>,
    /// device token → the account + device it authenticates.
    pub tokens: HashMap<String, FakeAccount>,
    /// hub device id → the account's device registry row (`GET /devices` /
    /// `POST /devices/{id}/revoke`, T7). A SEPARATE index from `tokens`: a
    /// device stays listed (and revocable/retirable) after it signs out or
    /// its token expires, exactly like a real hub's device registry.
    pub devices: HashMap<String, FakeDeviceRow>,
    /// account id → display name, kept apart from `tokens` (T4 ruling): a
    /// revoke removes only the token, never the account's display name, so a
    /// member who still has other devices — or is simply not the one just
    /// revoked — never renders as "former member".
    pub account_displays: HashMap<String, String>,
    /// Manifest (and holders-delta) page size (the hub's is
    /// [`MANIFEST_PAGE`]); tests lower it to exercise `next` paging.
    pub page_size: usize,
    /// Request paths (suffix match) answered with a 500 — a hub fault
    /// injected by a test.
    pub failing: HashSet<String>,
    /// One-shot state changes applied just before the next request whose
    /// path ends with the suffix is answered (a change landing "between" two
    /// app calls).
    pub triggers: Vec<(String, StateChange)>,
    /// Opaque; a restore rotates it (`FakeHub::rotate_epoch`).
    pub epoch: String,
    pub timings: FakeTimings,
    /// sessionId → the connected device's presence session.
    pub sessions: HashMap<String, FakeSession>,
    /// The event feed every open SSE stream subscribes to.
    pub feed: tokio::sync::broadcast::Sender<FeedMsg>,
    /// Claim rows whose visible state changed, summed across every
    /// `PUT holders/self` call (test assertion only).
    pub holder_writes: u64,
    /// project id → number of upcoming events to swallow instead of
    /// publishing (a gap the client must detect via `prev`).
    pub dropped_events: HashMap<String, usize>,
    /// When set, every v3 route (including the event stream) answers 409
    /// `collab_api_outdated`, except the two public routes.
    pub api_outdated: bool,
    session_seq: u64,
    /// Every event published, oldest first, capped at 256 — test-only,
    /// `FakeHub::last_event` reads from here so a test can inspect the exact
    /// payload the fake just sent without re-deriving it (T5 Step 5).
    event_log: VecDeque<FeedMsg>,
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
        self.account_displays
            .get(account_id)
            .cloned()
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

    /// Publish one event to a project's subscribers, unless a test has told
    /// the fake to drop it (a gap: the next `n` events of that project are
    /// swallowed instead of delivered).
    fn publish(&mut self, project_id: &str, name: &'static str, data: Value) {
        if let Some(n) = self.dropped_events.get_mut(project_id) {
            if *n > 0 {
                *n -= 1;
                return;
            }
        }
        self.record_and_send(FeedMsg {
            project_id: Some(project_id.to_string()),
            account_id: None,
            name,
            data: data.to_string(),
        });
    }

    /// Append to the bounded test-inspection log, then send. The ONE place
    /// that touches `self.feed` — every event, dropped or not, that reaches
    /// a stream goes through here.
    fn record_and_send(&mut self, msg: FeedMsg) {
        if self.event_log.len() >= 256 {
            self.event_log.pop_front();
        }
        self.event_log.push_back(msg.clone());
        let _ = self.feed.send(msg);
    }

    /// A `project` event for one version bump; frames inlined iff ≤ 50 and
    /// all published.
    fn publish_bump(&mut self, pid: &str, prev: i64, kinds: &[&str], frames: &[String]) {
        let Some(p) = self.projects.get(pid) else {
            return;
        };
        let rows: Vec<&FrameViewWire> = frames.iter().filter_map(|u| p.frames.get(u)).collect();
        let inline =
            !rows.is_empty() && rows.len() <= 50 && rows.iter().all(|f| f.state == "published");
        let mut ev = json!({
            "projectId": pid,
            "prev": prev,
            "version": p.version,
            "kinds": kinds,
            "more": !frames.is_empty() && !inline,
        });
        if inline {
            ev["frames"] = Value::Array(
                rows.iter()
                    .map(|f| {
                        let mut v = serde_json::to_value(f).expect("frame view serializes");
                        v.as_object_mut().expect("object").remove("own");
                        v
                    })
                    .collect(),
            );
        }
        self.publish(pid, "project", ev);
    }

    /// A `holders` event: `prev` is the cursor before this commit, `deltas`
    /// the per-device add/rm arrays.
    fn publish_holders(&mut self, pid: &str, prev: i64, deltas: Value) {
        let Some(p) = self.projects.get(pid) else {
            return;
        };
        let seq = p.holder_seq;
        self.publish(
            pid,
            "holders",
            json!({ "projectId": pid, "prev": prev, "seq": seq, "deltas": deltas }),
        );
    }

    /// An `account` event: own-account membership changed. Targeted at the
    /// account, not a project (`projectId: None` on the envelope).
    fn publish_account(&mut self, account_id: &str, kind: &str, project_id: &str) {
        self.record_and_send(FeedMsg {
            project_id: None,
            account_id: Some(account_id.to_string()),
            name: "account",
            data: json!({ "kind": kind, "projectId": project_id }).to_string(),
        });
    }

    /// `hello.projects` for one account's device: exactly its current
    /// projects, each with its cursors, digest and presence list.
    fn hello_projects(&self, account_id: &str, device: &str) -> Value {
        let mut out = serde_json::Map::new();
        for (pid, p) in &self.projects {
            if p.member(account_id).is_none() {
                continue;
            }
            let digest = p.digest_of(device);
            out.insert(
                pid.clone(),
                json!({
                    "version": p.version,
                    "holderSeq": p.holder_seq,
                    "claimCount": digest.count,
                    "claimDigest": digest.hex(),
                    "reportSeq": p.highest_report_seq(device),
                    "presence": self.presence_list(pid, p),
                }),
            );
        }
        Value::Object(out)
    }

    /// Connected devices of a project's members, own included, with their
    /// current serving flag and relay url.
    fn presence_list(&self, pid: &str, p: &FakeProject) -> Vec<Value> {
        self.sessions
            .values()
            .filter(|s| p.member(&s.account_id).is_some())
            .map(|s| {
                json!({
                    "device": s.device,
                    "serving": s.serving.get(pid).copied().unwrap_or(false),
                    "relayUrl": s.relay_url,
                })
            })
            .collect()
    }

    /// Open (or replace) the calling device's session: ends an older session
    /// of the same device (its kill fires so that stream closes), registers
    /// the new one, and builds the `hello` payload. Returns the serialized
    /// `hello` JSON, a feed subscription, this session's kill switch, the
    /// keepalive interval, the account id and the new session id.
    ///
    /// T4 fake fidelity ruling: a replacing stream of a STILL-VISIBLE device
    /// (one whose old session is found here — no offline event has been
    /// published for it, since that only happens via the presence ticker's
    /// expiry or a revoke, both of which remove the session themselves)
    /// carries the old session's `serving`/`relay_url` forward and publishes
    /// NO presence note (hub `Presence::open` treats it as the same online
    /// device, not a fresh connect). A genuinely new device still gets the
    /// `connected: true` broadcast.
    fn open_session(
        &mut self,
        acct: &FakeAccount,
    ) -> (
        String,
        tokio::sync::broadcast::Receiver<FeedMsg>,
        tokio::sync::watch::Receiver<bool>,
        Duration,
        String,
        String,
    ) {
        let existing = self
            .sessions
            .iter()
            .find(|(_, s)| s.device == acct.device_pubkey_b64)
            .map(|(id, s)| (id.clone(), s.serving.clone(), s.relay_url.clone()));
        if let Some((old_id, _, _)) = &existing {
            if let Some(old) = self.sessions.remove(old_id) {
                let _ = old.kill.send(true);
            }
        }
        self.session_seq += 1;
        let session_id = hex_of(
            &format!("sess-{}-{}", acct.device_pubkey_b64, self.session_seq),
            32,
        );
        let (kill_tx, kill_rx) = tokio::sync::watch::channel(false);
        let feed_rx = self.feed.subscribe();
        let (serving, relay_url) = match &existing {
            Some((_, serving, relay_url)) => (serving.clone(), relay_url.clone()),
            None => (BTreeMap::new(), acct.relay_url.clone()),
        };
        self.sessions.insert(
            session_id.clone(),
            FakeSession {
                device: acct.device_pubkey_b64.clone(),
                account_id: acct.account_id.clone(),
                serving,
                relay_url: relay_url.clone(),
                last_beat: Instant::now(),
                detached_at: None,
                kill: kill_tx,
            },
        );
        let projects = self.hello_projects(&acct.account_id, &acct.device_pubkey_b64);
        let hello = json!({
            "sessionId": session_id,
            "epoch": self.epoch,
            "accountId": acct.account_id,
            "projects": projects,
        })
        .to_string();
        if existing.is_none() {
            // Presence::open (hub) — the device is now online: tell every
            // other stream on this account's projects at once.
            let pids: Vec<String> = self
                .projects
                .iter()
                .filter(|(_, p)| p.member(&acct.account_id).is_some())
                .map(|(pid, _)| pid.clone())
                .collect();
            for pid in &pids {
                self.publish(
                    pid,
                    "presence",
                    json!({
                        "projectId": pid,
                        "replace": false,
                        "changes": [{ "device": acct.device_pubkey_b64, "connected": true, "serving": false, "relayUrl": relay_url }],
                    }),
                );
            }
        }
        (
            hello,
            feed_rx,
            kill_rx,
            self.timings.keepalive,
            acct.account_id.clone(),
            session_id,
        )
    }
}

/// The fake hub: a wiremock server + an axum front, both driven by the same
/// state.
pub struct FakeHub {
    pub server: MockServer,
    pub state: Arc<Mutex<FakeHubState>>,
    key: SigningKey,
    front_url: String,
    front_task: tokio::task::JoinHandle<()>,
    ticker_task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeHub {
    fn drop(&mut self) {
        self.front_task.abort();
        self.ticker_task.abort();
    }
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
/// fake's stand-in blake3/xxh3 for frames it seeds itself, and its session
/// id generator).
fn hex_of(seed: &str, len: usize) -> String {
    let full = blake3::hash(seed.as_bytes()).to_hex().to_string();
    full[..len].to_string()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl FakeHub {
    /// Start the wiremock server, mount the one catch-all responder, and
    /// start the axum front (event stream + presence, everything else
    /// proxied) plus the presence ticker.
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let (feed_tx, _) = tokio::sync::broadcast::channel(1024);
        let state = Arc::new(Mutex::new(FakeHubState {
            projects: HashMap::new(),
            tokens: HashMap::new(),
            devices: HashMap::new(),
            account_displays: HashMap::new(),
            page_size: MANIFEST_PAGE,
            failing: HashSet::new(),
            triggers: Vec::new(),
            epoch: "epoch-1".to_string(),
            timings: FakeTimings::default(),
            sessions: HashMap::new(),
            feed: feed_tx,
            holder_writes: 0,
            dropped_events: HashMap::new(),
            api_outdated: false,
            session_seq: 0,
            event_log: VecDeque::new(),
        }));
        let key = SigningKey::from_bytes(&[7u8; 32]);
        Mock::given(any())
            .respond_with(FakeResponder {
                state: Arc::clone(&state),
                key: key.clone(),
            })
            .mount(&server)
            .await;
        let front = axum::Router::new()
            .route("/api/v1/me/events", axum::routing::get(front_events))
            .route(
                "/api/v1/me/presence",
                axum::routing::post(front_beat).delete(front_leave),
            )
            .fallback(front_proxy)
            .with_state(FrontState {
                state: Arc::clone(&state),
                upstream: server.uri(),
                http: reqwest::Client::new(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake hub front binds");
        let front_url = format!("http://{}", listener.local_addr().expect("front addr"));
        let front_task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, front).await {
                tracing::error!(error = %e, "fake hub front stopped");
            }
        });
        let ticker_state = Arc::clone(&state);
        let ticker_task = tokio::spawn(presence_ticker(ticker_state));
        FakeHub {
            server,
            state,
            key,
            front_url,
            front_task,
            ticker_task,
        }
    }

    /// The axum front's URL — events and presence are answered locally,
    /// everything else is proxied to the wiremock upstream.
    pub fn uri(&self) -> String {
        self.front_url.clone()
    }

    /// The snapshot-signing pubkey (base64) — what `/collab/pubkey` serves.
    pub fn pubkey_b64(&self) -> String {
        B64.encode(self.key.verifying_key().to_bytes())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeHubState> {
        self.state.lock().expect("fake hub state poisoned")
    }

    /// Build the `hello` payload for `token`'s device exactly as
    /// `open_session` would compose it — same cursors, digest and presence
    /// list — without opening a session (test helper: a test crafts a
    /// `LiveEvent::Hello` by hand, e.g. to override `epoch` for an epoch-
    /// change scenario, rather than driving a real SSE connection).
    pub fn hello_for(&self, token: &str) -> Value {
        let st = self.lock();
        let acct = st
            .tokens
            .get(token)
            .unwrap_or_else(|| panic!("fake hub: unknown token {token}"));
        let projects = st.hello_projects(&acct.account_id, &acct.device_pubkey_b64);
        json!({
            "sessionId": hex_of(&format!("hello-for-{token}"), 32),
            "epoch": st.epoch,
            "accountId": acct.account_id,
            "projects": projects,
        })
    }

    /// The last event of `name` published for `project_id`, if any (test
    /// helper, fed by the bounded 256-entry log every `publish` writes to).
    pub fn last_event(&self, name: &str, project_id: &str) -> Option<Value> {
        let st = self.lock();
        st.event_log
            .iter()
            .rev()
            .find(|m| m.name == name && m.project_id.as_deref() == Some(project_id))
            .map(|m| serde_json::from_str(&m.data).expect("fake hub event data is valid json"))
    }

    /// The project's current `version` counter, as the hub sees it (test
    /// helper: lets a test assert a cursor against the hub's REAL state
    /// instead of a value derived from the test's own arithmetic).
    pub fn version(&self, project_id: &str) -> i64 {
        self.lock()
            .projects
            .get(project_id)
            .map(|p| p.version)
            .unwrap_or(0)
    }

    /// The project's current holder cursor (`holderSeq`), as the hub sees it
    /// (test helper, like [`Self::version`]).
    pub fn holder_seq(&self, project_id: &str) -> i64 {
        self.lock()
            .projects
            .get(project_id)
            .map(|p| p.holder_seq)
            .unwrap_or(0)
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
        let mut st = self.lock();
        st.tokens.insert(
            token.to_string(),
            FakeAccount {
                account_id: account_id.to_string(),
                display: display.to_string(),
                device_pubkey_b64: device_pubkey_b64.to_string(),
                relay_url: relay_url.map(str::to_string),
            },
        );
        st.account_displays
            .insert(account_id.to_string(), display.to_string());
    }

    /// Register a device in the account's device list (`GET /devices`), T7:
    /// `id` is the hub device id (what `revoke`/replace take), `pubkey_b64`
    /// its marker/claim identity. Independent of [`add_account`] — a device
    /// can be listed, offline, with no live token.
    pub fn add_device(
        &self,
        account_id: &str,
        pubkey_b64: &str,
        id: &str,
        name: &str,
        last_seen: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        self.lock().devices.insert(
            id.to_string(),
            FakeDeviceRow {
                account_id: account_id.to_string(),
                pubkey: pubkey_b64.to_string(),
                name: name.to_string(),
                created_at: now_rfc3339(),
                last_seen_at: last_seen.map(|t| t.to_rfc3339()),
                retired: false,
            },
        );
    }

    /// Whether `id` (a hub device id) has been revoked with `retire: true`.
    pub fn device_retired(&self, id: &str) -> bool {
        self.lock()
            .devices
            .get(id)
            .map(|d| d.retired)
            .unwrap_or(false)
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
                claims: BTreeMap::new(),
                holder_seq: 0,
                holder_floor: 0,
                members,
                require_approval,
                next_frame_seq: 1,
                report_hwm: HashMap::new(),
            },
        );
    }

    /// `version += 1` (any change a device must see), publishing a `meta`
    /// bump.
    pub fn bump(&self, project_id: &str) {
        let mut st = self.lock();
        let Some(p) = st.projects.get_mut(project_id) else {
            panic!("fake hub: no project {project_id}");
        };
        let prev = p.version;
        p.bump();
        st.publish_bump(project_id, prev, &["meta"], &[]);
    }

    /// Lower (or restore) the manifest/holders-delta page size.
    pub fn set_page_size(&self, n: usize) {
        self.lock().page_size = n.clamp(1, MANIFEST_PAGE);
    }

    /// Override the presence ticker's timings (defaults match the hub's;
    /// tests shorten them so a smoke doesn't wait 40 real seconds).
    pub fn set_timings(&self, t: FakeTimings) {
        self.lock().timings = t;
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

    /// Add (or re-add) a member. Bumps both versions and publishes a
    /// `members` bump plus an `account` event to the joining account.
    pub fn add_member(
        &self,
        project_id: &str,
        account_id: &str,
        data_role: &str,
        coordinator: bool,
    ) {
        let mut st = self.lock();
        let prev = {
            let p = st
                .projects
                .get_mut(project_id)
                .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
            let prev = p.version;
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
            prev
        };
        st.publish_bump(project_id, prev, &["members"], &[]);
        st.publish_account(account_id, "joined", project_id);
    }

    /// Replace a member's governance caps. Bumps, as the hub does.
    pub fn set_caps(&self, project_id: &str, account_id: &str, caps: &[&str]) {
        let mut st = self.lock();
        let prev = {
            let p = st
                .projects
                .get_mut(project_id)
                .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
            let prev = p.version;
            let m = p
                .members
                .iter_mut()
                .find(|m| m.account_id == account_id)
                .unwrap_or_else(|| panic!("fake hub: {account_id} is not a member"));
            m.gov_caps = caps.iter().map(|c| c.to_string()).collect();
            p.bump();
            prev
        };
        st.publish_bump(project_id, prev, &["members"], &[]);
    }

    /// Remove a member (they leave or are removed). Bumps both versions and
    /// publishes a `members` bump plus an `account` event to that account.
    pub fn remove_member(&self, project_id: &str, account_id: &str) {
        let mut st = self.lock();
        let prev = {
            let p = st
                .projects
                .get_mut(project_id)
                .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
            let prev = p.version;
            p.members.retain(|m| m.account_id != account_id);
            p.membership_version += 1;
            p.bump();
            prev
        };
        st.publish_bump(project_id, prev, &["members"], &[]);
        st.publish_account(account_id, "left", project_id);
    }

    /// Publish a new thresholds version (with no rules). Bumps.
    pub fn set_thresholds_version(&self, project_id: &str, version: i32) {
        let mut st = self.lock();
        let prev = {
            let p = st
                .projects
                .get_mut(project_id)
                .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
            let prev = p.version;
            p.thresholds_version = version;
            p.bump();
            prev
        };
        st.publish_bump(project_id, prev, &["thresholds"], &[]);
    }

    /// Publish a new dictionary version. Bumps.
    pub fn set_dictionary(&self, project_id: &str, version: i32, entries: Vec<DictionaryEntry>) {
        let mut st = self.lock();
        let prev = {
            let p = st
                .projects
                .get_mut(project_id)
                .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
            let prev = p.version;
            p.dictionary_version = version;
            p.dictionary = entries;
            p.bump();
            prev
        };
        st.publish_bump(project_id, prev, &["dictionary"], &[]);
    }

    /// Insert frames straight into the hub as `publisher_account` in one
    /// batch (one bump), bypassing the announce rules — how a test stands in
    /// for another member's app. Every device of the publisher implicitly
    /// claims them at content version 1. Defaults: `<uuid>.fits`, filter `L`,
    /// mono, 300 s, 1000 bytes, hashes derived from the uuid, the current
    /// gate version.
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
        let (prev_version, prev_holder_seq, touched, holder_adds) = {
            let p = st
                .projects
                .get_mut(project_id)
                .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
            let prev_version = p.version;
            let prev_holder_seq = p.holder_seq;
            let version = p.bump();
            let mut touched = Vec::new();
            let mut holder_adds: Vec<(String, i32, i32)> = Vec::new();
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
                touched.push(uuid.to_string());
                for device in &devices {
                    if p.write_hub_claim(device, uuid, 1) {
                        holder_adds.push((device.clone(), seq, 1));
                    }
                }
            }
            if !holder_adds.is_empty() {
                p.holder_seq = prev_holder_seq + 1;
            }
            (prev_version, prev_holder_seq, touched, holder_adds)
        };
        st.publish_bump(project_id, prev_version, &["frames"], &touched);
        if !holder_adds.is_empty() {
            let mut by_device: BTreeMap<String, Vec<[i32; 2]>> = BTreeMap::new();
            for (d, seq, cv) in holder_adds {
                by_device.entry(d).or_default().push([seq, cv]);
            }
            let deltas: Vec<Value> = by_device
                .into_iter()
                .map(|(d, add)| json!({ "device": d, "add": add, "rm": [] }))
                .collect();
            st.publish_holders(project_id, prev_holder_seq, Value::Array(deltas));
        }
    }

    /// Mutate one frame as a manifest edit: bumps, stamps its
    /// `manifestVersion`, then applies `f`, and publishes a `frames` bump.
    pub fn update_frame(&self, project_id: &str, uuid: &str, f: impl FnOnce(&mut FrameViewWire)) {
        let mut st = self.lock();
        let prev = {
            let p = st
                .projects
                .get_mut(project_id)
                .unwrap_or_else(|| panic!("fake hub: no project {project_id}"));
            let prev = p.version;
            let version = p.bump();
            let frame = p
                .frames
                .get_mut(uuid)
                .unwrap_or_else(|| panic!("fake hub: no frame {uuid}"));
            frame.manifest_version = version;
            f(frame);
            prev
        };
        st.publish_bump(project_id, prev, &["frames"], &[uuid.to_string()]);
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

    /// Device pubkeys (base64) with a live (non-removed) claim on the
    /// frame's CURRENT content version, sorted.
    pub fn holders_of(&self, project_id: &str, uuid: &str) -> Vec<String> {
        let st = self.lock();
        let Some(p) = st.projects.get(project_id) else {
            return Vec::new();
        };
        let Some(f) = p.frames.get(uuid) else {
            return Vec::new();
        };
        let mut out: Vec<String> = p
            .claims
            .iter()
            .filter(|((_, u), c)| u == uuid && !c.removed && c.content_version == f.content_version)
            .map(|((d, _), _)| d.clone())
            .collect();
        out.sort();
        out
    }

    /// The stored claim row for `(device, uuid)`, if any.
    pub fn claim_of(&self, project_id: &str, device: &str, uuid: &str) -> Option<FakeClaim> {
        let st = self.lock();
        st.projects
            .get(project_id)?
            .claims
            .get(&(device.to_string(), uuid.to_string()))
            .copied()
    }

    /// Claim rows whose visible `(content_version, removed)` changed, summed
    /// across every `PUT holders/self` call so far.
    pub fn holder_writes(&self) -> u64 {
        self.lock().holder_writes
    }

    /// Devices currently connected (an open event stream) in that project.
    pub fn connected(&self, project_id: &str) -> Vec<String> {
        let st = self.lock();
        let Some(p) = st.projects.get(project_id) else {
            return Vec::new();
        };
        let mut out: Vec<String> = st
            .sessions
            .values()
            .filter(|s| p.member(&s.account_id).is_some())
            .map(|s| s.device.clone())
            .collect();
        out.sort();
        out
    }

    /// Whether `device`'s session currently reports it is serving that
    /// project (`false` if not connected, or the project is absent from its
    /// last beat).
    pub fn serving(&self, project_id: &str, device: &str) -> bool {
        let st = self.lock();
        st.sessions
            .values()
            .find(|s| s.device == device)
            .map(|s| s.serving.get(project_id).copied().unwrap_or(false))
            .unwrap_or(false)
    }

    /// The next `n` events of that project are swallowed instead of
    /// delivered — a gap the client must detect via `prev`.
    pub fn drop_next_events(&self, project_id: &str, n: usize) {
        self.lock().dropped_events.insert(project_id.to_string(), n);
    }

    /// Send a `resync` event telling every stream of that project to catch
    /// up `what` (`"project"` | `"holders"`) over REST.
    pub fn send_resync(&self, project_id: &str, what: &str) {
        self.lock().publish(
            project_id,
            "resync",
            json!({ "projectId": project_id, "what": what }),
        );
    }

    /// Send the 60 s `versions` state vector once, on demand.
    pub fn send_versions(&self) {
        let mut st = self.lock();
        // One message per connected account, listing only THAT account's own
        // projects — never another account's, even one it shares no project
        // with.
        let accounts: HashSet<String> =
            st.sessions.values().map(|s| s.account_id.clone()).collect();
        for account_id in accounts {
            let mut map = serde_json::Map::new();
            for (pid, p) in &st.projects {
                if p.member(&account_id).is_some() {
                    map.insert(pid.clone(), json!([p.version, p.holder_seq]));
                }
            }
            st.record_and_send(FeedMsg {
                project_id: None,
                account_id: Some(account_id),
                name: "versions",
                data: Value::Object(map).to_string(),
            });
        }
    }

    /// A hub restart: every stream closes, sessions and presence clear.
    pub fn kill_streams(&self) {
        let mut st = self.lock();
        for (_, s) in st.sessions.drain() {
            let _ = s.kill.send(true);
        }
    }

    /// A restore: rotate to a new epoch, returning it.
    pub fn rotate_epoch(&self) -> String {
        let mut st = self.lock();
        let n: u64 = st
            .epoch
            .strip_prefix("epoch-")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        st.epoch = format!("epoch-{}", n + 1);
        st.epoch.clone()
    }

    /// A restore that lost these rows entirely: drop the frame and every
    /// claim on it.
    pub fn forget_frames(&self, project_id: &str, uuids: &[&str]) {
        let mut st = self.lock();
        let Some(p) = st.projects.get_mut(project_id) else {
            return;
        };
        for u in uuids {
            p.frames.remove(*u);
            let keys: Vec<(String, String)> =
                p.claims.keys().filter(|(_, uu)| uu == u).cloned().collect();
            for k in keys {
                p.claims.remove(&k);
            }
        }
    }

    /// I11: tombstone the device's claims everywhere it is a member, bump
    /// `members` on every affected project, and close its stream(s).
    pub fn revoke_device(&self, device_pubkey_b64: &str, retire: bool) {
        revoke_device_state(&mut self.lock(), device_pubkey_b64, retire);
    }

    /// Every v3 route (except `collab/pubkey` and the public project page)
    /// answers 409 `collab_api_outdated` while `on`.
    pub fn set_api_outdated(&self, on: bool) {
        self.lock().api_outdated = on;
    }
}

/// The guts of [`FakeHub::revoke_device`] — tombstone the device's claims
/// everywhere it is a member, bump `members` on every affected project, and
/// close its stream(s). A free function (not a `FakeHub` method) so the
/// `POST /devices/{id}/revoke` route handler, which already holds
/// `&mut FakeHubState` from [`route`], can call it without re-locking.
fn revoke_device_state(st: &mut FakeHubState, device_pubkey_b64: &str, retire: bool) {
    let _ = retire; // the fake treats revoke and retire identically.
    let account = st.device_accounts().get(device_pubkey_b64).cloned();
    // The device's token(s) die at once: the next authenticated call
    // gets 401, and it drops out of every `devices_of` (membership
    // `nodes`, `holders/snapshot`'s `devices`, future implicit claims).
    st.tokens
        .retain(|_, a| a.device_pubkey_b64 != device_pubkey_b64);
    let Some(account) = account else {
        return;
    };
    let pids: Vec<String> = st
        .projects
        .iter()
        .filter(|(_, p)| p.member(&account).is_some())
        .map(|(id, _)| id.clone())
        .collect();
    for pid in &pids {
        let (prev_version, prev_holder_seq, rm) = {
            let p = st.projects.get_mut(pid).expect("checked above");
            let touched: Vec<i32> = p
                .claims
                .iter()
                .filter(|((d, _), c)| d == device_pubkey_b64 && !c.removed)
                .filter_map(|((_, u), _)| p.frames.get(u).map(|f| f.frame_seq))
                .collect();
            let uuids: Vec<String> = p
                .claims
                .keys()
                .filter(|(d, _)| d == device_pubkey_b64)
                .map(|(_, u)| u.clone())
                .collect();
            let prev_holder_seq = p.holder_seq;
            if !touched.is_empty() {
                let seq = prev_holder_seq + 1;
                for u in &uuids {
                    if let Some(c) = p
                        .claims
                        .get_mut(&(device_pubkey_b64.to_string(), u.clone()))
                    {
                        if !c.removed {
                            c.removed = true;
                            c.changed_seq = seq;
                        }
                    }
                }
                p.holder_seq = seq;
            }
            let prev_version = p.version;
            p.membership_version += 1;
            p.bump();
            (prev_version, prev_holder_seq, touched)
        };
        st.publish_bump(pid, prev_version, &["members"], &[]);
        if !rm.is_empty() {
            st.publish_holders(
                pid,
                prev_holder_seq,
                json!([{ "device": device_pubkey_b64, "add": [], "rm": rm }]),
            );
        }
    }
    let dead_ids: Vec<String> = st
        .sessions
        .iter()
        .filter(|(_, s)| s.device == device_pubkey_b64)
        .map(|(id, _)| id.clone())
        .collect();
    for id in dead_ids {
        let Some(s) = st.sessions.remove(&id) else {
            continue;
        };
        let _ = s.kill.send(true);
        let pids: Vec<String> = st
            .projects
            .iter()
            .filter(|(_, p)| p.member(&s.account_id).is_some())
            .map(|(pid, _)| pid.clone())
            .collect();
        for pid in pids {
            st.publish(
                &pid,
                "presence",
                json!({
                    "projectId": pid,
                    "replace": false,
                    "changes": [{ "device": s.device, "connected": false, "serving": false, "relayUrl": s.relay_url }],
                }),
            );
        }
    }
}

/// Expires detached sessions after `timings.grace` and silent (no beat)
/// sessions after `timings.silence`, publishing a `presence` `connected:
/// false` for each.
async fn presence_ticker(state: Arc<Mutex<FakeHubState>>) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tick.tick().await;
        let mut st = state.lock().expect("fake hub state poisoned");
        let (grace, silence) = (st.timings.grace, st.timings.silence);
        let now = Instant::now();
        let expired: Vec<String> = st
            .sessions
            .iter()
            .filter(|(_, s)| match s.detached_at {
                Some(at) => now.duration_since(at) >= grace,
                None => now.duration_since(s.last_beat) >= silence,
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            let Some(s) = st.sessions.remove(&id) else {
                continue;
            };
            let _ = s.kill.send(true);
            let pids: Vec<String> = st
                .projects
                .iter()
                .filter(|(_, p)| p.member(&s.account_id).is_some())
                .map(|(pid, _)| pid.clone())
                .collect();
            for pid in pids {
                st.publish(
                    &pid,
                    "presence",
                    json!({
                        "projectId": pid,
                        "replace": false,
                        "changes": [{ "device": s.device, "connected": false, "serving": false, "relayUrl": s.relay_url }],
                    }),
                );
            }
        }
    }
}

// ── The axum front (events, presence; everything else proxied) ─────────────

#[derive(Clone)]
struct FrontState {
    state: Arc<Mutex<FakeHubState>>,
    upstream: String,
    http: reqwest::Client,
}

async fn front_proxy(
    axum::extract::State(fx): axum::extract::State<FrontState>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let path_q = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "fake hub proxy: request body read failed");
            return axum::response::Response::builder()
                .status(502)
                .body(axum::body::Body::empty())
                .expect("static response");
        }
    };
    let mut up = fx
        .http
        .request(parts.method.clone(), format!("{}{}", fx.upstream, path_q))
        .body(bytes.to_vec());
    for h in ["authorization", "content-type"] {
        if let Some(v) = parts.headers.get(h) {
            up = up.header(h, v.clone());
        }
    }
    match up.send().await {
        Ok(r) => {
            let status = r.status();
            let ct = r.headers().get("content-type").cloned();
            let body = match r.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(error = %e, "fake hub proxy: upstream body read failed");
                    return axum::response::Response::builder()
                        .status(502)
                        .body(axum::body::Body::empty())
                        .expect("static response");
                }
            };
            let mut resp = axum::response::Response::new(axum::body::Body::from(body));
            *resp.status_mut() = status;
            if let Some(ct) = ct {
                resp.headers_mut().insert("content-type", ct);
            }
            resp
        }
        Err(e) => {
            tracing::error!(error = %e, "fake hub proxy failed");
            axum::response::Response::builder()
                .status(502)
                .body(axum::body::Body::empty())
                .expect("static response")
        }
    }
}

async fn front_events(
    axum::extract::State(fx): axum::extract::State<FrontState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let opened = {
        let mut st = fx.state.lock().expect("fake hub state poisoned");
        if st.api_outdated {
            return (
                axum::http::StatusCode::CONFLICT,
                axum::Json(json!({"error": "collab_api_outdated"})),
            )
                .into_response();
        }
        let Some(acct) = token.and_then(|t| st.tokens.get(&t).cloned()) else {
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        };
        // Ends an older session of the same device, marks this one
        // connected, and returns (hello, feed rx, kill rx, keepalive,
        // accountId, sessionId).
        st.open_session(&acct)
    };
    let (hello, mut feed, mut kill, keepalive, account_id, session_id) = opened;
    let state = Arc::clone(&fx.state);
    let (tx, rx) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        let first = format!("retry: 3000\nevent: hello\ndata: {hello}\n\n");
        if tx.send(Ok(first.into())).await.is_err() {
            return;
        }
        let mut tick = tokio::time::interval(keepalive);
        tick.tick().await;
        loop {
            let frame = tokio::select! {
                _ = kill.changed() => break,
                _ = tick.tick() => ":\n\n".to_string(),
                msg = feed.recv() => match msg {
                    Ok(m) => {
                        let deliver = {
                            let st = state.lock().expect("fake hub state poisoned");
                            match (&m.project_id, &m.account_id) {
                                (Some(pid), _) => st.projects.get(pid).is_some_and(|p| p.member(&account_id).is_some()),
                                (None, Some(a)) => a == &account_id,
                                (None, None) => true,
                            }
                        };
                        if !deliver { continue; }
                        format!("event: {}\ndata: {}\n\n", m.name, m.data)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
            };
            if tx.send(Ok(frame.into())).await.is_err() {
                break;
            }
        }
        // The stream closed: start this session's grace clock (a reconnect
        // within `timings.grace` cancels it by opening a fresh session,
        // which replaces this one before the ticker ever sees it).
        let mut st = state.lock().expect("fake hub state poisoned");
        if let Some(s) = st.sessions.get_mut(&session_id) {
            s.detached_at = Some(Instant::now());
        }
    });
    let stream = n0_future::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    axum::response::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .body(axum::body::Body::from_stream(stream))
        .expect("static response")
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BeatIn {
    session_id: String,
    #[serde(default)]
    serving: BTreeMap<String, bool>,
    #[serde(default)]
    relay_url: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaveIn {
    session_id: String,
}

async fn front_beat(
    axum::extract::State(fx): axum::extract::State<FrontState>,
    axum::Json(body): axum::Json<BeatIn>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !is_lower_hex(&body.session_id, 32) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "bad sessionId"})),
        )
            .into_response();
    }
    if let Some(url) = &body.relay_url {
        if !url.starts_with("https://") {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "bad relayUrl"})),
            )
                .into_response();
        }
    }
    let mut st = fx.state.lock().expect("fake hub state poisoned");
    if st.api_outdated {
        return (
            axum::http::StatusCode::CONFLICT,
            axum::Json(json!({"error": "collab_api_outdated"})),
        )
            .into_response();
    }
    let (device, account_id, mut changed_pids, relay_changed) = {
        let Some(session) = st.sessions.get_mut(&body.session_id) else {
            return (
                axum::http::StatusCode::CONFLICT,
                axum::Json(json!({"error": "session_gone"})),
            )
                .into_response();
        };
        session.last_beat = Instant::now();
        session.detached_at = None;
        let relay_changed = session.relay_url != body.relay_url;
        session.relay_url = body.relay_url.clone();
        // The WHOLE serving map is compared (T6 fidelity ruling): a project
        // missing from the new map counts as `false`, so one the device was
        // serving and simply dropped from the beat is a change too.
        let mut changed_pids = HashSet::new();
        for pid in body.serving.keys().chain(session.serving.keys()) {
            let before = session.serving.get(pid).copied().unwrap_or(false);
            let after = body.serving.get(pid).copied().unwrap_or(false);
            if before != after {
                changed_pids.insert(pid.clone());
            }
        }
        session.serving = body.serving.clone();
        (
            session.device.clone(),
            session.account_id.clone(),
            changed_pids,
            relay_changed,
        )
    };
    // The relay changed: every one of the account's projects sees this
    // device's new relay, not just the ones whose `serving` flag moved.
    if relay_changed {
        let member_pids: Vec<String> = st
            .projects
            .iter()
            .filter(|(_, p)| p.member(&account_id).is_some())
            .map(|(pid, _)| pid.clone())
            .collect();
        changed_pids.extend(member_pids);
    }
    for pid in changed_pids {
        if st
            .projects
            .get(&pid)
            .is_some_and(|p| p.member(&account_id).is_some())
        {
            let serving = st
                .sessions
                .get(&body.session_id)
                .map(|s| s.serving.get(&pid).copied().unwrap_or(false))
                .unwrap_or(false);
            st.publish(
                &pid,
                "presence",
                json!({
                    "projectId": pid,
                    "replace": false,
                    "changes": [{ "device": device, "connected": true, "serving": serving, "relayUrl": body.relay_url }],
                }),
            );
        }
    }
    axum::http::StatusCode::NO_CONTENT.into_response()
}

async fn front_leave(
    axum::extract::State(fx): axum::extract::State<FrontState>,
    axum::Json(body): axum::Json<LeaveIn>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut st = fx.state.lock().expect("fake hub state poisoned");
    if st.api_outdated {
        return (
            axum::http::StatusCode::CONFLICT,
            axum::Json(json!({"error": "collab_api_outdated"})),
        )
            .into_response();
    }
    if let Some(session) = st.sessions.remove(&body.session_id) {
        let pids: Vec<String> = st
            .projects
            .iter()
            .filter(|(_, p)| p.member(&session.account_id).is_some())
            .map(|(pid, _)| pid.clone())
            .collect();
        for pid in pids {
            st.publish(
                &pid,
                "presence",
                json!({
                    "projectId": pid,
                    "replace": false,
                    "changes": [{ "device": session.device, "connected": false, "serving": false, "relayUrl": session.relay_url }],
                }),
            );
        }
        let _ = session.kill.send(true);
    }
    axum::http::StatusCode::NO_CONTENT.into_response()
}

// ── The wiremock responder (everything but events/presence) ────────────────

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

fn gone(msg: impl Into<String>, extra: Value) -> ResponseTemplate {
    let mut body = json!({ "error": msg.into() });
    if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
        b.extend(e);
    }
    ResponseTemplate::new(410).set_body_json(body)
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

    // Public routes and every retired route (predates the per-frame api, or
    // predates the v3 live exchange).
    match (method.as_str(), segs.as_slice()) {
        ("GET", ["collab", "pubkey"]) => {
            return ok(json!({ "pubkey": B64.encode(key.verifying_key().to_bytes()) }))
        }
        ("GET", ["projects", pid]) => return project_page(st, pid),
        (_, ["announcements", ..])
        | (_, ["projects", _, "announcements", ..])
        | (_, ["projects", _, "have"])
        | ("GET", ["me", "project-versions"])
        | ("GET", ["projects", _, "frames", _, "holders"]) => {
            return error(409, "collab_api_outdated")
        }
        _ => {}
    }
    if st.api_outdated {
        return error(409, "collab_api_outdated");
    }

    let Some(acct) = bearer(req).and_then(|t| st.tokens.get(&t).cloned()) else {
        return empty(401);
    };
    match (method.as_str(), segs.as_slice()) {
        ("GET", ["devices"]) => list_devices_route(st, &acct),
        ("POST", ["devices", id, "revoke"]) => revoke_device_route(st, &acct, id, req),
        ("GET", ["me", "projects"]) => my_projects(st, &acct),
        ("GET", ["projects", pid, "membership"]) => membership(st, key, &acct, pid),
        ("GET", ["projects", pid, "thresholds"]) => thresholds(st, &acct, pid),
        ("GET", ["projects", pid, "dictionary"]) => dictionary(st, &acct, pid),
        ("GET", ["projects", pid, "manifest"]) => manifest(st, &acct, pid, req),
        ("POST", ["projects", pid, "frames"]) => announce(st, &acct, pid, req),
        ("POST", ["projects", pid, "frames", uuid, "version"]) => {
            new_version(st, &acct, pid, uuid, req)
        }
        ("POST", ["projects", pid, "frames", "versions"]) => {
            frame_versions_batch(st, &acct, pid, req)
        }
        ("POST", ["projects", pid, "frames", uuid, "approve"]) => {
            approve(st, &acct, pid, uuid, req)
        }
        ("POST", ["projects", pid, "frames", uuid, "reject"]) => reject(st, &acct, pid, uuid, req),
        ("GET", ["projects", pid, "holders", "snapshot"]) => holders_snapshot(st, &acct, pid),
        ("GET", ["projects", pid, "holders"]) => holders_since(st, &acct, pid, req),
        ("PUT", ["projects", pid, "holders", "self"]) => report_holders(st, &acct, pid, req),
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

/// `GET /devices` — the calling account's own, non-retired devices (T7).
fn list_devices_route(st: &FakeHubState, acct: &FakeAccount) -> ResponseTemplate {
    let mut out: Vec<(String, Value)> = st
        .devices
        .iter()
        .filter(|(_, d)| d.account_id == acct.account_id && !d.retired)
        .map(|(id, d)| {
            (
                id.clone(),
                json!({
                    "id": id,
                    "name": d.name,
                    "pubkey": d.pubkey,
                    "capability": "athenaeum",
                    "createdAt": d.created_at,
                    "lastSeenAt": d.last_seen_at,
                }),
            )
        })
        .collect();
    out.sort_by(|(a, _), (b, _)| a.cmp(b));
    ok(Value::Array(out.into_iter().map(|(_, v)| v).collect()))
}

/// `POST /devices/{id}/revoke` — optional `{"retire":bool}` body (T7). Scoped
/// to the calling account: a device id of another account 404s, exactly like
/// an unknown id.
fn revoke_device_route(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    id: &str,
    req: &Request,
) -> ResponseTemplate {
    #[derive(serde::Deserialize, Default)]
    struct RevokeBody {
        #[serde(default)]
        retire: bool,
    }
    let retire = if req.body.is_empty() {
        false
    } else {
        req.body_json::<RevokeBody>()
            .map(|b| b.retire)
            .unwrap_or(false)
    };
    let Some(dev) = st.devices.get(id).cloned() else {
        return error(404, "no such device");
    };
    if dev.account_id != acct.account_id {
        return error(404, "no such device");
    }
    if let Some(d) = st.devices.get_mut(id) {
        d.retired = true;
    }
    revoke_device_state(st, &dev.pubkey, retire);
    empty(204)
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
    let device = acct.device_pubkey_b64.clone();
    let prev_version = p.version;
    let prev_holder_seq = p.holder_seq;
    let version = p.bump();
    let n = body.frames.len();
    let mut touched = Vec::new();
    let mut holder_adds: Vec<[i32; 2]> = Vec::new();
    for f in body.frames {
        let seq = p.next_seq();
        touched.push(f.frame_uuid.clone());
        p.frames.insert(
            f.frame_uuid.clone(),
            FrameViewWire {
                frame_uuid: f.frame_uuid.clone(),
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
        if p.write_hub_claim(&device, &f.frame_uuid, 1) {
            holder_adds.push([seq, 1]);
        }
    }
    if !holder_adds.is_empty() {
        p.holder_seq = prev_holder_seq + 1;
    }
    st.publish_bump(pid, prev_version, &["frames"], &touched);
    if !holder_adds.is_empty() {
        st.publish_holders(
            pid,
            prev_holder_seq,
            json!([{ "device": device, "add": holder_adds, "rm": [] }]),
        );
    }
    ok(json!({ "state": state, "projectVersion": version, "announced": n }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewVersionBody {
    #[serde(default)]
    expected_version: Option<i32>,
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
    let Some(expected) = body.expected_version else {
        return error(409, "collab_api_outdated");
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
    let current = p.frames.get(uuid).expect("checked above").content_version;
    if expected != current {
        return ResponseTemplate::new(409)
            .set_body_json(json!({ "error": "version_conflict", "contentVersion": current }));
    }
    let prev_version = p.version;
    let prev_holder_seq = p.holder_seq;
    let version = p.bump();
    let f = p.frames.get_mut(uuid).expect("checked above");
    f.content_version += 1;
    f.blake3 = body.blake3;
    f.byte_size = body.byte_size;
    f.xxh3 = body.xxh3;
    f.manifest_version = version;
    let next = f.content_version;
    let frame_seq = f.frame_seq;
    let device = acct.device_pubkey_b64.clone();
    let changed = p.write_hub_claim(&device, uuid, next);
    if changed {
        p.holder_seq = prev_holder_seq + 1;
    }
    st.publish_bump(pid, prev_version, &["frames"], &[uuid.to_string()]);
    if changed {
        st.publish_holders(
            pid,
            prev_holder_seq,
            json!([{ "device": device, "add": [[frame_seq, next]], "rm": [] }]),
        );
    }
    ok(json!({ "contentVersion": next, "projectVersion": version }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionBatchIn {
    uuid: String,
    expected_version: i32,
    blake3: String,
    byte_size: i64,
    xxh3: String,
}

#[derive(serde::Deserialize)]
struct VersionsBatchBody {
    versions: Vec<VersionBatchIn>,
}

fn frame_versions_batch(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    req: &Request,
) -> ResponseTemplate {
    let body: VersionsBatchBody = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad versions body: {e}")),
    };
    if body.versions.is_empty() || body.versions.len() > 500 {
        return error(400, "versions must contain 1..=500 items");
    }
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    if p.member(&acct.account_id).is_none() {
        return empty(403);
    }
    if p.status != "active" {
        return error(409, "project is closed");
    }
    let device = acct.device_pubkey_b64.clone();
    let prev_version = p.version;
    let prev_holder_seq = p.holder_seq;
    let mut any_ok = false;
    let mut any_holder_change = false;
    let mut results = Vec::new();
    let mut touched = Vec::new();
    let mut holder_adds: Vec<Value> = Vec::new();
    for v in &body.versions {
        let Some(f) = p.frames.get(&v.uuid) else {
            results.push(json!({"uuid": v.uuid, "status": "not_found", "contentVersion": 0}));
            continue;
        };
        if f.publisher_account_id != acct.account_id {
            results.push(
                json!({"uuid": v.uuid, "status": "forbidden", "contentVersion": f.content_version}),
            );
            continue;
        }
        if f.content_version != v.expected_version {
            results.push(
                json!({"uuid": v.uuid, "status": "conflict", "contentVersion": f.content_version}),
            );
            continue;
        }
        let frame = p.frames.get_mut(&v.uuid).expect("checked above");
        frame.content_version += 1;
        frame.blake3 = v.blake3.clone();
        frame.byte_size = v.byte_size;
        frame.xxh3 = v.xxh3.clone();
        let next = frame.content_version;
        let frame_seq = frame.frame_seq;
        any_ok = true;
        touched.push(v.uuid.clone());
        if p.write_hub_claim(&device, &v.uuid, next) {
            any_holder_change = true;
            holder_adds.push(json!([frame_seq, next]));
        }
        results.push(json!({"uuid": v.uuid, "status": "ok", "contentVersion": next}));
    }
    let version = if any_ok {
        let v = p.bump();
        for uuid in &touched {
            if let Some(f) = p.frames.get_mut(uuid) {
                f.manifest_version = v;
            }
        }
        v
    } else {
        p.version
    };
    if any_holder_change {
        p.holder_seq = prev_holder_seq + 1;
    }
    if any_ok {
        st.publish_bump(pid, prev_version, &["frames"], &touched);
    }
    if any_holder_change {
        st.publish_holders(
            pid,
            prev_holder_seq,
            json!([{ "device": device, "add": holder_adds, "rm": [] }]),
        );
    }
    ok(json!({ "projectVersion": version, "results": results }))
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
    let prev_version = p.version;
    let version = p.bump();
    let mut touched = vec![uuid.to_string()];
    let published = if body.trust {
        if let Some(m) = p.members.iter_mut().find(|m| m.account_id == publisher) {
            m.trusted = true;
        }
        let mut n = 0;
        touched.clear();
        for f in p.frames.values_mut() {
            if f.publisher_account_id == publisher && f.state == "pending" {
                f.state = "published".to_string();
                f.manifest_version = version;
                touched.push(f.frame_uuid.clone());
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
    st.publish_bump(pid, prev_version, &["frames"], &touched);
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
    // R13: a live holder outside the publisher and the moderators blocks it.
    let foreign = p.claims.iter().any(|((dev, u), c)| {
        u == uuid
            && !c.removed
            && match dev_acct.get(dev) {
                Some(a) if *a == f.publisher_account_id => false,
                Some(a) => !p.member(a).is_some_and(|m| m.has_cap("data.moderate")),
                None => true,
            }
    });
    if foreign {
        return error(
            409,
            "frame is already held by other members; exclude it instead",
        );
    }
    let prev_version = p.version;
    let version = p.bump();
    let frame = p.frames.get_mut(uuid).expect("checked above");
    frame.state = "rejected".to_string();
    frame.reject_reason = Some(reason);
    frame.manifest_version = version;
    let keys: Vec<(String, String)> = p
        .claims
        .keys()
        .filter(|(_, u)| u == uuid)
        .cloned()
        .collect();
    for k in keys {
        p.claims.remove(&k);
    }
    st.publish_bump(pid, prev_version, &["frames"], &[uuid.to_string()]);
    ok(json!({ "state": "rejected" }))
}

/// One run-length-encoded claim triple `[startSeq, runLength, contentVersion]`
/// (hub § "Run-length claims").
fn run_length_encode(rows: &[(i32, i32)]) -> Vec<[i32; 3]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let (start, cv) = rows[i];
        let mut len: i32 = 1;
        while (i + len as usize) < rows.len()
            && rows[i + len as usize].0 == start + len
            && rows[i + len as usize].1 == cv
        {
            len += 1;
        }
        out.push([start, len, cv]);
        i += len as usize;
    }
    out
}

fn holders_snapshot(st: &FakeHubState, acct: &FakeAccount, pid: &str) -> ResponseTemplate {
    let (p, m) = match member_of(st, acct, pid) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let moderator = m.has_cap("data.moderate");
    let mut frames: Vec<(i32, Value)> = p
        .frames
        .values()
        .filter(|f| visible(f, &acct.account_id, moderator))
        .map(|f| {
            (
                f.frame_seq,
                json!({ "seq": f.frame_seq, "uuid": f.frame_uuid, "contentVersion": f.content_version }),
            )
        })
        .collect();
    frames.sort_by_key(|(seq, _)| *seq);
    let frames: Vec<Value> = frames.into_iter().map(|(_, v)| v).collect();

    let mut devices = Vec::new();
    for member in &p.members {
        for device in st.devices_of(&member.account_id) {
            let mut rows: Vec<(i32, i32)> = p
                .claims
                .iter()
                .filter(|((d, _), c)| d == &device && !c.removed)
                .filter_map(|((_, u), c)| p.frames.get(u).map(|f| (f.frame_seq, c.content_version)))
                .collect();
            rows.sort_by_key(|(seq, _)| *seq);
            let claims = run_length_encode(&rows);
            // The live presence relay while connected, else the stored one.
            let relay_url = st
                .sessions
                .values()
                .find(|s| s.device == device)
                .map(|s| s.relay_url.clone())
                .unwrap_or_else(|| {
                    st.tokens
                        .values()
                        .find(|a| a.device_pubkey_b64 == device)
                        .and_then(|a| a.relay_url.clone())
                });
            devices.push(json!({
                "device": device,
                "displayName": st.display_of(&member.account_id),
                "relayUrl": relay_url,
                "claims": claims,
            }));
        }
    }

    ok(json!({
        "epoch": st.epoch,
        "holderSeq": p.holder_seq,
        "version": p.version,
        "frames": frames,
        "devices": devices,
    }))
}

fn holders_since(
    st: &FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    req: &Request,
) -> ResponseTemplate {
    let (p, _) = match member_of(st, acct, pid) {
        Ok(x) => x,
        Err(r) => return r,
    };
    if let Some(epoch) = query(req, "epoch") {
        if epoch != st.epoch {
            return gone("epoch_changed", json!({ "epoch": st.epoch }));
        }
    }
    let since: i64 = query(req, "since")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if since < p.holder_floor {
        return gone(
            "holders_below_floor",
            json!({ "holderSeq": p.holder_seq, "floor": p.holder_floor }),
        );
    }
    if since > p.holder_seq {
        return gone(
            "holders_cursor_ahead",
            json!({ "holderSeq": p.holder_seq, "floor": p.holder_floor }),
        );
    }
    // The page cursor is the keyset `(changed_seq, device, frameSeq)`,
    // carried in the opaque `after` string (hub plan P26, `holder_deltas`):
    // one commit can stamp many rows with the SAME changed_seq, so the key
    // must break the tie. `since` stays the caller's ORIGINAL cursor on every
    // page (`next.since` echoes it), exactly as the real hub pages.
    let after_key: Option<(i64, String, i32)> = match query(req, "after") {
        None => None,
        Some(raw) => {
            let mut parts = raw.splitn(3, ':');
            let parsed = (|| {
                Some((
                    parts.next()?.parse::<i64>().ok()?,
                    parts.next()?.to_string(),
                    parts.next()?.parse::<i32>().ok()?,
                ))
            })();
            match parsed {
                Some(k) => Some(k),
                None => return error(400, "after is not a cursor this hub issued"),
            }
        }
    };
    let limit = st.page_size.min(MANIFEST_PAGE).max(1);
    let mut rows: Vec<(String, i32, &FakeClaim)> = p
        .claims
        .iter()
        .filter_map(|((device, uuid), c)| {
            p.frames.get(uuid).map(|f| (device.clone(), f.frame_seq, c))
        })
        .filter(|(device, frame_seq, c)| {
            c.changed_seq > since
                && match &after_key {
                    Some((acs, ad, afs)) => {
                        (c.changed_seq, device.as_str(), *frame_seq) > (*acs, ad.as_str(), *afs)
                    }
                    None => true,
                }
        })
        .collect();
    rows.sort_by(|(d1, fs1, c1), (d2, fs2, c2)| {
        (c1.changed_seq, d1, fs1).cmp(&(c2.changed_seq, d2, fs2))
    });
    let has_more = rows.len() > limit;
    let page = &rows[..rows.len().min(limit)];
    let mut by_device: BTreeMap<String, (Vec<[i32; 2]>, Vec<i32>)> = BTreeMap::new();
    for (device, frame_seq, c) in page {
        let entry = by_device.entry(device.clone()).or_default();
        if c.removed {
            entry.1.push(*frame_seq);
        } else {
            entry.0.push([*frame_seq, c.content_version]);
        }
    }
    let deltas: Vec<Value> = by_device
        .into_iter()
        .map(|(device, (add, rm))| json!({ "device": device, "add": add, "rm": rm }))
        .collect();
    let next = if has_more {
        page.last().map(|(device, frame_seq, c)| {
            json!({ "since": since, "after": format!("{}:{device}:{frame_seq}", c.changed_seq) })
        })
    } else {
        None
    };
    ok(json!({
        "epoch": st.epoch,
        "holderSeq": p.holder_seq,
        "floor": p.holder_floor,
        "deltas": deltas,
        "hasMore": has_more,
        "next": next,
    }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportAddIn {
    uuid: String,
    content_version: i32,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportIn {
    #[serde(default)]
    report_seq: Option<i64>,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    add: Vec<ReportAddIn>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    digest: String,
    #[serde(default)]
    count: i64,
}

fn parse_hex16(hex: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    if hex.len() == 32 {
        for (i, o) in out.iter_mut().enumerate() {
            if let Ok(b) = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16) {
                *o = b;
            }
        }
    }
    out
}

fn report_holders(
    st: &mut FakeHubState,
    acct: &FakeAccount,
    pid: &str,
    req: &Request,
) -> ResponseTemplate {
    let body: ReportIn = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad holders report body: {e}")),
    };
    let Some(report_seq) = body.report_seq else {
        return error(409, "collab_api_outdated");
    };
    if report_seq < 1 {
        return error(400, "reportSeq must be >= 1");
    }
    if body.full && !body.remove.is_empty() {
        return error(400, "remove must be empty when full is true");
    }
    if body.add.len() > 10_000 || body.remove.len() > 10_000 {
        return error(400, "add/remove must each contain at most 10000 items");
    }
    let mut seen = HashSet::new();
    for a in &body.add {
        if !seen.insert(a.uuid.clone()) {
            return error(400, format!("duplicate frameUuid in batch: {}", a.uuid));
        }
    }
    if let Some(both) = body.add.iter().find(|a| body.remove.contains(&a.uuid)) {
        return error(
            400,
            format!("frameUuid {} present in both add and remove", both.uuid),
        );
    }
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    let Some(member) = p.member(&acct.account_id).cloned() else {
        return empty(403);
    };

    let moderator = member.has_cap("data.moderate");
    let any_frame = member.data_role == "send_receive" || moderator;
    let device = acct.device_pubkey_b64.clone();
    let mut refused = Vec::new();
    let mut accepted: Vec<(String, i32)> = Vec::new();
    for a in &body.add {
        let Some(f) = p.frames.get(&a.uuid) else {
            refused.push(a.uuid.clone());
            continue;
        };
        let mine = f.publisher_account_id == acct.account_id;
        let holdable =
            mine || (f.state == "published" && any_frame) || (f.state == "pending" && moderator);
        if a.content_version < 1 || a.content_version > f.content_version || !holdable {
            refused.push(a.uuid.clone());
            continue;
        }
        accepted.push((a.uuid.clone(), a.content_version));
    }

    let prev_holder_seq = p.holder_seq;
    let mut changed_any = false;
    let mut changed_rows: u64 = 0;
    // The report-seq high-water mark rises only from rows this report
    // ACTUALLY WRITES (real hub `store.rs`: `max_report_seq = max over
    // written rows`); an empty, all-stale or all-refused report writes
    // nothing and must not advance it.
    let mut any_written = false;
    let mut add_deltas: Vec<[i32; 2]> = Vec::new();
    let mut rm_deltas: Vec<i32> = Vec::new();

    for (uuid, cv) in &accepted {
        if let Some(changed) = p.write_claim(&device, uuid, *cv, false, report_seq) {
            any_written = true;
            if changed {
                changed_any = true;
                changed_rows += 1;
                if let Some(f) = p.frames.get(uuid) {
                    add_deltas.push([f.frame_seq, *cv]);
                }
            }
        }
    }
    // Refused frames: any LIVE claim we already hold is tombstoned.
    for uuid in &refused {
        let cv = match p.claims.get(&(device.clone(), uuid.clone())) {
            Some(c) if !c.removed => c.content_version,
            _ => continue,
        };
        if let Some(changed) = p.write_claim(&device, uuid, cv, true, report_seq) {
            any_written = true;
            if changed {
                changed_any = true;
                changed_rows += 1;
                if let Some(f) = p.frames.get(uuid) {
                    rm_deltas.push(f.frame_seq);
                }
            }
        }
    }
    if body.full {
        let keep: HashSet<&str> = accepted.iter().map(|(u, _)| u.as_str()).collect();
        let to_remove: Vec<(String, i32)> = p
            .claims
            .iter()
            .filter(|((d, u), c)| {
                d == &device
                    && !c.removed
                    && !keep.contains(u.as_str())
                    && c.report_seq < report_seq
            })
            .map(|((_, u), c)| (u.clone(), c.content_version))
            .collect();
        for (uuid, cv) in to_remove {
            if let Some(changed) = p.write_claim(&device, &uuid, cv, true, report_seq) {
                any_written = true;
                if changed {
                    changed_any = true;
                    changed_rows += 1;
                    if let Some(f) = p.frames.get(&uuid) {
                        rm_deltas.push(f.frame_seq);
                    }
                }
            }
        }
    } else {
        for uuid in &body.remove {
            // A remove of a frame the hub has never heard of pins nothing —
            // no phantom cv-0 tombstone row (hub behaviour: a row only
            // exists once the frame does).
            let Some((frame_seq, frame_cv)) =
                p.frames.get(uuid).map(|f| (f.frame_seq, f.content_version))
            else {
                continue;
            };
            let cv = p
                .claims
                .get(&(device.clone(), uuid.clone()))
                .map(|c| c.content_version)
                .unwrap_or(frame_cv);
            if let Some(changed) = p.write_claim(&device, uuid, cv, true, report_seq) {
                any_written = true;
                if changed {
                    changed_any = true;
                    changed_rows += 1;
                    rm_deltas.push(frame_seq);
                }
            }
        }
    }

    if any_written {
        p.raise_report_hwm(&device, report_seq);
    }
    if changed_any {
        p.holder_seq = prev_holder_seq + 1;
    }
    st.holder_writes += changed_rows;

    // The wire contract has the client fold every attempted add (including
    // ones the hub goes on to refuse) into the digest/count it sends
    // (hub `holders.rs`): `expected` starts as that declared digest, then
    // each refused `(uuid, the SENT content_version)` is folded back OUT of
    // it, and the result must equal the hub's own post-report digest for
    // this device.
    let mut expected_digest = ClaimDigest {
        count: body.count,
        xor: parse_hex16(&body.digest),
    };
    for uuid in &refused {
        if let Some(a) = body.add.iter().find(|a| &a.uuid == uuid) {
            expected_digest.remove(&uuid_for_digest(uuid), a.content_version as u32);
        }
    }
    let p = st.projects.get(pid).expect("checked above");
    let server_digest = p.digest_of(&device);
    let digest_match = expected_digest == server_digest;
    let holder_seq = p.holder_seq;

    if changed_any {
        let deltas = json!([{ "device": device, "add": add_deltas, "rm": rm_deltas }]);
        st.publish_holders(pid, prev_holder_seq, deltas);
    }

    ok(json!({
        "holderSeq": holder_seq,
        "digestMatch": digest_match,
        "nextFlushMs": 1000,
        "refused": refused,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::hub_client::CollabClient;
    use crate::collab::live::digest::ClaimDigest;
    use crate::collab::live::wire::{BeatWire, ClaimWire, HoldersReportWire};

    async fn hub_with_member() -> FakeHub {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project(
            "p1",
            "m31",
            &[("acc-me", "send_receive", false), ("acc-o", "send", false)],
            false,
        );
        hub.seed_frames("p1", "acc-o", &["u1", "u2"], "published");
        hub
    }

    /// Reads SSE frames from the front with reqwest.
    async fn open(hub: &FakeHub, token: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(format!("{}/api/v1/me/events", hub.uri()))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn next_event(
        resp: &mut reqwest::Response,
        buf: &mut String,
    ) -> (String, serde_json::Value) {
        loop {
            if let Some(end) = buf.find("\n\n") {
                let block: String = buf.drain(..end + 2).collect();
                let name = block
                    .lines()
                    .find_map(|l| l.strip_prefix("event: "))
                    .map(str::to_string);
                let data: String = block
                    .lines()
                    .filter_map(|l| l.strip_prefix("data: "))
                    .collect();
                if let Some(n) = name {
                    return (n, serde_json::from_str(&data).unwrap());
                }
                continue;
            }
            let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// Skips every event until a `presence` one naming `device` in its first
    /// change (the stream may see other devices' presence, or non-presence
    /// events, interleaved first).
    async fn next_presence_for(
        resp: &mut reqwest::Response,
        buf: &mut String,
        device: &str,
    ) -> serde_json::Value {
        loop {
            let (name, ev) = next_event(resp, buf).await;
            if name == "presence" && ev["changes"][0]["device"] == device {
                return ev;
            }
        }
    }

    #[tokio::test]
    async fn hello_carries_cursors_digest_and_presence() {
        let hub = hub_with_member().await;
        let mut resp = open(&hub, "tok").await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        let mut buf = String::new();
        let (name, hello) = next_event(&mut resp, &mut buf).await;
        assert_eq!(name, "hello");
        assert_eq!(hello["accountId"], "acc-me");
        let p = &hello["projects"]["p1"];
        assert_eq!(p["claimDigest"], crate::collab::live::digest::ZERO_HEX);
        assert_eq!(p["holderSeq"], 1); // the seeding publisher's implicit claims
        assert!(p["presence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["device"] == "AAA="));
    }

    #[tokio::test]
    async fn a_report_advances_the_holder_cursor_and_streams_a_contiguous_delta() {
        let hub = hub_with_member().await;
        let mut resp = open(&hub, "tok").await;
        let mut buf = String::new();
        let (_, hello) = next_event(&mut resp, &mut buf).await;
        let seq0 = hello["projects"]["p1"]["holderSeq"].as_i64().unwrap();
        let c = CollabClient::new(hub.uri()).unwrap();
        let digest = ClaimDigest::of_claims([("u1", 1)]).unwrap();
        let reply = c
            .report_holders(
                "tok",
                "p1",
                &HoldersReportWire {
                    report_seq: 1,
                    full: false,
                    add: vec![ClaimWire {
                        uuid: "u1".into(),
                        content_version: 1,
                    }],
                    remove: vec![],
                    digest: digest.hex(),
                    count: 1,
                },
            )
            .await
            .unwrap();
        assert!(reply.digest_match);
        assert_eq!(reply.holder_seq, seq0 + 1);
        let (name, ev) = loop {
            let e = next_event(&mut resp, &mut buf).await;
            if e.0 == "holders" {
                break e;
            }
        };
        assert_eq!(name, "holders");
        assert_eq!(
            (ev["prev"].as_i64().unwrap(), ev["seq"].as_i64().unwrap()),
            (seq0, seq0 + 1)
        );
        assert_eq!(ev["deltas"][0]["device"], "AAA=");
        assert_eq!(hub.holders_of("p1", "u1").len(), 2); // publisher + me
    }

    #[tokio::test]
    async fn an_older_report_seq_never_overrides_a_newer_one() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        let body = |seq: i64, add: bool| HoldersReportWire {
            report_seq: seq,
            full: false,
            add: if add {
                vec![ClaimWire {
                    uuid: "u1".into(),
                    content_version: 1,
                }]
            } else {
                vec![]
            },
            remove: if add { vec![] } else { vec!["u1".into()] },
            digest: crate::collab::live::digest::ZERO_HEX.into(),
            count: 0,
        };
        c.report_holders("tok", "p1", &body(5, false))
            .await
            .unwrap();
        c.report_holders("tok", "p1", &body(4, true)).await.unwrap(); // delayed older add
        assert!(hub.claim_of("p1", "AAA=", "u1").map_or(true, |c| c.removed));
    }

    #[tokio::test]
    async fn refused_claims_and_retired_routes() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        // The client folds every attempted add into the digest/count it
        // sends, refused ones included; the hub's digestMatch removes each
        // refused key (at the content version the client SENT) from that
        // declared digest before comparing to its own ground truth.
        let digest = ClaimDigest::of_claims([("nope", 1), ("u2", 9)]).unwrap();
        let reply = c
            .report_holders(
                "tok",
                "p1",
                &HoldersReportWire {
                    report_seq: 1,
                    full: false,
                    add: vec![
                        ClaimWire {
                            uuid: "nope".into(),
                            content_version: 1,
                        },
                        ClaimWire {
                            uuid: "u2".into(),
                            content_version: 9,
                        },
                    ],
                    remove: vec![],
                    digest: digest.hex(),
                    count: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(reply.refused.len(), 2);
        assert!(reply.digest_match); // both refused keys removed leaves the empty set, matching the hub

        // Negative case: a digest that does NOT already include the refused
        // entries (e.g. a client that never folded them in) stays mismatched
        // once the hub subtracts them anyway.
        let reply2 = c
            .report_holders(
                "tok",
                "p1",
                &HoldersReportWire {
                    report_seq: 2,
                    full: false,
                    add: vec![ClaimWire {
                        uuid: "nope".into(),
                        content_version: 1,
                    }],
                    remove: vec![],
                    digest: crate::collab::live::digest::ZERO_HEX.into(),
                    count: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(reply2.refused.len(), 1);
        assert!(!reply2.digest_match);

        let r = reqwest::Client::new()
            .get(format!("{}/api/v1/me/project-versions", hub.uri()))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 409);
    }

    #[tokio::test]
    async fn versions_compare_and_set_and_revocation_tombstones() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        let conflict = c
            .new_frame_version("tok-o", "p1", "u1", 7, &"c".repeat(64), 10, &"0".repeat(16))
            .await;
        assert!(matches!(
            conflict,
            Err(crate::account::AccountClientError::VersionConflict { content_version: 1 })
        ));
        let ok = c
            .new_frame_version("tok-o", "p1", "u1", 1, &"c".repeat(64), 10, &"0".repeat(16))
            .await
            .unwrap();
        assert_eq!(ok.content_version, 2);
        assert_eq!(hub.holders_of("p1", "u1"), vec!["BBB=".to_string()]); // implicit claim at v2
        hub.revoke_device("BBB=", true);
        assert!(hub.holders_of("p1", "u1").is_empty());
    }

    #[tokio::test]
    async fn snapshot_and_since_page_current_rows() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        let snap = c.holders_snapshot("tok", "p1").await.unwrap();
        assert_eq!(snap.frames.len(), 2);
        let publisher = snap.devices.iter().find(|d| d.device == "BBB=").unwrap();
        assert_eq!(publisher.claims, vec![[1, 2, 1]]);
        let page = c.holders_since("tok", "p1", 0, None, None).await.unwrap();
        assert_eq!(page.holder_seq, snap.holder_seq);
        let gone = c.holders_since("tok", "p1", 99, None, None).await;
        assert!(
            matches!(gone, Err(crate::account::AccountClientError::Gone(ref e)) if e == "holders_cursor_ahead")
        );
    }

    #[tokio::test]
    async fn holders_since_pages_through_one_commits_many_rows() {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project(
            "p1",
            "m31",
            &[("acc-me", "send_receive", false), ("acc-o", "send", false)],
            false,
        );
        // One commit: five claim rows for BBB=, all stamped with the SAME
        // changed_seq. A `since`-only cursor would silently drop rows once
        // it landed mid-group; `after` must carry the rest of the group.
        hub.seed_frames("p1", "acc-o", &["f1", "f2", "f3", "f4", "f5"], "published");
        hub.set_page_size(2);
        let c = CollabClient::new(hub.uri()).unwrap();
        let mut seen: HashSet<i32> = HashSet::new();
        let mut since = 0i64;
        let mut after: Option<String> = None;
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(pages <= 10, "paging did not converge");
            let page = c
                .holders_since("tok", "p1", since, after.as_deref(), None)
                .await
                .unwrap();
            for d in &page.deltas {
                for (seq, _cv) in &d.add {
                    seen.insert(*seq);
                }
            }
            if !page.has_more {
                break;
            }
            let next = page.next.expect("hasMore implies next");
            since = next.since;
            after = Some(next.after);
        }
        assert_eq!(seen.len(), 5, "every row of the one commit, across pages");
        assert!(
            pages >= 3,
            "a page size of 2 over 5 rows takes at least 3 pages"
        );
    }

    /// T6 fidelity ruling: `next` echoes the ORIGINAL `since` and carries the
    /// keyset `changed_seq:device:frameSeq` in `after` (the real hub's
    /// `holder_deltas`); an `after` this hub never issued is a 400.
    #[tokio::test]
    async fn holders_since_next_echoes_since_and_rejects_a_foreign_after() {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project(
            "p1",
            "m31",
            &[("acc-me", "send_receive", false), ("acc-o", "send", false)],
            false,
        );
        hub.seed_frames("p1", "acc-o", &["f1", "f2", "f3"], "published");
        hub.seed_frames("p1", "acc-o", &["f4", "f5"], "published");
        hub.set_page_size(2);
        let c = CollabClient::new(hub.uri()).unwrap();
        let page = c.holders_since("tok", "p1", 0, None, None).await.unwrap();
        assert!(page.has_more);
        let next = page.next.unwrap();
        assert_eq!(next.since, 0, "next.since is the caller's own cursor");
        assert_eq!(next.after, "1:BBB=:2");
        let page2 = c
            .holders_since("tok", "p1", next.since, Some(&next.after), None)
            .await
            .unwrap();
        let seqs: Vec<i32> = page2
            .deltas
            .iter()
            .flat_map(|d| d.add.iter().map(|a| a.0))
            .collect();
        assert_eq!(
            seqs,
            vec![3, 4],
            "the page resumes inside commit 1 and crosses into commit 2"
        );
        let bad = c.holders_since("tok", "p1", 0, Some("BBB=:2"), None).await;
        assert!(
            matches!(
                bad,
                Err(crate::account::AccountClientError::Http { status: 400, .. })
            ),
            "{bad:?}"
        );
    }

    /// T6 fidelity ruling: the beat compares the WHOLE serving map — a
    /// project the device was serving and then left out of a beat counts as
    /// `false`, a change the watcher sees.
    #[tokio::test]
    async fn a_project_dropped_from_the_beat_counts_as_no_longer_serving() {
        let hub = hub_with_member().await;
        let mut watcher = open(&hub, "tok-o").await;
        let mut wbuf = String::new();
        let _ = next_event(&mut watcher, &mut wbuf).await;
        let mut me = open(&hub, "tok").await;
        let mut mbuf = String::new();
        let (_, hello_me) = next_event(&mut me, &mut mbuf).await;
        let session_id = hello_me["sessionId"].as_str().unwrap().to_string();
        let _ = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        let c = CollabClient::new(hub.uri()).unwrap();
        c.presence_beat(&BeatWire {
            session_id: session_id.clone(),
            serving: [("p1".to_string(), true)].into_iter().collect(),
            relay_url: None,
        })
        .await
        .unwrap();
        let ev = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        assert_eq!(ev["changes"][0]["serving"], true);
        c.presence_beat(&BeatWire {
            session_id,
            serving: BTreeMap::new(),
            relay_url: None,
        })
        .await
        .unwrap();
        let ev = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        assert_eq!(ev["changes"][0]["serving"], false);
        assert!(!hub.serving("p1", "AAA="));
    }

    #[tokio::test]
    async fn implicit_claims_bypass_the_report_seq_guard() {
        let hub = FakeHub::start().await;
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project("p1", "m31", &[("acc-o", "send_receive", false)], false);
        hub.seed_frames("p1", "acc-o", &["u1"], "published");
        let c = CollabClient::new(hub.uri()).unwrap();
        // The publisher's own device re-affirms its claim via a full report
        // at a high report_seq — planting that report_seq on the stored row.
        let digest = ClaimDigest::of_claims([("u1", 1)]).unwrap();
        c.report_holders(
            "tok-o",
            "p1",
            &HoldersReportWire {
                report_seq: 100,
                full: true,
                add: vec![ClaimWire {
                    uuid: "u1".into(),
                    content_version: 1,
                }],
                remove: vec![],
                digest: digest.hex(),
                count: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(hub.claim_of("p1", "BBB=", "u1").unwrap().report_seq, 100);

        // A version bump's implicit claim must still take effect even though
        // the stored report_seq (100) already equals the device's own
        // high-water mark — that ordering guard is for client-submitted
        // reports only, never for hub-written implicit claims.
        let ok = c
            .new_frame_version("tok-o", "p1", "u1", 1, &"c".repeat(64), 10, &"0".repeat(16))
            .await
            .unwrap();
        assert_eq!(ok.content_version, 2);
        let claim = hub.claim_of("p1", "BBB=", "u1").unwrap();
        assert_eq!(
            claim.content_version, 2,
            "the implicit claim must track the new version, not be blocked and keep the old cv"
        );
        assert_eq!(hub.holders_of("p1", "u1"), vec!["BBB=".to_string()]);
    }

    #[tokio::test]
    async fn empty_report_does_not_raise_the_report_seq_high_water_mark() {
        let hub = hub_with_member().await;
        let c = CollabClient::new(hub.uri()).unwrap();
        // A pure digest check (empty add/remove) at a very high report_seq
        // writes nothing, so it must not move the device's stored
        // high-water mark.
        c.report_holders(
            "tok",
            "p1",
            &HoldersReportWire {
                report_seq: 500,
                full: false,
                add: vec![],
                remove: vec![],
                digest: crate::collab::live::digest::ZERO_HEX.into(),
                count: 0,
            },
        )
        .await
        .unwrap();
        let mut resp = open(&hub, "tok").await;
        let mut buf = String::new();
        let (_, hello) = next_event(&mut resp, &mut buf).await;
        assert_eq!(hello["projects"]["p1"]["reportSeq"], 0);

        // A genuinely fresh report at seq 1 is still accepted (nothing
        // stale about it — the guard compares per-key, not against a
        // wrongly-inflated high-water mark).
        let digest = ClaimDigest::of_claims([("u1", 1)]).unwrap();
        let reply = c
            .report_holders(
                "tok",
                "p1",
                &HoldersReportWire {
                    report_seq: 1,
                    full: false,
                    add: vec![ClaimWire {
                        uuid: "u1".into(),
                        content_version: 1,
                    }],
                    remove: vec![],
                    digest: digest.hex(),
                    count: 1,
                },
            )
            .await
            .unwrap();
        assert!(reply.digest_match);
        assert_eq!(hub.claim_of("p1", "AAA=", "u1").unwrap().report_seq, 1);
    }

    #[tokio::test]
    async fn presence_broadcasts_on_connect_beat_and_revoke() {
        let hub = hub_with_member().await;
        let mut watcher = open(&hub, "tok-o").await;
        let mut wbuf = String::new();
        let (_, _hello_o) = next_event(&mut watcher, &mut wbuf).await;

        // Connect: the watcher sees AAA= come online.
        let mut me = open(&hub, "tok").await;
        let mut mbuf = String::new();
        let (_, hello_me) = next_event(&mut me, &mut mbuf).await;
        let session_id = hello_me["sessionId"].as_str().unwrap().to_string();
        let ev = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        assert_eq!(ev["changes"][0]["connected"], true);

        // A beat that only changes the relay (serving unchanged) still
        // broadcasts.
        let c = CollabClient::new(hub.uri()).unwrap();
        c.presence_beat(&BeatWire {
            session_id: session_id.clone(),
            serving: BTreeMap::new(),
            relay_url: Some("https://relay.example".into()),
        })
        .await
        .unwrap();
        let ev = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        assert_eq!(ev["changes"][0]["relayUrl"], "https://relay.example");

        // Revoke: the device is told it's gone, drops off the snapshot, and
        // the watcher sees it go offline.
        hub.revoke_device("AAA=", true);
        let ev = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        assert_eq!(ev["changes"][0]["connected"], false);
        let r = reqwest::Client::new()
            .get(format!("{}/api/v1/me/projects", hub.uri()))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let snap = c.holders_snapshot("tok-o", "p1").await.unwrap();
        assert!(!snap.devices.iter().any(|d| d.device == "AAA="));
    }

    /// T4 fake fidelity ruling: a second stream from a device that never went
    /// offline (no revoke, no ticker expiry) is a replace, not a fresh
    /// connect — the watcher sees no new presence note, and the state
    /// (serving, relay) carries over into the new session's own `hello`.
    #[tokio::test]
    async fn a_replacing_stream_of_a_still_visible_device_publishes_no_note() {
        let hub = hub_with_member().await;
        let mut watcher = open(&hub, "tok-o").await;
        let mut wbuf = String::new();
        let (_, _hello_o) = next_event(&mut watcher, &mut wbuf).await;

        let mut me = open(&hub, "tok").await;
        let mut mbuf = String::new();
        let (_, hello_me) = next_event(&mut me, &mut mbuf).await;
        let session_id = hello_me["sessionId"].as_str().unwrap().to_string();
        let ev = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        assert_eq!(ev["changes"][0]["connected"], true);

        let c = CollabClient::new(hub.uri()).unwrap();
        c.presence_beat(&BeatWire {
            session_id,
            serving: [("p1".to_string(), true)].into_iter().collect(),
            relay_url: Some("https://relay.example".into()),
        })
        .await
        .unwrap();
        let ev = next_presence_for(&mut watcher, &mut wbuf, "AAA=").await;
        assert_eq!(ev["changes"][0]["serving"], true);

        // A second stream, same device, no revoke: a replace.
        let mut me2 = open(&hub, "tok").await;
        let mut mbuf2 = String::new();
        let (_, hello_me2) = next_event(&mut me2, &mut mbuf2).await;
        let presence_me2 = hello_me2["projects"]["p1"]["presence"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["device"] == "AAA=")
            .unwrap()
            .clone();
        assert_eq!(presence_me2["serving"], true);
        assert_eq!(presence_me2["relayUrl"], "https://relay.example");

        let no_note = tokio::time::timeout(
            Duration::from_millis(300),
            next_presence_for(&mut watcher, &mut wbuf, "AAA="),
        )
        .await;
        assert!(
            no_note.is_err(),
            "a replacing stream of a still-visible device must publish no presence note"
        );
    }

    /// T4 fake fidelity ruling: revoking an account's only device drops its
    /// token, but the member (and its real display name) stays on the
    /// project — display names live apart from tokens.
    #[tokio::test]
    async fn revoking_the_only_device_keeps_the_members_display_name() {
        let hub = hub_with_member().await;
        hub.revoke_device("AAA=", true);
        let c = CollabClient::new(hub.uri()).unwrap();
        let snap = c.membership_snapshot("tok-o", "p1").await.unwrap();
        let payload = B64.decode(&snap.payload).unwrap();
        let payload: Value = serde_json::from_slice(&payload).unwrap();
        let member = payload["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["accountId"] == "acc-me")
            .unwrap();
        assert_eq!(member["displayName"], "Me");
    }
}
