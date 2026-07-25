//! Perseus embedded web status page — router, auth, read + write endpoints.
//!
//! A tiny [`axum`] server, bound to [`Config::web_bind`](crate::config::Config)
//! (loopback by default), that lets an operator inspect and lightly manage a
//! headless capture node from a browser. The page is a slim two-tab shell
//! ([`index_html`]) whose Transfers + Settings sections are rendered client-side
//! by [`app_js`]/[`style_css`], over these endpoints:
//!
//! - `GET /` — the static, data-free HTML page shell. **Auth-exempt** (see
//!   below) so a browser can load it and then prompt for the token.
//! - `GET /app.js`, `GET /style.css` — the shell's static client script +
//!   stylesheet. **Auth-exempt** for the same reason as `GET /`.
//! - `GET /api/status` — capture dirs, live in-flight transfers, the current
//!   retention policy, and coarse package counts ([`StatusDto`]).
//! - `GET /api/transfers` — the grouped read model (one element per batch across
//!   every fan-out target) + `GET /api/transfers/events` for a batch's merged log.
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
//! - `POST /api/delete-files` — obligation-gated deletion of ONE batch's source
//!   capture files ([`DeleteFilesReport`]): the server re-runs the same
//!   [`obligation_verdict`] the UI showed and refuses (`409`) unless every
//!   participation delivered somewhere or the receiver closed it, then removes
//!   the sources through the shared safety contract and stamps the batch.
//! - `POST /api/delete` — history-delete whole batch groups ([`HistoryDeleteReport`]):
//!   drop the sender bookkeeping (outbound rows + per-file rows + journal + the
//!   `perseus_batch` row) of terminal batches. The `perseus_seen` linkage is kept
//!   so dedup identity + the retention audit trail survive.
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

use athenaeum_core::package::read_manifest;
#[cfg(test)]
use athenaeum_core::package::MANIFEST_FILENAME;
use athenaeum_core::sharing::iroh::node::SharedIrohNode;
use athenaeum_core::sync::store::StandaloneSyncStore;
use athenaeum_core::sync::{
    node_id_hex, OutboundFileRow, OutboundRow, OutboundState, SharedPackageCleanup,
    SyncEngineHandle, SyncStore, CANCELLED_BY_RECEIVER_DETAIL,
};
// `Direction` + `HistoryRow` are referenced only by the test harness now that the
// `/api/history` read endpoint has been retired (the store still records history;
// no live web route reads it back).
#[cfg(test)]
use athenaeum_core::sync::{Direction, HistoryRow};

use crate::batch_store::BatchStore;
use crate::batcher::BatcherHandle;
use crate::config::{Config, Mode, RetentionConfig, SendCfg};
use crate::config_edit::{
    apply_capture_dirs_edit, apply_device_name_edit, apply_retention_edit, apply_send_mode_edit,
    apply_targets_edit, apply_upload_limit_edit, RetentionEdit,
};
use crate::pending::{pending_tree, PendingNode};
use crate::resend::{self, is_declined};
use crate::run::delete_package_sources;
use crate::seen::SeenStore;
use crate::supervisor::AgentState;

mod account_api;
use account_api::*;

/// Max filenames each in-flight row on the status page reports (read from the
/// package manifest). The client renders the first 5; a present 6th is the "there
/// is at least one more" signal, shown as a "+ more" marker. Perseus packages are
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
    /// Perseus's stat-aware seen store (source-file linkage). The obligation-gated
    /// delete-files endpoint (`POST /api/delete-files`) resolves a batch back to
    /// its source capture files through this, via the exact same multi-source
    /// deleter retention uses ([`delete_package_sources`](crate::run::delete_package_sources)).
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
    /// The running engine (its `status_snapshot` is the live in-flight list).
    /// `None` while detached (setup).
    pub engine: RwLock<Option<Arc<SyncEngineHandle>>>,
    /// Every running target's `(peer hex, engine handle)` pair. Per-row actions
    /// (`/api/retry`, `/api/kick`, `/api/cancel`, `/api/resend-as-new`) route
    /// through [`engine_for_peer`](Self::engine_for_peer): the engine worker is
    /// peer-scoped (it ignores rows bound to another peer), so acting through
    /// `engines[0]` was a silent no-op for every other target's rows. Empty
    /// while detached.
    pub engines: RwLock<Vec<(String, Arc<SyncEngineHandle>)>>,
    /// The shared-payload cleanup coordinator, `Some` only for a ≥2-target
    /// fan-out (the same instance the fanned-out engines were spawned with).
    /// `POST /api/retry` bumps it after a successful re-enqueue so the retried
    /// row's terminal cannot prematurely free a still-offline target's payload.
    /// `None` while detached, and `None` for a single-target agent (no shared
    /// dir → the engine's own in-line cleanup, no coordinator).
    pub cleanup: RwLock<Option<Arc<SharedPackageCleanup>>>,
    /// This node's configured sync peer id (hex) — the same value transfer
    /// history rows carry. Stamped onto the `deleted_manual` audit rows written
    /// by `POST /api/delete-files` so the history shows the peer a confirmed
    /// package was sent to, not this agent's own node id (the manifest's
    /// `origin_device` is self — the earlier bug). Empty while detached.
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
    /// so `GET /api/transfers` lists recorded batches engine-attached or not.
    pub batches: Arc<BatchStore>,
    /// The running batcher's live send-config channel, threaded in from the agent
    /// on attach (a clone of [`Agent::send_cfg_tx`](crate::run::Agent::send_cfg_tx)).
    /// `PUT /api/send-mode` sends the re-validated [`SendCfg`] here so the running
    /// batcher live-applies an Auto↔Manual / quiet-window change with no restart.
    /// A placeholder (no receivers) while detached — sends are harmless no-ops —
    /// and, like `retention_tx`, left as-is on detach rather than cleared.
    pub send_cfg_tx: RwLock<watch::Sender<SendCfg>>,
    /// The running agent's shared iroh node, threaded in on attach (W1 T1.6).
    /// `PUT /api/upload-limit` calls
    /// [`SharedIrohNode::set_upload_limit`] on it so an upload-cap edit takes
    /// effect on the next offered chunk — mid-transfer included — with no engine
    /// restart. `None` while detached (and on the loopback injection path, which
    /// binds no node): the edit still persists to `perseus.toml` and is applied by
    /// the startup path when the engine next comes up.
    pub node: RwLock<Option<Arc<SharedIrohNode>>>,
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
            engines: RwLock::new(Vec::new()),
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
            // No node until the agent binds one (`attach`); an upload-limit edit
            // meanwhile is persisted and applied at the next startup.
            node: RwLock::new(None),
        }
    }

    /// Swap the engine-dependent bits in as the engine comes up. Called (via a
    /// `tokio::spawn`) from the supervisor's `on_agent` seam, which clones these
    /// out of the `&dyn ManagedAgent` synchronously first (the callback is sync;
    /// this is `async` and takes the write locks).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub async fn attach(
        &self,
        engine: Option<Arc<SyncEngineHandle>>,
        engines: Vec<(String, Arc<SyncEngineHandle>)>,
        cleanup: Option<Arc<SharedPackageCleanup>>,
        peer_device: String,
        retention_tx: watch::Sender<RetentionConfig>,
        retention_log: Arc<Mutex<VecDeque<RetentionRunRecord>>>,
        device_names: HashMap<String, String>,
        running_dirs: Vec<PathBuf>,
        running_targets: Vec<String>,
        batcher: Option<BatcherHandle>,
        send_cfg_tx: watch::Sender<SendCfg>,
        node: Option<Arc<SharedIrohNode>>,
    ) {
        *self.engine.write().await = engine;
        *self.engines.write().await = engines;
        *self.cleanup.write().await = cleanup;
        *self.peer_device.write().await = peer_device;
        *self.retention_tx.write().await = retention_tx;
        *self.retention_log.write().await = retention_log;
        *self.device_names.write().await = device_names;
        *self.running_dirs.write().await = running_dirs;
        *self.running_targets.write().await = running_targets;
        *self.batcher.write().await = batcher;
        *self.send_cfg_tx.write().await = send_cfg_tx;
        *self.node.write().await = node;
    }

    /// Drop the engine-dependent bits as the engine stops (setup lost, restart,
    /// or shutdown): the page falls back to its detached behaviour. `retention_tx`
    /// / `retention_log` / `device_names` are left as-is — harmlessly stale reads
    /// until the next attach — while the load-bearing safety bits are cleared.
    /// The engine bound to `peer`, if the running fan-out has one. Per-row
    /// actions must reach the row's own peer's engine — the worker ignores
    /// another peer's rows (peer-scoped resend/kick/cancel), so `engines[0]`
    /// routing was a silent no-op for every other target. `None` while detached
    /// or (defensively) for a row whose peer is no longer a configured target.
    pub async fn engine_for_peer(
        &self,
        peer: &athenaeum_core::sharing::types::NodeId,
    ) -> Option<Arc<SyncEngineHandle>> {
        let hex = node_id_hex(peer);
        self.engines
            .read()
            .await
            .iter()
            .find(|(peer_hex, _)| *peer_hex == hex)
            .map(|(_, engine)| Arc::clone(engine))
    }

    pub async fn detach(&self) {
        *self.engine.write().await = None;
        *self.engines.write().await = Vec::new();
        *self.cleanup.write().await = None;
        *self.peer_device.write().await = String::new();
        *self.running_dirs.write().await = Vec::new();
        *self.running_targets.write().await = Vec::new();
        // The batcher is a load-bearing safety bit (it drives sends): clear it so
        // a detached page's send-now is an honest no-op. `send_cfg_tx` is left
        // as-is (a harmless stale sender) exactly like `retention_tx`.
        *self.batcher.write().await = None;
        // The node dies with the agent (its endpoint + device-key lock are torn
        // down on shutdown): drop our handle so a detached upload-limit edit is an
        // honest file-only write, applied when the next agent binds.
        *self.node.write().await = None;
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

/// One outbound package row, for the status page's in-flight list.
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
    /// True iff the RECEIVER declined this transfer (server-computed from the
    /// exact all-cancelled-ack detail, so the JS never string-matches errors).
    /// Renders the "Resend as new transfer" divert instead of Retry.
    declined: bool,
    /// True iff the reset-in-place Retry ("Send again" on a confirmed row) is
    /// offered: any terminal state except a receiver decline. Payload presence
    /// is deliberately NOT part of this — a cleaned dir is rebuilt from the
    /// original capture files at retry time, and honesty about missing/changed
    /// originals arrives in the retry report.
    resendable: bool,
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
        // The sync upload-speed cap (W1 T1.6): read + live edit. Applies to the
        // running node immediately, so a big sync stops starving the observatory's
        // uplink without stopping the agent.
        .route(
            "/api/upload-limit",
            get(api_get_upload_limit).put(api_put_upload_limit),
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
        // Perseus UI v2 (Task 4): the grouped transfers read model — one element
        // per batch across every fan-out target, its per-file × per-target matrix,
        // and the server-computed obligation verdict — plus a batch's merged event
        // log. Read-only; the deletion endpoints are a later task.
        .route("/api/transfers", get(api_transfers))
        .route("/api/transfers/events", get(api_transfer_events))
        // Obligation-gated source-file deletion (per batch) + batch-history delete
        // (per group). Both bearer-gated like every other `/api/*` route.
        .route("/api/delete-files", post(api_delete_files))
        .route("/api/delete", post(api_delete))
        .route("/api/retry", post(api_retry))
        // The explicit operator divert for a receiver-declined transfer (a
        // decline is final per batch_uuid; this mints a NEW transfer).
        .route("/api/resend-as-new", post(api_resend_as_new))
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
        // `/app.js` + `/style.css` are the page's static assets. Like `GET /`
        // they carry no node data, so they are deliberately EXEMPT from the
        // bearer layer: a browser must fetch them to bootstrap the page before
        // its JS can supply a token on the `/api/*` calls. Gating them would
        // 401 the very asset loads that render the token prompt.
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
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

/// `GET /` — the static page shell (Perseus UI v2). A slim two-tab skeleton that
/// pulls in `/style.css` + `/app.js`, which render the Transfers + Settings tabs.
/// Auth-exempt (see the exemption note in [`build_router`]).
async fn index_html() -> Html<&'static str> {
    Html(include_str!("web/index.html"))
}

/// `GET /app.js` — the page's client script. Like `GET /` it is a static,
/// data-free asset and is deliberately EXEMPT from the bearer layer: a browser
/// must fetch it to bootstrap the page before its JS can supply a token on the
/// `/api/*` calls.
async fn app_js() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("web/app.js"),
    )
}

