//! Perseus embedded web status page — router, auth, read + write endpoints.
//!
//! A tiny [`axum`] server, bound to [`Config::web_bind`](crate::config::Config)
//! (loopback by default), that lets an operator inspect and lightly manage a
//! headless capture node from a browser. The page ([`index_html`]) renders four
//! sections — status banner, sent packages, transfer history, and the retention
//! panel (policy editor + recent-pass log) — over these endpoints:
//!
//! - `GET /` — the static, data-free HTML/JS page shell. **Auth-exempt** (see
//!   below) so a browser can load it and then prompt for the token.
//! - `GET /api/status` — capture dirs, live in-flight transfers, the current
//!   retention policy, and coarse package counts ([`StatusDto`]).
//! - `GET /api/sent` — outbound packages, newest first, optionally filtered by
//!   `?state=` ([`SentDto`]).
//! - `GET /api/history` — the transfer audit log, optionally filtered by
//!   `?query=` (filename) and `?direction=` ([`HistoryDto`]).
//! - `GET`/`PUT /api/retention/policy` — read the live retention config +
//!   read-only soak gate ([`PolicyDto`]); a whitelisted [`RetentionEdit`] is
//!   applied to `perseus.toml` and adopted live. Live deletion can never be
//!   enabled here (the edit carries no soak field).
//! - `GET /api/retention/log` — the recent retention-pass ring buffer
//!   ([`RetentionRunRecord`], newest-first).
//! - `GET`/`PUT /api/capture-dirs` — the configured vs. running capture
//!   directories + a `restartPending` flag ([`CaptureDirsDto`]); a PUT rewrites
//!   `perseus.toml`'s capture selection, adopts it into the live config, and
//!   rings the supervisor so it applies the edit live (engine restart only, no
//!   process restart). The watchers keep their spawn-time dirs until that
//!   engine relaunch, which is the window `restartPending` reports.
//! - `POST /api/delete` — delete the source capture files of CONFIRMED packages
//!   through the same confirmed-only deleter retention uses ([`DeleteReport`]).
//!
//! # Auth
//!
//! Bearer-token auth ([`auth_layer`]): when a token is configured, every
//! `/api/*` request must present `Authorization: Bearer <token>`. `GET /` is the
//! sole exemption — the static page shell must load without a token so the
//! browser's JS can then supply one on the `/api/*` calls. The `token` is
//! **snapshotted at spawn** — changing it needs an agent restart, which keeps
//! the middleware trivial (no shared mutable auth state).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Query, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use tokio::sync::{watch, Notify, RwLock};

use athenaeum_core::package::{read_manifest, MANIFEST_FILENAME};
use athenaeum_core::sync::store::StandaloneSyncStore;
use athenaeum_core::sync::{
    node_id_hex, Direction, HistoryQuery, HistoryRow, OutboundRow, OutboundState,
    SharedPackageCleanup, SyncEngineHandle, SyncStore,
};

use crate::batch_store::BatchStore;
use crate::batcher::BatcherHandle;
use crate::config::{Config, Mode, RetentionConfig, SendCfg};
use crate::config_edit::{
    apply_capture_dirs_edit, apply_device_name_edit, apply_retention_edit, apply_send_mode_edit,
    apply_targets_edit, RetentionEdit,
};
use crate::pending::{pending_tree, PendingNode};
use crate::run::{delete_confirmed_packages, DeleteReport};
use crate::seen::SeenStore;
use crate::supervisor::AgentState;

mod account_api;
use account_api::*;

/// Default cap for `GET /api/sent` when the caller supplies no `?limit=`.
const DEFAULT_SENT_LIMIT: u32 = 500;
/// Default cap for `GET /api/history` when the caller supplies no `?limit=`.
const DEFAULT_HISTORY_LIMIT: u32 = 500;
/// Max filenames `GET /api/sent` returns per row (read from the package
/// manifest). The client renders the first 5; a present 6th is the "there is at
/// least one more" signal, shown as a "+ more" marker. Perseus packages are
/// one-file-per-frame today, so the cap is defensive.
const SENT_FILES_CAP: usize = 6;
/// Row window `GET /api/status` tallies its terminal counts over. The status
/// page is a summary, not a lifetime ledger — confirmed rows accrue forever
/// (retention deletes source files, never outbound rows), so counts over the
/// most recent N packages keep the endpoint bounded in time and memory.
const STATUS_SCAN_LIMIT: u32 = 5000;

/// Shared state for the always-on status-page router.
///
/// The page is owned by the [`supervisor`](crate::supervisor), not the agent:
/// it stays up for the whole process lifetime, in setup mode or running. The
/// **engine-dependent** bits (`engine`, `peer_device`, `retention_tx`,
/// `retention_log`, `device_names`, `running_dirs`) are behind their own locks
/// and swapped in by [`attach`](Self::attach) when the engine launches and
/// cleared by [`detach`](Self::detach) when it stops — so a request that arrives
/// while the node is mid-setup sees `engine = None` and degrades honestly
/// (empty in-flight list, `503` on retry) rather than reading a stale handle.
pub struct WebState {
    /// The durable sync store — source of the sent/history/counts reads. Opened
    /// once at supervisor start (a second WAL connection beside the agent's own)
    /// so the page reads even while the engine is detached.
    pub store: Arc<StandaloneSyncStore>,
    /// Perseus's stat-aware seen store (source-file linkage). The manual-delete
    /// endpoint (`POST /api/delete`) resolves a confirmed package back to its
    /// source capture file through this, via the exact same deleter retention
    /// uses ([`delete_confirmed_packages`](crate::run::delete_confirmed_packages)).
    pub seen: Arc<SeenStore>,
    /// Path to `perseus.toml` — the retention / capture-dirs edits write it via
    /// [`config_edit`](crate::config_edit).
    pub config_path: PathBuf,
    /// The live config. The supervisor refreshes it every pass (so DTOs track
    /// on-disk edits); the PUT handlers swap in an edited copy after re-validation.
    pub config: RwLock<Config>,
    /// The supervisor's live lifecycle state — `agentState`/`agentDetail` and the
    /// `restartPending` gate come from here (`GET /api/status`).
    pub agent_state: watch::Receiver<AgentState>,
    /// Prod the supervisor into an immediate config re-read (Task 5's account
    /// page rings this after a sign-in so readiness is picked up at once).
    pub supervisor_wake: Arc<Notify>,
    // ── swapped by attach()/detach() as the engine starts/stops ──────────────
    /// The running engine (its `status_snapshot` is the live in-flight list;
    /// `POST /api/retry` re-enqueues through it). `None` while detached (setup).
    pub engine: RwLock<Option<Arc<SyncEngineHandle>>>,
    /// The shared-payload cleanup coordinator, `Some` only for a ≥2-target
    /// fan-out (the same instance the fanned-out engines were spawned with).
    /// `POST /api/retry` bumps it after a successful re-enqueue so the retried
    /// row's terminal cannot prematurely free a still-offline target's payload.
    /// `None` while detached, and `None` for a single-target agent (no shared
    /// dir → the engine's own in-line cleanup, no coordinator).
    pub cleanup: RwLock<Option<Arc<SharedPackageCleanup>>>,
    /// This node's configured sync peer id (hex) — the same value transfer
    /// history rows carry. Stamped onto the `deleted_manual` audit rows written
    /// by `POST /api/delete` so the history shows the peer a confirmed package
    /// was sent to, not this agent's own node id (the manifest's `origin_device`
    /// is self — the earlier bug). Empty while detached.
    pub peer_device: RwLock<String>,
    /// Retention live-edit channel: the PUT-policy handler pushes an edited
    /// [`RetentionConfig`] here so the running retention loop adopts it without a
    /// restart. A placeholder (no receivers) while detached — sends are no-ops.
    pub retention_tx: RwLock<watch::Sender<RetentionConfig>>,
    /// Rolling record (cap 50, newest-first) of the retention loop's recent
    /// passes, surfaced read-only at `GET /api/retention/log`. An empty buffer
    /// while detached (no retention loop yet).
    pub retention_log: RwLock<Arc<Mutex<VecDeque<RetentionRunRecord>>>>,
    /// Peer node id (hex) → friendly device name, for enriching history rows.
    /// Loaded from the pairing cache on attach.
    pub device_names: RwLock<HashMap<String, String>>,
    /// The capture directories the running engine was launched over. Set on
    /// attach, cleared on detach; `restartPending` is this differing from the
    /// configured set while the engine is running.
    pub running_dirs: RwLock<Vec<PathBuf>>,
    /// The send-target list the running engines were launched over (Sync 2C).
    /// Set on attach, cleared on detach; the targets editor's `restartPending`
    /// is this differing from the configured `targets` while the engine runs.
    pub running_targets: RwLock<Vec<String>>,
    /// The running batcher's control handle (Sync Phase 2). `Some` only while the
    /// engine is attached: `GET /api/pending` reads its pending snapshot for the
    /// "To sync" tree and `POST /api/send-now` triggers a manual flush through it.
    /// Set on attach, cleared on detach — mirror `engine` (a request mid-setup
    /// sees `None` and degrades to an empty tree / a `0`-flush no-op).
    pub batcher: RwLock<Option<BatcherHandle>>,
    /// Durable per-batch send record (`perseus_batch`). Opened once at web start
    /// beside `store`/`seen` (a second WAL connection to the same `perseus.db`),
    /// so `GET /api/batches` lists recorded batches engine-attached or not.
    pub batches: Arc<BatchStore>,
    /// The running batcher's live send-config channel, threaded in from the agent
    /// on attach (a clone of [`Agent::send_cfg_tx`](crate::run::Agent::send_cfg_tx)).
    /// `PUT /api/send-mode` sends the re-validated [`SendCfg`] here so the running
    /// batcher live-applies an Auto↔Manual / quiet-window change with no restart.
    /// A placeholder (no receivers) while detached — sends are harmless no-ops —
    /// and, like `retention_tx`, left as-is on detach rather than cleared.
    pub send_cfg_tx: RwLock<watch::Sender<SendCfg>>,
}

impl WebState {
    /// Build a **detached** (setup-mode) state: store + seen open, engine absent.
    /// The supervisor upgrades it to running via [`attach`](Self::attach) once
    /// the node is ready and the engine launches.
    pub fn detached(
        store: Arc<StandaloneSyncStore>,
        seen: Arc<SeenStore>,
        batches: Arc<BatchStore>,
        config: Config,
        config_path: PathBuf,
        agent_state: watch::Receiver<AgentState>,
        supervisor_wake: Arc<Notify>,
    ) -> Self {
        // A placeholder send-config seed for the not-yet-attached channel; the
        // real value arrives with the batcher's sender on `attach`.
        let send_cfg = config.send_cfg();
        Self {
            store,
            seen,
            config_path,
            config: RwLock::new(config),
            agent_state,
            supervisor_wake,
            engine: RwLock::new(None),
            cleanup: RwLock::new(None),
            peer_device: RwLock::new(String::new()),
            // A placeholder sender with no receivers: `send` is a harmless no-op
            // until `attach` swaps in the running agent's real retention channel.
            retention_tx: RwLock::new(watch::channel(RetentionConfig::default()).0),
            retention_log: RwLock::new(Arc::new(Mutex::new(VecDeque::new()))),
            device_names: RwLock::new(HashMap::new()),
            running_dirs: RwLock::new(Vec::new()),
            running_targets: RwLock::new(Vec::new()),
            batcher: RwLock::new(None),
            batches,
            // Placeholder send-config channel (no receivers) until `attach` swaps
            // in the running batcher's real sender — mirrors `retention_tx`.
            send_cfg_tx: RwLock::new(watch::channel(send_cfg).0),
        }
    }

    /// Swap the engine-dependent bits in as the engine comes up. Called (via a
    /// `tokio::spawn`) from the supervisor's `on_agent` seam, which clones these
    /// out of the `&dyn ManagedAgent` synchronously first (the callback is sync;
    /// this is `async` and takes the write locks).
    #[allow(clippy::too_many_arguments)]
    pub async fn attach(
        &self,
        engine: Option<Arc<SyncEngineHandle>>,
        cleanup: Option<Arc<SharedPackageCleanup>>,
        peer_device: String,
        retention_tx: watch::Sender<RetentionConfig>,
        retention_log: Arc<Mutex<VecDeque<RetentionRunRecord>>>,
        device_names: HashMap<String, String>,
        running_dirs: Vec<PathBuf>,
        running_targets: Vec<String>,
        batcher: Option<BatcherHandle>,
        send_cfg_tx: watch::Sender<SendCfg>,
    ) {
        *self.engine.write().await = engine;
        *self.cleanup.write().await = cleanup;
        *self.peer_device.write().await = peer_device;
        *self.retention_tx.write().await = retention_tx;
        *self.retention_log.write().await = retention_log;
        *self.device_names.write().await = device_names;
        *self.running_dirs.write().await = running_dirs;
        *self.running_targets.write().await = running_targets;
        *self.batcher.write().await = batcher;
        *self.send_cfg_tx.write().await = send_cfg_tx;
    }

    /// Drop the engine-dependent bits as the engine stops (setup lost, restart,
    /// or shutdown): the page falls back to its detached behaviour. `retention_tx`
    /// / `retention_log` / `device_names` are left as-is — harmlessly stale reads
    /// until the next attach — while the load-bearing safety bits are cleared.
    pub async fn detach(&self) {
        *self.engine.write().await = None;
        *self.cleanup.write().await = None;
        *self.peer_device.write().await = String::new();
        *self.running_dirs.write().await = Vec::new();
        *self.running_targets.write().await = Vec::new();
        // The batcher is a load-bearing safety bit (it drives sends): clear it so
        // a detached page's send-now is an honest no-op. `send_cfg_tx` is left
        // as-is (a harmless stale sender) exactly like `retention_tx`.
        *self.batcher.write().await = None;
    }
}

/// One recorded retention pass, for `GET /api/retention/log`. Built by the
/// retention loop ([`crate::run`]) from the pass's `RetentionOutcome` and
/// push-fronted into [`WebState::retention_log`] (cap 50, newest-first).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionRunRecord {
    /// RFC3339 timestamp of when the pass completed.
    pub at: String,
    /// Whether the pass ran in dry-run mode (deleted nothing).
    pub dry_run: bool,
    /// The policy label in force for the pass (same snake_case string as TOML).
    pub policy: String,
    /// Source files actually removed this pass (empty in dry-run).
    pub deleted: Vec<String>,
    /// Confirmed candidates the policy deemed eligible this pass.
    pub would_delete: Vec<String>,
    /// Pass-level failures / warnings (a failed tick, or disk still over cap).
    pub errors: Vec<String>,
}

/// `GET /api/status` payload: the operator's one-screen picture of the node.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    /// The supervisor's lifecycle label: `needs_setup` | `starting` | `running`
    /// | `failed` (from [`AgentState::label`]).
    agent_state: String,
    /// Human-readable detail for the state (joined setup needs, or the error
    /// text); `None` for `starting` / `running` (from [`AgentState::detail`]).
    agent_detail: Option<String>,
    /// The engine is running but over a stale capture-dir set (a saved edit not
    /// yet applied): the page shows an "applying…" banner and awaits the restart.
    restart_pending: bool,
    /// Watched capture directories (as display strings).
    capture_dirs: Vec<String>,
    /// Live non-terminal packages (queued/announced/transferring). Empty while
    /// the engine is detached (setup mode).
    in_flight: Vec<SentDto>,
    /// The current retention policy + tuning.
    retention: RetentionDto,
    /// Coarse package counts (see [`CountsDto`]).
    counts: CountsDto,
}

/// The retention table, flattened for the status page. `policy` is the same
/// snake_case string the TOML uses.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetentionDto {
    policy: String,
    dry_run: bool,
    soak_opt_in: bool,
    keep_days: u32,
    disk_max_pct: u8,
    interval_secs: u64,
}

/// Coarse package tallies. `queued` is exact (derived from the live in-flight
/// list); `confirmedTotal`/`failedTotal`/`cancelledTotal` are over the most
/// recent [`STATUS_SCAN_LIMIT`] packages — a summary, not an exact lifetime
/// total.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CountsDto {
    confirmed_total: u64,
    failed_total: u64,
    /// User-cancelled terminals over the recent window (Task 9). Bucketed
    /// separately so a cancelled package is not silently dropped from the
    /// status page's tallies.
    cancelled_total: u64,
    queued: u64,
}