/// `GET /style.css` — the page's stylesheet. A static, data-free asset, EXEMPT
/// from the bearer layer for the same reason as `GET /` and `/app.js`.
async fn style_css() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/css; charset=utf-8",
        )],
        include_str!("web/style.css"),
    )
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

/// `POST /api/delete-files` request body: the single batch whose SOURCE capture
/// files to delete. Obligation-gated — the server re-runs [`obligation_verdict`]
/// before touching disk.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFilesRequest {
    package_ref: String,
}

/// `POST /api/delete` (history-group) request body: the batches whose sender
/// bookkeeping to drop. `perseus_seen` linkage is kept (dedup + audit history).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryDeleteRequest {
    package_refs: Vec<String>,
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

// ── W1 T1.6: sync upload-speed cap ───────────────────────────────────────────

/// `GET`/`PUT /api/upload-limit` payload: the sync upload cap in decimal MB/s
/// (`0` = unlimited). Also the applied-value echo a successful `PUT` returns.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadLimitDto {
    max_upload_mbps: u32,
    /// Whether the value was applied to a RUNNING node as well as written to
    /// `perseus.toml`. `false` while the engine is detached (setup/restart): the
    /// edit is saved and takes effect when the agent next binds, which the UI says
    /// out loud rather than implying an instant cap that did not happen.
    applied_live: bool,
}

/// `PUT /api/upload-limit` request body. Decimal MB/s; `0` = unlimited.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadLimitEdit {
    max_upload_mbps: u32,
}

/// `GET /api/upload-limit` — the configured sync upload cap (MB/s, `0` =
/// unlimited). Read-only.
async fn api_get_upload_limit(State(state): State<Arc<WebState>>) -> Json<UploadLimitDto> {
    let max_upload_mbps = state.config.read().await.max_upload_mbps;
    Json(UploadLimitDto {
        max_upload_mbps,
        applied_live: state.node.read().await.is_some(),
    })
}

/// `PUT /api/upload-limit` — cap the sync upload rate, **live**.
/// [`apply_upload_limit_edit`] rewrites `perseus.toml` (comment-preserving,
/// re-validated, atomic — a rejected edit leaves the file byte-identical and
/// returns `422`), the live config is swapped, and the new rate is pushed onto the
/// running [`SharedIrohNode`] so it takes effect on the next offered chunk,
/// mid-transfer included. With no node attached (engine in setup/restart) the edit
/// is file-only and the response says so via `appliedLive: false`; the startup path
/// applies it when the agent next binds. The supervisor is woken so its config view
/// refreshes at once. Returns the applied `{maxUploadMbps, appliedLive}`.
async fn api_put_upload_limit(
    State(state): State<Arc<WebState>>,
    Json(edit): Json<UploadLimitEdit>,
) -> Result<Json<UploadLimitDto>, (StatusCode, String)> {
    let new_cfg =
        apply_upload_limit_edit(&state.config_path, edit.max_upload_mbps).map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "web upload-limit edit rejected");
            (StatusCode::UNPROCESSABLE_ENTITY, msg)
        })?;
    let max_upload_mbps = new_cfg.max_upload_mbps;
    let bytes_per_sec = new_cfg.upload_limit_bytes_per_sec();
    *state.config.write().await = new_cfg;
    // Live-apply: the pacer is shared by every role handle / peer / concurrent
    // GET on this node, so one call caps the whole device — no engine restart.
    let applied_live = match state.node.read().await.as_ref() {
        Some(node) => {
            node.set_upload_limit(bytes_per_sec);
            true
        }
        None => {
            tracing::warn!(
                max_upload_mbps,
                "upload limit saved but no node attached — applies at next start"
            );
            false
        }
    };
    // Wake the supervisor so its per-pass config view refreshes immediately.
    state.supervisor_wake.notify_one();
    Ok(Json(UploadLimitDto {
        max_upload_mbps,
        applied_live,
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

// ── Perseus UI v2 (Task 4): grouped /api/transfers read model ────────────────

/// One `GET /api/transfers` element: a `perseus_batch` row grouped across every
/// fan-out target (one [`sync_outbound`] row per target sharing the batch's
/// `package_ref`), with a per-file × per-target matrix and the server-computed
/// obligation verdict. The single source of truth the UI v2 renders — the raw
/// UUIDs live only in the per-target details.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferDto {
    /// The batch's package directory path (the grouping key).
    package_ref: String,
    /// The wire batch id — the basename of `package_ref` (its `file_name`), the
    /// stable handle both sides key on. Falls back to the whole ref if it has no
    /// final component.
    batch_uuid: String,
    /// First non-empty per-target `display_name` across the group; `None` → the
    /// UI shows `batchUuid`.
    display_name: Option<String>,
    /// `auto` (watcher quiet-timer) or `manual` (operator "send now").
    mode: String,
    created_at: String,
    file_count: i64,
    /// Total on-wire size: the summed `byte_size` of the group's richest manifest
    /// (the target with the most per-file rows — a partial rebuild can shrink an
    /// attempt's manifest, so the fullest one is authoritative).
    total_bytes: u64,
    /// When this batch's source payload copies were deleted from disk, else
    /// `None`. A set value forces `deletable.allowed = false`.
    files_deleted_at: Option<String>,
    /// The batch-level outcome derived from its targets (see [`aggregate_outcome`]).
    outcome: String,
    /// The user-facing "attempt N" — the max `generation` across the group's rows.
    generation: u32,
    /// One entry per fan-out target (per `sync_outbound` row).
    targets: Vec<TransferTargetDto>,
    /// The per-file × per-target matrix (union of every target's manifest).
    files: Vec<TransferFileDto>,
    /// The server-computed "may delete source files" verdict (obligation model).
    deletable: DeletableDto,
}

/// One fan-out target of a grouped transfer: the `sync_outbound` row's live
/// state, its friendly name, and its rolled-up byte totals.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferTargetDto {
    row_id: i64,
    peer_hex: String,
    /// Friendly device name (peer hex when unknown).
    name: String,
    state: String,
    generation: u32,
    last_error: Option<String>,
    next_retry_at: Option<String>,
    /// Cumulative bytes served across this target's files.
    bytes_done: u64,
    /// Summed `byte_size` across this target's files.
    byte_size: u64,
    created_at: String,
    confirmed_at: Option<String>,
}

/// One file of a grouped transfer, with its per-target status. A target is
/// omitted from `targets` when that attempt's manifest never carried this file
/// (a partial rebuild shrank it).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferFileDto {
    rel_path: String,
    byte_size: u64,
    targets: Vec<TransferFileTargetDto>,
}

/// One cell of the file × target matrix: how one file fared to one target.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferFileTargetDto {
    peer_hex: String,
    state: String,
    outcome: Option<String>,
    error: Option<String>,
    bytes_done: u64,
}

/// The server-computed "may delete source files" verdict (obligation model).
/// Produced by the pure [`obligation_verdict`]; Task 5 re-runs the same function
/// server-side at delete time so the UI's affordance and the guard never drift.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DeletableDto {
    /// The source files are safe to delete: every participation delivered
    /// somewhere (or was closed by the receiver) and at least one row exists.
    allowed: bool,
    /// How many participating rows are `Confirmed` (delivered).
    delivered_targets: u32,
    /// Human labels for rows the receiver closed without fulfilling
    /// (`"<name>: declined"` / `"<name>: cancelled"`) — informational, not a block.
    closed: Vec<String>,
    /// Reasons the delete is blocked (a still-in-flight or failed participation,
    /// an aged-out row window, or files already deleted). Empty ⇒ nothing blocks.
    blockers: Vec<String>,
}

/// One `GET /api/transfers/events` entry: a single journal event tagged with the
/// target device it belongs to.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EventDto {
    ts: String,
    kind: String,
    detail: Option<String>,
    /// Friendly name of the target whose journal this event came from.
    target: String,
}

/// The obligation verdict (spec §3.1/§4.1) — **pure** so Task 5 can re-run it
/// server-side at delete time and reach the same answer the UI showed.
///
/// `participations` are the batches a delete would implicate: this batch plus
/// every batch that re-sends any of its source files (a divert), each paired with
/// its visible outbound rows. The rule is **row-granular** — a `Confirmed` row
/// already guarantees every receipt was non-`Rejected`, so a `cancelled` per-file
/// receipt inside a confirmed batch is the receiver's own decision, not a blocker:
///
/// - `Confirmed` → delivered (fulfils the obligation, counted in `deliveredTargets`).
/// - `Cancelled` → closed (`"<name>: declined"` when `last_error` starts with the
///   receiver-decline detail, else `"<name>: cancelled"`) — recorded, not a block.
/// - `Failed` → blocker `"<name>: failed — <last_error>"`.
/// - any non-terminal → blocker `"<name>: in flight"`.
/// - a participation with **no visible rows** (aged out of the scan window) →
///   blocker `"transfer rows unavailable (aged out)"` — fail closed.
///
/// `allowed` = no blockers AND at least one row was seen. The `files already
/// deleted` blocker is applied by the caller (it is per-batch state, not a
/// participation input — see [`api_transfers`]).
fn obligation_verdict(
    participations: &[(String, Vec<&OutboundRow>)],
    names: &HashMap<String, String>,
) -> DeletableDto {
    let mut delivered_targets = 0u32;
    let mut closed: Vec<String> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let mut total_rows = 0usize;

    for (_package_ref, rows) in participations {
        if rows.is_empty() {
            // A participation whose rows aged out of the visible outbound window:
            // we cannot prove delivery, so fail closed.
            blockers.push("transfer rows unavailable (aged out)".to_string());
            continue;
        }
        for row in rows {
            total_rows += 1;
            let hex = node_id_hex(&row.peer);
            let name = names.get(&hex).cloned().unwrap_or(hex);
            match row.state {
                OutboundState::Confirmed => delivered_targets += 1,
                OutboundState::Cancelled => {
                    let declined = row
                        .last_error
                        .as_deref()
                        .is_some_and(|e| e.starts_with(CANCELLED_BY_RECEIVER_DETAIL));
                    let label = if declined { "declined" } else { "cancelled" };
                    closed.push(format!("{name}: {label}"));
                }
                OutboundState::Failed => {
                    let reason = row.last_error.as_deref().unwrap_or("unknown");
                    blockers.push(format!("{name}: failed — {reason}"));
                }
                // Queued / Announced / Transferring / Delivered — still on the wire.
                _ => blockers.push(format!("{name}: in flight")),
            }
        }
    }

    DeletableDto {
        allowed: blockers.is_empty() && total_rows >= 1,
        delivered_targets,
        closed,
        blockers,
    }
}

/// Resolve the obligation participations for `package_ref`: its own source files
/// (`files_for`) fan out via [`packages_for_sources`](BatchStore::packages_for_sources)
/// to every batch that re-sends any of them (a divert), unioned with `package_ref`
/// itself; each ref is then paired with its visible outbound rows from `by_ref`.
/// A batch with no recorded source linkage (pre-linkage row) degrades to just
/// itself. A ref present here but absent from `by_ref` yields an empty row list —
/// the verdict's aged-out fail-closed signal.
fn resolve_participations<'a>(
    batches: &BatchStore,
    package_ref: &str,
    by_ref: &HashMap<&str, Vec<&'a OutboundRow>>,
) -> anyhow::Result<Vec<(String, Vec<&'a OutboundRow>)>> {
    let sources: Vec<String> = batches
        .files_for(package_ref)?
        .into_iter()
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .collect();
    let refs: Vec<String> = if sources.is_empty() {
        vec![package_ref.to_string()]
    } else {
        let mut refs = batches.packages_for_sources(&sources)?;
        if !refs.iter().any(|r| r == package_ref) {
            refs.push(package_ref.to_string());
        }
        refs
    };
    Ok(refs
        .into_iter()
        .map(|r| {
            let rows = by_ref.get(r.as_str()).cloned().unwrap_or_default();
            (r, rows)
        })
        .collect())
}

/// `GET /api/transfers` — the grouped transfers read model (Perseus UI v2). One
/// element per recorded `perseus_batch` row (newest-first), joined by
/// `package_ref` to every fan-out target's outbound row, carrying the per-file ×
/// per-target matrix and the server-computed obligation verdict. Read-only; the
/// sole batch read surface since the legacy `/api/batches` was retired.
async fn api_transfers(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<TransferDto>>, (StatusCode, String)> {
    let batches = state.batches.list().map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web transfers: list batches failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    let outbound = state.store.all_outbound(STATUS_SCAN_LIMIT).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web transfers: read outbound failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    // Group outbound rows by package_ref (one row per target on a fan-out).
    let mut by_ref: HashMap<&str, Vec<&OutboundRow>> = HashMap::new();
    for row in &outbound {
        by_ref.entry(row.package_ref.as_str()).or_default().push(row);
    }
    let device_names = state.device_names.read().await;

    let mut dtos: Vec<TransferDto> = Vec::with_capacity(batches.len());
    for batch in &batches {
        let rows: &[&OutboundRow] = by_ref
            .get(batch.package_ref.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        // Fetch each target's per-file rows once; reused for the target rollups,
        // the file matrix, and the total-bytes pick.
        let mut per_target: Vec<(&OutboundRow, Vec<OutboundFileRow>)> = Vec::with_capacity(rows.len());
        for &row in rows {
            let files = state.store.list_outbound_files(row.id).map_err(|e| {
                let msg = format!("{e:#}");
                tracing::error!(error = %msg, outbound_id = row.id, "web transfers: read outbound files failed");
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            })?;
            per_target.push((row, files));
        }

        // Per-target rollups.
        let targets: Vec<TransferTargetDto> = per_target
            .iter()
            .map(|(row, files)| {
                let hex = node_id_hex(&row.peer);
                TransferTargetDto {
                    row_id: row.id,
                    name: device_names.get(&hex).cloned().unwrap_or_else(|| hex.clone()),
                    peer_hex: hex,
                    state: row.state.as_str().to_string(),
                    generation: row.generation,
                    last_error: row.last_error.clone(),
                    next_retry_at: row.next_retry_at.clone(),
                    bytes_done: files.iter().map(|f| f.bytes_done).sum(),
                    byte_size: files.iter().map(|f| f.byte_size).sum(),
                    created_at: row.created_at.clone(),
                    confirmed_at: row.confirmed_at.clone(),
                }
            })
            .collect();

        // File × target matrix: union of every target's rel_paths (BTreeMap keeps
        // it rel_path-ordered and deterministic), each carrying only the targets
        // whose manifest actually held that file.
        let mut matrix: std::collections::BTreeMap<String, (u64, Vec<TransferFileTargetDto>)> =
            std::collections::BTreeMap::new();
        for (row, files) in &per_target {
            let hex = node_id_hex(&row.peer);
            for f in files {
                let entry = matrix.entry(f.rel_path.clone()).or_insert((f.byte_size, Vec::new()));
                entry.0 = f.byte_size;
                entry.1.push(TransferFileTargetDto {
                    peer_hex: hex.clone(),
                    state: f.state.as_str().to_string(),
                    outcome: f.outcome.clone(),
                    error: f.error.clone(),
                    bytes_done: f.bytes_done,
                });
            }
        }
        let files: Vec<TransferFileDto> = matrix
            .into_iter()
            .map(|(rel_path, (byte_size, targets))| TransferFileDto {
                rel_path,
                byte_size,
                targets,
            })
            .collect();

        // Total bytes = the richest manifest's summed byte_size (rows share the
        // manifest; a partial rebuild can shrink an attempt's, so pick the fullest).
        let total_bytes = per_target
            .iter()
            .max_by_key(|(_, files)| files.len())
            .map(|(_, files)| files.iter().map(|f| f.byte_size).sum())
            .unwrap_or(0);

        // Obligation verdict, then the per-batch `files already deleted` override
        // (per-batch state, not a participation input, so applied here).
        let participations = resolve_participations(&state.batches, &batch.package_ref, &by_ref)
            .map_err(|e| {
                let msg = format!("{e:#}");
                tracing::error!(error = %msg, package_ref = %batch.package_ref, "web transfers: resolve participations failed");
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            })?;
        let mut deletable = obligation_verdict(&participations, &device_names);
        if batch.files_deleted_at.is_some() {
            deletable.allowed = false;
            deletable.blockers.push("files already deleted".to_string());
        }

        let batch_uuid = Path::new(&batch.package_ref)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(batch.package_ref.as_str())
            .to_string();
        let display_name = rows
            .iter()
            .find_map(|r| r.display_name.as_deref().filter(|s| !s.is_empty()))
            .map(str::to_string);
        let generation = rows.iter().map(|r| r.generation).max().unwrap_or(0);

        dtos.push(TransferDto {
            package_ref: batch.package_ref.clone(),
            batch_uuid,
            display_name,
            mode: batch.mode.clone(),
            created_at: batch.created_at.clone(),
            file_count: batch.file_count,
            total_bytes,
            files_deleted_at: batch.files_deleted_at.clone(),
            outcome: aggregate_outcome(rows),
            generation,
            targets,
            files,
            deletable,
        });
    }
    Ok(Json(dtos))
}

/// Query string for `GET /api/transfers/events`.
#[derive(serde::Deserialize)]
struct TransferEventsQuery {
    /// The batch's `package_ref` (the grouping key) whose merged journal to read.
    #[serde(rename = "ref")]
    package_ref: String,
}

/// `GET /api/transfers/events?ref=<package_ref>` — the batch's event log: every
/// fan-out target's sender journal ([`list_sync_events_for`](StandaloneSyncStore::list_sync_events_for),
/// newest-first per row) tagged with the target's device name, merged, and
/// re-sorted oldest-first by `ts`. Read-only.
async fn api_transfer_events(
    State(state): State<Arc<WebState>>,
    Query(q): Query<TransferEventsQuery>,
) -> Result<Json<Vec<EventDto>>, (StatusCode, String)> {
    let outbound = state.store.all_outbound(STATUS_SCAN_LIMIT).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web transfer events: read outbound failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    let device_names = state.device_names.read().await;
    let mut events: Vec<EventDto> = Vec::new();
    for row in outbound.iter().filter(|r| r.package_ref == q.package_ref) {
        let hex = node_id_hex(&row.peer);
        let name = device_names.get(&hex).cloned().unwrap_or(hex);
        let journal = state.store.list_sync_events_for(row.id).map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, outbound_id = row.id, "web transfer events: read journal failed");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;
        for e in journal {
            events.push(EventDto {
                ts: e.ts,
                kind: e.kind,
                detail: e.detail,
                target: name.clone(),
            });
        }
    }
    // Merge order is per-row newest-first; the batch log reads oldest-first.
    events.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(Json(events))
}

/// 200 body for an allowed `POST /api/delete-files`: the per-file deletion
/// outcome (honest no-ops and failures kept distinct), the batch stamp, and the
/// verdict fields the UI showed — so the client renders the same delivery
/// picture the server acted on.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFilesReport {
    /// Source files removed from disk (display paths).
    removed: Vec<String>,
    /// `(path, reason)` for a source skipped honestly (already gone, or changed
    /// since confirmation — the TOCTOU guard).
    skipped: Vec<(String, String)>,
    /// `(path, error)` for a source whose delete errored — a non-empty list
    /// leaves the batch **un-stamped** so the operator can retry the stragglers.
    failed: Vec<(String, String)>,
    /// RFC3339 UTC stamp of when the batch's sources were deleted — `Some` only
    /// when `failed` is empty AND `removed` is non-empty (a pass that actually
    /// removed something cleanly); `None` on a partial failure OR a zero-work
    /// pass (e.g. a divert-relinked batch whose live source linkage now points
    /// at a different package — nothing to remove here is not "files deleted"),
    /// either way still re-deletable.
    files_deleted_at: Option<String>,
    /// How many participating targets are `Confirmed` (from the pre-verdict).
    delivered_targets: u32,
    /// Receiver-closed target labels (`"<name>: declined"` / `"<name>: cancelled"`)
    /// carried through from the verdict — informational, never a block.
    closed: Vec<String>,
}