/// One outbound package row, for `GET /api/sent` and the status page's
/// in-flight list.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SentDto {
    id: i64,
    package_ref: String,
    state: String,
    attempts: u32,
    created_at: String,
    confirmed_at: Option<String>,
    /// The most recent failed-attempt reason (Task 9), or `None` when the package
    /// has never failed / was cleared on success. Rendered beside `attempts` on the
    /// status page so an operator sees *why* a package is stuck or failed.
    last_error: Option<String>,
    /// The single safe-to-delete predicate surfaced to the UI: only a
    /// `confirmed` (fully received by the peer) package may be deleted.
    deletable: bool,
    /// Filenames inside the package, read from its manifest — the human-facing
    /// row content (the raw `package_ref` is a `data/packages/<uuid>` dir that
    /// misled operators into thinking a delete had failed). Capped at
    /// [`SENT_FILES_CAP`]; empty when the manifest can't be read. See
    /// [`sent_manifest_summary`].
    files: Vec<String>,
    /// The wall-clock deadline of the next scheduled retry (Task 9), straight
    /// from [`OutboundRow::next_retry_at`] — `None` when the package is not
    /// waiting out a backoff window. Drives the status page's live countdown.
    next_retry_at: Option<String>,
    /// Total on-wire size of the package: the sum of every manifest record's
    /// `byte_size` (Task 9). `0` when the manifest can't be read.
    byte_size: u64,
}

/// One transfer-history row, for `GET /api/history`.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryDto {
    filename: String,
    object: Option<String>,
    peer_device: String,
    /// Friendly name for `peer_device`, when known (Task 11); else `None`.
    peer_name: Option<String>,
    direction: String,
    bytes: u64,
    started_at: String,
    finished_at: Option<String>,
    /// `finished - started` in seconds when both stamps parse as RFC3339; else
    /// `None` (never a panic).
    duration_secs: Option<f64>,
    outcome: String,
}

/// Build the status-page router. `token` is snapshotted from
/// [`Config::web_token`](crate::config::Config) at spawn time — an auth change
/// needs an agent restart, which keeps [`auth_layer`] free of shared mutable
/// state. Task 10 adds write routes onto this same router + [`WebState`].
pub fn build_router(state: Arc<WebState>, token: Option<String>) -> Router {
    // `GET /` is the static, data-free page shell — it is deliberately EXEMPT
    // from the bearer layer. On any token-protected (non-loopback) deployment a
    // browser navigation must be able to load the page so its JS token prompt
    // can run; gating `/` would 401 that navigation before any JS loads, making
    // the README's documented flow dead on arrival. Every `/api/*` route (which
    // carries actual node data) stays behind [`auth_layer`].
    let api = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/sent", get(api_sent))
        .route("/api/history", get(api_history))
        .route(
            "/api/retention/policy",
            get(api_get_retention_policy).put(api_put_retention_policy),
        )
        .route("/api/retention/log", get(api_retention_log))
        .route(
            "/api/capture-dirs",
            get(api_get_capture_dirs).put(api_put_capture_dirs),
        )
        .route("/api/targets", get(api_get_targets).put(api_put_targets))
        .route("/api/targets/options", get(api_get_target_options))
        .route(
            "/api/device-name",
            get(api_get_device_name).put(api_put_device_name),
        )
        // Sync Phase 2 (send workflow): the pending "To sync" tree, the live
        // Auto↔Manual send-mode toggle, the manual "send now" trigger, and the
        // batched send history. All bearer-gated like every other `/api/*` route.
        .route("/api/pending", get(api_get_pending))
        .route(
            "/api/send-mode",
            get(api_get_send_mode).put(api_put_send_mode),
        )
        .route("/api/send-now", post(api_send_now))
        .route("/api/batches", get(api_batches))
        .route("/api/delete", post(api_delete))
        .route("/api/retry", post(api_retry))
        // Per-row send-now (wake out of backoff) and user-cancel (Task 9). Both
        // bearer-gated; `/api/send-now` (the batcher flush) is a different route.
        .route("/api/kick", post(api_kick))
        .route("/api/cancel", post(api_cancel))
        // Account sign-in (Task 5) — email→OTP through the hub. These are
        // deliberately part of the bearer-gated `api` router, NOT exempt: they
        // read/mutate account state and must never be reachable without the token.
        .route("/api/account", get(api_account_get))
        .route("/api/account/request-code", post(api_account_request_code))
        .route("/api/account/verify", post(api_account_verify))
        .route("/api/account/logout", post(api_account_logout))
        .layer(axum::middleware::from_fn(move |req, next| {
            auth_layer(token.clone(), req, next)
        }));
    Router::new()
        .route("/", get(index_html))
        .merge(api)
        .with_state(state)
}

/// Bearer-token gate. With `token = None` (the loopback default) every request
/// passes through. With `Some(expected)`, a request must present
/// `Authorization: Bearer <expected>` or it is refused `401` — no logging, so a
/// port scanner cannot flood the log one line per probe (the 401 body tells a
/// legitimate caller exactly what is wrong).
async fn auth_layer(token: Option<String>, req: Request, next: Next) -> Response {
    let Some(expected) = token.as_deref() else {
        return next.run(req).await;
    };
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    // Constant-time comparison (finding M1): a plain `==` on the token short-
    // circuits at the first differing byte, leaking a per-byte timing oracle a
    // LAN attacker could use to recover the token. `constant_time_eq` compares
    // in time independent of the contents (only the length is observable, which
    // is not secret).
    let ok = presented.is_some_and(|p| constant_time_eq(p.as_bytes(), expected.as_bytes()));
    if ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "invalid or missing bearer token").into_response()
    }
}

/// Compare two byte slices in constant time (no data-dependent early exit).
/// Returns `false` immediately on a length mismatch — the length of a bearer
/// token is not sensitive, its contents are.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── DNS-rebinding Host guard (finding M2) ────────────────────────────────────

/// Which `Host` header values this server will answer, derived from its bind
/// address. Defends against DNS rebinding: a malicious page the operator visits
/// cannot rebind its own hostname to the loopback agent and drive the API,
/// because such requests carry the attacker's `Host`, not an allowed one.
#[derive(Clone)]
pub(crate) enum HostPolicy {
    /// The bind is a wildcard (`0.0.0.0` / `::`) so the legitimate host is not
    /// enumerable — the bearer token (mandatory for a non-loopback bind) is the
    /// defense; the Host check is a no-op.
    AllowAll,
    /// Answer only these host values (loopback names + the specific bind IP).
    Allowed(HashSet<String>),
}

impl HostPolicy {
    /// Build the policy for a bind address. A loopback or specific-IP bind gets a
    /// concrete allow-list (loopback names + that IP); a wildcard bind can't be
    /// enumerated, so it allows all (token-protected).
    pub(crate) fn for_bind(addr: std::net::SocketAddr) -> Self {
        let ip = addr.ip();
        if ip.is_unspecified() {
            return HostPolicy::AllowAll;
        }
        let mut set = HashSet::from([
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
            "[::1]".to_string(),
        ]);
        set.insert(ip.to_string());
        if ip.is_ipv6() {
            set.insert(format!("[{ip}]"));
        }
        HostPolicy::Allowed(set)
    }

    /// Whether a request carrying this `Host` header is permitted. A *missing*
    /// Host is allowed — a DNS-rebinding attack always presents the attacker's
    /// hostname, so absence cannot be that attack, and rejecting it would only
    /// break odd non-browser clients.
    fn permits(&self, host_header: Option<&str>) -> bool {
        match self {
            HostPolicy::AllowAll => true,
            HostPolicy::Allowed(set) => match host_header {
                None => true,
                Some(h) => {
                    let raw = h.trim().to_ascii_lowercase();
                    set.contains(&raw) || set.contains(&host_only(&raw))
                }
            },
        }
    }
}

/// The host component of a `Host` header value, with any `:port` stripped.
/// Handles bracketed IPv6 (`[::1]:8686` → `[::1]`) and `host:port`.
fn host_only(h: &str) -> String {
    let h = h.trim();
    if let Some(rest) = h.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return format!("[{}]", &rest[..end]);
        }
    }
    match h.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            host.to_string()
        }
        _ => h.to_string(),
    }
}

/// Wrap `router` with the [`HostPolicy`] guard (finding M2). Applied at the
/// single production serving choke point (`run::bind_and_spawn_web`), so it
/// covers every route including the auth-exempt `GET /`.
pub(crate) fn apply_host_guard(router: Router, policy: HostPolicy) -> Router {
    router.layer(axum::middleware::from_fn(move |req: Request, next: Next| {
        let policy = policy.clone();
        async move {
            let host = req
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if policy.permits(host.as_deref()) {
                next.run(req).await
            } else {
                (StatusCode::FORBIDDEN, "host not allowed").into_response()
            }
        }
    }))
}

/// `GET /` — placeholder status page. The interactive dashboard is Task 10.
async fn index_html() -> Html<&'static str> {
    Html(include_str!("web/index.html"))
}