/// 409 body for a refused `POST /api/delete-files`: the reasons the delete is
/// blocked (a still-in-flight/failed participation, an aged-out row window, or
/// files already deleted). Mirrors [`DeletableDto::blockers`].
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BlockedResponse {
    blockers: Vec<String>,
}

/// `POST /api/delete` (history-group) response: the refs whose sender bookkeeping
/// was dropped, and the refs refused (each with a reason).
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct HistoryDeleteReport {
    deleted: Vec<String>,
    rejected: Vec<HistoryDeleteRejection>,
}

/// One refused ref from `POST /api/delete`, with a human reason (today: a still
/// non-terminal transfer in the group).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryDeleteRejection {
    /// The batch's `package_ref`. Serialized as `ref` — the UI's grouping handle.
    #[serde(rename = "ref")]
    package_ref: String,
    reason: String,
}

/// `POST /api/delete-files` — obligation-gated deletion of ONE batch's SOURCE
/// capture files. Looks the batch up (`404` if it has no `perseus_batch` row),
/// re-runs the SAME [`obligation_verdict`] the read model showed (never trusting
/// the UI's cached verdict), and refuses `409` with the blockers when the delete
/// is not permitted. The `files already deleted` guard is applied here caller-side
/// (the pure verdict does not know `files_deleted_at`), matching [`api_transfers`].
/// On allow it removes every live source through the shared safety contract
/// ([`delete_package_sources`] — audit-before-delete, TOCTOU stat guard, honest
/// no-ops), stamps the batch **only when the pass both failed nothing AND
/// actually removed at least one source** (never on a zero-work pass — e.g. a
/// divert-relinked batch whose live `perseus_seen` linkage now points at a
/// different package, or sources already gone/changed out-of-band — which must
/// stay re-deletable rather than claim a false "files deleted"), and returns the
/// full per-file detail plus the delivery verdict either way.
async fn api_delete_files(
    State(state): State<Arc<WebState>>,
    Json(req): Json<DeleteFilesRequest>,
) -> Response {
    // Look the batch up: a ref with no recorded `perseus_batch` row is a 404.
    // `mark_files_deleted` is a silent no-op on an unknown ref, so absence must
    // be detected from `list()`, never inferred from a write's return.
    let batches = match state.batches.list() {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "web delete-files: list batches failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
        }
    };
    let Some(batch) = batches.iter().find(|b| b.package_ref == req.package_ref) else {
        return (StatusCode::NOT_FOUND, "unknown batch").into_response();
    };

    // CRITICAL SEAM: re-apply the caller-side `files already deleted` guard the
    // read model applies — the pure verdict does not know `files_deleted_at`.
    if batch.files_deleted_at.is_some() {
        tracing::info!(package_ref = %batch.package_ref, blockers = 1, "web delete-files refused");
        return (
            StatusCode::CONFLICT,
            Json(BlockedResponse {
                blockers: vec!["files already deleted".to_string()],
            }),
        )
            .into_response();
    }

    // Re-run the obligation verdict server-side (same participation set as the
    // read model: this batch plus every batch that re-sends any of its sources).
    let outbound = match state.store.all_outbound(STATUS_SCAN_LIMIT) {
        Ok(o) => o,
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "web delete-files: read outbound failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
        }
    };
    let mut by_ref: HashMap<&str, Vec<&OutboundRow>> = HashMap::new();
    for row in &outbound {
        by_ref.entry(row.package_ref.as_str()).or_default().push(row);
    }
    let participations = match resolve_participations(&state.batches, &batch.package_ref, &by_ref) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, package_ref = %batch.package_ref, "web delete-files: resolve participations failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
        }
    };
    let verdict = {
        let device_names = state.device_names.read().await;
        obligation_verdict(&participations, &device_names)
    };
    if !verdict.allowed {
        tracing::info!(package_ref = %batch.package_ref, blockers = verdict.blockers.len(), "web delete-files refused");
        return (
            StatusCode::CONFLICT,
            Json(BlockedResponse {
                blockers: verdict.blockers,
            }),
        )
            .into_response();
    }

    // Allowed: delete every live source through the shared safety contract.
    let peer_device = state.peer_device.read().await.clone();
    let detail = match delete_package_sources(
        &*state.store,
        &state.seen,
        Path::new(&batch.package_ref),
        "deleted_manual",
        &peer_device,
    ) {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, package_ref = %batch.package_ref, "web delete-files: source delete failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
        }
    };

    // Stamp the batch ONLY when the pass actually removed something cleanly —
    // `failed.is_empty()` alone is not enough: a divert-relinked batch (its live
    // `perseus_seen` linkage was repointed onto a NEW package ref by the operator
    // divert) or a batch whose sources are already gone/changed out-of-band
    // resolves to ZERO live sources, so `delete_package_sources` legitimately
    // returns empty `removed`/`skipped`/`failed` — an honest no-op, not a
    // deletion. Stamping `files_deleted_at` on that zero-work pass would be a
    // FALSE "files deleted" marker: the files are still on disk (owned by the new
    // batch, in the divert case), yet the old batch would claim they're gone and
    // become permanently un-re-deletable. Requiring `!detail.removed.is_empty()`
    // keeps a zero-work pass an idempotent no-op — `filesDeletedAt: null`, the
    // batch stays re-deletable — while a partial failure (some failed) still
    // correctly leaves it un-stamped so the operator can retry the stragglers.
    let files_deleted_at = if detail.failed.is_empty() && !detail.removed.is_empty() {
        let at = now_rfc3339();
        if let Err(e) = state.batches.mark_files_deleted(&batch.package_ref, &at) {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, package_ref = %batch.package_ref, "web delete-files: mark files_deleted failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
        }
        Some(at)
    } else {
        None
    };

    tracing::info!(package_ref = %batch.package_ref, removed = detail.removed.len(), "manual source delete");
    (
        StatusCode::OK,
        Json(DeleteFilesReport {
            removed: detail.removed,
            skipped: detail.skipped,
            failed: detail.failed,
            files_deleted_at,
            delivered_targets: verdict.delivered_targets,
            closed: verdict.closed,
        }),
    )
        .into_response()
}

/// `POST /api/delete` — history-delete whole batch groups. Per requested ref:
/// if ANY of its outbound rows is still non-terminal the ref is `rejected`
/// (nothing touched); otherwise the group's sender bookkeeping — every outbound
/// row's per-file rows + `Sent` journal + the row itself
/// ([`delete_outbound_group`](StandaloneSyncStore::delete_outbound_group)) and the
/// `perseus_batch` row — is dropped, and the ref is `deleted`. The `perseus_seen`
/// linkage is deliberately KEPT so dedup identity + the retention audit trail
/// survive a history delete.
async fn api_delete(
    State(state): State<Arc<WebState>>,
    Json(req): Json<HistoryDeleteRequest>,
) -> Result<Json<HistoryDeleteReport>, (StatusCode, String)> {
    let outbound = state.store.all_outbound(STATUS_SCAN_LIMIT).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web history delete: read outbound failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;
    let mut by_ref: HashMap<&str, Vec<&OutboundRow>> = HashMap::new();
    for row in &outbound {
        by_ref.entry(row.package_ref.as_str()).or_default().push(row);
    }

    let mut report = HistoryDeleteReport::default();
    for pref in &req.package_refs {
        let rows = by_ref.get(pref.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        // Refuse while any row of the group is still on the wire — a live transfer
        // must terminalize (confirm/cancel/fail) before its history is dropped.
        if let Some(active) = rows.iter().find(|r| !r.state.is_terminal()) {
            tracing::info!(package_ref = %pref, state = active.state.as_str(), "web history delete refused: active transfer");
            report.rejected.push(HistoryDeleteRejection {
                package_ref: pref.clone(),
                reason: "a transfer of this batch is still active".to_string(),
            });
            continue;
        }
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        state.store.delete_outbound_group(&ids).map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, package_ref = %pref, "web history delete: delete_outbound_group failed");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;
        state.batches.delete(pref).map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, package_ref = %pref, "web history delete: batch delete failed");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;
        tracing::info!(package_ref = %pref, rows = ids.len(), "history delete removed batch group");
        report.deleted.push(pref.clone());
    }
    Ok(Json(report))
}

/// RFC3339 UTC (millis, `Z`) — the timestamp form every Perseus store write uses
/// (`seen::now_iso`, `batcher::now_rfc3339`). Mirrored here for the `files_deleted_at`
/// batch stamp so all Perseus timestamps read identically.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

/// One resent package. v2.1 resets the SAME row in place, so `new_id == old_id`
/// (the pair shape is kept for the JS contract); `missing_files` /
/// `changed_files` carry the rebuild honesty report when the payload had to be
/// restored from the originals (empty and omitted otherwise).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryPair {
    old_id: i64,
    new_id: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed_files: Vec<String>,
}

/// `POST /api/resend-as-new` request: the ONE receiver-declined outbound row to
/// divert into a new transfer. Single-id by design (see [`api_resend_as_new`]).
#[derive(serde::Deserialize)]
struct ResendAsNewRequest {
    id: i64,
}

/// `POST /api/resend-as-new` response: the declined row left as history and the
/// freshly enqueued transfer that replaces it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ResendAsNewReport {
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