/// `GET /api/status`
async fn api_status(
    State(state): State<Arc<WebState>>,
) -> Result<Json<StatusDto>, (StatusCode, String)> {
    // Snapshot the config once (clone, drop the guard) so no lock is held across
    // the store/engine reads below.
    let config = state.config.read().await.clone();
    let retention = config.retention.clone();
    let configured = config.capture_dirs_resolved();

    // Lifecycle state (cloned out of the watch borrow immediately, never held
    // across an await).
    let agent = state.agent_state.borrow().clone();
    let running = state.running_dirs.read().await.clone();
    let restart_pending = matches!(agent, AgentState::Running { .. }) && running != configured;

    // Live in-flight picture — only when the engine is attached. Detached
    // (setup mode) reports an empty list, never an error.
    let engine = state.engine.read().await.clone();
    let (in_flight, queued) = match &engine {
        Some(engine) => {
            let rows = engine.status_snapshot().map_err(|e| {
                tracing::error!(error = %e, "web status: read in-flight failed");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;
            let queued = rows
                .iter()
                .filter(|r| r.state == OutboundState::Queued)
                .count() as u64;
            let in_flight: Vec<SentDto> = rows.iter().map(to_sent_dto).collect();
            (in_flight, queued)
        }
        None => (Vec::new(), 0),
    };

    // Terminal counts over a bounded recent window (see STATUS_SCAN_LIMIT). The
    // store is always open, engine or not.
    let recent = state.store.all_outbound(STATUS_SCAN_LIMIT).map_err(|e| {
        tracing::error!(error = %e, "web status: read outbound window failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let confirmed_total = recent
        .iter()
        .filter(|r| r.state == OutboundState::Confirmed)
        .count() as u64;
    let failed_total = recent
        .iter()
        .filter(|r| r.state == OutboundState::Failed)
        .count() as u64;
    let cancelled_total = recent
        .iter()
        .filter(|r| r.state == OutboundState::Cancelled)
        .count() as u64;

    Ok(Json(StatusDto {
        agent_state: agent.label().to_string(),
        agent_detail: agent.detail(),
        restart_pending,
        capture_dirs: configured
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        in_flight,
        retention: RetentionDto {
            policy: crate::config_edit::policy_str(&retention.policy).to_string(),
            dry_run: retention.dry_run,
            soak_opt_in: retention.i_have_verified_the_soak,
            keep_days: retention.keep_days,
            disk_max_pct: retention.disk_max_pct,
            interval_secs: retention.interval_secs,
        },
        counts: CountsDto {
            confirmed_total,
            failed_total,
            cancelled_total,
            queued,
        },
    }))
}

/// Query string for `GET /api/sent`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SentQuery {
    /// Restrict to one state (`queued`/`announced`/`transferring`/`confirmed`/
    /// `failed`). Absent → every state.
    state: Option<String>,
    /// Row cap; defaults to [`DEFAULT_SENT_LIMIT`].
    limit: Option<u32>,
}

/// `GET /api/sent`
async fn api_sent(
    State(state): State<Arc<WebState>>,
    Query(q): Query<SentQuery>,
) -> Result<Json<Vec<SentDto>>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(DEFAULT_SENT_LIMIT);
    let rows = state.store.all_outbound(limit).map_err(|e| {
        tracing::error!(error = %e, "web sent: read outbound failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let filter = q.state.as_deref().filter(|s| !s.is_empty());
    let dtos: Vec<SentDto> = rows
        .iter()
        .filter(|r| filter.is_none_or(|s| r.state.as_str() == s))
        .map(to_sent_dto)
        .collect();
    Ok(Json(dtos))
}

/// Query string for `GET /api/history`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryQ {
    /// Exact filename to filter on. `search_history` AND-combines its filters,
    /// so a single free-text box maps to filename (the most useful axis), not
    /// filename-and-object (which would require both columns to equal it).
    query: Option<String>,
    /// Restrict to one direction (`sent`/`received`). Invalid → `400`.
    direction: Option<String>,
    /// Row cap; defaults to [`DEFAULT_HISTORY_LIMIT`].
    limit: Option<u32>,
}

/// `GET /api/history`
async fn api_history(
    State(state): State<Arc<WebState>>,
    Query(q): Query<HistoryQ>,
) -> Result<Json<Vec<HistoryDto>>, (StatusCode, String)> {
    let direction = match q.direction.as_deref().filter(|d| !d.is_empty()) {
        None => None,
        Some(d) => Some(Direction::from_db(d).map_err(|e| {
            tracing::error!(error = %e, direction = d, "web history: bad direction filter");
            (StatusCode::BAD_REQUEST, e.to_string())
        })?),
    };
    let hq = HistoryQuery {
        filename: q.query.filter(|s| !s.is_empty()),
        object: None,
        direction,
        peer: None,
        // Perseus's web status page has no project dimension (personal sync).
        project: None,
        // No per-batch detail surface on the Perseus agent (Task 14).
        package_id: None,
        limit: q.limit.unwrap_or(DEFAULT_HISTORY_LIMIT),
    };
    let rows = state.store.search_history(hq).map_err(|e| {
        tracing::error!(error = %e, "web history: search failed");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    let device_names = state.device_names.read().await;
    let dtos = rows
        .iter()
        .map(|r| to_history_dto(r, &device_names))
        .collect();
    Ok(Json(dtos))
}

/// `GET`/`PUT /api/retention/policy` payload: the writable retention knobs plus
/// the read-only two-key soak gate. `soakOptIn`/`liveDeletionPossible` mirror
/// the config's soak state and are **never** writable here — [`RetentionEdit`]
/// carries no soak field, so live deletion can only ever be enabled by
/// hand-editing `perseus.toml`. That guarantee is the whole point of the
/// separate read-only surface.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PolicyDto {
    policy: String,
    keep_days: u32,
    disk_max_pct: u8,
    interval_secs: u64,
    dry_run: bool,
    /// The operator's typed soak acknowledgement (`i_have_verified_the_soak`,
    /// a `perseus.toml`-only key). Read-only here.
    soak_opt_in: bool,
    /// Whether confirmed sources are actually being deleted right now — the soak
    /// opt-in is set AND the pass is live (not dry-run). Derived, read-only.
    live_deletion_possible: bool,
}

impl PolicyDto {
    fn from_retention(r: &RetentionConfig) -> Self {
        Self {
            policy: crate::config_edit::policy_str(&r.policy).to_string(),
            keep_days: r.keep_days,
            disk_max_pct: r.disk_max_pct,
            interval_secs: r.interval_secs,
            dry_run: r.dry_run,
            soak_opt_in: r.i_have_verified_the_soak,
            live_deletion_possible: r.i_have_verified_the_soak && !r.dry_run,
        }
    }
}

/// `POST /api/delete` request body.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteRequest {
    /// Outbound-row ids to delete. Non-confirmed / unknown ids are rejected
    /// per-id in the [`DeleteReport`] response, never deleted.
    ids: Vec<i64>,
}

/// `GET /api/retention/policy` — current retention config + read-only soak gate.
async fn api_get_retention_policy(State(state): State<Arc<WebState>>) -> Json<PolicyDto> {
    let retention = state.config.read().await.retention.clone();
    Json(PolicyDto::from_retention(&retention))
}

/// `PUT /api/retention/policy` — apply a whitelisted [`RetentionEdit`] to
/// `perseus.toml` (comment-preserving, re-validated, atomic), then adopt it live.
///
/// [`apply_retention_edit`] does the file write on an in-memory copy and only
/// swaps it in after re-validation, so a rejected edit (notably an attempt to
/// enable live deletion without the `perseus.toml`-only soak opt-in) leaves the
/// file byte-identical and returns `422`. On success the new config is adopted
/// both in the live web state and on the retention watch channel — the running
/// retention loop picks it up on its next pass, no restart.
async fn api_put_retention_policy(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<RetentionEdit>,
) -> Result<Json<PolicyDto>, (StatusCode, String)> {
    let new_cfg = apply_retention_edit(&state.config_path, &edit).map_err(|e| {
        // Validation failure (e.g. the soak gate) or any file error: the file is
        // left untouched by construction. Surface the actionable text as 422.
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web retention edit rejected");
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;
    // The file was already rewritten by `apply_retention_edit`. Adopt the new
    // config in the live web state, and push it onto the retention watch channel
    // so the running loop re-borrows it next pass. A send with no receiver (an
    // agent started without `watch`) is a harmless no-op — discard the SendError.
    let retention = new_cfg.retention.clone();
    *state.config.write().await = new_cfg;
    let _ = state.retention_tx.read().await.send(retention.clone());
    Ok(Json(PolicyDto::from_retention(&retention)))
}

/// `GET /api/retention/log` — the retention-run ring buffer, newest-first.
async fn api_retention_log(State(state): State<Arc<WebState>>) -> Json<Vec<RetentionRunRecord>> {
    let log_handle = state.retention_log.read().await;
    let log = log_handle.lock().expect("retention_log mutex poisoned");
    Json(log.iter().cloned().collect())
}

/// `GET`/`PUT /api/capture-dirs` payload. `configured` is the directory list in
/// the live config (`perseus.toml`, freshly rewritten by a PUT); `runtime` is
/// the list the watchers were actually spawned over. They diverge exactly in
/// the window after an edit is saved and before the supervisor has relaunched
/// the engine over the new dirs — `restartPending` is that difference,
/// ordered-list compared, so the page can show an honest "applying…" banner
/// that survives reloads and clears itself once the relaunched engine reports
/// `runtime == configured` again.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureDirsDto {
    configured: Vec<String>,
    runtime: Vec<String>,
    restart_pending: bool,
}

/// `PUT /api/capture-dirs` request body: the new capture-directory selection.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureDirsEdit {
    dirs: Vec<String>,
}

/// Build the current [`CaptureDirsDto`] from live state: `configured` from the
/// (possibly just-edited) config, `runtime` from the spawn-time snapshot, and
/// `restartPending` = the two differ as ordered lists.
async fn capture_dirs_dto(state: &WebState) -> CaptureDirsDto {
    let configured: Vec<String> = state
        .config
        .read()
        .await
        .capture_dirs_resolved()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let runtime: Vec<String> = state
        .running_dirs
        .read()
        .await
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let restart_pending = configured != runtime;
    CaptureDirsDto {
        configured,
        runtime,
        restart_pending,
    }
}

/// `GET /api/capture-dirs` — the configured vs. running capture directories and
/// whether a restart is pending. Read-only; never touches disk.
async fn api_get_capture_dirs(State(state): State<Arc<WebState>>) -> Json<CaptureDirsDto> {
    Json(capture_dirs_dto(&state).await)
}

/// `PUT /api/capture-dirs` — rewrite `perseus.toml`'s capture-directory
/// selection to the array form (comment-preserving, re-validated, atomic), then
/// adopt it into the live config and ring the supervisor. The running watchers
/// keep their spawn-time directories, so [`WebState::running_dirs`] is
/// intentionally **not** touched here — that gap is what makes the returned
/// `restartPending` true. The wake lets the supervisor apply the edit live: it
/// reloads config each pass and restarts the engine when
/// `running_dirs != configured`, so no operator-driven process restart is
/// needed and the pending state clears itself once the engine relaunches.
///
/// [`apply_capture_dirs_edit`] writes on an in-memory copy and only swaps it in
/// after re-validation, so a rejected edit (an empty list, or a directory that
/// does not exist on the box) leaves the file byte-identical and returns `422`.
async fn api_put_capture_dirs(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<CaptureDirsEdit>,
) -> Result<Json<CaptureDirsDto>, (StatusCode, String)> {
    let new_cfg = apply_capture_dirs_edit(&state.config_path, &edit.dirs).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web capture-dirs edit rejected");
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;
    // The file was already rewritten. Adopt the new config in the live web state
    // so `configured` reflects the edit; the runtime snapshot stays as-is, which
    // is exactly what surfaces the restart-pending state.
    *state.config.write().await = new_cfg;
    // Ring the supervisor: it reloads config each pass and restarts the engine
    // when `running_dirs != configured`, so the edit applies live (engine
    // restart only) with no operator-driven process restart. The banner clears
    // itself once the restarted engine reports `running_dirs == configured`.
    state.supervisor_wake.notify_one();
    Ok(Json(capture_dirs_dto(&state).await))
}

/// `GET`/`PUT /api/targets` payload. `configured` is the send-target list in the
/// live config (`perseus.toml`, freshly rewritten by a PUT); `runtime` is the
/// list the running engines were actually spawned over. They diverge exactly in
/// the window after an edit is saved and before the supervisor has relaunched the
/// engines over the new targets — `restartPending` is that difference (ordered
/// compare), so the page shows an honest "applying…" note that clears itself once
/// the relaunched engines report `runtime == configured` again. Mirrors
/// [`CaptureDirsDto`] (targets are likewise restart-to-apply — bound at spawn).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetsDto {
    configured: Vec<String>,
    runtime: Vec<String>,
    restart_pending: bool,
}

/// `PUT /api/targets` request body: the new send-target selection.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetsEdit {
    targets: Vec<String>,
}

/// Build the current [`TargetsDto`] from live state: `configured` from the
/// (possibly just-edited) config, `runtime` from the spawn-time snapshot, and
/// `restartPending` = the two differ as ordered lists.
async fn targets_dto(state: &WebState) -> TargetsDto {
    let configured = state.config.read().await.targets.clone();
    let runtime = state.running_targets.read().await.clone();
    let restart_pending = configured != runtime;
    TargetsDto {
        configured,
        runtime,
        restart_pending,
    }
}

/// `GET /api/targets` — the configured vs. running send targets and whether a
/// restart is pending. Read-only; never touches disk.
async fn api_get_targets(State(state): State<Arc<WebState>>) -> Json<TargetsDto> {
    Json(targets_dto(&state).await)
}

/// `PUT /api/targets` — rewrite `perseus.toml`'s `targets` list (comment-preserving,
/// re-validated, atomic), then adopt it into the live config and ring the
/// supervisor. The running engines keep their spawn-time targets, so
/// [`WebState::running_targets`] is intentionally **not** touched here — that gap
/// is what makes the returned `restartPending` true. The wake lets the supervisor
/// apply the edit live: it reloads config each pass and restarts the engines when
/// `running_targets != configured`, so no operator-driven process restart is
/// needed and the pending state clears itself once the engines relaunch.
///
/// [`apply_targets_edit`] writes on an in-memory copy and only swaps it in after
/// re-validation at the parse-valid tier (syntax + field constraints, not
/// run-readiness). An empty list is accepted — "at least one send target" is a
/// run/start concern, not an edit gate; a genuinely malformed edit still leaves
/// the file byte-identical and returns `422`.
async fn api_put_targets(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<TargetsEdit>,
) -> Result<Json<TargetsDto>, (StatusCode, String)> {
    let new_cfg = apply_targets_edit(&state.config_path, &edit.targets).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web targets edit rejected");
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;
    // The file was already rewritten. Adopt the new config so `configured`
    // reflects the edit; the runtime snapshot stays as-is, surfacing the
    // restart-pending state.
    *state.config.write().await = new_cfg;
    state.supervisor_wake.notify_one();
    Ok(Json(targets_dto(&state).await))
}

/// `GET /api/targets/options` payload: the account's receiver-capable devices for
/// the Send Targets picker. The list already EXCLUDES send-only Perseus devices
/// and this device itself (see [`crate::account::list_target_options`]), so the
/// picker only ever offers valid receivers.
///
/// `signedIn` is `true` only when a device list was obtained from the hub (a
/// token is stored AND the hub answered). A signed-out node or an unreachable hub
/// yields `signedIn: false` + an empty `devices` list; a hub failure additionally
/// carries a human-readable `error` (omitted entirely on the clean/happy paths).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetOptionsDto {
    signed_in: bool,
    devices: Vec<TargetOptionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// One receiver-capable device offered by the picker. `id` is the stable value
/// the UI writes into `targets` (rename-robust); `name` is display only.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetOptionDto {
    id: String,
    name: String,
    capability: String,
}

/// `GET /api/targets/options` — the account's receiver-capable devices for the
/// Send Targets picker. Makes ONE hub `list_devices` call; the frontend fetches
/// this only on load, after a sign-in state change, and on an explicit refresh —
/// never on its 2 s poll, because a hub call is a remote round trip. Never fails:
/// signed-out / hub-unreachable degrade to an empty list (`signedIn: false`) with
/// an optional `error`, so the picker stays usable.
async fn api_get_target_options(State(state): State<Arc<WebState>>) -> Json<TargetOptionsDto> {
    let config = state.config.read().await.clone();
    let opts = crate::account::list_target_options(&config).await;
    Json(TargetOptionsDto {
        signed_in: opts.signed_in,
        devices: opts
            .devices
            .into_iter()
            .map(|d| TargetOptionDto {
                id: d.id,
                name: d.name,
                capability: d.capability.as_str().to_string(),
            })
            .collect(),
        error: opts.error,
    })
}

/// `GET`/`PUT /api/device-name` payload. `deviceName` is the explicit
/// `perseus.toml` override (`None` → the hostname default is used). On a PUT,
/// `hubError` carries a best-effort live-hub-rename problem (a duplicate name, or
/// an unreachable hub) — the LOCAL edit always succeeds regardless, so the UI
/// surfaces `hubError` as a warning rather than a failure.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceNameDto {
    device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hub_error: Option<String>,
}

/// `PUT /api/device-name` request body: the new friendly name (blank clears the
/// override back to the hostname default).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceNameEdit {
    name: String,
}

/// `GET /api/device-name` — the current `device_name` override (never the
/// hostname default; `None` means "use the hostname"). Read-only.
async fn api_get_device_name(State(state): State<Arc<WebState>>) -> Json<DeviceNameDto> {
    let device_name = state.config.read().await.device_name.clone();
    Json(DeviceNameDto {
        device_name,
        hub_error: None,
    })
}

/// `PUT /api/device-name` — rewrite `perseus.toml`'s `device_name`
/// (comment-preserving, re-validated, atomic) and adopt it live, then
/// **best-effort** rename the live hub device so the account device list updates
/// without waiting for the next sign-in.
///
/// The local config edit is authoritative: a `409` duplicate name from the hub or
/// an unreachable hub does NOT fail the request — the name is already saved
/// locally and re-syncs on the next registration. The hub problem is surfaced in
/// `hubError` (a `409` message, or the transport error) so the UI can warn. The
/// local edit re-validates at the parse-valid tier only, so a missing send target
/// does NOT reject the rename (the fresh-setup case); a genuinely malformed edit
/// still returns `422` with the file left byte-identical. `device_name` is not
/// engine-bound, so no restart is needed; the supervisor is woken so
/// re-registration picks up the new name.
async fn api_put_device_name(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<DeviceNameEdit>,
) -> Result<Json<DeviceNameDto>, (StatusCode, String)> {
    let new_cfg = apply_device_name_edit(&state.config_path, &edit.name).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web device-name edit rejected");
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;
    let device_name = new_cfg.device_name.clone();
    *state.config.write().await = new_cfg.clone();

    // Best-effort live hub rename (no-op when not signed in). A duplicate name or
    // an unreachable hub is surfaced, not fatal — the local edit stands.
    let hub_error = match crate::account::rename_hub_device(&new_cfg).await {
        crate::account::HubRenameOutcome::Renamed(_) | crate::account::HubRenameOutcome::Skipped => {
            None
        }
        crate::account::HubRenameOutcome::Duplicate(msg) => Some(msg),
        crate::account::HubRenameOutcome::Unreachable(msg) => {
            Some(format!("saved locally, but the hub could not be updated: {msg}"))
        }
    };
    // Re-registration (next sign-in / supervisor pass) picks up the local name.
    state.supervisor_wake.notify_one();
    Ok(Json(DeviceNameDto {
        device_name,
        hub_error,
    }))
}

// ── Sync Phase 2: pending tree / send-mode / send-now / batched history ───────

/// `GET /api/pending` payload: the "To sync" tree plus the live send-mode header
/// the page renders above it (so a single fetch drives the whole panel).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingDto {
    /// The pending accumulator grouped into a `rel_path` trie (see [`pending_tree`]).
    tree: PendingNode,
    /// The current send mode (`auto` | `manual`) — the same snake_case string the
    /// TOML uses ([`crate::config_edit::mode_str`]).
    mode: String,
    /// The auto-mode quiet window in seconds (inert in manual mode).
    auto_quiet_secs: u64,
    /// Total pending files — the batcher's accumulator length, i.e. the "N
    /// pending" the manual "send now" button acts on.
    count: usize,
}

/// `GET /api/send-mode` payload, and the applied-values echo a successful `PUT`
/// returns.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SendModeDto {
    mode: String,
    auto_quiet_secs: u64,
}

/// `PUT /api/send-mode` request body. `mode` is a free string (not the [`Mode`]
/// enum) so an unknown value is a clean `400` from the handler rather than a
/// `422` deserialization error from the extractor.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendModeEdit {
    mode: String,
    auto_quiet_secs: u64,
}

/// `POST /api/send-now` response: how many pending files the manual flush carried
/// (`0` when nothing was pending — a no-op, never an error).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SendNowDto {
    flushed: usize,
}

/// One `GET /api/batches` row: a recorded send-batch ([`crate::batch_store::BatchRow`])
/// joined with the sync engine's per-target outbound state.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchDto {
    package_ref: String,
    /// `auto` (watcher quiet-timer) or `manual` (operator "send now").
    mode: String,
    created_at: String,
    file_count: i64,
    /// Frames actually transferred vs. dropped as the peer's duplicates. These
    /// are the sender's ephemeral `sync-finished` dedup outcome — NOT persisted in
    /// `sync_outbound` and not carried on [`OutboundRow`] — so they are reported as
    /// `0` here (the honest "not tracked post-flight" value). Wired as named
    /// fields so a future durable source can fill them without a shape change.
    #[serde(rename = "new")]
    new_count: u32,
    #[serde(rename = "duplicate")]
    duplicate_count: u32,
    /// One entry per target the package was fanned to (friendly name + live
    /// outbound state). Empty for a batch whose outbound rows aren't visible yet.
    targets: Vec<BatchTargetDto>,
    /// The batch-level outcome derived from its targets (see [`aggregate_outcome`]).
    outcome: String,
}

/// One send target of a batch: the friendly device name (peer hex when unknown)
/// and its current outbound state string.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTargetDto {
    name: String,
    state: String,
}

/// Parse a wire `mode` string into a [`Mode`]. `None` for any unknown value — the
/// handler maps that to a `400`.
fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "auto" => Some(Mode::Auto),
        "manual" => Some(Mode::Manual),
        _ => None,
    }
}

/// `GET /api/pending` — the "To sync" tree over the batcher's current pending
/// accumulator, plus the live send-mode header. A detached page (engine in setup,
/// batcher `None`) reports an empty tree and a `0` count — never an error.
async fn api_get_pending(State(state): State<Arc<WebState>>) -> Json<PendingDto> {
    // Clone the config once (drop the guard) so no lock is held across the read.
    let config = state.config.read().await.clone();
    let send = config.send_cfg();
    let snapshot = match state.batcher.read().await.as_ref() {
        Some(b) => b.pending_snapshot(),
        None => Vec::new(),
    };
    let count = snapshot.len();
    let tree = pending_tree(&snapshot, &config);
    Json(PendingDto {
        tree,
        mode: crate::config_edit::mode_str(send.mode).to_string(),
        auto_quiet_secs: send.auto_quiet_secs,
        count,
    })
}

/// `GET /api/send-mode` — the current send mode + auto quiet window. Read-only.
async fn api_get_send_mode(State(state): State<Arc<WebState>>) -> Json<SendModeDto> {
    let send = state.config.read().await.send_cfg();
    Json(SendModeDto {
        mode: crate::config_edit::mode_str(send.mode).to_string(),
        auto_quiet_secs: send.auto_quiet_secs,
    })
}

/// `PUT /api/send-mode` — flip Auto↔Manual (and/or change the quiet window),
/// **live**. An unknown `mode` string is a `400` before anything is touched.
/// Otherwise [`apply_send_mode_edit`] rewrites `perseus.toml` (comment-preserving,
/// re-validated, atomic — a rejected edit leaves the file byte-identical and
/// returns `422`), the live config is swapped, and the new [`SendCfg`] is pushed
/// onto the batcher's `send_cfg_tx` so the running batcher adopts it on its next
/// select! turn — no restart. The supervisor is woken so its config view refreshes
/// at once. Returns the applied `{mode, autoQuietSecs}`.
async fn api_put_send_mode(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<SendModeEdit>,
) -> Result<Json<SendModeDto>, (StatusCode, String)> {
    let mode = parse_mode(&edit.mode).ok_or_else(|| {
        tracing::error!(mode = %edit.mode, "web send-mode edit: unknown mode");
        (
            StatusCode::BAD_REQUEST,
            format!("unknown send mode: {}", edit.mode),
        )
    })?;
    let new_cfg =
        apply_send_mode_edit(&state.config_path, mode, edit.auto_quiet_secs).map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "web send-mode edit rejected");
            (StatusCode::UNPROCESSABLE_ENTITY, msg)
        })?;
    let send = new_cfg.send_cfg();
    *state.config.write().await = new_cfg;
    // Live-apply: push the new send config onto the running batcher's watch
    // channel (a no-op send when detached — no receiver). This is what makes the
    // Auto↔Manual / quiet-window change take effect with no engine restart.
    let _ = state.send_cfg_tx.read().await.send(send);
    // Wake the supervisor so its per-pass config view refreshes immediately.
    state.supervisor_wake.notify_one();
    Ok(Json(SendModeDto {
        mode: crate::config_edit::mode_str(send.mode).to_string(),
        auto_quiet_secs: send.auto_quiet_secs,
    }))
}

/// `POST /api/send-now` — flush the whole pending set now as one manual batch.
/// The pending count is read **at flush time** and returned as `flushed`; a flush
/// with nothing pending (or a detached page with no batcher) is a `0`-flush no-op,
/// records no batch row, and is never an error. The handle is cloned out of the
/// lock so the guard is not held across the `flush_now().await`.
async fn api_send_now(State(state): State<Arc<WebState>>) -> Json<SendNowDto> {
    let batcher = state.batcher.read().await.clone();
    let flushed = match batcher {
        Some(b) => {
            let count = b.pending_snapshot().len();
            b.flush_now().await;
            count
        }
        None => 0,
    };
    Json(SendNowDto { flushed })
}

/// The batch-level outcome derived from its per-target outbound states:
/// `failed` if any target failed; else `cancelled` once every target is
/// terminal and at least one was user-cancelled (Task 3 added the `Cancelled`
/// terminal — an all-terminal batch must NEVER read as `sending`); else
/// `confirmed` once every target confirmed; else `sending` (some target still
/// in flight). An empty set — a batch whose outbound rows are not visible yet
/// (just recorded, or aged out of the scan window) — is reported as `sending`
/// so a just-sent batch reads honestly rather than as an error.
fn aggregate_outcome(rows: &[&OutboundRow]) -> String {
    if rows.is_empty() {
        return "sending".to_string();
    }
    if rows.iter().any(|r| r.state == OutboundState::Failed) {
        return "failed".to_string();
    }
    // Every target terminal but not all confirmed → at least one was cancelled
    // (the only remaining terminal after failed is ruled out above). Report it
    // as `cancelled` rather than leaving the batch stuck at `sending` forever.
    let all_terminal = rows.iter().all(|r| r.state.is_terminal());
    if all_terminal && rows.iter().any(|r| r.state == OutboundState::Cancelled) {
        return "cancelled".to_string();
    }
    if rows.iter().all(|r| r.state == OutboundState::Confirmed) {
        return "confirmed".to_string();
    }
    "sending".to_string()
}

/// `GET /api/batches` — every recorded send-batch (newest-first), each joined by
/// `package_ref` with the sync engine's outbound rows so the operator sees where
/// a package went (per target) and how it fared. Read-only.
///
/// A fan-out writes one outbound row per target, so the join groups them by
/// `package_ref`. [`all_outbound`](StandaloneSyncStore::all_outbound) (every
/// state, unlike the non-terminal-only `status_snapshot`) is used so a `confirmed`
/// batch still resolves its targets. `{new, duplicate}` are the sender's ephemeral
/// dedup outcome and are not persisted, so they are reported as `0` (see
/// [`BatchDto`]). A batch with no matching outbound row (just recorded, or aged
/// past the scan window) degrades to `outcome: "sending"` with no targets.
async fn api_batches(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<BatchDto>>, (StatusCode, String)> {
    let rows = state.batches.list().map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web batches: list failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    let outbound = state.store.all_outbound(STATUS_SCAN_LIMIT).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web batches: read outbound failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    // Group outbound rows by package_ref (one row per target on a fan-out).
    let mut by_ref: HashMap<&str, Vec<&OutboundRow>> = HashMap::new();
    for row in &outbound {
        by_ref
            .entry(row.package_ref.as_str())
            .or_default()
            .push(row);
    }
    let device_names = state.device_names.read().await;
    let dtos = rows
        .iter()
        .map(|b| {
            let matched: &[&OutboundRow] = by_ref
                .get(b.package_ref.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let targets = matched
                .iter()
                .map(|r| {
                    let hex = node_id_hex(&r.peer);
                    BatchTargetDto {
                        name: device_names.get(&hex).cloned().unwrap_or(hex),
                        state: r.state.as_str().to_string(),
                    }
                })
                .collect();
            BatchDto {
                package_ref: b.package_ref.clone(),
                mode: b.mode.clone(),
                created_at: b.created_at.clone(),
                file_count: b.file_count,
                new_count: 0,
                duplicate_count: 0,
                targets,
                outcome: aggregate_outcome(matched),
            }
        })
        .collect();
    Ok(Json(dtos))
}

/// `POST /api/delete` — delete the source capture files of the given CONFIRMED
/// packages. Verifies each id is `confirmed` before touching disk; non-confirmed
/// / unknown ids come back in `rejected` with a reason. Shares the same
/// confirmed-only deleter as retention (audit-before-delete, TOCTOU guard).
async fn api_delete(
    State(state): State<Arc<WebState>>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<DeleteReport>, (StatusCode, String)> {
    let peer_device = state.peer_device.read().await.clone();
    let report = delete_confirmed_packages(&state.store, &state.seen, &req.ids, &peer_device)
        .map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "web manual delete failed");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;
    Ok(Json(report))
}

/// `POST /api/retry` request body.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryRequest {
    /// Outbound-row ids to re-enqueue. Only `failed` rows with intact package
    /// data are retried; everything else comes back per-id in `rejected`.
    ids: Vec<i64>,
}

/// `POST /api/retry` response: the ids re-enqueued (old→new mapping) and the
/// ids rejected (each with a human reason).
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct RetryReport {
    /// One entry per package re-enqueued: the original (still-`failed`) row id
    /// and the brand-new queued row id the receiver will drive.
    retried: Vec<RetryPair>,
    /// Ids not retried, each with the reason (unknown, not failed, or the
    /// package data is gone).
    rejected: Vec<RetryRejection>,
}

/// One re-enqueued package: the original failed row id and its new queued row.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryPair {
    old_id: i64,
    new_id: i64,
}

/// One rejected id from `POST /api/retry` / `/api/kick` / `/api/cancel`, with a
/// reason for the UI to surface.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryRejection {
    id: i64,
    reason: String,
}

/// `POST /api/kick` and `POST /api/cancel` response: the ids acted on and the
/// ids rejected (each with a reason). Mirrors [`RetryReport`]'s shape but with a
/// flat `done` list — kick/cancel act on the row in place, so there is no
/// old→new id mapping. Reuses [`RetryRequest`] for the request body.
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct KickReport {
    /// Ids the engine accepted the kick/cancel for.
    done: Vec<i64>,
    /// Ids not acted on, each with the reason (unknown, or terminal).
    rejected: Vec<RetryRejection>,
}

/// True iff `dir` holds `manifest.ndjson` AND at least one non-manifest regular
/// file — i.e. there is real payload left to re-serve. A confirmed-then-cleaned
/// dir is manifest-only (task 1) and a vanished dir fails `read_dir`; both
/// return `false` so the retry handler can reject them honestly as "package
/// data missing" rather than enqueueing an empty package.
fn package_has_payload(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut has_manifest = false;
    let mut has_payload = false;
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        if entry.file_name() == std::ffi::OsStr::new(MANIFEST_FILENAME) {
            has_manifest = true;
        } else {
            has_payload = true;
        }
    }
    has_manifest && has_payload
}

/// `POST /api/retry` — re-enqueue terminal packages. For each id: look up the
/// outbound row, require a terminal `failed` OR `cancelled` state (Task 9 — a
/// user-cancelled package is retryable just like a failed one), require the
/// package dir to still hold its manifest + payload, then `enqueue_package` it —
/// the sanctioned retry model. Re-enqueueing the same package dir mints a NEW
/// durable row (the receiver dedups by frame uuid); the original terminal row is
/// left untouched. Unknown / non-terminal / data-missing ids are rejected
/// per-id, never enqueued.
async fn api_retry(
    State(state): State<Arc<WebState>>,
    Json(req): Json<RetryRequest>,
) -> Result<Json<RetryReport>, (StatusCode, String)> {
    // Retry re-enqueues through the live engine; there is nothing to retry into
    // while the node is still in setup (engine detached). Honest 503, not a crash.
    let Some(engine) = state.engine.read().await.clone() else {
        tracing::warn!("web retry: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    };
    // The shared-payload cleanup coordinator — present only for a ≥2-target
    // fan-out. A re-enqueue mints a NEW outbound row against the SAME shared
    // package dir, so its eventual terminal must raise the coordinator's
    // `expected` (via `bump`); without that, the retried row's terminal
    // over-counts against the stale `expected` and frees the payload while a
    // still-offline target has yet to receive it (the data-loss hole). `None`
    // for a single-target agent (no shared dir; the engine's in-line cleanup
    // is unchanged).
    let cleanup = state.cleanup.read().await.clone();
    let mut report = RetryReport::default();
    for &id in &req.ids {
        // A genuine store read failure is a 500 (the request failed), not a
        // per-id reject — mirror the delete handler's error philosophy.
        let row = match state.store.get_outbound(id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                report.rejected.push(RetryRejection {
                    id,
                    reason: "unknown package".to_string(),
                });
                continue;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(id, error = %msg, "web retry: outbound lookup failed");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, msg));
            }
        };
        // State check first: only a terminal `failed` or `cancelled` row is
        // retryable (Task 9). (A confirmed id is manifest-only after task-1
        // cleanup, but it never reaches the payload gate — it is "not terminal"
        // here.)
        if !matches!(row.state, OutboundState::Failed | OutboundState::Cancelled) {
            report.rejected.push(RetryRejection {
                id,
                reason: "not terminal".to_string(),
            });
            continue;
        }
        let dir = Path::new(&row.package_ref);
        if !package_has_payload(dir) {
            report.rejected.push(RetryRejection {
                id,
                reason: "package data missing".to_string(),
            });
            continue;
        }
        match engine.enqueue_package(dir, None, Vec::new()).await {
            Ok(new_id) => {
                tracing::info!(old_id = id, new_id, "failed package re-enqueued via web");
                // The retry always routes to the sinked engine (`engines[0]`),
                // regardless of which target failed — per-target retry routing is
                // a separate follow-up (mis-delivery is a *reported* failure with
                // a history row, and each target's own engine auto-retries its
                // non-terminal packages on reconnect, so it is not data loss).
                // Bump the coordinator so this extra row's terminal cannot
                // prematurely free the shared payload of a still-offline target.
                if let Some(coord) = &cleanup {
                    coord.bump(dir, 1);
                }
                report.retried.push(RetryPair { old_id: id, new_id });
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(old_id = id, error = %msg, "web retry: re-enqueue failed");
                report.rejected.push(RetryRejection {
                    id,
                    reason: format!("re-enqueue failed: {msg}"),
                });
            }
        }
    }
    Ok(Json(report))
}

/// `POST /api/kick` — send-now for one or more pending packages (spec §2 wake
/// event, Task 9). Per id: look up the outbound row, reject a terminal row
/// (`terminal` — nothing to wake), else [`kick`](SyncEngineHandle::kick) it so
/// the worker re-announces on the next pass. `503` while the engine is detached
/// (setup mode). Unknown / terminal ids are rejected per-id, never kicked.
///
/// This is the per-row wake; `/api/send-now` (the batcher flush) is unrelated.
async fn api_kick(
    State(state): State<Arc<WebState>>,
    Json(req): Json<RetryRequest>,
) -> Result<Json<KickReport>, (StatusCode, String)> {
    let Some(engine) = state.engine.read().await.clone() else {
        tracing::warn!("web kick: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    };
    let mut report = KickReport::default();
    for &id in &req.ids {
        let row = match state.store.get_outbound(id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                report.rejected.push(RetryRejection {
                    id,
                    reason: "unknown package".to_string(),
                });
                continue;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(id, error = %msg, "web kick: outbound lookup failed");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, msg));
            }
        };
        // A terminal row has no pending slot to wake — reject honestly rather
        // than firing a no-op kick.
        if row.state.is_terminal() {
            report.rejected.push(RetryRejection {
                id,
                reason: "terminal".to_string(),
            });
            continue;
        }
        // The engine's kick channel only errors when the worker has stopped; log
        // it (never swallow) and report it per-id like the retry enqueue path,
        // so one bad id doesn't 500 the whole batch.
        if let Err(e) = engine.kick(id).await {
            let msg = format!("{e:#}");
            tracing::error!(id, error = %msg, "web kick: engine kick failed");
            report.rejected.push(RetryRejection {
                id,
                reason: format!("kick failed: {msg}"),
            });
            continue;
        }
        tracing::info!(id, "package kicked via web");
        report.done.push(id);
    }
    Ok(Json(report))
}

/// `POST /api/cancel` — user-cancel one or more pending packages (Task 9). Per
/// id: look up the outbound row, reject a terminal row (`terminal` — already
/// done), else [`cancel`](SyncEngineHandle::cancel) it (the engine drives it to
/// the `Cancelled` terminal). `503` while the engine is detached (setup mode).
/// A cancelled package is retryable again via `/api/retry`.
async fn api_cancel(
    State(state): State<Arc<WebState>>,
    Json(req): Json<RetryRequest>,
) -> Result<Json<KickReport>, (StatusCode, String)> {
    let Some(engine) = state.engine.read().await.clone() else {
        tracing::warn!("web cancel: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    };
    let mut report = KickReport::default();
    for &id in &req.ids {
        let row = match state.store.get_outbound(id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                report.rejected.push(RetryRejection {
                    id,
                    reason: "unknown package".to_string(),
                });
                continue;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(id, error = %msg, "web cancel: outbound lookup failed");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, msg));
            }
        };
        if row.state.is_terminal() {
            report.rejected.push(RetryRejection {
                id,
                reason: "terminal".to_string(),
            });
            continue;
        }
        if let Err(e) = engine.cancel(id).await {
            let msg = format!("{e:#}");
            tracing::error!(id, error = %msg, "web cancel: engine cancel failed");
            report.rejected.push(RetryRejection {
                id,
                reason: format!("cancel failed: {msg}"),
            });
            continue;
        }
        tracing::info!(id, "package cancel requested via web");
        report.done.push(id);
    }
    Ok(Json(report))
}