/// `POST /api/retry` — reset-in-place resend of terminal packages (Transfers
/// Batch Model v2.1 §D1). For each id: look up the outbound row, then hand it
/// to [`resend::resend_in_place`] on the row's OWN peer's engine — eligibility
/// (`failed` / `cancelled` / `confirmed`; receiver-declined rejected with a
/// divert hint), payload rebuild from the originals when the dir is
/// manifest-only, the fan-out cleanup re-arm, the same-row reset
/// (`generation`+1, fresh wire id) and the re-drive all live there. The row id
/// never changes (`newId == oldId`); `missingFiles` / `changedFiles` carry the
/// rebuild honesty report. Unknown / non-terminal / unrebuildable ids are
/// rejected per-id, never half-acted-on.
async fn api_retry(
    State(state): State<Arc<WebState>>,
    Json(req): Json<RetryRequest>,
) -> Result<Json<RetryReport>, (StatusCode, String)> {
    // Retry re-drives through the live engines; there is nothing to retry into
    // while the node is still in setup (engines detached). Honest 503.
    if state.engines.read().await.is_empty() {
        tracing::warn!("web retry: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    }
    // The shared-payload cleanup coordinator — present only for a ≥2-target
    // fan-out; `resend_in_place` re-arms it so the reset row's SECOND terminal
    // cannot free a still-pending sibling target's payload (nor stay dead
    // after a post-confirm cleanup whose payload the rebuild restored).
    let cleanup = state.cleanup.read().await.clone();
    let config = state.config.read().await.clone();
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
        let Some(engine) = state.engine_for_peer(&row.peer).await else {
            report.rejected.push(RetryRejection {
                id,
                reason: "no running engine for this package's peer".to_string(),
            });
            continue;
        };
        match resend::resend_in_place(
            &state.store,
            &engine,
            cleanup.as_deref(),
            &config,
            &state.batches,
            &row,
        )
        .await
        {
            Ok(done) => {
                tracing::info!(id, rebuilt = done.rebuilt, "package resent in place via web");
                report.retried.push(RetryPair {
                    old_id: id,
                    new_id: id,
                    missing_files: done.missing,
                    changed_files: done.changed,
                });
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(id, error = %msg, "web retry: resend rejected");
                report.rejected.push(RetryRejection { id, reason: msg });
            }
        }
    }
    Ok(Json(report))
}

/// `POST /api/resend-as-new {id}` — the explicit operator divert for ONE
/// receiver-declined transfer ([`resend::resend_declined_as_new`]): fresh dir
/// basename ⇒ fresh wire `batch_uuid` ⇒ a brand-new transfer on both sides,
/// while the declined row stays as history (its error gains the
/// `resent as new transfer #N` suffix — a double-click bounces). Deliberately
/// single-id: this overrides a human decline and must stay a per-transfer
/// human decision, never a bulk action or an autonomous retry.
async fn api_resend_as_new(
    State(state): State<Arc<WebState>>,
    Json(req): Json<ResendAsNewRequest>,
) -> Result<Json<ResendAsNewReport>, (StatusCode, String)> {
    if state.engines.read().await.is_empty() {
        tracing::warn!("web resend-as-new: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    }
    let row = match state.store.get_outbound(req.id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err((StatusCode::BAD_REQUEST, "unknown package".to_string()));
        }
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(id = req.id, error = %msg, "web resend-as-new: outbound lookup failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, msg));
        }
    };
    let Some(engine) = state.engine_for_peer(&row.peer).await else {
        return Err((
            StatusCode::BAD_REQUEST,
            "no running engine for this package's peer".to_string(),
        ));
    };
    let cleanup = state.cleanup.read().await.clone();
    let config = state.config.read().await.clone();
    match resend::resend_declined_as_new(
        &state.store,
        &engine,
        cleanup.as_deref(),
        &config,
        &state.batches,
        &state.seen,
        &row,
    )
    .await
    {
        Ok(new_id) => {
            tracing::info!(old_id = req.id, new_id, "declined package diverted via web");
            Ok(Json(ResendAsNewReport {
                old_id: req.id,
                new_id,
            }))
        }
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(id = req.id, error = %msg, "web resend-as-new rejected");
            Err((StatusCode::BAD_REQUEST, msg))
        }
    }
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
    if state.engines.read().await.is_empty() {
        tracing::warn!("web kick: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    }
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
        // Route to the row's OWN peer's engine: a kick only wakes a slot in
        // that engine's pending map — sent anywhere else it is a silent no-op.
        let Some(engine) = state.engine_for_peer(&row.peer).await else {
            report.rejected.push(RetryRejection {
                id,
                reason: "no running engine for this package's peer".to_string(),
            });
            continue;
        };
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
    if state.engines.read().await.is_empty() {
        tracing::warn!("web cancel: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    }
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
        // Route to the row's OWN peer's engine — the worker cancels only its
        // own pending slots (elsewhere the cancel is silently dropped).
        let Some(engine) = state.engine_for_peer(&row.peer).await else {
            report.rejected.push(RetryRejection {
                id,
                reason: "no running engine for this package's peer".to_string(),
            });
            continue;
        };
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
    // The server computes both affordance flags so the JS never has to match
    // error strings: `declined` keys the divert button, `resendable` the
    // reset-in-place Retry / "Send again" (payload presence is NOT required —
    // a manifest-only dir is rebuilt from the originals at retry time).
    let declined = is_declined(r);
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
        declined,
        resendable: !declined
            && matches!(
                r.state,
                OutboundState::Failed | OutboundState::Cancelled | OutboundState::Confirmed
            ),
    }
}

/// Read a package's manifest ONCE and derive both the operator-facing filenames
/// and the package's total byte size, for a [`SentDto`] in-flight row. `files` are
/// the file-name component of each record's `rel_path`, capped at
/// [`SENT_FILES_CAP`]; `byte_size` is the sum of EVERY record's `byte_size` (the
/// full manifest, not just the capped names). An unreadable or missing manifest
/// yields `(vec![], 0)` — never an error, so the status page still lists the row
/// and the UI falls back to the dir basename. T1 keeps the manifest alive through
/// payload cleanup, so a confirmed row still resolves its names + size.
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
            engines: RwLock::new(vec![(node_id_hex(&PEER), Arc::clone(&engine))]),
            engine: RwLock::new(Some(engine)),
            // Single-target test agent: no fan-out coordinator (the retry
            // re-arm path is exercised by the coordinator's own unit tests).
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
            // No iroh node in the harness (loopback transports bind none): an
            // upload-limit edit is file-only here, `appliedLive: false`.
            node: RwLock::new(None),
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

    /// The two static assets (`/app.js`, `/style.css`) are served with the right
    /// content-type AND are bearer-EXEMPT — like `GET /`, they must load without a
    /// token so the browser can bootstrap the page before its JS supplies one.
    #[tokio::test]
    async fn assets_served_ungated_with_content_types() {
        let (state, _tmp) = test_state().await;
        // Token-protected router, and NO auth header on either asset request.
        let app = build_router(state, Some("tok".to_string()));

        let js = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            js.status(),
            StatusCode::OK,
            "/app.js must load without a token"
        );
        assert_eq!(
            js.headers().get("content-type").unwrap(),
            "application/javascript; charset=utf-8"
        );

        let css = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/style.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            css.status(),
            StatusCode::OK,
            "/style.css must load without a token"
        );
        assert_eq!(
            css.headers().get("content-type").unwrap(),
            "text/css; charset=utf-8"
        );
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

    /// Migrated from the retired `/api/sent` tests: `to_sent_dto` +
    /// `sent_manifest_summary` now live on only because `/api/status`'s in-flight
    /// list reuses the same mapper. A non-terminal (queued) row surfaces its
    /// manifest filenames (dir stripped, capped at `SENT_FILES_CAP`), the summed
    /// `byteSize` over the FULL manifest (not just the capped names), and
    /// `nextRetryAt` straight off the row; a row whose package has no readable
    /// manifest reports empty `files` (never an error).
    #[tokio::test]
    async fn status_in_flight_reports_manifest_files_capped_bytes_and_next_retry() {
        let (state, tmp) = test_state().await;
        // Eight sized records (one path carrying a directory) → files cap at
        // SENT_FILES_CAP with the file-name component only; byteSize sums all eight.
        let sized: Vec<(String, u64)> = (0..8)
            .map(|i| (format!("frames/f-{i}.fits"), 1000 * (i as u64 + 1)))
            .collect();
        let refs: Vec<(&str, u64)> = sized.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let pkg = write_sized_manifest_package(&tmp.path().join("pkg-sized"), &refs);
        let id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state
            .store
            .set_next_retry_at(id, Some("2026-07-16T12:00:00Z"))
            .unwrap();

        let app = build_router(state, None);
        let v = body_json(get(&app, "/api/status").await).await;
        let in_flight = v["inFlight"].as_array().unwrap();

        let sized_row = in_flight
            .iter()
            .find(|r| r["packageRef"].as_str().unwrap().ends_with("pkg-sized"))
            .expect("the queued sized package is in flight");
        let files = sized_row["files"].as_array().unwrap();
        assert_eq!(files.len(), SENT_FILES_CAP, "filenames capped at SENT_FILES_CAP");
        assert_eq!(files[0], "f-0.fits", "file-name component only (dir stripped)");
        // byteSize = 1000·(1+2+…+8) = 36000 — the full manifest, not the capped files.
        assert_eq!(sized_row["byteSize"].as_u64().unwrap(), 36000, "sum of every record");
        assert_eq!(sized_row["nextRetryAt"], "2026-07-16T12:00:00Z");

        // The seeded `pkg-transferring` row has no manifest on disk → empty files,
        // never an error row.
        let bogus = in_flight
            .iter()
            .find(|r| r["packageRef"] == "pkg-transferring")
            .expect("the seeded transferring row is in flight");
        assert!(
            bogus["files"].as_array().unwrap().is_empty(),
            "an unreadable manifest yields empty files, not an error row"
        );
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

    // ── Task 5 (Perseus UI v2): obligation-gated delete-files + history delete ─

    /// Write a real capture file on disk and record its seen linkage under
    /// `package_ref`, returning the path. The delete-files path resolves + removes
    /// sources through this exact linkage (`seen.sources_for_package`).
    fn seed_source(
        tmp: &tempfile::TempDir,
        state: &WebState,
        name: &str,
        package_ref: &str,
    ) -> PathBuf {
        let source = tmp.path().join(name);
        std::fs::write(&source, b"capture-source-bytes").unwrap();
        let meta = std::fs::metadata(&source).unwrap();
        let mtime = crate::seen::mtime_millis(meta.modified().ok());
        state
            .seen
            .mark_enqueued(&source, meta.len(), mtime, package_ref)
            .unwrap();
        source
    }

    /// A Confirmed + a still-Announced target: the obligation is unmet (the open
    /// target has not delivered), so delete-files is refused `409`, the source is
    /// untouched, and the batch is left un-stamped.
    #[tokio::test]
    async fn delete_files_refuses_while_a_target_is_open() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        let (state, tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        const P2: [u8; 32] = [2u8; 32];
        let pref = "/data/packages/open-batch";
        let files = [AnnounceFileEntry {
            rel_path: "a.fits".into(),
            byte_size: 10,
            frame_uuid: "u-a".into(),
        }];
        let c = state.store.enqueue(pref, P1, None, &files).unwrap();
        state.store.confirm(c, &[]).unwrap();
        let open = state.store.enqueue(pref, P2, None, &files).unwrap();
        state.store.set_state(open, OutboundState::Announced).unwrap();
        state
            .batches
            .record(pref, "auto", "2026-07-23T10:00:00Z", 1)
            .unwrap();
        let source = seed_source(&tmp, &state, "a.fits", pref);

        let app = build_router(Arc::clone(&state), None);
        let res = post_json(&app, "/api/delete-files", serde_json::json!({ "packageRef": pref }))
            .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let v = body_json(res).await;
        assert!(
            v["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b.as_str().unwrap().contains("in flight")),
            "the open target is an in-flight blocker: {v}"
        );
        assert!(source.exists(), "a refusal never touches the sources");
        assert_eq!(
            state
                .batches
                .list()
                .unwrap()
                .into_iter()
                .find(|b| b.package_ref == pref)
                .unwrap()
                .files_deleted_at,
            None,
            "the batch is left un-stamped on a refusal"
        );
    }

    /// Two Confirmed targets over two real source files: delete-files removes both
    /// from disk, stamps the batch, and a follow-up `GET /api/transfers` blocks any
    /// re-delete (`files already deleted`).
    #[tokio::test]
    async fn delete_files_removes_sources_and_stamps_batch() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        let (state, tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        const P2: [u8; 32] = [2u8; 32];
        let pref = "/data/packages/done-batch";
        let files = [
            AnnounceFileEntry { rel_path: "a.fits".into(), byte_size: 10, frame_uuid: "u-a".into() },
            AnnounceFileEntry { rel_path: "b.fits".into(), byte_size: 20, frame_uuid: "u-b".into() },
        ];
        let c1 = state.store.enqueue(pref, P1, None, &files).unwrap();
        state.store.confirm(c1, &[]).unwrap();
        let c2 = state.store.enqueue(pref, P2, None, &files).unwrap();
        state.store.confirm(c2, &[]).unwrap();
        state
            .batches
            .record(pref, "auto", "2026-07-23T10:00:00Z", 2)
            .unwrap();
        let a = seed_source(&tmp, &state, "a.fits", pref);
        let b = seed_source(&tmp, &state, "b.fits", pref);

        let app = build_router(Arc::clone(&state), None);
        let res = post_json(&app, "/api/delete-files", serde_json::json!({ "packageRef": pref }))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(
            v["removed"].as_array().unwrap().len(),
            2,
            "both live sources removed: {v}"
        );
        assert!(v["filesDeletedAt"].as_str().is_some(), "the stamp is in the body: {v}");
        assert!(!a.exists() && !b.exists(), "both source files are gone from disk");
        assert!(
            state
                .batches
                .list()
                .unwrap()
                .into_iter()
                .find(|r| r.package_ref == pref)
                .unwrap()
                .files_deleted_at
                .is_some(),
            "the batch stamp is persisted"
        );

        // A re-delete is now blocked by the read model's `files already deleted`.
        let tv = body_json(get(&app, "/api/transfers").await).await;
        let batch = tv
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["packageRef"] == pref)
            .unwrap()
            .clone();
        assert_eq!(batch["deletable"]["allowed"], false, "re-delete blocked: {batch}");
        assert!(batch["deletable"]["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b.as_str().unwrap().contains("already deleted")));
    }

    /// A Confirmed target plus a receiver-declined (Cancelled) one: the obligation
    /// is met (delivered somewhere; the decline is the receiver's own choice), so
    /// delete-files is allowed and the 200 body surfaces the delivery verdict.
    #[tokio::test]
    async fn delete_files_confirmed_plus_declined_is_allowed_and_reports_delivery() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        let (state, tmp) = test_state().await;
        {
            let mut g = state.device_names.write().await;
            g.insert(node_id_hex(&[1u8; 32]), "Studio".to_string());
            g.insert(node_id_hex(&[2u8; 32]), "NAS".to_string());
        }
        const P1: [u8; 32] = [1u8; 32];
        const P2: [u8; 32] = [2u8; 32];
        let pref = "/data/packages/mixed-batch";
        let files = [AnnounceFileEntry {
            rel_path: "a.fits".into(),
            byte_size: 10,
            frame_uuid: "u-a".into(),
        }];
        let c = state.store.enqueue(pref, P1, None, &files).unwrap();
        state.store.confirm(c, &[]).unwrap();
        let d = state.store.enqueue(pref, P2, None, &files).unwrap();
        state.store.set_state(d, OutboundState::Cancelled).unwrap();
        state
            .store
            .set_last_error(d, Some(CANCELLED_BY_RECEIVER_DETAIL))
            .unwrap();
        state
            .batches
            .record(pref, "auto", "2026-07-23T10:00:00Z", 1)
            .unwrap();
        let source = seed_source(&tmp, &state, "a.fits", pref);

        let app = build_router(Arc::clone(&state), None);
        let res = post_json(&app, "/api/delete-files", serde_json::json!({ "packageRef": pref }))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(
            v["deliveredTargets"], 1,
            "the one confirmed target is surfaced via the pre-verdict: {v}"
        );
        assert!(
            v["closed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c.as_str().unwrap().contains("declined")),
            "the decline is recorded (not a block): {v}"
        );
        assert!(!source.exists(), "the source was removed");
    }

    /// Spec §8: a Confirmed batch whose per-file outcomes are all `duplicate` (the
    /// dedup handshake — nothing traveled) is deletable. Pins dedup-counts-as-
    /// confirmed against a future per-file verdict refactor.
    #[tokio::test]
    async fn delete_files_all_duplicate_confirmed_batch_is_deletable() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        use athenaeum_core::sync::OutboundFileState;
        let (state, tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        let pref = "/data/packages/dup-batch";
        let files = [AnnounceFileEntry {
            rel_path: "a.fits".into(),
            byte_size: 10,
            frame_uuid: "u-a".into(),
        }];
        let c = state.store.enqueue(pref, P1, None, &files).unwrap();
        // Every per-file outcome is `duplicate` (Done, nothing on the wire), then
        // the batch confirms.
        state
            .store
            .set_outbound_file_state(c, "a.fits", OutboundFileState::Done, 0, Some("duplicate"), None)
            .unwrap();
        state.store.confirm(c, &[]).unwrap();
        state
            .batches
            .record(pref, "auto", "2026-07-23T10:00:00Z", 1)
            .unwrap();
        let source = seed_source(&tmp, &state, "a.fits", pref);

        let app = build_router(Arc::clone(&state), None);
        let res = post_json(&app, "/api/delete-files", serde_json::json!({ "packageRef": pref }))
            .await;
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "an all-duplicate confirmed batch is deletable"
        );
        let v = body_json(res).await;
        assert_eq!(v["removed"].as_array().unwrap().len(), 1);
        assert!(!source.exists());
    }

    /// Review fix pin: a divert-relinked batch (its live `perseus_seen` linkage
    /// was repointed onto a NEW package ref — exactly what the declined→resend-
    /// as-new divert does) resolves to ZERO live sources under the OLD ref, so
    /// `delete_package_sources` legitimately returns empty `removed`/`skipped`/
    /// `failed`. That zero-work pass must be an honest no-op — `filesDeletedAt:
    /// null`, the batch row stays un-stamped/re-deletable — never a false "files
    /// deleted" marker (the file is untouched on disk, now owned by the new batch).
    #[tokio::test]
    async fn delete_files_divert_relinked_batch_is_a_noop_not_a_false_stamp() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        let (state, tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        let pref = "/data/packages/old-batch";
        let new_pref = "/data/packages/new-batch";
        let files = [AnnounceFileEntry {
            rel_path: "a.fits".into(),
            byte_size: 10,
            frame_uuid: "u-a".into(),
        }];
        let c = state.store.enqueue(pref, P1, None, &files).unwrap();
        state.store.confirm(c, &[]).unwrap();
        state
            .batches
            .record(pref, "auto", "2026-07-23T10:00:00Z", 1)
            .unwrap();
        let source = seed_source(&tmp, &state, "a.fits", pref);
        // Simulate the divert: the live linkage is repointed onto a NEW package
        // ref (the same call `resend_declined_as_new` makes), so the OLD ref now
        // resolves to zero live sources.
        state.seen.relink_package(pref, new_pref).unwrap();

        let app = build_router(Arc::clone(&state), None);
        let res = post_json(&app, "/api/delete-files", serde_json::json!({ "packageRef": pref }))
            .await;
        assert_eq!(res.status(), StatusCode::OK, "a zero-work pass is still a 200, not an error");
        let v = body_json(res).await;
        assert!(v["removed"].as_array().unwrap().is_empty(), "nothing live under the old ref: {v}");
        assert!(v["skipped"].as_array().unwrap().is_empty());
        assert!(v["failed"].as_array().unwrap().is_empty());
        assert!(
            v["filesDeletedAt"].is_null(),
            "a zero-work pass must never claim files were deleted: {v}"
        );
        assert!(source.exists(), "the file is untouched — still owned by the new batch");
        assert_eq!(
            state
                .batches
                .list()
                .unwrap()
                .into_iter()
                .find(|b| b.package_ref == pref)
                .unwrap()
                .files_deleted_at,
            None,
            "the old batch stays un-stamped and re-deletable"
        );
    }

    /// A terminal group history-deletes cleanly: its outbound rows, per-file rows,
    /// journal, and the `perseus_batch` row are gone — but the `perseus_seen`
    /// linkage and the source file on disk are KEPT (dedup identity + audit).
    #[tokio::test]
    async fn history_delete_removes_group_keeps_seen() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        let (state, tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        let pref = "/data/packages/hist-batch";
        let files = [AnnounceFileEntry {
            rel_path: "a.fits".into(),
            byte_size: 10,
            frame_uuid: "u-a".into(),
        }];
        let id = state.store.enqueue(pref, P1, None, &files).unwrap();
        state.store.confirm(id, &[]).unwrap();
        state
            .store
            .append_sync_event(Direction::Sent, &id.to_string(), "announce_sent", None)
            .unwrap();
        state
            .batches
            .record(pref, "auto", "2026-07-23T10:00:00Z", 1)
            .unwrap();
        let source = seed_source(&tmp, &state, "a.fits", pref);

        let app = build_router(Arc::clone(&state), None);
        let res = post_json(&app, "/api/delete", serde_json::json!({ "packageRefs": [pref] }))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert!(
            v["deleted"].as_array().unwrap().iter().any(|r| r == pref),
            "the terminal group was deleted: {v}"
        );
        assert!(v["rejected"].as_array().unwrap().is_empty());

        assert!(
            state.store.all_outbound(100).unwrap().iter().all(|r| r.package_ref != pref),
            "outbound rows removed"
        );
        assert!(
            state.store.list_outbound_files(id).unwrap().is_empty(),
            "per-file rows removed"
        );
        assert!(
            state.store.list_sync_events_for(id).unwrap().is_empty(),
            "journal removed"
        );
        assert!(
            state.batches.list().unwrap().iter().all(|b| b.package_ref != pref),
            "perseus_batch row removed"
        );
        // The dedup linkage + retention audit survive a history delete.
        assert_eq!(
            state.seen.sources_for_package(pref).unwrap().len(),
            1,
            "the perseus_seen linkage is kept"
        );
        assert!(source.exists(), "history delete never touches source files on disk");
    }

    /// A group with a still-non-terminal (Announced) row is refused: the ref comes
    /// back in `rejected` with a reason and nothing is deleted.
    #[tokio::test]
    async fn history_delete_refuses_active_group() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        let (state, _tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        let pref = "/data/packages/active-batch";
        let files = [AnnounceFileEntry {
            rel_path: "a.fits".into(),
            byte_size: 10,
            frame_uuid: "u-a".into(),
        }];
        let id = state.store.enqueue(pref, P1, None, &files).unwrap();
        state.store.set_state(id, OutboundState::Announced).unwrap();
        state
            .batches
            .record(pref, "auto", "2026-07-23T10:00:00Z", 1)
            .unwrap();

        let app = build_router(Arc::clone(&state), None);
        let res = post_json(&app, "/api/delete", serde_json::json!({ "packageRefs": [pref] }))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert!(
            v["deleted"].as_array().unwrap().is_empty(),
            "nothing deleted while a target is active: {v}"
        );
        let rej = v["rejected"].as_array().unwrap();
        assert_eq!(rej.len(), 1);
        assert_eq!(rej[0]["ref"], pref);
        assert!(rej[0]["reason"].as_str().unwrap().contains("still active"));
        assert!(
            state.store.all_outbound(100).unwrap().iter().any(|r| r.package_ref == pref),
            "the outbound row is untouched"
        );
        assert!(
            state.batches.list().unwrap().iter().any(|b| b.package_ref == pref),
            "the batch row is untouched"
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

    /// POST `/api/resend-as-new` for one id, returning the raw response (the
    /// divert rejects with a non-200, which several tests assert on).
    async fn post_resend_as_new(app: Router, id: i64) -> Response {
        let body = serde_json::json!({ "id": id });
        app.oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/resend-as-new")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    /// A REAL one-file package dir: payload bytes on disk plus a parseable
    /// manifest record whose `(byte_size, xxh3)` match them — enough for the
    /// divert path (manifest read + enqueue) to run for real.
    fn make_real_package_dir(base: &std::path::Path, name: &str) -> PathBuf {
        let pkg = base.join(name);
        std::fs::create_dir_all(&pkg).unwrap();
        let payload_bytes: &[u8] = b"real-payload-bytes";
        let payload = pkg.join("frame-0001.fits");
        std::fs::write(&payload, payload_bytes).unwrap();
        let rec = serde_json::json!({
            "v": 1,
            "frame_uuid": "u-real",
            "origin_catalog_uuid": "u-real",
            "origin_device": "aa",
            "payload_kind": "RawFrame",
            "rel_path": "frame-0001.fits",
            "byte_size": payload_bytes.len(),
            "xxh3": athenaeum_core::package::xxh3_full_file(&payload).unwrap(),
            "frame_meta": {},
            "analysis": null,
            "app_version": "test"
        });
        std::fs::write(pkg.join(MANIFEST_FILENAME), format!("{rec}\n")).unwrap();
        pkg
    }

    /// `/api/resend-as-new` is ONLY for receiver-declined rows: a plain
    /// (sender-)cancelled row is rejected — that's `/api/retry` territory.
    #[tokio::test]
    async fn resend_as_new_rejects_non_declined() {
        let (state, tmp) = test_state().await;
        let pkg = make_real_package_dir(tmp.path(), "pkg-plain-cancelled");
        let id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state.store.set_state(id, OutboundState::Cancelled).unwrap();

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let res = post_resend_as_new(app, id).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            store.get_outbound(id).unwrap().unwrap().state,
            OutboundState::Cancelled,
            "the rejected row is untouched"
        );
    }

    /// The happy divert: a declined row mints a NEW transfer (fresh id + fresh
    /// dir), the old row stays cancelled with the guard suffix, and a second
    /// click on the same row is rejected (the suffix broke the strict guard).
    #[tokio::test]
    async fn resend_as_new_diverts_then_rejects_double_click() {
        let (state, tmp) = test_state().await;
        let pkg = make_real_package_dir(tmp.path(), "pkg-declined-divert");
        let id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state.store.set_state(id, OutboundState::Cancelled).unwrap();
        state
            .store
            .set_last_error(id, Some(athenaeum_core::sync::CANCELLED_BY_RECEIVER_DETAIL))
            .unwrap();

        let store = Arc::clone(&state.store);
        let app = build_router(Arc::clone(&state), None);
        let res = post_resend_as_new(app, id).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["oldId"].as_i64().unwrap(), id);
        let new_id = v["newId"].as_i64().unwrap();
        assert_ne!(new_id, id, "the divert mints a brand-new transfer");

        let new_row = store.get_outbound(new_id).unwrap().expect("new row exists");
        assert_ne!(new_row.package_ref, pkg.to_string_lossy(), "fresh dir ⇒ fresh batch_uuid");
        let old = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(old.state, OutboundState::Cancelled, "the declined row stays history");
        assert!(old
            .last_error
            .unwrap()
            .contains(&format!("resent as new transfer #{new_id}")));

        // Double-click: the suffix broke the strict declined guard.
        let app = build_router(state, None);
        let res = post_resend_as_new(app, id).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    /// v2.1 §D1: retrying a failed package resets the SAME row in place —
    /// `newId == oldId`, `generation` bumps to 2, the row leaves its terminal
    /// state, and no additional outbound row is minted for the package dir.
    #[tokio::test]
    async fn retry_failed_resets_same_row_same_id() {
        let (state, tmp) = test_state().await;
        let pkg = make_package_dir(tmp.path(), "pkg-failed-intact", false);
        let old_id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state
            .store
            .set_state(old_id, OutboundState::Failed)
            .unwrap();

        let store = Arc::clone(&state.store);
        let before = store.all_outbound(100).unwrap().len();
        let app = build_router(state, None);
        let v = post_retry(app, &[old_id]).await;

        let retried = v["retried"].as_array().unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0]["oldId"].as_i64().unwrap(), old_id);
        assert_eq!(
            retried[0]["newId"].as_i64().unwrap(),
            old_id,
            "reset-in-place: the transfer keeps its row id"
        );
        assert!(v["rejected"].as_array().unwrap().is_empty());

        let row = store.get_outbound(old_id).unwrap().expect("row still exists");
        assert_eq!(row.generation, 2, "attempt counter bumped by the reset");
        assert!(
            !row.state.is_terminal(),
            "the row is live again (re-driven by its peer's engine), got {:?}",
            row.state
        );
        assert_eq!(
            store.all_outbound(100).unwrap().len(),
            before,
            "no extra outbound row minted for the package dir"
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

    /// A **sender-cancelled** package is retryable exactly like a failed one
    /// (v2.1): the SAME row resets in place and leaves its terminal state.
    #[tokio::test]
    async fn retry_cancelled_resets_same_row_in_place() {
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
        assert_eq!(retried[0]["newId"].as_i64().unwrap(), old_id, "same row, attempt+1");
        assert!(v["rejected"].as_array().unwrap().is_empty());
        let row = store.get_outbound(old_id).unwrap().unwrap();
        assert!(!row.state.is_terminal(), "the cancelled row is live again");
        assert_eq!(row.generation, 2);
    }

    /// A **receiver-declined** row (`cancelled` + the exact all-cancelled-ack
    /// detail) is NOT silently retried — a same-batch re-announce would only
    /// bounce, and an autonomous/casual retry must never override a human
    /// decline. Rejected with a pointer at the explicit divert action.
    #[tokio::test]
    async fn retry_declined_rejected_with_divert_hint() {
        let (state, tmp) = test_state().await;
        let pkg = make_package_dir(tmp.path(), "pkg-declined", false);
        let id = state.store.enqueue(&pkg.to_string_lossy(), PEER, None, &[]).unwrap();
        state.store.set_state(id, OutboundState::Cancelled).unwrap();
        state
            .store
            .set_last_error(id, Some(athenaeum_core::sync::CANCELLED_BY_RECEIVER_DETAIL))
            .unwrap();

        let store = Arc::clone(&state.store);
        let app = build_router(state, None);
        let v = post_retry(app, &[id]).await;

        assert!(v["retried"].as_array().unwrap().is_empty());
        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["id"].as_i64().unwrap(), id);
        let reason = rejected[0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("Resend as new transfer"),
            "the reject points the operator at the divert action, got {reason:?}"
        );
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.state, OutboundState::Cancelled, "the declined row is untouched");
        assert_eq!(row.generation, 1, "no reset happened");
    }

    /// A manifest-only dir (post-confirm cleanup) whose ORIGINALS are also gone
    /// has nothing to rebuild from: the retry is rejected honestly, before any
    /// row mutation. (`make_package_dir`'s placeholder manifest names no real
    /// capture file, so the rebuild resolves zero sources.)
    #[tokio::test]
    async fn retry_rejects_when_nothing_is_restorable() {
        let (state, tmp) = test_state().await;
        let pkg = tmp.path().join("pkg-manifest-only");
        std::fs::create_dir_all(&pkg).unwrap();
        // A real (parseable) manifest record whose source file exists nowhere.
        let rec = serde_json::json!({
            "v": 1,
            "frame_uuid": "u-1",
            "origin_catalog_uuid": "u-1",
            "origin_device": "aa",
            "payload_kind": "RawFrame",
            "rel_path": "gone-forever.fits",
            "byte_size": 13,
            "xxh3": "deadbeefdeadbeef",
            "frame_meta": {},
            "analysis": null,
            "app_version": "0.0.0"
        });
        std::fs::write(pkg.join(MANIFEST_FILENAME), format!("{rec}\n")).unwrap();
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
        let reason = rejected[0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("could be restored"),
            "the reject names the rebuild failure, got {reason:?}"
        );
        assert_eq!(
            store.get_outbound(id).unwrap().unwrap().generation,
            1,
            "no reset for an unrebuildable package"
        );
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
            generation: 1,
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
            max_upload_mbps: 0,
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

    /// W1 T1.6: `PUT /api/upload-limit` rewrites `perseus.toml`, swaps the live
    /// config, and a follow-up `GET` reflects it. This harness binds no iroh node,
    /// so the response reports `appliedLive: false` (saved, applies at next start)
    /// rather than claiming a live cap that never happened.
    #[tokio::test]
    async fn put_upload_limit_applies_and_get_reflects() {
        let (state, _tmp) = test_state().await; // sample config: no max_upload_mbps
        let config_path = state.config_path.clone();
        let app = build_router(state, None);

        // GET reflects the unlimited default.
        let v = body_json(get(&app, "/api/upload-limit").await).await;
        assert_eq!(v["maxUploadMbps"], 0, "absent key reads as unlimited");

        let body = serde_json::json!({ "maxUploadMbps": 8 });
        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/upload-limit")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["maxUploadMbps"], 8, "the PUT echoes the applied cap");
        assert_eq!(
            v["appliedLive"], false,
            "no node attached in the harness — saved, applied at next start"
        );

        // The on-disk config carries the cap, and the conversion is decimal MB/s.
        let text = std::fs::read_to_string(&config_path).unwrap();
        let reloaded = Config::from_toml_str(&text).unwrap();
        assert_eq!(reloaded.max_upload_mbps, 8, "written to disk: {text}");
        assert_eq!(reloaded.upload_limit_bytes_per_sec(), 8_000_000);

        // A follow-up GET reflects the adopted cap.
        let v = body_json(get(&app, "/api/upload-limit").await).await;
        assert_eq!(v["maxUploadMbps"], 8);
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
            engines: RwLock::new(vec![(node_id_hex(&PEER), Arc::clone(&engine))]),
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
            node: RwLock::new(None),
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

    // ── Task 4 (Perseus UI v2): grouped /api/transfers + events + verdict ─────

    /// Build a bare [`OutboundRow`] for the pure-verdict unit tests. Only the
    /// fields the verdict reads (`peer`, `state`, `last_error`) matter; the rest
    /// carry inert placeholders.
    fn verdict_row(id: i64, peer: [u8; 32], state: OutboundState, last_error: Option<&str>) -> OutboundRow {
        OutboundRow {
            id,
            package_ref: format!("pkg-{id}"),
            peer,
            state,
            attempts: 0,
            created_at: "2026-07-23T10:00:00.000Z".into(),
            confirmed_at: None,
            last_error: last_error.map(str::to_string),
            next_retry_at: None,
            wire_package_id: None,
            display_name: None,
            project_id: None,
            generation: 1,
        }
    }

    /// `GET /api/transfers` groups ONE `perseus_batch` row across two fan-out
    /// targets: a single element carrying two targets, a two-file matrix each
    /// crossing both targets, the basename as `batchUuid`, and `sending` while
    /// both rows are still non-terminal.
    #[tokio::test]
    async fn transfers_groups_one_batch_across_two_targets() {
        use athenaeum_core::sharing::types::AnnounceFileEntry;
        let (state, _tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        const P2: [u8; 32] = [2u8; 32];
        let files = [
            AnnounceFileEntry { rel_path: "a.fits".into(), byte_size: 100, frame_uuid: "u-a".into() },
            AnnounceFileEntry { rel_path: "b.fits".into(), byte_size: 200, frame_uuid: "u-b".into() },
        ];
        // Two outbound rows (one per target) under the same package_ref, each with
        // the same two-file manifest, plus the batch record that groups them.
        state.store.enqueue("/data/packages/batch-xyz", P1, None, &files).unwrap();
        state.store.enqueue("/data/packages/batch-xyz", P2, None, &files).unwrap();
        state
            .batches
            .record("/data/packages/batch-xyz", "auto", "2026-07-23T10:00:00Z", 2)
            .unwrap();

        let app = build_router(state, None);
        let v = body_json(get(&app, "/api/transfers").await).await;
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1, "one element per perseus_batch row");
        assert_eq!(rows[0]["batchUuid"], "batch-xyz");
        assert_eq!(rows[0]["outcome"], "sending", "both targets still non-terminal");
        assert_eq!(rows[0]["targets"].as_array().unwrap().len(), 2);
        let matrix = rows[0]["files"].as_array().unwrap();
        assert_eq!(matrix.len(), 2, "union of the two rows' rel_paths");
        assert_eq!(
            matrix[0]["targets"].as_array().unwrap().len(),
            2,
            "each file crosses both targets"
        );
    }

    /// A Confirmed row fulfils its files; a receiver-declined Cancelled row is
    /// `closed` (labelled `declined`) — the batch is deletable with the decline
    /// recorded, not blocked.
    #[test]
    fn verdict_confirmed_plus_declined_allows_with_closed_label() {
        let names = HashMap::from([(node_id_hex(&[1u8; 32]), "Studio".to_string())]);
        let confirmed = verdict_row(1, [1u8; 32], OutboundState::Confirmed, None);
        let declined = verdict_row(
            2,
            [1u8; 32],
            OutboundState::Cancelled,
            Some(athenaeum_core::sync::CANCELLED_BY_RECEIVER_DETAIL),
        );
        let parts = vec![("pkg-a".to_string(), vec![&confirmed, &declined])];
        let v = obligation_verdict(&parts, &names);
        assert!(v.allowed, "confirmed + declined → deletable");
        assert_eq!(v.delivered_targets, 1);
        assert_eq!(v.closed, vec!["Studio: declined".to_string()]);
        assert!(v.blockers.is_empty());
    }

    /// A Failed row blocks (its reason surfaced); a non-terminal (Announced) row
    /// blocks as `in flight`. Either way `allowed == false`.
    #[test]
    fn verdict_failed_or_inflight_blocks() {
        let names = HashMap::from([(node_id_hex(&[1u8; 32]), "Studio".to_string())]);

        let failed = verdict_row(1, [1u8; 32], OutboundState::Failed, Some("payload gone"));
        let v = obligation_verdict(&[("pkg-a".to_string(), vec![&failed])], &names);
        assert!(!v.allowed);
        assert!(
            v.blockers.iter().any(|b| b.contains("failed") && b.contains("payload gone")),
            "failed blocker carries the reason: {:?}",
            v.blockers
        );

        let inflight = verdict_row(2, [1u8; 32], OutboundState::Announced, None);
        let v2 = obligation_verdict(&[("pkg-b".to_string(), vec![&inflight])], &names);
        assert!(!v2.allowed);
        assert!(
            v2.blockers.iter().any(|b| b.contains("in flight")),
            "non-terminal blocker reads 'in flight': {:?}",
            v2.blockers
        );
    }

    /// A divert: batch A is all-cancelled (closed, deletable on its own), but its
    /// source file also travels in batch B (linked via `packages_for_sources`).
    /// While B is in flight, A's obligation is blocked; once B confirms, A's files
    /// are delivered somewhere and A becomes deletable.
    #[test]
    fn verdict_divert_participation_blocks_until_new_batch_confirms() {
        let dir = tempfile::tempdir().unwrap();
        let batches = crate::batch_store::BatchStore::open(dir.path().join("perseus.db")).unwrap();
        // A and B both carry the same source capture file → shared participation.
        batches
            .record_files("pkg-a", &[("a.fits".into(), std::path::PathBuf::from("/cap/a.fits"))])
            .unwrap();
        batches
            .record_files("pkg-b", &[("a.fits".into(), std::path::PathBuf::from("/cap/a.fits"))])
            .unwrap();

        let names = HashMap::new();
        let a_row = verdict_row(1, [1u8; 32], OutboundState::Cancelled, Some("cancelled"));
        let mut b_row = verdict_row(2, [2u8; 32], OutboundState::Announced, None);

        // Resolve A's participations through the real linkage query, then verdict.
        let by_ref: HashMap<&str, Vec<&OutboundRow>> =
            HashMap::from([("pkg-a", vec![&a_row]), ("pkg-b", vec![&b_row])]);
        let parts = resolve_participations(&batches, "pkg-a", &by_ref).unwrap();
        let refs: Vec<&str> = parts.iter().map(|(r, _)| r.as_str()).collect();
        assert!(refs.contains(&"pkg-a") && refs.contains(&"pkg-b"), "B is a participation of A: {refs:?}");
        let blocked = obligation_verdict(&parts, &names);
        assert!(!blocked.allowed, "B in flight blocks A's obligation");

        // Flip B to Confirmed → A's files are delivered via B → deletable.
        b_row.state = OutboundState::Confirmed;
        let by_ref2: HashMap<&str, Vec<&OutboundRow>> =
            HashMap::from([("pkg-a", vec![&a_row]), ("pkg-b", vec![&b_row])]);
        let parts2 = resolve_participations(&batches, "pkg-a", &by_ref2).unwrap();
        let ok = obligation_verdict(&parts2, &names);
        assert!(ok.allowed, "once B confirms, A becomes deletable");
        assert_eq!(ok.delivered_targets, 1);
    }

    /// `GET /api/transfers/events?ref=` merges the per-target journals of every
    /// row in the group, tags each event with the target's device name, and
    /// returns them oldest-first.
    #[tokio::test]
    async fn transfer_events_merges_rows_sorted_and_named() {
        let (state, _tmp) = test_state().await;
        const P1: [u8; 32] = [1u8; 32];
        const P2: [u8; 32] = [2u8; 32];
        {
            let mut g = state.device_names.write().await;
            g.insert(node_id_hex(&P1), "Studio".to_string());
            g.insert(node_id_hex(&P2), "NAS".to_string());
        }
        let id1 = state.store.enqueue("/data/packages/evt", P1, None, &[]).unwrap();
        let id2 = state.store.enqueue("/data/packages/evt", P2, None, &[]).unwrap();

        // Interleave the two journals; distinct kinds per target make the
        // name-attachment assertion independent of the (asserted) ts ordering.
        // Small sleeps keep the millisecond timestamps strictly increasing.
        let gap = std::time::Duration::from_millis(5);
        state.store.append_sync_event(Direction::Sent, &id1.to_string(), "announce_sent", None).unwrap();
        tokio::time::sleep(gap).await;
        state.store.append_sync_event(Direction::Sent, &id2.to_string(), "serve_start", None).unwrap();
        tokio::time::sleep(gap).await;
        state.store.append_sync_event(Direction::Sent, &id1.to_string(), "ack_received", None).unwrap();
        tokio::time::sleep(gap).await;
        state.store.append_sync_event(Direction::Sent, &id2.to_string(), "dial_failed", Some("timeout")).unwrap();

        let app = build_router(state, None);
        let v = body_json(get(&app, "/api/transfers/events?ref=/data/packages/evt").await).await;
        let events = v.as_array().unwrap();
        assert_eq!(events.len(), 4, "both journals merged");

        // ts strictly ascending (oldest-first).
        let ts: Vec<&str> = events.iter().map(|e| e["ts"].as_str().unwrap()).collect();
        let mut sorted = ts.clone();
        sorted.sort_unstable();
        assert_eq!(ts, sorted, "events oldest-first by ts");

        // Names attached by target: P1's kinds → Studio, P2's kinds → NAS.
        for e in events {
            let kind = e["kind"].as_str().unwrap();
            let target = e["target"].as_str().unwrap();
            match kind {
                "announce_sent" | "ack_received" => assert_eq!(target, "Studio", "{kind}"),
                "serve_start" | "dial_failed" => assert_eq!(target, "NAS", "{kind}"),
                other => panic!("unexpected kind {other}"),
            }
        }
    }
}