/// Map an [`OutboundRow`] to its wire DTO. `deletable` is the single
/// safe-to-delete predicate: only `confirmed` packages.
fn to_sent_dto(r: &OutboundRow) -> SentDto {
    let (files, byte_size) = sent_manifest_summary(&r.package_ref);
    SentDto {
        id: r.id,
        package_ref: r.package_ref.clone(),
        state: r.state.as_str().to_string(),
        attempts: r.attempts,
        created_at: r.created_at.clone(),
        confirmed_at: r.confirmed_at.clone(),
        last_error: r.last_error.clone(),
        deletable: r.state == OutboundState::Confirmed,
        files,
        next_retry_at: r.next_retry_at.clone(),
        byte_size,
    }
}

/// Read a package's manifest ONCE and derive both the operator-facing filenames
/// and the package's total byte size, for the `Sent` row. `files` are the
/// file-name component of each record's `rel_path`, capped at [`SENT_FILES_CAP`];
/// `byte_size` is the sum of EVERY record's `byte_size` (the full manifest, not
/// just the capped names). An unreadable or missing manifest yields
/// `(vec![], 0)` — never an error, so `/api/sent` still lists the row and the UI
/// falls back to the dir basename. T1 keeps the manifest alive through payload
/// cleanup, so a confirmed row still resolves its names + size.
fn sent_manifest_summary(package_ref: &str) -> (Vec<String>, u64) {
    match read_manifest(Path::new(package_ref)) {
        Ok(records) => {
            let byte_size = records.iter().map(|r| r.byte_size).sum();
            let files = records
                .iter()
                .take(SENT_FILES_CAP)
                .map(|r| {
                    Path::new(&r.rel_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(r.rel_path.as_str())
                        .to_string()
                })
                .collect();
            (files, byte_size)
        }
        Err(error) => {
            tracing::debug!(package_ref, %error, "sent: manifest unreadable; no filenames");
            (Vec::new(), 0)
        }
    }
}

/// Map a [`HistoryRow`] to its wire DTO, resolving a friendly peer name (when
/// known) and computing the transfer duration.
fn to_history_dto(r: &HistoryRow, device_names: &HashMap<String, String>) -> HistoryDto {
    HistoryDto {
        filename: r.filename.clone(),
        object: r.object.clone(),
        peer_device: r.peer_device.clone(),
        peer_name: device_names.get(&r.peer_device).cloned(),
        direction: r.direction.as_str().to_string(),
        bytes: r.bytes,
        started_at: r.started_at.clone(),
        finished_at: r.finished_at.clone(),
        duration_secs: duration_secs(&r.started_at, r.finished_at.as_deref()),
        outcome: r.outcome.clone(),
    }
}

/// `finished - started` in seconds, or `None` when the row is unfinished or
/// either stamp fails to parse as RFC3339. Never panics.
fn duration_secs(started: &str, finished: Option<&str>) -> Option<f64> {
    let finished = finished?;
    let s = chrono::DateTime::parse_from_rfc3339(started).ok()?;
    let f = chrono::DateTime::parse_from_rfc3339(finished).ok()?;
    Some((f - s).num_milliseconds() as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::supervisor::AgentState;
    use athenaeum_core::package::{ManifestRecord, PayloadKind, MANIFEST_VERSION};
    use athenaeum_core::sharing::loopback::LoopbackNetwork;
    use athenaeum_core::sharing::SharingTransport;
    use athenaeum_core::sync::{node_id_hex, SyncEngine}; // Direction/HistoryRow come via `super::*`
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt; // for `oneshot`

    const PEER: [u8; 32] = [5u8; 32];

    /// A `WebState` over a temp store seeded with one confirmed + one
    /// transferring outbound row and two history rows (one sent, one received).
    /// The engine is spawned over an EMPTY store so its crash-resume finds
    /// nothing to re-drive; the rows are seeded directly afterwards, invisible
    /// to the idle worker (it never re-polls the DB). Returns the tempdir guard
    /// so the DB file outlives the test.
    async fn test_state() -> (Arc<WebState>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
        // Perseus's seen store shares the same db file under WAL (production wiring).
        let seen = Arc::new(crate::seen::SeenStore::open(tmp.path().join("sync.db")).unwrap());

        // Spawn the engine on the empty store (nothing to resume), then seed.
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let engine = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            transport,
            PEER,
        ));

        let confirmed = store.enqueue("pkg-confirmed", PEER, None, &[]).unwrap();
        store.confirm(confirmed, &[]).unwrap();
        let transferring = store.enqueue("pkg-transferring", PEER, None, &[]).unwrap();
        store
            .set_state(transferring, OutboundState::Transferring)
            .unwrap();

        store
            .append_history(HistoryRow {
                frame_uuid: "uuid-1".into(),
                filename: "frame-0001.fits".into(),
                object: Some("M42".into()),
                peer_device: "aa".repeat(32),
                direction: Direction::Sent,
                bytes: 1000,
                started_at: "2026-07-08T10:00:00.000Z".into(),
                finished_at: Some("2026-07-08T10:00:02.500Z".into()),
                outcome: "sent".into(),
                project: None,
                package_id: None,
                batch_name: None,
            })
            .unwrap();
        store
            .append_history(HistoryRow {
                frame_uuid: "uuid-2".into(),
                filename: "other-0002.fits".into(),
                object: Some("M31".into()),
                peer_device: "bb".repeat(32),
                direction: Direction::Received,
                bytes: 2000,
                started_at: "2026-07-08T11:00:00.000Z".into(),
                finished_at: None, // unfinished → durationSecs is None
                outcome: "ingested".into(),
                project: None,
                package_id: None,
                batch_name: None,
            })
            .unwrap();

        // Materialize the config on disk too, so the PUT-policy handler (which
        // rewrites `config_path` via `apply_retention_edit`) has a real file.
        let toml_str = sample_toml(tmp.path());
        let config_path = tmp.path().join("perseus.toml");
        std::fs::write(&config_path, &toml_str).unwrap();
        let config = Config::from_toml_str(&toml_str).unwrap();
        // The attached (running) shape the handler tests exercise: a live engine,
        // the peer + capture dirs the engine was launched over, and a Running
        // lifecycle state. (The two detached-mode tests use `detached_test_state`.)
        let batches =
            Arc::new(crate::batch_store::BatchStore::open(tmp.path().join("sync.db")).unwrap());
        let (_state_tx, state_rx) = watch::channel(AgentState::Running { in_flight: 0 });
        let state = Arc::new(WebState {
            store,
            seen,
            config_path,
            config: RwLock::new(config.clone()),
            agent_state: state_rx,
            supervisor_wake: Arc::new(Notify::new()),
            engine: RwLock::new(Some(engine)),
            // Single-target test agent: no fan-out coordinator (the retry-bump
            // path is exercised by the coordinator's own unit tests).
            cleanup: RwLock::new(None),
            peer_device: RwLock::new(node_id_hex(&PEER)),
            retention_tx: RwLock::new(watch::channel(config.retention.clone()).0),
            retention_log: RwLock::new(Arc::new(Mutex::new(VecDeque::new()))),
            device_names: RwLock::new(HashMap::new()),
            running_dirs: RwLock::new(config.capture_dirs_resolved()),
            running_targets: RwLock::new(config.targets.clone()),
            // No live batcher in this base harness (the send-workflow tests use
            // `test_state_with_batcher`); the batch store + a placeholder
            // send-config channel keep the new endpoints callable.
            batcher: RwLock::new(None),
            batches,
            send_cfg_tx: RwLock::new(watch::channel(config.send_cfg()).0),
        });
        (state, tmp)
    }

    fn sample_toml(dir: &std::path::Path) -> String {
        // `dir` exists (tempdir), so the capture-dir existence check passes.
        format!(
            "capture_dir = \"{}\"\ndata_dir = \"{}\"\npairing_ticket = \"t\"\nmode = \"auto\"\n[retention]\npolicy = \"keep_days\"\ndry_run = true\nkeep_days = 21\n",
            dir.display(),
            dir.display()
        )
    }

    /// A **detached** (setup-mode) `WebState`: store + seen open, engine absent,
    /// with the supervisor's live [`AgentState`] plumbed through. Built via
    /// [`WebState::detached`] — the exact shape the always-on status page runs in
    /// before the node is signed in and the engine launches.
    async fn detached_test_state(agent: AgentState) -> (Arc<WebState>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
        let seen = Arc::new(crate::seen::SeenStore::open(tmp.path().join("sync.db")).unwrap());
        let batches =
            Arc::new(crate::batch_store::BatchStore::open(tmp.path().join("sync.db")).unwrap());
        let toml_str = sample_toml(tmp.path());
        let config_path = tmp.path().join("perseus.toml");
        std::fs::write(&config_path, &toml_str).unwrap();
        let config = Config::from_toml_str(&toml_str).unwrap();
        // A live watch channel for the lifecycle state; the sender is dropped
        // (borrow() still returns the seeded value after the last sender goes).
        let (_state_tx, state_rx) = watch::channel(agent);
        let state = Arc::new(WebState::detached(
            store,
            seen,
            batches,
            config,
            config_path,
            state_rx,
            Arc::new(Notify::new()),
        ));
        (state, tmp)
    }

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A detached node (engine absent, mid-setup) still serves `/api/status`: it
    /// reports the lifecycle `agentState`/`agentDetail` from the supervisor's
    /// watch channel and an empty in-flight list (no engine to snapshot).
    #[tokio::test]
    async fn status_reports_agent_state_and_empty_in_flight_when_detached() {
        let (state, _tmp) = detached_test_state(AgentState::NeedsSetup {
            needs: vec!["not signed in".to_string()],
        })
        .await;
        let app = build_router(state, None);
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["agentState"], "needs_setup");
        assert!(
            v["agentDetail"].as_str().unwrap().contains("not signed in"),
            "agentDetail carries the setup need: {}",
            v["agentDetail"]
        );
        assert!(
            v["inFlight"].as_array().unwrap().is_empty(),
            "no engine → empty in-flight list, never an error"
        );
    }

    /// `POST /api/retry` on a detached node (engine absent) is a `503`, not a
    /// crash — the operator is told to finish setup first.
    #[tokio::test]
    async fn retry_returns_503_when_engine_absent() {
        let (state, _tmp) = detached_test_state(AgentState::NeedsSetup {
            needs: vec!["not signed in".to_string()],
        })
        .await;
        let app = build_router(state, None);
        let body = serde_json::json!({ "ids": [1] });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/retry")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn status_endpoint_shape() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert!(v["captureDirs"].is_array());
        assert_eq!(v["retention"]["policy"], "keep_days");
        assert_eq!(v["retention"]["keepDays"], 21);
        assert!(v["inFlight"].is_array());
        // One transferring row is in flight; the confirmed one is not.
        assert_eq!(v["inFlight"].as_array().unwrap().len(), 1);
        assert_eq!(v["inFlight"][0]["state"], "transferring");
        assert_eq!(v["inFlight"][0]["deletable"], false);
        assert_eq!(v["counts"]["confirmedTotal"], 1);
        assert_eq!(v["counts"]["queued"], 0);
    }

    #[tokio::test]
    async fn bearer_required_when_token_set() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, Some("s3cret".to_string()));
        let unauth = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let wrong = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .header("authorization", "Bearer nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        // `GET /` (the static page shell) is EXEMPT even when a token is set: it
        // must load without a token so the browser can run its JS token prompt.
        let root = app
            .clone()
            .oneshot(HttpRequest::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            root.status(),
            StatusCode::OK,
            "GET / must load without a token even when one is configured"
        );

        let auth = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .header("authorization", "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(auth.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn no_token_allows_unauthenticated() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "loopback (no token) needs no auth"
        );
    }

    // ── Task 7 (Sync 2C): targets + device-name editors ──────────────────────

    /// `PUT /api/targets` rewrites `perseus.toml`, adopts the new list live, and
    /// reports `restartPending` (the running engines still hold their spawn-time
    /// targets). A fresh `GET` reflects the edit and the file on disk carries it.
    #[tokio::test]
    async fn targets_put_writes_adopts_and_flags_restart_pending() {
        let (state, tmp) = test_state().await;
        let app = build_router(state, None);

        let body = serde_json::json!({ "targets": ["studio", "nas"] });
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/targets")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["configured"], serde_json::json!(["studio", "nas"]));
        assert_eq!(
            v["restartPending"], true,
            "the engines still run over the old (empty) targets until relaunch"
        );

        // The on-disk config carries the new targets.
        let text = std::fs::read_to_string(tmp.path().join("perseus.toml")).unwrap();
        assert!(text.contains("targets"), "targets written to disk: {text}");
        assert!(text.contains("\"nas\""));

        // A follow-up GET reflects the adopted config.
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/targets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        assert_eq!(v["configured"], serde_json::json!(["studio", "nas"]));
    }

    /// `PUT /api/targets` with an empty list is ALLOWED when a dev pairing ticket
    /// is the send route (unlike capture dirs, targets are not always required) —
    /// it validates and returns `200`.
    #[tokio::test]
    async fn targets_put_empty_allowed_with_ticket_route() {
        let (state, _tmp) = test_state().await; // sample config has a pairing_ticket
        let app = build_router(state, None);
        let body = serde_json::json!({ "targets": [] });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/targets")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "empty targets is valid when a pairing ticket provides the send route"
        );
    }

    /// `PUT /api/device-name` writes the trimmed name, adopts it live, and — when
    /// not signed in — makes no hub call, so `hubError` is absent. `GET` then
    /// reflects it.
    #[tokio::test]
    async fn device_name_put_writes_and_no_hub_error_when_signed_out() {
        let (state, tmp) = test_state().await; // sample config has no [account]
        let app = build_router(state, None);

        let body = serde_json::json!({ "name": "  Observatory Pi  " });
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/device-name")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["deviceName"], "Observatory Pi", "name trimmed + saved");
        assert!(v.get("hubError").is_none(), "no hub call when signed out: {v}");

        let text = std::fs::read_to_string(tmp.path().join("perseus.toml")).unwrap();
        assert!(text.contains("device_name = \"Observatory Pi\""), "written to disk: {text}");

        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/device-name")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        assert_eq!(v["deviceName"], "Observatory Pi");
    }

    /// Clearing the device name via `PUT` removes the override (blank → hostname
    /// default): `deviceName` comes back `null` and the key is gone from disk.
    #[tokio::test]
    async fn device_name_put_blank_clears_override() {
        let (state, tmp) = test_state().await;
        let app = build_router(state, None);
        let body = serde_json::json!({ "name": "   " });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/device-name")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert!(v["deviceName"].is_null(), "a blank name clears the override: {v}");
        let text = std::fs::read_to_string(tmp.path().join("perseus.toml")).unwrap();
        assert!(!text.contains("device_name"), "the key is removed from disk: {text}");
    }

    /// A **detached** `WebState` whose on-disk config is account-only with NO
    /// targets and NO pairing_ticket — parse-valid but not run-ready, the exact
    /// fresh-setup shape the owner hit (hub set, not signed in, no targets yet).
    /// The in-memory config is built leniently for the same reason.
    async fn no_target_account_state() -> (Arc<WebState>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
        let seen = Arc::new(crate::seen::SeenStore::open(tmp.path().join("sync.db")).unwrap());
        let batches =
            Arc::new(crate::batch_store::BatchStore::open(tmp.path().join("sync.db")).unwrap());
        let toml_str = format!(
            "capture_dir = \"{d}\"\ndata_dir = \"{d}\"\nmode = \"auto\"\n[account]\nhub_url = \"https://test-hub.artfrom.space\"\n[retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
            d = tmp.path().display()
        );
        let config_path = tmp.path().join("perseus.toml");
        std::fs::write(&config_path, &toml_str).unwrap();
        // Not run-ready (no send target), so build it at the parse-valid tier.
        let config = Config::from_toml_str_lenient(&toml_str).unwrap();
        let (_tx, rx) = watch::channel(AgentState::NeedsSetup {
            needs: vec!["no send target configured".to_string()],
        });
        let state = Arc::new(WebState::detached(
            store,
            seen,
            batches,
            config,
            config_path,
            rx,
            Arc::new(Notify::new()),
        ));
        (state, tmp)
    }

    /// The bug (2026-07-15): `PUT /api/device-name` must succeed even when the
    /// config has no `targets` and no `pairing_ticket`. Field edits validate
    /// config syntax only (parse-valid tier), not run-readiness — so renaming a
    /// device while mid-setup returns `200`, not the `422` "no send target" the
    /// owner hit. No stored token → the best-effort hub rename is a no-op
    /// (`hubError` absent).
    #[tokio::test]
    async fn device_name_put_succeeds_without_send_target() {
        let (state, tmp) = no_target_account_state().await;
        let app = build_router(state, None);

        let body = serde_json::json!({ "name": "Test Instance" });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/device-name")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "a device rename must not be blocked by a missing send target"
        );
        let v = body_json(res).await;
        assert_eq!(v["deviceName"], "Test Instance", "name saved");
        assert!(v.get("hubError").is_none(), "no hub call when signed out: {v}");

        let text = std::fs::read_to_string(tmp.path().join("perseus.toml")).unwrap();
        assert!(
            text.contains("device_name = \"Test Instance\""),
            "written to disk: {text}"
        );
    }

    // ── M2: DNS-rebinding Host guard ─────────────────────────────────────────

    #[test]
    fn host_policy_loopback_allows_localhost_and_rejects_foreign() {
        let policy = HostPolicy::for_bind("127.0.0.1:8686".parse().unwrap());
        assert!(policy.permits(Some("localhost:8686")));
        assert!(policy.permits(Some("127.0.0.1:8686")));
        assert!(policy.permits(Some("127.0.0.1")));
        assert!(policy.permits(None), "a missing Host is not a rebinding attack");
        assert!(!policy.permits(Some("evil.com")), "a foreign host is rejected");
        assert!(
            !policy.permits(Some("attacker.example:8686")),
            "a rebinding host with the right port is still rejected"
        );
    }

    #[test]
    fn host_policy_wildcard_allows_all() {
        let policy = HostPolicy::for_bind("0.0.0.0:8686".parse().unwrap());
        assert!(policy.permits(Some("anything.example")));
        assert!(policy.permits(None));
    }

    /// Integration: the guard layer wrapping a real router 403s a foreign Host
    /// and passes a loopback Host through to the handler.
    #[tokio::test]
    async fn host_guard_layer_blocks_rebinding_host() {
        let (state, _tmp) = test_state().await;
        let policy = HostPolicy::for_bind("127.0.0.1:8686".parse().unwrap());
        let app = apply_host_guard(build_router(state, None), policy);

        let evil = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .header("host", "evil.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            evil.status(),
            StatusCode::FORBIDDEN,
            "a foreign Host must be refused before reaching the handler"
        );

        let ok = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/status")
                    .header("host", "127.0.0.1:8686")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK, "a loopback Host passes through");
    }

    #[tokio::test]
    async fn sent_lists_all_states_and_filters_by_state() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);

        // Unfiltered → both rows, with their state strings + deletable flag.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/sent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let states: Vec<&str> = rows.iter().map(|r| r["state"].as_str().unwrap()).collect();
        assert!(states.contains(&"confirmed"));
        assert!(states.contains(&"transferring"));
        // deletable is exactly `state == confirmed`.
        for r in rows {
            let expect = r["state"] == "confirmed";
            assert_eq!(r["deletable"], expect);
        }

        // Filtered → only the confirmed one.
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/sent?state=confirmed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "confirmed");
        assert_eq!(rows[0]["deletable"], true);
    }

    /// Write a real package dir: a `manifest.ndjson` with one record per
    /// `rel_path`, no payload files (we only read the manifest). Returns the dir.
    fn write_manifest_package(dir: &std::path::Path, rel_paths: &[&str]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let mut ndjson = String::new();
        for (i, rp) in rel_paths.iter().enumerate() {
            let rec = ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("uuid-{i}"),
                origin_catalog_uuid: format!("uuid-{i}"),
                origin_device: "self-node".to_string(),
                payload_kind: PayloadKind::RawFrame,
                rel_path: rp.to_string(),
                byte_size: 0,
                xxh3: "0".repeat(16),
                frame_meta: serde_json::json!({}),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            };
            ndjson.push_str(&serde_json::to_string(&rec).unwrap());
            ndjson.push('\n');
        }
        std::fs::write(dir.join(MANIFEST_FILENAME), ndjson).unwrap();
        dir.to_path_buf()
    }

    /// `/api/sent` surfaces the package's filenames (the file-name component of
    /// each manifest `rel_path`), capped at `SENT_FILES_CAP`; a row whose
    /// package dir has no readable manifest reports an empty `files` (never an
    /// error — the row still lists).
    #[tokio::test]
    async fn sent_reports_manifest_filenames_capped_and_empty_when_unreadable() {
        let (state, tmp) = test_state().await;

        // A real one-file package → its filename surfaces (dir stripped).
        let pkg = write_manifest_package(&tmp.path().join("pkg-real"), &["frames/light-0009.fits"]);
        state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();

        // Seven files → capped at SENT_FILES_CAP (6).
        let many: Vec<String> = (0..7).map(|i| format!("f-{i}.fits")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let big = write_manifest_package(&tmp.path().join("pkg-many"), &refs);
        state.store.enqueue(&big.to_string_lossy(), PEER, None, &[]).unwrap();

        let app = build_router(state, None);
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/sent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();

        let real = rows
            .iter()
            .find(|r| r["packageRef"].as_str().unwrap().ends_with("pkg-real"))
            .unwrap();
        assert_eq!(real["files"].as_array().unwrap().len(), 1);
        assert_eq!(
            real["files"][0], "light-0009.fits",
            "file-name component only"
        );

        let big_row = rows
            .iter()
            .find(|r| r["packageRef"].as_str().unwrap().ends_with("pkg-many"))
            .unwrap();
        assert_eq!(
            big_row["files"].as_array().unwrap().len(),
            SENT_FILES_CAP,
            "the server caps filenames at SENT_FILES_CAP"
        );

        // The seeded bogus-ref rows have no manifest on disk → empty files.
        let bogus = rows
            .iter()
            .find(|r| r["packageRef"] == "pkg-confirmed")
            .unwrap();
        assert!(
            bogus["files"].as_array().unwrap().is_empty(),
            "an unreadable manifest yields empty files, not an error row"
        );
    }

    #[tokio::test]
    async fn history_filters_and_computes_duration_and_peer_name() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);

        // Unfiltered → both rows.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/history")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        assert_eq!(v.as_array().unwrap().len(), 2);
        // The finished 'sent' row carries a 2.5s duration; peerName is None
        // (no device-name cache until Task 11).
        let sent = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["direction"] == "sent")
            .unwrap();
        assert_eq!(sent["filename"], "frame-0001.fits");
        assert_eq!(sent["durationSecs"], 2.5);
        assert!(sent["peerName"].is_null());
        // The unfinished 'received' row has no duration.
        let recv = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["direction"] == "received")
            .unwrap();
        assert!(recv["durationSecs"].is_null());

        // Exact-filename filter.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/history?query=frame-0001.fits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["filename"], "frame-0001.fits");

        // Direction filter.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/history?direction=received")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["direction"], "received");

        // Bad direction → 400.
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/history?direction=sideways")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn index_is_served() {
        let (state, _tmp) = test_state().await;
        let app = build_router(state, None);
        let res = app
            .oneshot(HttpRequest::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    // ── Task 10: manual delete, retention policy GET/PUT, retention log ───────

    /// Manual delete is the same confirmed()-only chokepoint retention uses: a
    /// confirmed package's source is removed (with a `deleted_manual` audit row),
    /// while a non-confirmed id is rejected with a reason and never touched.
    #[tokio::test]
    async fn delete_rejects_non_confirmed() {
        let (state, tmp) = test_state().await;

        // Register a real on-disk source file for the seeded confirmed package.
        let source = tmp.path().join("light-A.fits");
        std::fs::write(&source, b"confirmed-source-bytes").unwrap();
        let meta = std::fs::metadata(&source).unwrap();
        let mtime = crate::seen::mtime_millis(meta.modified().ok());
        state
            .seen
            .mark_enqueued(&source, meta.len(), mtime, "pkg-confirmed")
            .unwrap();

        // Resolve the two seeded outbound ids by state.
        let rows = state.store.all_outbound(100).unwrap();
        let a = rows
            .iter()
            .find(|r| r.state == OutboundState::Confirmed)
            .unwrap()
            .id;
        let b = rows
            .iter()
            .find(|r| r.state == OutboundState::Transferring)
            .unwrap()
            .id;

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let body = serde_json::json!({ "ids": [a, b] });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/delete")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;

        let deleted = v["deleted"].as_array().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(
            deleted[0].as_i64().unwrap(),
            a,
            "only the confirmed package is deleted"
        );

        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), b);
        assert_eq!(rejected[0]["reason"], "not confirmed");

        assert!(
            !source.exists(),
            "the confirmed package's source is removed from disk"
        );

        let hist = store
            .search_history(HistoryQuery {
                filename: None,
                object: None,
                direction: None,
                peer: None,
                project: None,
                package_id: None,
                limit: 1000,
            })
            .unwrap();
        let audit: Vec<_> = hist
            .iter()
            .filter(|h| h.outcome == "deleted_manual")
            .collect();
        assert_eq!(
            audit.len(),
            1,
            "exactly one deleted_manual audit row for the confirmed package"
        );
        // The audit row stamps the CONFIGURED SYNC PEER (the same hex transfer
        // rows carry), not this agent's own node id — the earlier bug stamped
        // the manifest's `origin_device` (self).
        assert_eq!(
            audit[0].peer_device,
            node_id_hex(&PEER),
            "the deleted_manual row is stamped with the sync peer, not self"
        );
    }

    /// GET returns the live policy + read-only soak indicators; a valid PUT is
    /// adopted (watch receiver + file rewritten); a `dry_run = false` PUT is
    /// refused 422 and leaves the file byte-identical (the web can never enable
    /// live deletion — `RetentionEdit` has no soak field).
    #[tokio::test]
    async fn retention_policy_roundtrip() {
        let (state, _tmp) = test_state().await;
        // A live receiver so the PUT's watch send is both observable and succeeds.
        let mut rx = state.retention_tx.read().await.subscribe();
        let config_path = state.config_path.clone();
        let app = build_router(state, None);

        // GET — current values + read-only soak gate.
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/retention/policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["policy"], "keep_days");
        assert_eq!(v["keepDays"], 21);
        assert_eq!(v["dryRun"], true);
        assert_eq!(v["soakOptIn"], false, "soak opt-in is read-only and off");
        assert_eq!(v["liveDeletionPossible"], false);

        // PUT a valid edit.
        let edit = crate::config_edit::RetentionEdit {
            policy: crate::config::RetentionPolicy::KeepDays,
            keep_days: 14,
            disk_max_pct: 90,
            interval_secs: 1800,
            dry_run: true,
        };
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/retention/policy")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&edit).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "a valid edit is accepted");

        assert_eq!(
            rx.borrow_and_update().keep_days,
            14,
            "the running retention loop's watch receiver adopts the edit"
        );
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            text.contains("keep_days = 14"),
            "the config file was rewritten: {text}"
        );

        // PUT dry_run = false with no soak opt-in in the file → 422, file untouched.
        let before = std::fs::read_to_string(&config_path).unwrap();
        let bad = crate::config_edit::RetentionEdit {
            policy: crate::config::RetentionPolicy::KeepDays,
            keep_days: 14,
            disk_max_pct: 90,
            interval_secs: 1800,
            dry_run: false,
        };
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/retention/policy")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&bad).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "the web can never enable live deletion"
        );
        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(
            after, before,
            "a rejected edit leaves the file byte-identical"
        );
    }

    /// The retention-run log endpoint serializes the ring buffer newest-first.
    #[tokio::test]
    async fn retention_log_returns_ring_buffer() {
        let (state, _tmp) = test_state().await;
        {
            let log_handle = state.retention_log.read().await;
            let mut log = log_handle.lock().unwrap();
            // Push oldest first; each push_front puts the newest at the head.
            log.push_front(RetentionRunRecord {
                at: "2026-07-08T10:00:00.000Z".into(),
                dry_run: true,
                policy: "keep_days".into(),
                deleted: vec![],
                would_delete: vec!["/cap/a.fits".into()],
                errors: vec![],
            });
            log.push_front(RetentionRunRecord {
                at: "2026-07-08T11:00:00.000Z".into(),
                dry_run: true,
                policy: "keep_days".into(),
                deleted: vec!["/cap/b.fits".into()],
                would_delete: vec![],
                errors: vec![],
            });
        }
        let app = build_router(state, None);
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/retention/log")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["at"], "2026-07-08T11:00:00.000Z", "newest first");
        assert_eq!(rows[0]["deleted"][0], "/cap/b.fits");
        assert_eq!(rows[1]["wouldDelete"][0], "/cap/a.fits");
    }

    // ── Task 3 (S1.5.1): capture-dirs editor (restart-to-apply) ───────────────

    async fn get_capture_dirs(app: &Router) -> serde_json::Value {
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/capture-dirs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        body_json(res).await
    }

    async fn put_capture_dirs(app: &Router, dirs: &[String]) -> Response {
        let body = serde_json::json!({ "dirs": dirs });
        app.clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/capture-dirs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// GET reports `configured`/`runtime`/`restartPending`. A valid PUT rewrites
    /// `perseus.toml` to the array form and adopts it into the live config, so
    /// `configured` reflects the edit while `runtime` stays the spawn-time
    /// snapshot — making `restartPending` true. The pending flag is server-
    /// derived, so a subsequent GET still reports it (survives reloads).
    #[tokio::test]
    async fn capture_dirs_get_and_put_roundtrip() {
        let (state, _tmp) = test_state().await;
        let config_path = state.config_path.clone();
        // The spawn-time runtime snapshot — never mutated by a web edit.
        let runtime_snapshot: Vec<String> = state
            .running_dirs
            .read()
            .await
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let app = build_router(state, None);

        // GET before any edit: configured == runtime, nothing pending.
        let v = get_capture_dirs(&app).await;
        assert!(v["configured"].is_array());
        assert!(v["runtime"].is_array());
        assert_eq!(v["restartPending"], false, "no edit yet → not pending");
        assert_eq!(
            v["configured"], v["runtime"],
            "configured mirrors the runtime snapshot"
        );

        // PUT a new, existing directory.
        let newdir = tempfile::tempdir().unwrap();
        let new = newdir.path().display().to_string();
        let res = put_capture_dirs(&app, &[new.clone()]).await;
        assert_eq!(res.status(), StatusCode::OK, "a valid edit is accepted");
        let v = body_json(res).await;
        assert_eq!(
            v["restartPending"], true,
            "config changed but runtime is still the spawn snapshot → pending"
        );
        assert_eq!(v["configured"].as_array().unwrap().len(), 1);
        assert_eq!(v["configured"][0], new, "the live config adopts the edit");

        // The file was rewritten to the array form with the singular key removed.
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(text.contains("capture_dirs"), "array form written: {text}");
        assert!(
            !text.contains("capture_dir ="),
            "singular key removed: {text}"
        );

        // A later GET still reports pending (server-derived), and `runtime` is
        // unchanged from the spawn snapshot.
        let v = get_capture_dirs(&app).await;
        assert_eq!(
            v["restartPending"], true,
            "pending survives across requests"
        );
        let runtime_now: Vec<String> = v["runtime"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            runtime_now, runtime_snapshot,
            "runtime stays the spawn-time snapshot"
        );
    }

    /// A PUT naming a directory that does not exist is refused `422` and leaves
    /// the config file byte-identical (edit-on-copy, write-after-validate).
    #[tokio::test]
    async fn capture_dirs_put_nonexistent_rejected_byte_identical() {
        let (state, tmp) = test_state().await;
        let config_path = state.config_path.clone();
        let before = std::fs::read_to_string(&config_path).unwrap();
        let app = build_router(state, None);

        let missing = tmp.path().join("does-not-exist");
        let res = put_capture_dirs(&app, &[missing.display().to_string()]).await;
        assert_eq!(
            res.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a non-existent directory is rejected"
        );
        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(
            after, before,
            "a rejected edit leaves the file byte-identical"
        );
    }

    /// A valid capture-dirs PUT rings the supervisor's wake so the edit is
    /// adopted live — the supervisor reloads config each pass and restarts the
    /// engine when `running_dirs != configured`, no process restart. The wake
    /// future is armed BEFORE the PUT so a `notify_one` fired inside the handler
    /// cannot race ahead of the assertion.
    #[tokio::test]
    async fn capture_dirs_put_wakes_supervisor() {
        use std::time::Duration;

        let (state, _tmp) = test_state().await;
        let wake = state.supervisor_wake.clone();
        let state_ref = Arc::clone(&state);
        let app = build_router(state, None);

        // Arm the wake future BEFORE issuing the PUT (per the brief) so the
        // handler's `notify_one` cannot be missed.
        let woken = wake.notified();
        tokio::pin!(woken);

        let newdir = tempfile::tempdir().unwrap();
        let new = newdir.path().display().to_string();
        let res = put_capture_dirs(&app, &[new.clone()]).await;
        assert_eq!(res.status(), StatusCode::OK, "a valid edit is accepted");

        tokio::time::timeout(Duration::from_secs(1), woken)
            .await
            .expect("a capture-dirs edit must wake the supervisor");

        // The live config lock adopted the new selection.
        let configured: Vec<String> = state_ref
            .config
            .read()
            .await
            .capture_dirs_resolved()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            configured,
            vec![new],
            "the config lock shows the new dirs"
        );
    }

    // ── Task 2 (S1.5.1): retry failed packages ───────────────────────────────

    /// Build a real package dir under `base`: a `manifest.ndjson` plus, unless
    /// `manifest_only`, one payload file — the shape task 1 preserves for a
    /// non-confirmed package (confirmed dirs are cleaned to manifest-only).
    fn make_package_dir(base: &std::path::Path, name: &str, manifest_only: bool) -> PathBuf {
        let pkg = base.join(name);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("manifest.ndjson"), b"{}\n").unwrap();
        if !manifest_only {
            std::fs::write(pkg.join("frame-0001.fits"), b"payload-bytes").unwrap();
        }
        pkg
    }

    async fn post_retry(app: Router, ids: &[i64]) -> serde_json::Value {
        let body = serde_json::json!({ "ids": ids });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/retry")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        body_json(res).await
    }

    /// A failed package whose dir still holds its manifest + payload (task 1
    /// keeps non-confirmed payloads) re-enqueues: a brand-new outbound row is
    /// created for the same package dir, the response maps old→new id, and the
    /// original failed row is left untouched (the sanctioned retry model — the
    /// old row stays failed, the new row lives its own lifecycle).
    #[tokio::test]
    async fn retry_reenqueues_failed_with_intact_payload() {
        let (state, tmp) = test_state().await;
        let pkg = make_package_dir(tmp.path(), "pkg-failed-intact", false);
        let old_id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state
            .store
            .set_state(old_id, OutboundState::Failed)
            .unwrap();

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let v = post_retry(app, &[old_id]).await;

        let retried = v["retried"].as_array().unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0]["oldId"].as_i64().unwrap(), old_id);
        let new_id = retried[0]["newId"].as_i64().unwrap();
        assert_ne!(new_id, old_id, "a brand-new row id, not the old one");
        assert!(v["rejected"].as_array().unwrap().is_empty());

        // A real new row exists for the same package dir…
        let new_row = store.get_outbound(new_id).unwrap().expect("new row exists");
        assert_eq!(new_row.package_ref, pkg.to_string_lossy());
        // …and the original failed row is untouched.
        assert_eq!(
            store.get_outbound(old_id).unwrap().unwrap().state,
            OutboundState::Failed,
            "the old failed row is left as-is"
        );
    }

    /// A non-terminal id (here: the seeded `transferring` package) is rejected
    /// "not terminal" and never re-enqueued — no new row is created.
    #[tokio::test]
    async fn retry_rejects_non_terminal() {
        let (state, _tmp) = test_state().await;
        let before = state.store.all_outbound(100).unwrap();
        let transferring = before
            .iter()
            .find(|r| r.state == OutboundState::Transferring)
            .unwrap()
            .id;

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let v = post_retry(app, &[transferring]).await;

        assert!(v["retried"].as_array().unwrap().is_empty());
        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), transferring);
        assert_eq!(rejected[0]["reason"], "not terminal");
        assert_eq!(
            store.all_outbound(100).unwrap().len(),
            before.len(),
            "no new row created for a rejected retry"
        );
    }

    /// Task 9: a **cancelled** package (user gave up) is retryable exactly like a
    /// failed one — provided its payload is still on disk. A brand-new outbound
    /// row is minted; the original cancelled row is left untouched.
    #[tokio::test]
    async fn retry_reenqueues_cancelled_with_intact_payload() {
        let (state, tmp) = test_state().await;
        let pkg = make_package_dir(tmp.path(), "pkg-cancelled-intact", false);
        let old_id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state
            .store
            .set_state(old_id, OutboundState::Cancelled)
            .unwrap();

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let v = post_retry(app, &[old_id]).await;

        let retried = v["retried"].as_array().unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0]["oldId"].as_i64().unwrap(), old_id);
        let new_id = retried[0]["newId"].as_i64().unwrap();
        assert_ne!(new_id, old_id, "a brand-new row id, not the old one");
        assert!(v["rejected"].as_array().unwrap().is_empty());
        assert_eq!(
            store.get_outbound(old_id).unwrap().unwrap().state,
            OutboundState::Cancelled,
            "the old cancelled row is left as-is"
        );
    }

    /// A failed package whose dir was cleaned to manifest-only (the task-1
    /// confirmed-then-cleaned shape) has nothing left to re-send: rejected
    /// "package data missing", honestly — no new row.
    #[tokio::test]
    async fn retry_rejects_missing_payload() {
        let (state, tmp) = test_state().await;
        let pkg = make_package_dir(tmp.path(), "pkg-manifest-only", true);
        let id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state.store.set_state(id, OutboundState::Failed).unwrap();

        let store = Arc::clone(&state.store);
        let before = store.all_outbound(100).unwrap().len();
        let app = build_router(state, None);
        let v = post_retry(app, &[id]).await;

        assert!(v["retried"].as_array().unwrap().is_empty());
        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), id);
        assert_eq!(rejected[0]["reason"], "package data missing");
        assert_eq!(
            store.all_outbound(100).unwrap().len(),
            before,
            "no new row created for a data-missing reject"
        );
    }

    // ── Task 9: kick / cancel endpoints, aggregate_outcome, DTO fields ────────

    /// An [`OutboundRow`] in a given state for the pure `aggregate_outcome`
    /// truth-table test (the other fields are irrelevant to it).
    fn ob(state: OutboundState) -> OutboundRow {
        OutboundRow {
            id: 0,
            package_ref: "pkg".to_string(),
            peer: PEER,
            state,
            attempts: 0,
            created_at: "2026-07-16T00:00:00Z".to_string(),
            confirmed_at: None,
            last_error: None,
            next_retry_at: None,
            wire_package_id: None,
            display_name: None,
            project_id: None,
        }
    }

    /// The batch-outcome truth table — the regression is a batch that includes a
    /// `Cancelled` terminal (Task 3): the old code matched neither the all-Failed
    /// nor the all-Confirmed branch, so it reported "sending" **forever**. An
    /// all-terminal batch must resolve to a terminal outcome.
    #[test]
    fn aggregate_outcome_truth_table() {
        use OutboundState::*;
        let (confirmed, cancelled, failed, transferring) =
            (ob(Confirmed), ob(Cancelled), ob(Failed), ob(Transferring));

        // Empty (rows not visible yet) → sending, never an error.
        assert_eq!(aggregate_outcome(&[]), "sending");
        // All confirmed → confirmed.
        assert_eq!(aggregate_outcome(&[&confirmed, &confirmed]), "confirmed");
        // The regression: one cancelled among confirmed. All terminal → must NOT
        // be "sending"; a user-cancel present → "cancelled".
        assert_eq!(
            aggregate_outcome(&[&confirmed, &cancelled]),
            "cancelled",
            "a cancelled-among-confirmed batch is terminal, never stuck at sending"
        );
        assert_eq!(aggregate_outcome(&[&cancelled, &cancelled]), "cancelled");
        // Any failed dominates (even alongside a cancelled).
        assert_eq!(aggregate_outcome(&[&failed, &cancelled]), "failed");
        assert_eq!(aggregate_outcome(&[&failed, &confirmed]), "failed");
        // A still-in-flight target keeps the batch "sending" — even next to a
        // cancelled one (the batch as a whole is not done).
        assert_eq!(aggregate_outcome(&[&confirmed, &transferring]), "sending");
        assert_eq!(aggregate_outcome(&[&cancelled, &transferring]), "sending");
    }

    /// `/api/status` buckets a cancelled terminal into `cancelledTotal` (Task 9)
    /// — it must not vanish from the tallies the way it did before.
    #[tokio::test]
    async fn status_counts_bucket_cancelled() {
        let (state, _tmp) = test_state().await;
        let id = state.store.enqueue("pkg-cancelled", PEER, None, &[]).unwrap();
        state.store.set_state(id, OutboundState::Cancelled).unwrap();
        let app = build_router(state, None);
        let v = body_json(get(&app, "/api/status").await).await;
        assert_eq!(v["counts"]["cancelledTotal"], 1);
        assert_eq!(v["counts"]["confirmedTotal"], 1, "the seeded confirmed row still counts");
        assert_eq!(v["counts"]["failedTotal"], 0);
    }

    /// A package dir with a real manifest whose records carry the given
    /// `(rel_path, byte_size)` pairs — for the `byteSize` DTO test.
    fn write_sized_manifest_package(dir: &std::path::Path, files: &[(&str, u64)]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let mut ndjson = String::new();
        for (i, (rp, size)) in files.iter().enumerate() {
            let rec = ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("uuid-{i}"),
                origin_catalog_uuid: format!("uuid-{i}"),
                origin_device: "self-node".to_string(),
                payload_kind: PayloadKind::RawFrame,
                rel_path: rp.to_string(),
                byte_size: *size,
                xxh3: "0".repeat(16),
                frame_meta: serde_json::json!({}),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            };
            ndjson.push_str(&serde_json::to_string(&rec).unwrap());
            ndjson.push('\n');
        }
        std::fs::write(dir.join(MANIFEST_FILENAME), ndjson).unwrap();
        dir.to_path_buf()
    }

    /// `/api/sent` surfaces `nextRetryAt` (straight from the row) and `byteSize`
    /// (sum over the FULL manifest, not just the capped `files`) — Task 9.
    #[tokio::test]
    async fn sent_reports_next_retry_at_and_byte_size() {
        let (state, tmp) = test_state().await;
        let pkg = write_sized_manifest_package(
            &tmp.path().join("pkg-sized"),
            &[("a.fits", 1000), ("b.fits", 2000), ("c.fits", 3000)],
        );
        let id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state
            .store
            .set_next_retry_at(id, Some("2026-07-16T12:00:00Z"))
            .unwrap();

        let app = build_router(state, None);
        let v = body_json(get(&app, "/api/sent").await).await;
        let row = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["packageRef"].as_str().unwrap().ends_with("pkg-sized"))
            .unwrap();
        assert_eq!(row["byteSize"].as_u64().unwrap(), 6000, "sum of every record");
        assert_eq!(row["nextRetryAt"], "2026-07-16T12:00:00Z");
    }

    async fn post_ids(app: Router, uri: &str, ids: &[i64]) -> serde_json::Value {
        let body = serde_json::json!({ "ids": ids });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        body_json(res).await
    }

    /// `POST /api/kick` on a non-terminal (transferring) row reports it `done`;
    /// a terminal (confirmed) row is rejected "terminal". The engine's kick is a
    /// no-op on a seeded row with no live slot, but still succeeds.
    #[tokio::test]
    async fn kick_acts_on_pending_rejects_terminal() {
        let (state, _tmp) = test_state().await;
        let rows = state.store.all_outbound(100).unwrap();
        let transferring = rows
            .iter()
            .find(|r| r.state == OutboundState::Transferring)
            .unwrap()
            .id;
        let confirmed = rows
            .iter()
            .find(|r| r.state == OutboundState::Confirmed)
            .unwrap()
            .id;
        let app = build_router(state, None);
        let v = post_ids(app, "/api/kick", &[transferring, confirmed]).await;
        assert_eq!(v["done"].as_array().unwrap(), &[serde_json::json!(transferring)]);
        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), confirmed);
        assert_eq!(rejected[0]["reason"], "terminal");
    }

    /// `POST /api/cancel` on a non-terminal row reports it `done`; a terminal
    /// (confirmed) row is rejected "terminal"; an unknown id is rejected too.
    #[tokio::test]
    async fn cancel_acts_on_pending_rejects_terminal_and_unknown() {
        let (state, _tmp) = test_state().await;
        let rows = state.store.all_outbound(100).unwrap();
        let transferring = rows
            .iter()
            .find(|r| r.state == OutboundState::Transferring)
            .unwrap()
            .id;
        let confirmed = rows
            .iter()
            .find(|r| r.state == OutboundState::Confirmed)
            .unwrap()
            .id;
        let app = build_router(state, None);
        let v = post_ids(app, "/api/cancel", &[transferring, confirmed, 99999]).await;
        assert_eq!(v["done"].as_array().unwrap(), &[serde_json::json!(transferring)]);
        let rejected = v["rejected"].as_array().unwrap();
        let reason_for = |id: i64| {
            rejected
                .iter()
                .find(|r| r["id"].as_i64() == Some(id))
                .map(|r| r["reason"].as_str().unwrap().to_string())
        };
        assert_eq!(reason_for(confirmed).as_deref(), Some("terminal"));
        assert_eq!(reason_for(99999).as_deref(), Some("unknown package"));
    }

    /// Both write endpoints 503 on a detached node (engine absent, setup mode) —
    /// there is nothing to kick/cancel into yet.
    #[tokio::test]
    async fn kick_and_cancel_return_503_when_engine_absent() {
        for uri in ["/api/kick", "/api/cancel"] {
            let (state, _tmp) = detached_test_state(AgentState::NeedsSetup {
                needs: vec!["not signed in".to_string()],
            })
            .await;
            let app = build_router(state, None);
            let res = app
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&serde_json::json!({ "ids": [1] })).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri} needs the engine");
        }
    }

    // ── Task 5: account sign-in (/api/account/*) ──────────────────────────────

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A **detached** `WebState` whose config carries an `[account]` table
    /// pointing at `hub_url`, over a real (temp) data dir the sign-in core writes
    /// the token + pairing cache into. Engine absent (setup mode) — the exact
    /// shape the always-on page runs in before the node is signed in.
    async fn account_test_state(hub_url: &str) -> (Arc<WebState>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
        let seen = Arc::new(crate::seen::SeenStore::open(tmp.path().join("sync.db")).unwrap());
        let batches =
            Arc::new(crate::batch_store::BatchStore::open(tmp.path().join("sync.db")).unwrap());
        let config = Config {
            capture_dir: Some(tmp.path().to_path_buf()),
            capture_dirs: Vec::new(),
            data_dir: tmp.path().to_path_buf(),
            pairing_ticket: None,
            account: Some(crate::config::AccountConfig {
                hub_url: hub_url.to_string(),
                // No email in the file — the sign-in form supplies it and it is
                // cached; account_status must surface it via the cache fallback.
                email: None,
                allow_default_relays: false,
            }),
            targets: Vec::new(),
            device_name: None,
            mode: crate::config::Mode::Auto,
            auto_quiet_secs: crate::config::DEFAULT_AUTO_QUIET_SECS,
            retention: RetentionConfig::default(),
            stability_secs: 1,
            poll_interval_secs: 1,
            web_bind: String::new(),
            web_token: None,
        };
        let config_path = tmp.path().join("perseus.toml");
        let (_tx, rx) = watch::channel(AgentState::NeedsSetup {
            needs: vec!["not signed in".to_string()],
        });
        let state = Arc::new(WebState::detached(
            store,
            seen,
            batches,
            config,
            config_path,
            rx,
            Arc::new(Notify::new()),
        ));
        (state, tmp)
    }

    /// Mount the two hub endpoints a successful web sign-in touches: verify →
    /// token + device id, and the device list (to refresh the friendly-name
    /// cache). Sync 2C registers the Perseus capability at verify; there is no
    /// role/primary-pairing call.
    async fn mount_successful_signin(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceToken": "tok-secret-xyz",
                "deviceId": "perseus-dev",
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "studio-1", "name": "Studio", "pubkey": "cHVia2V5",
                    "capability": "athenaeum",
                    "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null
                }
            ])))
            .mount(server)
            .await;
    }

    async fn post_json(app: &Router, uri: &str, body: serde_json::Value) -> Response {
        app.clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn get(app: &Router, uri: &str) -> Response {
        app.clone()
            .oneshot(HttpRequest::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// The happy path: request a code, verify it (→ signed in, and the supervisor
    /// is woken so the engine can start), read the account snapshot, then log out
    /// (→ signed out again). The wake future is armed BEFORE the verify request so
    /// there is no race between `notify_one` and the assertion.
    #[tokio::test]
    async fn account_flow_signs_in_and_wakes_supervisor() {
        use std::time::Duration;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/otp"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        mount_successful_signin(&server).await;

        let (state, _tmp) = account_test_state(&server.uri()).await;
        let wake = state.supervisor_wake.clone();
        let app = build_router(state, None);

        // request-code → 200.
        let res = post_json(&app, "/api/account/request-code", serde_json::json!({ "email": "u@e.com" })).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Arm the wake future BEFORE the verify request (per the brief) so a
        // `notify_one` during the handler cannot be missed.
        let woken = wake.notified();
        tokio::pin!(woken);

        let res = post_json(
            &app,
            "/api/account/verify",
            serde_json::json!({ "email": "u@e.com", "code": "123456" }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["signedIn"], true, "verify returns the signed-in snapshot");
        assert_eq!(v["email"], "u@e.com");

        tokio::time::timeout(Duration::from_secs(1), woken)
            .await
            .expect("verify must wake the supervisor");

        // GET /api/account reflects the signed-in state.
        let v = body_json(get(&app, "/api/account").await).await;
        assert_eq!(v["signedIn"], true);
        assert_eq!(v["email"], "u@e.com");
        assert_eq!(v["deviceId"], "perseus-dev", "the signed-in device id is surfaced");

        // Logout → signed out again.
        let res = post_json(&app, "/api/account/logout", serde_json::json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(get(&app, "/api/account").await).await;
        assert_eq!(v["signedIn"], false, "logout clears the signed-in state");
    }

    /// A hub failure on verify passes through honestly as `502` carrying the
    /// error text (never swallowed), and stores nothing — `signedIn` stays false.
    #[tokio::test]
    async fn account_endpoints_pass_hub_errors_through() {
        let server = MockServer::start().await;
        // The hub rejects the code. (401 → the client maps to its Unauthorized
        // rendering; the point is the failure surfaces, not the literal body.)
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(serde_json::json!({ "error": "bad code" })),
            )
            .mount(&server)
            .await;

        let (state, _tmp) = account_test_state(&server.uri()).await;
        let app = build_router(state, None);

        let res = post_json(
            &app,
            "/api/account/verify",
            serde_json::json!({ "email": "u@e.com", "code": "000000" }),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_GATEWAY,
            "a hub failure is a 502, not a swallowed error"
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            body.contains("verify code"),
            "the hub error chain passes through to the body: {body}"
        );

        // Nothing was stored: the account is still signed out.
        let v = body_json(get(&app, "/api/account").await).await;
        assert_eq!(v["signedIn"], false, "a failed verify stores nothing");
    }

    /// The account endpoints live behind the bearer gate: with a token configured,
    /// an unauthenticated request is refused `401` before the handler runs (no hub
    /// is even contacted).
    #[tokio::test]
    async fn account_endpoints_are_behind_bearer_gate() {
        // Hub URL is never dialed — the auth layer rejects first.
        let (state, _tmp) = account_test_state("http://127.0.0.1:1").await;
        let app = build_router(state, Some("s3cret".to_string()));

        let res = post_json(&app, "/api/account/request-code", serde_json::json!({ "email": "u@e.com" })).await;
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "no bearer token → 401 before the handler"
        );

        // GET /api/account is gated too.
        let res = get(&app, "/api/account").await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // ── Send-target picker options (/api/targets/options) ─────────────────────

    /// Signed out (no stored token): the picker endpoint reports `signedIn:false`
    /// with an empty device list and no error — a clean degrade, and the hub is
    /// never dialed (the loopback url below would fail if it were).
    #[tokio::test]
    async fn target_options_signed_out_is_empty() {
        let (state, _tmp) = account_test_state("http://127.0.0.1:1").await;
        let app = build_router(state, None);

        let v = body_json(get(&app, "/api/targets/options").await).await;
        assert_eq!(v["signedIn"], false, "no token → not signed in");
        assert_eq!(
            v["devices"].as_array().unwrap().len(),
            0,
            "no devices are listed while signed out"
        );
        assert!(
            v.get("error").map_or(true, |e| e.is_null()),
            "a clean signed-out state carries no error: {v}"
        );
    }

    /// Signed in: the picker lists only receiver-capable devices — a send-only
    /// Perseus device and THIS device itself (own hub id from the sign-in) are
    /// both excluded, leaving only the other full-peer receivers.
    #[tokio::test]
    async fn target_options_lists_receivers_excluding_perseus_and_self() {
        let server = MockServer::start().await;
        // Sign-in returns this node's own hub device id ("self-dev").
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "deviceToken": "tok-secret-xyz",
                "deviceId": "self-dev",
            })))
            .mount(&server)
            .await;
        // The account device list: one athenaeum receiver, one send-only Perseus
        // agent, and this device itself (given the athenaeum capability so it is
        // excluded ONLY by the self-id rule, not by the Perseus rule).
        Mock::given(method("GET"))
            .and(path("/api/v1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "studio-1", "name": "Studio", "pubkey": "cHVia2V5",
                  "capability": "athenaeum", "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null },
                { "id": "cam-1", "name": "CaptureCam", "pubkey": "cHVia2V5",
                  "capability": "perseus", "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null },
                { "id": "self-dev", "name": "ThisNode", "pubkey": "cHVia2V5",
                  "capability": "athenaeum", "createdAt": "2026-07-01T00:00:00Z", "lastSeenAt": null }
            ])))
            .mount(&server)
            .await;

        let (state, _tmp) = account_test_state(&server.uri()).await;
        let app = build_router(state, None);

        // Sign in so a token + this device's id ("self-dev") are stored.
        let res = post_json(
            &app,
            "/api/account/verify",
            serde_json::json!({ "email": "u@e.com", "code": "123456" }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK, "sign-in succeeds");

        let v = body_json(get(&app, "/api/targets/options").await).await;
        assert_eq!(v["signedIn"], true, "a live hub list means signed in");
        let devices = v["devices"].as_array().unwrap();
        assert_eq!(
            devices.len(),
            1,
            "only the non-Perseus, non-self receiver remains: {v}"
        );
        assert_eq!(devices[0]["id"], "studio-1");
        assert_eq!(devices[0]["name"], "Studio");
        assert_eq!(devices[0]["capability"], "athenaeum");
        assert!(
            v.get("error").map_or(true, |e| e.is_null()),
            "no error on the happy path: {v}"
        );
    }

    /// The picker endpoint lives behind the bearer gate: with a token configured,
    /// an unauthenticated request is refused `401` before the handler runs (the
    /// hub is never contacted).
    #[tokio::test]
    async fn target_options_is_behind_bearer_gate() {
        let (state, _tmp) = account_test_state("http://127.0.0.1:1").await;
        let app = build_router(state, Some("s3cret".to_string()));

        let res = get(&app, "/api/targets/options").await;
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "no bearer token → 401 before the handler"
        );
    }

    // ── Task 6 (Sync Phase 2): pending / send-mode / send-now / batches ────────

    /// `PUT /api/send-mode` rewrites `perseus.toml` (mode + auto_quiet_secs),
    /// swaps the live config, live-applies the new [`SendCfg`] onto the batcher's
    /// watch channel, and returns the applied values; a follow-up `GET` reflects
    /// them and the on-disk file carries them.
    #[tokio::test]
    async fn put_send_mode_applies_and_get_reflects() {
        let (state, _tmp) = test_state().await; // sample config: mode = "auto"
        // Subscribe BEFORE the PUT so the live-apply send has a receiver and is
        // observable — this proves the running batcher would adopt the change.
        let mut rx = state.send_cfg_tx.read().await.subscribe();
        let config_path = state.config_path.clone();
        let app = build_router(state, None);

        // GET reflects the seeded auto mode.
        let v = body_json(get(&app, "/api/send-mode").await).await;
        assert_eq!(v["mode"], "auto");

        // PUT manual / 45.
        let body = serde_json::json!({ "mode": "manual", "autoQuietSecs": 45 });
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/send-mode")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["mode"], "manual", "the PUT echoes the applied mode");
        assert_eq!(v["autoQuietSecs"], 45);

        // The live-apply reached the batcher's watch channel.
        assert_eq!(
            *rx.borrow_and_update(),
            SendCfg {
                mode: Mode::Manual,
                auto_quiet_secs: 45,
            },
            "the running batcher's send-config channel adopts the edit"
        );

        // The on-disk config carries mode=manual + auto_quiet_secs=45.
        let text = std::fs::read_to_string(&config_path).unwrap();
        let reloaded = Config::from_toml_str(&text).unwrap();
        assert_eq!(reloaded.mode, Mode::Manual, "written to disk: {text}");
        assert_eq!(reloaded.auto_quiet_secs, 45);

        // A follow-up GET reflects the adopted mode.
        let v = body_json(get(&app, "/api/send-mode").await).await;
        assert_eq!(v["mode"], "manual");
        assert_eq!(v["autoQuietSecs"], 45);
    }

    /// An unknown `mode` string is a clean `400` (not a `422` extractor error nor
    /// a silent no-op) and leaves the config byte-identical.
    #[tokio::test]
    async fn put_send_mode_unknown_mode_is_400() {
        let (state, _tmp) = test_state().await;
        let config_path = state.config_path.clone();
        let before = std::fs::read_to_string(&config_path).unwrap();
        let app = build_router(state, None);

        let body = serde_json::json!({ "mode": "sideways", "autoQuietSecs": 30 });
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/send-mode")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            before,
            "an unknown mode leaves the config untouched"
        );
    }

    /// `POST /api/send-now` over a live batcher whose accumulator is empty is a
    /// no-op: `{flushed: 0}` and no batch row recorded.
    #[tokio::test]
    async fn send_now_with_empty_pending_is_noop() {
        // A WebState whose batcher is live but never fed a file → empty pending.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("sync.db");
        let store = Arc::new(StandaloneSyncStore::open(&db).unwrap());
        let seen = Arc::new(crate::seen::SeenStore::open(&db).unwrap());
        let batches = Arc::new(crate::batch_store::BatchStore::open(&db).unwrap());

        let toml_str = sample_toml(tmp.path());
        let config_path = tmp.path().join("perseus.toml");
        std::fs::write(&config_path, &toml_str).unwrap();
        let config = Config::from_toml_str(&toml_str).unwrap();

        // A loopback engine as the batcher's sole fan-out target.
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let engine = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            transport,
            PEER,
        ));

        // A real batcher over an empty stable channel (never fed → nothing pending).
        let (_stable_tx, stable_rx) = tokio::sync::mpsc::channel::<(PathBuf, PathBuf)>(8);
        let (send_cfg_tx, send_cfg_rx) = watch::channel(config.send_cfg());
        let (batcher, _task) = crate::batcher::spawn_batcher(
            stable_rx,
            vec![Arc::clone(&engine)],
            Arc::clone(&seen),
            Arc::clone(&batches),
            config.clone(),
            node_id_hex(&PEER),
            None,
            send_cfg_rx,
        );

        let (_state_tx, state_rx) = watch::channel(AgentState::Running { in_flight: 0 });
        let state = Arc::new(WebState {
            store,
            seen,
            config_path,
            config: RwLock::new(config.clone()),
            agent_state: state_rx,
            supervisor_wake: Arc::new(Notify::new()),
            engine: RwLock::new(Some(engine)),
            cleanup: RwLock::new(None),
            peer_device: RwLock::new(node_id_hex(&PEER)),
            retention_tx: RwLock::new(watch::channel(config.retention.clone()).0),
            retention_log: RwLock::new(Arc::new(Mutex::new(VecDeque::new()))),
            device_names: RwLock::new(HashMap::new()),
            running_dirs: RwLock::new(config.capture_dirs_resolved()),
            running_targets: RwLock::new(config.targets.clone()),
            batcher: RwLock::new(Some(batcher)),
            batches: Arc::clone(&batches),
            send_cfg_tx: RwLock::new(send_cfg_tx),
        });

        let app = build_router(state, None);
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/send-now")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["flushed"], 0, "an empty pending set flushes nothing");
        assert!(
            batches.list().unwrap().is_empty(),
            "a no-op send-now records no batch row"
        );
        // `_stable_tx` / `_task` stay bound to keep the batcher alive to here.
    }

    /// `GET /api/pending` degrades honestly on a detached page (batcher `None`):
    /// an empty tree, zero count, and the seeded send mode — never an error.
    #[tokio::test]
    async fn pending_empty_when_detached() {
        let (state, _tmp) = detached_test_state(AgentState::NeedsSetup {
            needs: vec!["not signed in".to_string()],
        })
        .await;
        let app = build_router(state, None);
        let v = body_json(get(&app, "/api/pending").await).await;
        assert_eq!(v["count"], 0, "no batcher → nothing pending");
        assert_eq!(v["tree"]["count"], 0);
        assert!(v["tree"]["children"].as_array().unwrap().is_empty());
        assert_eq!(v["mode"], "auto", "the send mode still surfaces");
    }

    /// `GET /api/batches` lists recorded batches (newest-first) joined with the
    /// engine's outbound state: a batch whose package has a confirmed outbound row
    /// reads `outcome: confirmed`; one with no outbound row degrades to `sending`.
    #[tokio::test]
    async fn batches_lists_and_joins_outbound_state() {
        let (state, _tmp) = test_state().await;
        // The seeded confirmed outbound row is `pkg-confirmed`; record a batch for
        // it plus one for a package with no outbound row at all.
        state
            .batches
            .record("pkg-confirmed", "manual", "2026-07-12T02:00:00Z", 3)
            .unwrap();
        state
            .batches
            .record("pkg-orphan", "auto", "2026-07-12T01:00:00Z", 1)
            .unwrap();
        let app = build_router(state, None);

        let v = body_json(get(&app, "/api/batches").await).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // Newest-first: pkg-confirmed (02:00) before pkg-orphan (01:00).
        assert_eq!(rows[0]["packageRef"], "pkg-confirmed");
        assert_eq!(rows[0]["mode"], "manual");
        assert_eq!(rows[0]["fileCount"], 3);
        assert_eq!(rows[0]["new"], 0, "new/duplicate are not persisted → 0");
        assert_eq!(rows[0]["duplicate"], 0);
        assert_eq!(
            rows[0]["outcome"], "confirmed",
            "its outbound row is confirmed"
        );
        assert_eq!(rows[0]["targets"].as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["targets"][0]["state"], "confirmed");

        assert_eq!(rows[1]["packageRef"], "pkg-orphan");
        assert_eq!(
            rows[1]["outcome"], "sending",
            "no outbound row yet → sending, not an error"
        );
        assert!(rows[1]["targets"].as_array().unwrap().is_empty());
    }
}
