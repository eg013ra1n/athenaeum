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
//! - `POST /api/transfers/send-to` — send an already-recorded batch to another
//!   device as a brand-new transfer ([`SendToRequest`] → [`SendToReport`]), the
//!   source batch and its history untouched. Two steps on one route:
//!   `confirm: false` answers with "sends N of M" and builds nothing,
//!   `confirm: true` performs. Missing originals are dropped and counted; only
//!   an all-gone batch is refused (`409`).
//! - `GET`/`PUT /api/retention/policy` — read the live retention config +
//!   read-only soak gate ([`PolicyDto`]); a whitelisted [`RetentionEdit`] is
//!   applied to `perseus.toml` and adopted live. Live deletion can never be
//!   enabled here (the edit carries no soak field).
//! - `GET /api/retention/log` — the recent retention-pass ring buffer
//!   ([`RetentionRunRecord`], newest-first).
//! - `GET /api/library?root=<idx>&path=<rel>` — one directory of one capture
//!   root, each file joined to its derived send status
//!   ([`library::LibraryListing`](crate::library::LibraryListing)). Shallow (one
//!   `read_dir`, never a walk); an offline root is a `502`, not a `404`.
//! - `GET /api/library/preview?root=<idx>&path=<rel>&w=<px>` — one frame,
//!   auto-stretched to a JPEG ([`crate::preview`]). Serialized to one render at
//!   a time behind a small LRU, and ETag-conditional: a repeat look costs a
//!   `stat(2)` and a `304`. Inherits `/api/library`'s status contract, adding
//!   `415` (not a FITS/XISF frame) and `422` (a frame that will not decode).
//!   Without the `preview` feature the route answers `404 preview not built`.
//! - `POST /api/library/send` — send an arbitrary library selection now, as one
//!   explicit `browser` batch ([`LibrarySendRequest`] → [`LibrarySendReport`]).
//!   Selected files leave the batcher's pending set first, so the next
//!   auto/scheduled flush cannot send them a second time.
//! - `POST /api/library/delete` — delete a library selection
//!   ([`LibraryDeleteRequest`] → [`LibraryDeleteReport`]). Two steps on one
//!   route: `confirm: false` returns the per-file consequence preview and
//!   touches nothing, `confirm: true` performs and returns a per-path outcome.
//!   Nothing is ever forbidden except a Perseus-internal path (spec §2).
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
use std::time::Instant;

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
use crate::batcher::{BatcherHandle, Delivery};
use crate::config::{Config, Mode, RetentionConfig, SendCfg};
use crate::config_edit::{
    apply_capture_dirs_edit, apply_device_name_edit, apply_retention_edit, apply_send_mode_edit,
    apply_targets_edit, apply_upload_limit_edit, RetentionEdit,
};
use crate::diskspace::VolumeInfo;
use crate::pending::{pending_tree, PendingNode};
use crate::resend::{self, is_declined};
use crate::run::delete_package_sources;
use crate::seen::SeenStore;
use crate::supervisor::AgentState;
use crate::watcher::WatcherForget;

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

/// One memoized free-space reading: when it was taken, the exact path set it
/// answers for, and the volumes measured behind those paths.
type FreeSpaceReading = (Instant, Vec<PathBuf>, Vec<VolumeInfo>);

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
    /// The running watchers' aggregate forget handle (0.5.1 T9b), threaded in
    /// from the agent on attach. Both deletion routes (`POST /api/library/delete`
    /// and `POST /api/delete-files`) hand it the paths they removed, so a frame
    /// re-copied to a deleted path is enqueued again in the SAME process run —
    /// the stamped seen row alone cannot do that, because the watcher drops an
    /// already-emitted path before it ever consults the store. Empty while
    /// detached (no watchers to correct), which is a silent no-op.
    pub watcher_forget: RwLock<WatcherForget>,
    /// The running agent's shared iroh node, threaded in on attach (W1 T1.6).
    /// `PUT /api/upload-limit` calls
    /// [`SharedIrohNode::set_upload_limit`] on it so an upload-cap edit takes
    /// effect on the next offered chunk — mid-transfer included — with no engine
    /// restart. `None` while detached (and on the loopback injection path, which
    /// binds no node): the edit still persists to `perseus.toml` and is applied by
    /// the startup path when the engine next comes up.
    pub node: RwLock<Option<Arc<SharedIrohNode>>>,
    /// Memoized free-space reading: `(taken_at, probed_paths, volumes)`.
    ///
    /// Held only for the clone in and out — never across the probe, so a poller
    /// can always read the last reading. The probed path set is part of the key
    /// so a capture-dir edit is not served a reading for the old roots.
    pub free_space: Mutex<Option<FreeSpaceReading>>,
    /// Single-flight token for refreshing [`free_space`](Self::free_space).
    ///
    /// The probe is a blocking syscall that can hang as long as a wedged network
    /// mount takes to time out, and `/api/status` is polled every 2 s by every
    /// open browser. Whoever wins this token runs the one probe (never a growing
    /// pile of stuck blocking-pool threads); everyone else is served the memo's
    /// last value instead of queueing behind it. It is a `try_lock`-only token —
    /// nobody ever awaits it — and being a guard it is released even if the
    /// winner's request is cancelled mid-probe.
    pub free_space_refresh: tokio::sync::Mutex<()>,
    /// The library preview renderer (0.5.1 T6): a one-permit gate plus a small
    /// LRU of rendered JPEGs. Lives here rather than in a global so its lifetime
    /// is the page's, and so tests get a fresh one per state. Engine-independent
    /// — a detached node can still browse and preview its capture folders.
    #[cfg(feature = "preview")]
    pub preview: crate::preview::PreviewCache,
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
            // No watchers until the agent arms them (`attach`); a detached
            // delete's forget is a no-op, which is correct — there is no emitted
            // set to correct.
            watcher_forget: RwLock::new(WatcherForget::none()),
            // No node until the agent binds one (`attach`); an upload-limit edit
            // meanwhile is persisted and applied at the next startup.
            node: RwLock::new(None),
            // Probed on the first `/api/status`, then reused for FREE_SPACE_TTL.
            free_space: Mutex::new(None),
            free_space_refresh: tokio::sync::Mutex::new(()),
            #[cfg(feature = "preview")]
            preview: crate::preview::PreviewCache::new(),
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
        watcher_forget: WatcherForget,
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
        *self.watcher_forget.write().await = watcher_forget;
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
        // The watchers die with the agent, so their forget channels are closed
        // senders: drop them rather than keep sending into the void.
        *self.watcher_forget.write().await = WatcherForget::none();
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
    /// Free space per unique volume behind the capture dirs + data dir. A
    /// volume that could not be probed (offline share, vanished root) is simply
    /// absent — see [`crate::diskspace`].
    volumes: Vec<VolumeDto>,
    /// When the next scheduled send is due (RFC-3339, **local offset** — the
    /// operator configured `06:00` in local time and must read `06:00` back),
    /// or `None` whenever nothing is armed: any mode but `scheduled`, or
    /// `scheduled` with no times.
    ///
    /// Recomputed per poll from the live config rather than plumbed out of the
    /// batcher: it is a pure function of `(schedule_times, now)`
    /// ([`crate::schedule::next_fire`]), so a stateless recompute is both cheaper
    /// than a channel and incapable of going stale — including on a detached
    /// node, which has no batcher to ask.
    next_scheduled_send: Option<String>,
}

/// One volume's free-space reading, for the status header's chips (and, later,
/// the Library tab's root rows).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeDto {
    /// The configured path this reading was taken for (display string).
    root: String,
    /// Bytes available to Perseus on that volume.
    free_bytes: u64,
    /// Total bytes on that volume.
    total_bytes: u64,
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
        // The local library browser (0.5.1 T4): one directory of one capture
        // root, per-file send status joined in. Read-only; send/delete are
        // later tasks on the same `library` contract.
        .route("/api/library", get(api_library))
        // One frame → an auto-stretched JPEG (0.5.1 T6). Registered
        // unconditionally: without the `preview` feature the handler is a `404
        // preview not built` stub, so the router shape never depends on how the
        // binary was compiled.
        .route("/api/library/preview", get(api_library_preview))
        // Send an explicit library selection now, as one `browser` batch
        // (0.5.1 T8, spec §1a). Bearer-gated like every other `/api/*` route.
        .route("/api/library/send", post(api_library_send))
        // Delete a library selection — two steps (preview / confirm) on one
        // route, spec §2's consequence matrix (0.5.1 T9).
        .route("/api/library/delete", post(api_library_delete))
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
        // Send an already-recorded batch to ANOTHER device as a new transfer
        // (0.5.1 §6). Two steps on one route (`confirm`), like /api/library/delete.
        .route("/api/transfers/send-to", post(api_send_to))
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

/// How long one free-space reading is reused before `/api/status` re-probes.
/// Short enough that a filling capture disk is visibly filling, long enough that
/// several browsers polling at 2 s do not each pay for a syscall round-trip.
const FREE_SPACE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// The free-space reading for `paths`, memoized on [`WebState::free_space`].
///
/// **At most one request ever waits on a probe.** The refresher holds only
/// [`WebState::free_space_refresh`] across the blocking syscall, never the memo
/// itself, so a concurrent poller that finds a refresh in flight is served the
/// last reading immediately (see [`last_free_space`]) instead of queueing. A
/// wedged network mount therefore stalls the one caller that started the probe,
/// not every `/api/status` behind it — the status page keeps rendering with a
/// value a few seconds old.
///
/// Never fails: a join error (or any per-path failure inside
/// [`crate::diskspace::probe_volumes`]) degrades to fewer chips, never a 500.
/// The status page has to render with a capture share offline.
async fn free_space_snapshot(state: &WebState, paths: Vec<PathBuf>) -> Vec<VolumeInfo> {
    free_space_snapshot_with(state, paths, |paths| {
        crate::diskspace::probe_volumes(&paths)
    })
    .await
}

/// [`free_space_snapshot`] with the probe injected, so tests can hold one in
/// flight and observe what a concurrent caller is handed.
async fn free_space_snapshot_with<P>(
    state: &WebState,
    paths: Vec<PathBuf>,
    probe: P,
) -> Vec<VolumeInfo>
where
    P: FnOnce(Vec<PathBuf>) -> Vec<VolumeInfo> + Send + 'static,
{
    if let Some(vols) = fresh_free_space(state, &paths) {
        return vols;
    }

    // A refresh is due — but only for whoever takes the token. `try_lock`, never
    // `lock`: a caller that loses serves stale rather than parking on a syscall
    // that a dead mount can hold for minutes.
    let Ok(_refreshing) = state.free_space_refresh.try_lock() else {
        return last_free_space(state);
    };

    // The winner may have raced a refresh that finished between the check above
    // and the token — re-check before paying for a second probe.
    if let Some(vols) = fresh_free_space(state, &paths) {
        return vols;
    }

    let probed = paths.clone();
    let vols = match tokio::task::spawn_blocking(move || probe(probed)).await {
        Ok(vols) => vols,
        Err(error) => {
            tracing::warn!(%error, "web status: free-space probe task failed");
            Vec::new()
        }
    };
    *state.free_space.lock().expect("free_space mutex poisoned") =
        Some((Instant::now(), paths, vols.clone()));
    vols
}

/// The memoized reading — only if it was taken for exactly `paths` and is still
/// inside [`FREE_SPACE_TTL`].
fn fresh_free_space(state: &WebState, paths: &[PathBuf]) -> Option<Vec<VolumeInfo>> {
    let cached = state.free_space.lock().expect("free_space mutex poisoned");
    let (at, cached_paths, vols) = cached.as_ref()?;
    (at.elapsed() < FREE_SPACE_TTL && cached_paths.as_slice() == paths).then(|| vols.clone())
}

/// The last reading of any age, whatever path set it was taken for — what a
/// caller gets when someone else's refresh is in flight. Empty until the first
/// probe has ever completed, which renders as "no chips yet", not an error.
fn last_free_space(state: &WebState) -> Vec<VolumeInfo> {
    state
        .free_space
        .lock()
        .expect("free_space mutex poisoned")
        .as_ref()
        .map(|(_, _, vols)| vols.clone())
        .unwrap_or_default()
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

    // The next calendar send (0.5.1 §3), derived — see `StatusDto`. Seconds
    // resolution and the local offset are both deliberate: the schedule has
    // minute resolution, and rendering it in UTC would show an operator in
    // UTC+3 a "next send" three hours off their own 06:00.
    let send = config.send_cfg();
    let next_scheduled_send = (send.mode == Mode::Scheduled)
        .then(|| crate::schedule::next_fire(&send.schedule_times, chrono::Local::now()))
        .flatten()
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false));

    // Free space per unique volume behind the capture dirs + data dir.
    let mut probe_paths = configured.clone();
    probe_paths.push(config.data_dir.clone());
    let volumes: Vec<VolumeDto> = free_space_snapshot(&state, probe_paths)
        .await
        .into_iter()
        .map(|v| VolumeDto {
            root: v.root.display().to_string(),
            free_bytes: v.free_bytes,
            total_bytes: v.total_bytes,
        })
        .collect();

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
        volumes,
        next_scheduled_send,
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
    /// Whether a node is attached right now — i.e. whether this response's value
    /// could be handed straight to the running transport.
    ///
    /// Read it as "applied to a live node **at this instant**", not as "the file
    /// value is in force": on a `PUT` those coincide (the handler applies before
    /// answering), but on a `GET` it only reports node presence. `false` means the
    /// engine is detached (setup, restart, or a launch still in flight) and the
    /// persisted value is not live *yet* — it converges regardless, either at the
    /// next bind (`Agent::start` applies it) or within one supervisor pass (the
    /// pass pushes any on-disk change onto the running node). So `false` is never
    /// "lost", only "not yet".
    applied_live: bool,
}

/// `PUT /api/upload-limit` request body. Decimal MB/s; `0` = unlimited.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadLimitEdit {
    max_upload_mbps: u32,
}

/// `GET /api/upload-limit` — the configured sync upload cap (MB/s, `0` =
/// unlimited), plus whether a node is attached to apply it to right now
/// (`appliedLive`, see [`UploadLimitDto`] — node presence, not "the file value is
/// in force"). Read-only.
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
/// mid-transfer included. This direct apply exists for instant feedback while the
/// node is up; it is not the only route — the supervisor pass pushes any on-disk
/// change onto the running node, which is what covers a hand-edited
/// `perseus.toml`.
///
/// With no node attached (engine in setup/restart, or a launch still in flight)
/// the edit is file-only and the response says so via `appliedLive: false`. It
/// still converges: whichever of the two gets there first — `Agent::start`'s
/// bind-time apply, or the next supervisor pass reconciling the file against what
/// the node was last given. That reconciliation is also what repairs a PUT that
/// raced a launch (the launch applied the pre-edit snapshot). The supervisor is
/// woken so its config view — and that push — happen at once rather than on the
/// next tick. Returns the applied `{maxUploadMbps, appliedLive}`.
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
    /// The current send mode (`auto` | `scheduled` | `manual`) — the same
    /// snake_case string the TOML uses ([`crate::config_edit::mode_str`]).
    mode: String,
    /// The auto-mode quiet window in seconds (inert in the other two modes).
    auto_quiet_secs: u64,
    /// The scheduled-mode send times, normalised `HH:MM` (see
    /// [`schedule_times_wire`]). Carried here as well as on `/api/send-mode` so
    /// the page renders the whole To-Sync strip — mode, quiet window, schedule —
    /// off the single 2 s poll, and can never PUT a schedule it never read.
    schedule_times: Vec<String>,
    /// Whether a schedule point missed while the agent was down fires once at
    /// startup.
    schedule_catchup: bool,
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
    /// Normalised `HH:MM` schedule points (see [`schedule_times_wire`]).
    /// Reported in **every** mode, not only `scheduled`: switching to
    /// `auto`/`manual` keeps the times in the file, and the page has to be able
    /// to show (and re-arm from) them without the operator retyping. Only
    /// `scheduled` mode makes the batcher act on them.
    schedule_times: Vec<String>,
    schedule_catchup: bool,
}

/// `PUT /api/send-mode` request body. `mode` is a free string (not the [`Mode`]
/// enum) so an unknown value is a clean `400` from the handler rather than a
/// `422` deserialization error from the extractor.
///
/// The two schedule fields are `Option` and **absent means "leave it alone"**,
/// not "clear it": a client that predates the scheduler (a browser tab loaded
/// before this build, which still PUTs `{mode, autoQuietSecs}`) must not erase
/// an operator's send times as a side effect of flipping the mode. Sending
/// `scheduleTimes: []` explicitly IS a clear — and then the validator refuses it
/// if the mode is `scheduled`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendModeEdit {
    mode: String,
    auto_quiet_secs: u64,
    #[serde(default)]
    schedule_times: Option<Vec<String>>,
    #[serde(default)]
    schedule_catchup: Option<bool>,
}

/// The wire form of a [`SendCfg`]'s schedule points: zero-padded `HH:MM`, in the
/// canonical (sorted, deduped) order [`crate::schedule::parse_points`] produced.
///
/// Rendering from the parsed points rather than the raw `schedule_times` strings
/// is deliberate — the page shows what the scheduler will actually do, so an
/// operator who hand-edited `["14:30", "6:00", "6:00"]` into the file reads back
/// the two points that exist, in the order they fire.
fn schedule_times_wire(send: &SendCfg) -> Vec<String> {
    send.schedule_times
        .iter()
        .map(|(h, m)| format!("{h:02}:{m:02}"))
        .collect()
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
///
/// The strings are exactly [`crate::config_edit::mode_str`]'s, so a value read
/// out of a `GET` can always be PUT back unchanged. `scheduled` was deliberately
/// absent until this task: T12 taught the *config* about the mode while the web
/// edit path could not yet carry its times, and accepting the mode without them
/// would have written a file the validator rejects.
fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "auto" => Some(Mode::Auto),
        "scheduled" => Some(Mode::Scheduled),
        "manual" => Some(Mode::Manual),
        _ => None,
    }
}

/// Query for [`api_library`]: which capture root, and which directory in it.
#[derive(serde::Deserialize)]
struct LibraryQuery {
    /// Index into [`Config::capture_dirs_resolved`]. Defaults to the first root,
    /// which is the only one on a single-root node.
    #[serde(default)]
    root: usize,
    /// Forward-slash wire rel-path of the directory to list; the default `""` is
    /// the root itself.
    #[serde(default)]
    path: String,
}

/// Map a [`crate::library`] error onto the HTTP status contract built on T1's
/// stable message prefixes.
///
/// The FULL anyhow chain is logged with `{:#}` **first**, unconditionally:
/// `resolve_in_root` labels any failed canonicalize of the target `"not found"`,
/// so a permission hole and a genuinely absent file produce the same 404 — only
/// the logged chain says which it really was.
fn library_error_status(
    err: &anyhow::Error,
    root_index: usize,
    path: &str,
) -> (StatusCode, String) {
    let chain = format!("{err:#}");
    tracing::error!(error = %chain, root_index, path, "web library: listing failed");
    library_status_for(&err.to_string(), &chain)
}

/// The same mapping for the preview route (0.5.1 T6). Split from
/// [`library_error_status`] only so each route logs its own stable message —
/// the status contract itself is shared, deliberately: a path that 404s for the
/// listing must 404 for the preview too.
#[cfg(feature = "preview")]
fn preview_error_status(
    err: &anyhow::Error,
    root_index: usize,
    path: &str,
) -> (StatusCode, String) {
    let chain = format!("{err:#}");
    tracing::error!(error = %chain, root_index, path, "web library: preview failed");
    library_status_for(&err.to_string(), &chain)
}

/// The pure prefix → status decision, shared by every library route.
///
/// `head` is the error's own message (the stable prefix); `chain` is the full
/// `{:#}` rendering, used only as the body of the catch-all `500` where there is
/// no contract to honour and the operator needs everything we know.
fn library_status_for(head: &str, chain: &str) -> (StatusCode, String) {
    let head = head.to_string();
    if head.starts_with("canonicalize root") {
        // The configured root itself is unreachable — an unmounted share, the T3
        // offline-at-boot case. That is an upstream fault, not a bad request, and
        // NOT a 404 (which would read as "the path you asked for is gone").
        (StatusCode::BAD_GATEWAY, "root unavailable".to_string())
    } else if head.starts_with("not found") {
        (StatusCode::NOT_FOUND, head)
    } else if head.starts_with("invalid path segment")
        || head.starts_with("path escapes root")
        || head.starts_with("not a directory")
        // Send/delete only (T8+): the client said "file" (or "directory") and the
        // filesystem says otherwise. The path resolved, so it is the request that
        // is wrong — a stale listing — not the target that is missing.
        || head.starts_with("not a file")
    {
        (StatusCode::BAD_REQUEST, head)
    } else if head.starts_with("not a renderable frame") {
        // Preview-only (T6): the path resolved perfectly, the file is simply not
        // something we can turn into pixels. That is the media type being wrong,
        // not the request — a `400` would send the UI hunting for a bad path.
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, head)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, chain.to_string())
    }
}

/// `GET /api/library?root=<idx>&path=<rel>` — one directory of one capture root,
/// each file carrying its derived send status.
///
/// The three status sources are snapshotted ONCE per request (the batcher's
/// pending set, every outbound row, and the two Perseus stores) and handed to
/// [`list_directory`](crate::library::list_directory) as borrowed handles, so the
/// per-file join never re-reads them.
///
/// The whole filesystem + SQLite pass runs on a blocking thread: a capture root is
/// routinely a network share, and a wedged mount must stall one blocking-pool
/// thread rather than a reactor worker (the same stance as the free-space probe).
async fn api_library(
    State(state): State<Arc<WebState>>,
    Query(q): Query<LibraryQuery>,
) -> Result<Json<crate::library::LibraryListing>, (StatusCode, String)> {
    let roots = state.config.read().await.capture_dirs_resolved();
    let Some(root) = roots.get(q.root).cloned() else {
        tracing::error!(
            root_index = q.root,
            count = roots.len(),
            "web library: root index out of range"
        );
        return Err((StatusCode::NOT_FOUND, "unknown root".to_string()));
    };
    // A detached page (engine in setup) has no batcher: nothing is pending, which
    // is the honest answer, not an error.
    let pending = match state.batcher.read().await.as_ref() {
        Some(b) => b.pending_snapshot(),
        None => Vec::new(),
    };
    // Unbounded window on purpose: a file's status must not flip to `unsent`
    // because its batch fell out of a recent-N cap.
    let outbound = state.store.all_outbound(u32::MAX).map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web library: read outbound failed");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

    let batches = Arc::clone(&state.batches);
    let seen = Arc::clone(&state.seen);
    let (root_index, rel) = (q.root, q.path.clone());
    let listed = tokio::task::spawn_blocking(move || {
        let src = crate::library::StatusSources {
            pending: &pending,
            batches: &batches,
            seen: &seen,
            outbound: &outbound,
        };
        crate::library::list_directory(root_index, &root, &rel, &src)
    })
    .await
    .map_err(|e| {
        let msg = format!("{e}");
        tracing::error!(error = %msg, "web library: listing task panicked");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

    match listed {
        Ok(listing) => Ok(Json(listing)),
        Err(e) => Err(library_error_status(&e, q.root, &q.path)),
    }
}

/// Query for [`api_library_preview`]: which file, and how wide.
#[cfg(feature = "preview")]
#[derive(serde::Deserialize)]
struct PreviewQuery {
    #[serde(default)]
    root: usize,
    /// Forward-slash wire rel-path of the FILE to render (the T1 contract).
    #[serde(default)]
    path: String,
    /// Target width in pixels, clamped into
    /// [`MIN_WIDTH`](crate::preview::MIN_WIDTH)..=[`MAX_WIDTH`](crate::preview::MAX_WIDTH)
    /// by the renderer. Absent means [`DEFAULT_PREVIEW_WIDTH`].
    #[serde(default = "default_preview_width")]
    w: u32,
}

/// Width served when the client does not ask for one — a comfortable inline
/// pane on a laptop, and cheap enough for a Pi to produce in well under a second.
#[cfg(feature = "preview")]
const DEFAULT_PREVIEW_WIDTH: u32 = 640;

#[cfg(feature = "preview")]
fn default_preview_width() -> u32 {
    DEFAULT_PREVIEW_WIDTH
}

/// Does the request's `If-None-Match` cover `etag`?
///
/// Handles the list form (`"a", "b"`), the `*` wildcard, and the `W/` weak
/// prefix — all of which a real browser or proxy will send. The comparison is
/// deliberately weak (RFC 9110 §13.1.2: `If-None-Match` uses weak comparison),
/// so `W/"x"` matches `"x"`.
#[cfg(feature = "preview")]
fn if_none_match_matches(headers: &axum::http::HeaderMap, etag: &str) -> bool {
    let Some(raw) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    raw.split(',').any(|tok| {
        let tok = tok.trim();
        tok == "*" || tok.strip_prefix("W/").unwrap_or(tok) == etag
    })
}

/// `GET /api/library/preview?root=<idx>&path=<rel>&w=<px>` — one frame, rendered
/// to an auto-stretched JPEG (0.5.1 T6).
///
/// The shape is built around making the *repeat* request free. One `stat(2)`
/// yields the [`PreviewKey`](crate::preview::PreviewKey), the key yields the
/// ETag, and a matching `If-None-Match` is answered `304` right there — before
/// the concurrency gate, before any decode, without even needing the bytes to
/// still be cached. Only a genuine miss reaches
/// [`render_jpeg`](crate::preview::render_jpeg), which serializes renders and
/// keeps the last few results.
///
/// Statuses mirror `/api/library` exactly (`502` offline root, `404` missing,
/// `400` hostile path) plus two of its own: `415` for a file that is not a
/// FITS/XISF frame, and `422` when the file IS one but will not decode.
///
/// The resolve + stat runs on a blocking thread for the same reason the listing
/// does: a capture root is routinely a network share, and a wedged mount must
/// stall one blocking-pool thread rather than a reactor worker.
#[cfg(feature = "preview")]
async fn api_library_preview(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PreviewQuery>,
) -> Result<Response, (StatusCode, String)> {
    let roots = state.config.read().await.capture_dirs_resolved();
    let Some(root) = roots.get(q.root).cloned() else {
        tracing::error!(
            root_index = q.root,
            count = roots.len(),
            "web preview: root index out of range"
        );
        return Err((StatusCode::NOT_FOUND, "unknown root".to_string()));
    };

    let rel = q.path.clone();
    let requested_width = q.w;
    let resolved = tokio::task::spawn_blocking(
        move || -> anyhow::Result<(PathBuf, crate::preview::PreviewKey)> {
            // T1 guard first: nothing below may see a path that has not been
            // canonicalized and prefix-checked against the root.
            let abs = crate::library::resolve_in_root(&root, &rel)?;
            // Same extension set the watcher enqueues — the browser must not be
            // able to talk this route into decoding perseus.toml. The `is_file`
            // half catches a DIRECTORY that happens to be named `something.fits`:
            // without it that reaches the decoder and comes back as a confusing
            // `422 "failed to read image"` instead of an honest `415`.
            if !abs.is_file() || !crate::watcher::is_eligible(&abs) {
                anyhow::bail!("not a renderable frame: {rel:?}");
            }
            let key = crate::preview::PreviewKey::stat(&abs, requested_width)?;
            Ok((abs, key))
        },
    )
    .await
    .map_err(|e| {
        let msg = format!("{e}");
        tracing::error!(error = %msg, "web preview: resolve task panicked");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

    let (abs, key) = match resolved {
        Ok(v) => v,
        Err(e) => return Err(preview_error_status(&e, q.root, &q.path)),
    };

    let etag = key.etag();
    // `no-cache` (store it, but revalidate) rather than `no-store`: it is exactly
    // what turns the ETag into a saving — the browser keeps the JPEG and we
    // answer the next look with an empty 304.
    let cache_control = "private, no-cache";
    if if_none_match_matches(&headers, &etag) {
        tracing::debug!(path = %abs.display(), width = key.width(), "preview not modified");
        return Ok((
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::CACHE_CONTROL, cache_control.to_string()),
            ],
        )
            .into_response());
    }

    let bytes = crate::preview::render_jpeg(&state.preview, &key, &abs)
        .await
        .map_err(|e| {
            let chain = format!("{e:#}");
            tracing::error!(
                error = %chain,
                root_index = q.root,
                path = %q.path,
                "web preview: render failed"
            );
            (StatusCode::UNPROCESSABLE_ENTITY, chain)
        })?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/jpeg".to_string()),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, cache_control.to_string()),
        ],
        // One copy out of the shared cache buffer. The alternative (handing the
        // `Arc` to the body) would pin a cache entry for as long as a slow client
        // takes to drain it; a few hundred KB memcpy is the cheaper trade.
        axum::body::Body::from(bytes.to_vec()),
    )
        .into_response())
}

/// Stub for builds without the `preview` feature, so the router has the same
/// shape either way and the UI gets an honest, greppable answer instead of a
/// bare axum 404 that looks like a version mismatch.
#[cfg(not(feature = "preview"))]
async fn api_library_preview() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "preview not built".to_string())
}

/// `POST /api/library/send` request body (0.5.1 §1a).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibrarySendRequest {
    /// Which running send targets to fan this batch out to — each either a peer
    /// device id (hex) or that peer's friendly device name. Empty / omitted
    /// means **every** configured target, exactly what an auto or manual flush
    /// does; an unresolvable *or ambiguous* entry is a `400` before anything is
    /// built.
    #[serde(default)]
    targets: Vec<String>,
    /// The browser selection: files and/or whole directories, in the
    /// `(root, rel)` wire form every library route uses.
    items: Vec<crate::library::SendItem>,
}

/// `POST /api/library/send` success payload.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LibrarySendReport {
    /// Files that actually made it into the package. Can be fewer than the
    /// selection asked for: a named file that has already vanished is dropped
    /// during expansion, and one that will not parse is dropped at build time —
    /// both with a `warn!`, and the rest of the batch proceeds (spec §1a).
    enqueued: usize,
    /// How many named files were dropped during expansion because they were no
    /// longer on disk. The counterpart to `enqueued`: together they let the page
    /// say "sent 4, 1 vanished" instead of leaving the operator to work out why
    /// fewer files shipped than were picked. `0` on a whole-selection send.
    skipped: usize,
    /// The package directory the batch was staged in — the same `package_ref`
    /// the transfers list and the `perseus_batch` row key on.
    package_ref: String,
}

/// Map a selection-expansion error onto the shared library status contract.
/// Split from [`library_error_status`] only so each route logs its own stable
/// message; the statuses themselves are deliberately identical.
fn send_error_status(err: &anyhow::Error) -> (StatusCode, String) {
    let chain = format!("{err:#}");
    tracing::error!(error = %chain, "web library send: selection expansion failed");
    library_status_for(&err.to_string(), &chain)
}

/// Resolve the request's `targets` onto running engines.
///
/// `None` (an empty request list) means "the batcher's own fan-out set" — the
/// status quo for every other send. A named target matches a running peer's id
/// (hex) or its friendly device name, case-insensitively; repeats collapse, so
/// naming a device twice cannot enqueue the same package to it twice. An
/// unknown name is a `400`: sending "to a device" that is not a configured
/// target must fail loudly, never silently fan out to everyone.
///
/// So is an **ambiguous** one. Friendly device names are not unique — two nodes
/// can genuinely both be called "obs-pi" — and picking whichever one the engine
/// list happens to hold first would report a `200` for a batch that reached the
/// other machine. The operator gets a `400` naming the candidates and re-sends
/// by peer id (always unique).
///
/// **Ambiguity is counted in DEVICES, not in matching entries.** The engine list
/// can legitimately hold the same peer twice (a device configured under both its
/// id and its name, a duplicated target entry), and both of those speak to one
/// machine — there is nothing for the operator to disambiguate, so refusing the
/// send would be a false `400`. Matches are therefore collapsed by resolved peer
/// hex *before* the one/many decision; only genuinely different peers can make a
/// name ambiguous.
fn resolve_send_targets(
    engines: &[(String, Arc<SyncEngineHandle>)],
    names: &HashMap<String, String>,
    targets: &[String],
) -> Result<Option<Vec<Arc<SyncEngineHandle>>>, (StatusCode, String)> {
    if targets.is_empty() {
        return Ok(None);
    }
    let mut chosen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for want in targets {
        let (peer, engine) = match resolve_one_target(engines, names, want) {
            Ok(hit) => hit,
            Err(TargetMiss::Unknown) => {
                tracing::error!(target = %want, "web library send: unknown send target");
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("unknown send target: {want}"),
                ));
            }
            Err(TargetMiss::Ambiguous(peers)) => {
                let count = peers.len();
                let peers = peers.join(", ");
                tracing::error!(
                    target = %want,
                    count,
                    peers = %peers,
                    "web library send: ambiguous send target"
                );
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!(
                        "ambiguous target name: {want} matches {count} devices ({peers}) — send by device id"
                    ),
                ));
            }
        };
        if chosen.iter().any(|p| p == peer) {
            continue;
        }
        chosen.push(peer.clone());
        out.push(Arc::clone(engine));
    }
    Ok(Some(out))
}

/// Why one requested target did not land on exactly one running engine.
/// Rendered into a `400` by each route in its own words — the same failure means
/// something different to "send these files" than it does to "divert this batch".
enum TargetMiss {
    /// Nothing running answers to this id or name.
    Unknown,
    /// Two or more genuinely different peers answer to it (hex ids of each).
    Ambiguous(Vec<String>),
}

/// Resolve ONE requested target — a peer id (hex) or a friendly device name,
/// case-insensitively — against the RUNNING engines.
///
/// Shared by every route that sends somewhere, so they cannot drift on what
/// "that device" means. See [`resolve_send_targets`] for why ambiguity is
/// counted in devices rather than in matching entries.
fn resolve_one_target<'a>(
    engines: &'a [(String, Arc<SyncEngineHandle>)],
    names: &HashMap<String, String>,
    want: &str,
) -> Result<(&'a String, &'a Arc<SyncEngineHandle>), TargetMiss> {
    let mut matched: Vec<(&String, &Arc<SyncEngineHandle>)> = engines
        .iter()
        .filter(|(peer, _)| {
            peer.eq_ignore_ascii_case(want)
                || names
                    .get(peer)
                    .is_some_and(|name| name.eq_ignore_ascii_case(want))
        })
        .map(|(peer, engine)| (peer, engine))
        .collect();
    // Collapse repeats of the SAME peer first: one device reached through two
    // engine entries is not an ambiguity, and only what survives this may be
    // weighed one-versus-many below.
    let mut seen_peers: HashSet<String> = HashSet::new();
    matched.retain(|(peer, _)| seen_peers.insert(peer.to_ascii_lowercase()));
    match matched.as_slice() {
        [] => Err(TargetMiss::Unknown),
        [one] => Ok(*one),
        many => Err(TargetMiss::Ambiguous(
            many.iter().map(|(peer, _)| (*peer).clone()).collect(),
        )),
    }
}

/// `POST /api/library/send` — send an arbitrary library selection **now**, as
/// one explicit `browser` batch (spec §1a).
///
/// This is not a flush: the operator picked these files, so already-sent ones are
/// deliberately allowed (re-sending is the button's whole point — the receiver's
/// dedup handshake decides what actually travels, and a fully-duplicate selection
/// confirms as "already on peer"). Whatever of the selection is sitting in the
/// batcher's pending set is taken OUT of it first, inside
/// [`BatcherHandle::send_explicit`], so the next auto or scheduled flush cannot
/// send it a second time.
///
/// Delivery and bookkeeping are the batcher's own — same build, same fan-out,
/// same record-seen-only-what-shipped rule, same batch rows — with one
/// difference: a batch that reaches no target is **not** re-queued for the next
/// automatic flush. It is a `502` with its staged package removed and its own
/// pending removal undone (a failed send changes nothing), and the operator
/// retries explicitly.
///
/// Statuses: `503` while the engine is detached (nothing to send into), `400` for
/// a hostile / mistyped path or an unknown / ambiguous target, `404` for an
/// unknown root or a missing directory, `502` for an unmounted root or a
/// zero-target delivery, `422` when the selection expands to nothing or nothing
/// in it can be built.
///
/// A named file that has vanished under a stale listing is **not** a `404`: it is
/// dropped from the selection with a `warn!` and the rest ships (spec §1a) — the
/// browser listing is a snapshot, and one deleted frame must not fail the whole
/// send. Only a selection that expands to *nothing* becomes the `422`.
///
/// The expansion (canonicalize + a recursive walk) runs on a blocking thread for
/// the same reason the listing does: a capture root is routinely a network share.
async fn api_library_send(
    State(state): State<Arc<WebState>>,
    Json(req): Json<LibrarySendRequest>,
) -> Result<Json<LibrarySendReport>, (StatusCode, String)> {
    // Sending needs both the running engines (somewhere to send) and the running
    // batcher (the send seam + the pending accumulator the guard clears). They
    // arrive and leave together on attach/detach, so either being absent is the
    // same honest answer the other engine-dependent write routes give.
    let engines = state.engines.read().await.clone();
    let batcher = match state.batcher.read().await.clone() {
        Some(batcher) if !engines.is_empty() => batcher,
        _ => {
            tracing::warn!("web library send: sync engine is not running");
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "sync engine is not running — finish setup first".to_string(),
            ));
        }
    };

    // Targets first: an unknown one must fail before any file is hashed or copied.
    let names = state.device_names.read().await.clone();
    let selected = resolve_send_targets(&engines, &names, &req.targets)?;

    let config = state.config.read().await.clone();
    let items = req.items;
    let expanded =
        tokio::task::spawn_blocking(move || crate::library::expand_selection(&config, &items))
            .await
            .map_err(|e| {
                let msg = format!("{e}");
                tracing::error!(error = %msg, "web library send: expansion task panicked");
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            })?;
    let (files, skipped) = expanded.map_err(|e| send_error_status(&e))?;

    if files.is_empty() {
        tracing::warn!("web library send: selection expanded to no files");
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "selection contains no files to send".to_string(),
        ));
    }

    match batcher.send_explicit(&files, selected.as_deref()).await {
        Delivery::Sent(outcome) => {
            tracing::info!(
                package_ref = %outcome.package_ref,
                file_count = outcome.file_count,
                selected = files.len(),
                skipped,
                targets = selected.as_ref().map_or(engines.len(), Vec::len),
                "library selection sent as a browser batch"
            );
            Ok(Json(LibrarySendReport {
                enqueued: outcome.file_count,
                skipped,
                package_ref: outcome.package_ref,
            }))
        }
        Delivery::Unbuildable(error) => {
            // Every picked file vanished or will not parse. Nothing was written,
            // nothing was recorded — say so instead of reporting an empty send.
            let msg = format!("{error:#}");
            tracing::error!(error = %msg, count = files.len(), "web library send: nothing buildable");
            Err((StatusCode::UNPROCESSABLE_ENTITY, msg))
        }
        Delivery::NoTarget {
            package_ref,
            file_count,
        } => {
            tracing::error!(
                package_ref = %package_ref,
                file_count,
                "web library send: package reached no target"
            );
            Err((
                StatusCode::BAD_GATEWAY,
                "no target accepted the package".to_string(),
            ))
        }
    }
}

/// `POST /api/library/delete` request body (0.5.1 §2). Two steps, one route.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryDeleteRequest {
    /// The browser selection: files and/or whole directories, in the
    /// `(root, rel)` wire form every library route uses.
    items: Vec<crate::library::SendItem>,
    /// `false` (the default) previews and touches nothing; `true` performs.
    /// Defaulting to the harmless step is deliberate: a malformed or truncated
    /// body can only ever under-delete.
    #[serde(default)]
    confirm: bool,
}

/// `POST /api/library/delete` payload. Exactly one of the two lists is
/// populated, per `confirmed` — so a client that ignored its own `confirm` flag
/// still cannot mistake a preview for a completed deletion.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryDeleteReport {
    /// Echo of the request's `confirm`: did this call actually delete?
    confirmed: bool,
    /// One row per file the selection resolves to, with the consequence of
    /// removing it. Empty when `confirmed`.
    preview: Vec<crate::library::DeletePreviewItem>,
    /// One row per path acted on. Empty when not `confirmed`.
    outcomes: Vec<crate::library::DeleteOutcomeItem>,
}

/// Map a delete-planning error onto the shared library status contract. Split
/// from [`library_error_status`] only so each route logs its own stable message.
fn delete_error_status(err: &anyhow::Error) -> (StatusCode, String) {
    let chain = format!("{err:#}");
    tracing::error!(error = %chain, "web library delete: selection planning failed");
    library_status_for(&err.to_string(), &chain)
}

/// `POST /api/library/delete` — the operator deletes anything, at any moment,
/// with the consequence stated instead of the deletion forbidden (spec §2).
///
/// Two steps on one route. `confirm: false` returns the per-file preview and
/// touches nothing — for each file: is it waiting in the batcher, which live
/// transfers carry it, how many confirmed batches would degrade. `confirm: true`
/// performs and returns a per-path outcome. Both plan the same way over the same
/// selection, so the dialog and the action differ only by what really changed on
/// disk in between — and that lands as a per-file outcome, not a surprise.
///
/// Deletion is engine-INDEPENDENT: a detached node (still in setup) can delete
/// its capture folders, it simply has no pending set to clear. That is why this
/// route, unlike `/api/library/send`, has no `503` arm.
///
/// Statuses: `400` for a hostile / mistyped path or a stale kind (`"not a
/// file"`), `404` for an unknown root, `502` for an unmounted one. A path that
/// merely vanished is **not** a status — it is that item's own `error` row,
/// while the rest of the selection proceeds.
///
/// The planning walk, the fs deletions and the SQLite writes all run on blocking
/// threads: a capture root is routinely a network share, and a wedged mount must
/// stall one blocking-pool thread rather than a reactor worker.
async fn api_library_delete(
    State(state): State<Arc<WebState>>,
    Json(req): Json<LibraryDeleteRequest>,
) -> Result<Json<LibraryDeleteReport>, (StatusCode, String)> {
    let config = state.config.read().await.clone();
    let items = req.items;
    let plan = tokio::task::spawn_blocking(move || crate::library::plan_deletion(&config, &items))
        .await
        .map_err(|e| {
            let msg = format!("{e}");
            tracing::error!(error = %msg, "web library delete: planning task panicked");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?
        .map_err(|e| delete_error_status(&e))?;

    // A detached page has no batcher: nothing is pending, which is the honest
    // answer rather than an error (the same stance the listing takes).
    let pending = match state.batcher.read().await.as_ref() {
        Some(b) => b.pending_snapshot(),
        None => Vec::new(),
    };

    if !req.confirm {
        // Unbounded window on purpose, exactly as in the listing: a file's
        // in-flight batch must not go unmentioned because it fell out of a cap.
        let outbound = state.store.all_outbound(u32::MAX).map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "web library delete: read outbound failed");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;
        let batches = Arc::clone(&state.batches);
        let preview = tokio::task::spawn_blocking(move || {
            let src = crate::library::DeleteSources {
                pending: &pending,
                batches: &batches,
                outbound: &outbound,
            };
            crate::library::delete_preview(&plan, &src)
        })
        .await
        .map_err(|e| {
            let msg = format!("{e}");
            tracing::error!(error = %msg, "web library delete: preview task panicked");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?
        .map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(error = %msg, "web library delete: preview failed");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;
        tracing::info!(files = preview.len(), "library delete preview");
        return Ok(Json(LibraryDeleteReport {
            confirmed: false,
            preview,
            outcomes: Vec::new(),
        }));
    }

    let batcher = state.batcher.read().await.clone();
    // Cloned out of the lock, like `batcher`: the pass runs on a blocking thread
    // and re-arms the running watchers for every path it unlinks (T9b).
    let watcher_forget = state.watcher_forget.read().await.clone();
    let store = Arc::clone(&state.store);
    let seen = Arc::clone(&state.seen);
    let report = tokio::task::spawn_blocking(move || {
        let ctx = crate::library::DeleteContext {
            store: &*store,
            seen: &seen,
            batcher: batcher.as_ref(),
            actor: crate::run::MANUAL_WEB_ACTOR,
            watcher_forget: &watcher_forget,
        };
        crate::library::delete_perform(&plan, &ctx)
    })
    .await
    .map_err(|e| {
        let msg = format!("{e}");
        tracing::error!(error = %msg, "web library delete: delete task panicked");
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    })?;

    // Spec §2's audit half: the per-file `sync_history` rows are already
    // written; this puts the PASS in the retention log beside the automatic
    // ones, labelled by its actor so an operator reading that log can tell a
    // policy deletion from their own.
    if !report.deleted_paths.is_empty() || !report.notes.is_empty() {
        let record = RetentionRunRecord {
            at: now_rfc3339(),
            dry_run: false,
            policy: crate::run::MANUAL_WEB_ACTOR.to_string(),
            deleted: report.deleted_paths.clone(),
            // A manual pass has no eligibility phase: the operator's selection
            // IS the verdict.
            would_delete: Vec::new(),
            errors: report.notes.clone(),
        };
        let handle = state.retention_log.read().await;
        let mut log = handle.lock().expect("retention_log mutex poisoned");
        log.push_front(record);
        log.truncate(50);
    }

    tracing::info!(
        deleted = report.deleted_paths.len(),
        reported = report.items.len(),
        problems = report.notes.len(),
        "library delete finished"
    );
    Ok(Json(LibraryDeleteReport {
        confirmed: true,
        preview: Vec::new(),
        outcomes: report.items,
    }))
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
        schedule_times: schedule_times_wire(&send),
        schedule_catchup: send.schedule_catchup,
        count,
    })
}

/// `GET /api/send-mode` — the current send mode, auto quiet window and schedule.
/// Read-only.
async fn api_get_send_mode(State(state): State<Arc<WebState>>) -> Json<SendModeDto> {
    let send = state.config.read().await.send_cfg();
    Json(SendModeDto {
        mode: crate::config_edit::mode_str(send.mode).to_string(),
        auto_quiet_secs: send.auto_quiet_secs,
        schedule_times: schedule_times_wire(&send),
        schedule_catchup: send.schedule_catchup,
    })
}

/// `PUT /api/send-mode` — switch between Auto / Scheduled / Manual (and/or change
/// the quiet window or the schedule), **live**. An unknown `mode` string is a
/// `400` before anything is touched. Otherwise [`apply_send_mode_edit`] rewrites
/// `perseus.toml` (comment-preserving, re-validated, atomic — a rejected edit
/// leaves the file byte-identical and returns `422`), the live config is swapped,
/// and the new [`SendCfg`] is pushed onto the batcher's `send_cfg_tx` so the
/// running batcher adopts it on its next select! turn — no restart. The
/// supervisor is woken so its config view refreshes at once. Returns the applied
/// `{mode, autoQuietSecs, scheduleTimes, scheduleCatchup}`.
///
/// Mode and schedule travel into **one** file edit: `scheduled` with no times is
/// a validation error, so switching to it and supplying its times must be a
/// single validated document or the legitimate switch would be impossible (see
/// [`apply_send_mode_edit`]). A `scheduled` PUT that brings no usable time is
/// therefore a `422` carrying the validator's own actionable message, with the
/// file untouched — never a half-applied mode.
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
    let new_cfg = apply_send_mode_edit(
        &state.config_path,
        mode,
        edit.auto_quiet_secs,
        edit.schedule_times.as_deref(),
        edit.schedule_catchup,
    )
    .map_err(|e| {
        let msg = format!("{e:#}");
        tracing::error!(error = %msg, "web send-mode edit rejected");
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;
    let send = new_cfg.send_cfg();
    *state.config.write().await = new_cfg;
    // Live-apply: push the new send config onto the running batcher's watch
    // channel (a no-op send when detached — no receiver). This is what makes the
    // mode / quiet-window / schedule change take effect with no engine restart —
    // the batcher re-arms its schedule timer on every config change (T13).
    let _ = state.send_cfg_tx.read().await.send(send.clone());
    // Wake the supervisor so its per-pass config view refreshes immediately.
    state.supervisor_wake.notify_one();
    Ok(Json(SendModeDto {
        mode: crate::config_edit::mode_str(send.mode).to_string(),
        auto_quiet_secs: send.auto_quiet_secs,
        schedule_times: schedule_times_wire(&send),
        schedule_catchup: send.schedule_catchup,
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
    let watcher_forget = state.watcher_forget.read().await.clone();
    let detail = match delete_package_sources(
        &*state.store,
        &state.seen,
        Path::new(&batch.package_ref),
        "deleted_manual",
        &peer_device,
        &watcher_forget,
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

/// `POST /api/transfers/send-to` request body (0.5.1 §6). Two steps, one route
/// — the same shape `POST /api/library/delete` uses.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendToRequest {
    /// The outbound row to re-send from (any row of the batch — every fan-out
    /// row of one batch shares its package dir).
    id: i64,
    /// The device to send to: a peer id (hex) or its friendly device name,
    /// resolved against the RUNNING engines.
    target: String,
    /// `false` (the default) counts what would travel and touches nothing;
    /// `true` builds and queues the transfer. Defaulting to the harmless step is
    /// deliberate: a malformed or truncated body can only ever under-send.
    #[serde(default)]
    confirm: bool,
}

/// `POST /api/transfers/send-to` payload. On the preview step `newId` is
/// `null` and nothing was built.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SendToReport {
    /// Which step answered — so a client that ignored its own `confirm` flag
    /// still cannot mistake a preview for a queued transfer.
    confirmed: bool,
    /// The new outbound row's id (`confirm: true` only).
    new_id: Option<i64>,
    /// Files the new transfer carries (the eligible subset).
    sent: usize,
    /// Files the batch recorded that can no longer be served — the original is
    /// gone from disk, or was rewritten since it shipped.
    skipped: usize,
}

/// `POST /api/transfers/send-to` — send an already-recorded batch to another
/// device as a brand-NEW transfer (0.5.1 §6).
///
/// Fresh package dir basename ⇒ fresh wire `batch_uuid` ⇒ a brand-new inbound
/// row on the chosen peer, while **the source batch and its history are left
/// exactly as they are** ([`resend::SourceDisposition::Keep`]) — it is still
/// serving its Files tab, its *Delete source files* verdict and any in-place
/// resend. Bytes come from the package dir when it still holds them, else are
/// rebuilt from the original capture files; whatever no longer exists is
/// dropped and counted rather than failing the send (the owner rule: act on the
/// eligible portion, report `N of M`).
///
/// Two steps on one route: `confirm: false` resolves the same plan read-only and
/// answers with the counts the confirm dialog shows, `confirm: true` performs.
/// The two are independent reads of the disk, so a file deleted between them
/// simply shows up in the confirm's own `skipped` — the executed counts are the
/// authoritative ones.
///
/// **The target must be a device with a running engine on THIS node**, which is
/// a strict subset of the account's device list: an account device that is not
/// in this node's `targets` (or whose engine failed to start) has nothing here
/// to send through, and is refused by name rather than silently fanned out to
/// whoever is around. This narrows §6's "picker of the account's receive-capable
/// devices" to what can actually be honoured; the UI picker lists the same
/// running set the library Send dialog does.
///
/// Sending to the SOURCE row's own peer is deliberately allowed: it is an
/// explicit operator re-ask, and the receiver's dedup handshake decides what
/// actually travels (a fully-duplicate re-ask confirms as "already on peer").
///
/// Statuses: `503` while the engine is detached, `404` for an unknown row,
/// `400` for an unknown / ambiguous target, `409` when the batch is still in
/// flight or when every one of its files is gone from disk, `500` for a genuine
/// build failure.
async fn api_send_to(
    State(state): State<Arc<WebState>>,
    Json(req): Json<SendToRequest>,
) -> Result<Json<SendToReport>, (StatusCode, String)> {
    let engines = state.engines.read().await.clone();
    if engines.is_empty() {
        tracing::warn!("web send-to: sync engine is not running");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sync engine is not running — finish setup first".to_string(),
        ));
    }
    let row = match state.store.get_outbound(req.id) {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::warn!(id = req.id, "web send-to: unknown transfer");
            return Err((StatusCode::NOT_FOUND, "unknown transfer".to_string()));
        }
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(id = req.id, error = %msg, "web send-to: outbound lookup failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, msg));
        }
    };
    // Every row on this package dir must be settled, not just the named one: a
    // live target is still serving from the dir this send copies, and its
    // terminal can free the payload mid-copy (the fan-out cleanup coordinator
    // frees a dir the moment its last target finishes). Same gate the UI shows
    // the action behind.
    let live = match state.store.all_outbound(u32::MAX) {
        Ok(rows) => rows
            .into_iter()
            .any(|other| other.package_ref == row.package_ref && !other.state.is_terminal()),
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!(id = req.id, error = %msg, "web send-to: outbound scan failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, msg));
        }
    };
    if live {
        tracing::warn!(id = req.id, "web send-to: the batch is still in flight");
        return Err((
            StatusCode::CONFLICT,
            "this batch is still in flight — send it on once it has finished".to_string(),
        ));
    }

    let names = state.device_names.read().await.clone();
    let engine = match resolve_one_target(&engines, &names, &req.target) {
        Ok((_, engine)) => Arc::clone(engine),
        Err(TargetMiss::Unknown) => {
            tracing::error!(target = %req.target, "web send-to: target has no running engine here");
            return Err((
                StatusCode::BAD_REQUEST,
                "target not configured on this node — add it under Settings → Send Targets"
                    .to_string(),
            ));
        }
        Err(TargetMiss::Ambiguous(peers)) => {
            let count = peers.len();
            let peers = peers.join(", ");
            tracing::error!(target = %req.target, count, peers = %peers, "web send-to: ambiguous target");
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "ambiguous target name: {} matches {count} devices ({peers}) — send by device id",
                    req.target
                ),
            ));
        }
    };

    let config = state.config.read().await.clone();
    if !req.confirm {
        // The dry-run behind the confirm dialog: resolve the same sources the
        // send would, build nothing. It stats and hashes candidate originals, so
        // it goes to a blocking thread like every other capture-dir walk here.
        let batches = Arc::clone(&state.batches);
        let dir = PathBuf::from(&row.package_ref);
        let counts = tokio::task::spawn_blocking(move || preview_send_to(&dir, &config, &batches))
            .await
            .map_err(|e| {
                let msg = format!("{e}");
                tracing::error!(id = req.id, error = %msg, "web send-to: preview task panicked");
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            })?;
        let (sent, skipped) = counts.map_err(|e| {
            let msg = format!("{e:#}");
            tracing::error!(id = req.id, error = %msg, "web send-to: preview failed");
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        })?;
        if sent == 0 {
            tracing::warn!(id = req.id, skipped, "web send-to: every source file is gone");
            return Err((
                StatusCode::CONFLICT,
                "all source files deleted locally".to_string(),
            ));
        }
        return Ok(Json(SendToReport {
            confirmed: false,
            new_id: None,
            sent,
            skipped,
        }));
    }

    let cleanup = state.cleanup.read().await.clone();
    match resend::send_batch_to_target(
        &state.store,
        &engine,
        cleanup.as_deref(),
        &config,
        &state.batches,
        &state.seen,
        &row,
        resend::SourceDisposition::Keep,
    )
    .await
    {
        Ok(done) => {
            tracing::info!(
                old_id = req.id,
                new_id = done.new_id,
                target = %req.target,
                sent = done.sent,
                skipped = done.skipped,
                "batch sent to another device via web"
            );
            Ok(Json(SendToReport {
                confirmed: true,
                new_id: Some(done.new_id),
                sent: done.sent,
                skipped: done.skipped,
            }))
        }
        Err(resend::SendBatchError::AllSourcesGone(detail)) => {
            tracing::warn!(id = req.id, error = %detail, "web send-to: every source file is gone");
            Err((
                StatusCode::CONFLICT,
                "all source files deleted locally".to_string(),
            ))
        }
        Err(e) => {
            let msg = format!("{e}");
            tracing::error!(id = req.id, target = %req.target, error = %msg, "web send-to failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, msg))
        }
    }
}

/// The read-only half of [`api_send_to`]: `(sendable, skipped)` for the batch in
/// `dir`, without writing anything. A dir that still holds its payload serves
/// its manifest as-is; a cleaned one is resolved against the originals exactly
/// as the send would.
fn preview_send_to(
    dir: &Path,
    config: &Config,
    batches: &BatchStore,
) -> anyhow::Result<(usize, usize)> {
    if resend::package_has_payload(dir) {
        return Ok((read_manifest(dir)?.len(), 0));
    }
    let plan = resend::plan_package_rebuild(dir, config, batches)?;
    Ok((plan.sendable(), plan.skipped()))
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
            // No capture watcher in the harness: the forget fan-out is a no-op,
            // exactly as on a detached node.
            watcher_forget: RwLock::new(WatcherForget::none()),
            // No iroh node in the harness (loopback transports bind none): an
            // upload-limit edit is file-only here, `appliedLive: false`.
            node: RwLock::new(None),
            free_space: Mutex::new(None),
            free_space_refresh: tokio::sync::Mutex::new(()),
            #[cfg(feature = "preview")]
            preview: crate::preview::PreviewCache::new(),
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
        // Free space per unique volume: the harness's capture dir and data dir
        // are the same tempdir, so they de-duplicate to exactly one entry —
        // camelCase, non-empty, and internally consistent.
        let vols = v["volumes"].as_array().expect("volumes is an array");
        assert_eq!(vols.len(), 1, "capture dir + data dir share one volume");
        assert!(vols[0]["root"].is_string());
        let free = vols[0]["freeBytes"]
            .as_u64()
            .expect("freeBytes camelCase u64");
        let total = vols[0]["totalBytes"]
            .as_u64()
            .expect("totalBytes camelCase u64");
        assert!(total > 0 && free <= total);
        // Auto mode: nothing is on the calendar, so the field is absent rather
        // than a fabricated time.
        assert!(
            v["nextScheduledSend"].is_null(),
            "auto mode has no scheduled send, got {}",
            v["nextScheduledSend"]
        );
    }

    /// In scheduled mode `/api/status` carries the next calendar send — derived
    /// per poll from the live config (0.5.1 §3), rendered RFC-3339 in **local**
    /// time so the operator reads back the `06:00` they configured.
    #[tokio::test]
    async fn status_reports_the_next_scheduled_send() {
        let (state, tmp) = test_state().await;
        let toml_str = format!(
            "capture_dir = \"{}\"\ndata_dir = \"{}\"\npairing_ticket = \"t\"\n\
             mode = \"scheduled\"\nschedule_times = [\"06:00\"]\n\
             [retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
            tmp.path().display(),
            tmp.path().display()
        );
        *state.config.write().await = Config::from_toml_str(&toml_str).unwrap();

        let app = build_router(Arc::clone(&state), None);
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
        let next = v["nextScheduledSend"]
            .as_str()
            .expect("scheduled mode reports a next send");
        assert!(
            next.contains("T06:00:00"),
            "the next send is the configured local 06:00, got {next}"
        );
        // A real RFC-3339 instant, and it is in the future (strictly-after
        // semantics: standing exactly at 06:00 arms tomorrow's).
        let parsed = chrono::DateTime::parse_from_rfc3339(next).expect("RFC-3339");
        assert!(
            parsed > chrono::Local::now(),
            "the next send is ahead of now"
        );
    }

    /// 0.5.1 T14: the status header's "Next scheduled send" follows a **live**
    /// send-mode edit — no restart. This is the whole page-level claim of the
    /// scheduler UI: the operator flips to On schedule, adds a time, and the
    /// header updates on the next 2 s poll because `/api/status` recomputes the
    /// deadline from the config the PUT just swapped in.
    #[tokio::test]
    async fn status_next_scheduled_send_follows_a_live_send_mode_edit() {
        let (state, _tmp) = test_state().await; // sample config: mode = "auto"
        let app = build_router(state, None);
        let status = |app: axum::Router| async move {
            body_json(
                app.oneshot(
                    HttpRequest::builder()
                        .uri("/api/status")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
            )
            .await
        };
        let put = |body: serde_json::Value| {
            let app = app.clone();
            async move {
                app.oneshot(
                    HttpRequest::builder()
                        .method("PUT")
                        .uri("/api/send-mode")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };

        // Auto: nothing armed, so the header line is absent.
        assert!(status(app.clone()).await["nextScheduledSend"].is_null());

        // Flip to On schedule with one time.
        let res = put(serde_json::json!({
            "mode": "scheduled", "autoQuietSecs": 30,
            "scheduleTimes": ["06:00"], "scheduleCatchup": true,
        }))
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let next = status(app.clone()).await["nextScheduledSend"]
            .as_str()
            .expect("a schedule saved through the web edit arms a deadline")
            .to_string();
        let next_dt = chrono::DateTime::parse_from_rfc3339(&next).expect("RFC-3339");
        assert_eq!(
            next_dt.time().format("%H:%M:%S").to_string(),
            "06:00:00",
            "the armed deadline is the time just saved, got {next}"
        );

        // Add a second point: the deadline re-derives from the NEW schedule, so it
        // is one of the configured points — never a stamp left over from the
        // schedule that was just replaced.
        //
        // Deliberately NOT asserted here: that the deadline moved earlier, or that
        // it is ahead of a `now` sampled after the response. Both compare a stamp
        // from one call against a clock read in another, so a clock crossing a
        // point in between flips them — a real flake for claims that are pure
        // arithmetic, already pinned by `schedule::next_fire`'s own unit tests.
        // What this test owns is the plumbing: the edit reaches the deadline.
        let res = put(serde_json::json!({
            "mode": "scheduled", "autoQuietSecs": 30,
            "scheduleTimes": ["06:00", "05:00"], "scheduleCatchup": true,
        }))
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let two = status(app.clone()).await["nextScheduledSend"]
            .as_str()
            .expect("still armed")
            .to_string();
        // Clock-free: parse the stamp and compare its time-of-day against the two
        // fixture points. No `now` is read, so nothing here can race the clock.
        let two_dt = chrono::DateTime::parse_from_rfc3339(&two).expect("RFC-3339");
        let two_hhmmss = two_dt.time().format("%H:%M:%S").to_string();
        assert!(
            two_hhmmss == "05:00:00" || two_hhmmss == "06:00:00",
            "the deadline is one of the two configured points, got {two}"
        );

        // Back to Manual: the line disappears — the header never advertises a
        // send the batcher will not make.
        let res = put(serde_json::json!({ "mode": "manual", "autoQuietSecs": 30 })).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            status(app.clone()).await["nextScheduledSend"].is_null(),
            "manual mode arms nothing, even though the times are still in the file"
        );

        // …and the times ARE still in the file, so switching back re-arms without
        // the operator retyping them.
        let res = put(serde_json::json!({ "mode": "scheduled", "autoQuietSecs": 30 })).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            status(app).await["nextScheduledSend"].is_string(),
            "the preserved schedule re-arms on the way back"
        );
    }

    /// The free-space memo: a second poll inside the TTL is served from the
    /// cache (no second syscall against a possibly-wedged mount), while a
    /// changed path set — a capture-dir edit — forces a fresh probe instead of
    /// reporting the old roots.
    #[tokio::test]
    async fn free_space_snapshot_memoizes_per_path_set() {
        let (state, tmp) = test_state().await;
        let dir = tmp.path().to_path_buf();

        let first = free_space_snapshot(&state, vec![dir.clone()]).await;
        assert_eq!(first.len(), 1, "the tempdir's volume probes fine");

        // Plant a sentinel in the memo. Still inside the TTL with the same path
        // set, so the next call must return it verbatim — proof it did not
        // re-probe.
        *state.free_space.lock().unwrap() = Some((
            Instant::now(),
            vec![dir.clone()],
            vec![VolumeInfo {
                root: PathBuf::from("/sentinel"),
                free_bytes: 7,
                total_bytes: 9,
            }],
        ));
        let cached = free_space_snapshot(&state, vec![dir.clone()]).await;
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].root, PathBuf::from("/sentinel"));

        // A different path set invalidates: this one probes to nothing.
        let other =
            free_space_snapshot(&state, vec![PathBuf::from("/definitely/not/mounted/xyz")]).await;
        assert!(
            other.is_empty(),
            "a changed path set re-probes instead of serving the cached roots"
        );
    }

    /// Contention serves stale: while one probe is in flight (a wedged mount is
    /// exactly this, for minutes), a second poller gets the last reading back
    /// immediately — it neither waits on the probe nor starts its own.
    #[tokio::test]
    async fn concurrent_poll_is_served_stale_never_queued() {
        let (state, tmp) = test_state().await;
        let dir = tmp.path().to_path_buf();

        // A reading that no longer answers the question being asked — here
        // because it was taken for the previous capture-dir set (an expired TTL
        // takes the identical path). A refresh is due, and this last value is
        // what a caller who loses the token must be handed.
        *state.free_space.lock().unwrap() = Some((
            Instant::now(),
            vec![PathBuf::from("/previous/capture/dir")],
            vec![VolumeInfo {
                root: PathBuf::from("/stale"),
                free_bytes: 1,
                total_bytes: 2,
            }],
        ));

        // The winner's probe parks until this test releases it — no sleeping.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let winner_state = Arc::clone(&state);
        let winner_paths = vec![dir.clone()];
        let winner = tokio::spawn(async move {
            free_space_snapshot_with(&winner_state, winner_paths, move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                vec![VolumeInfo {
                    root: PathBuf::from("/fresh"),
                    free_bytes: 3,
                    total_bytes: 4,
                }]
            })
            .await
        });
        started_rx.await.expect("the probe started");

        // Second caller while the probe is still stuck: the stale value, right
        // now. Its probe returns a marker instead of panicking so that a second
        // probe shows up as a wrong value, not as a laundered empty vec.
        let marker = || {
            vec![VolumeInfo {
                root: PathBuf::from("/second-probe-ran"),
                free_bytes: 0,
                total_bytes: 0,
            }]
        };
        let contended =
            free_space_snapshot_with(&state, vec![dir.clone()], move |_| marker()).await;
        assert_eq!(contended.len(), 1);
        assert_eq!(
            contended[0].root,
            PathBuf::from("/stale"),
            "a contended caller serves stale, it does not probe or wait"
        );

        // The winner still gets — and memoizes — the fresh reading.
        release_tx.send(()).unwrap();
        let fresh = winner.await.unwrap();
        assert_eq!(fresh[0].root, PathBuf::from("/fresh"));
        let next_poll = free_space_snapshot_with(&state, vec![dir], move |_| marker()).await;
        assert_eq!(
            next_poll[0].root,
            PathBuf::from("/fresh"),
            "inside the TTL the next poll is served from the memo"
        );
    }

    /// Nothing probed yet + a refresh in flight → an empty reading (no chips),
    /// never a wait and never an error.
    #[tokio::test]
    async fn contention_with_an_empty_memo_yields_no_volumes() {
        let (state, tmp) = test_state().await;
        let dir = tmp.path().to_path_buf();
        assert!(state.free_space.lock().unwrap().is_none());

        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let winner_state = Arc::clone(&state);
        let winner_paths = vec![dir.clone()];
        let winner = tokio::spawn(async move {
            free_space_snapshot_with(&winner_state, winner_paths, move |paths| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                crate::diskspace::probe_volumes(&paths)
            })
            .await
        });
        started_rx.await.expect("the probe started");

        // A second probe would return this marker (rather than panicking, which
        // `spawn_blocking` would launder into the same empty vec the assertion
        // expects) — so an empty result really means "no second probe".
        let contended = free_space_snapshot_with(&state, vec![dir], |_| {
            vec![VolumeInfo {
                root: PathBuf::from("/second-probe-ran"),
                free_bytes: 0,
                total_bytes: 0,
            }]
        })
        .await;
        assert!(contended.is_empty(), "no reading yet → no chips, no wait");

        release_tx.send(()).unwrap();
        assert_eq!(winner.await.unwrap().len(), 1, "the real probe still ran");
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
            schedule_times: Vec::new(),
            schedule_catchup: true,
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
                ..SendCfg::default()
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

    /// 0.5.1 T14 (the brief's RED test): a `scheduled` PUT that carries its own
    /// times round-trips — through the wire echo, the on-disk file, the live
    /// `SendCfg` channel, and a follow-up `GET`. Times normalise on the way out:
    /// zero-padded, sorted, deduped.
    #[tokio::test]
    async fn put_send_mode_scheduled_with_times_roundtrips() {
        let (state, _tmp) = test_state().await; // sample config: mode = "auto"
        let mut rx = state.send_cfg_tx.read().await.subscribe();
        let config_path = state.config_path.clone();
        let app = build_router(state, None);

        // GET on an auto config reports an empty schedule and catch-up ON.
        let v = body_json(get(&app, "/api/send-mode").await).await;
        assert_eq!(v["mode"], "auto");
        assert_eq!(v["scheduleTimes"], serde_json::json!([]));
        assert_eq!(v["scheduleCatchup"], true, "catch-up defaults ON");

        let body = serde_json::json!({
            "mode": "scheduled",
            "autoQuietSecs": 30,
            // Deliberately unsorted, unpadded and duplicated.
            "scheduleTimes": ["14:30", "6:00", "06:00"],
            "scheduleCatchup": false,
        });
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
        assert_eq!(v["mode"], "scheduled", "the PUT echoes the applied mode");
        assert_eq!(
            v["scheduleTimes"],
            serde_json::json!(["06:00", "14:30"]),
            "the echo is normalised: padded, sorted, deduped"
        );
        assert_eq!(v["scheduleCatchup"], false);

        // The live-apply reached the batcher's send-config channel.
        let live = rx.borrow_and_update().clone();
        assert_eq!(live.mode, Mode::Scheduled);
        assert_eq!(live.schedule_times, vec![(6, 0), (14, 30)]);
        assert!(!live.schedule_catchup);

        // The on-disk config carries the scheduled mode + its times.
        let text = std::fs::read_to_string(&config_path).unwrap();
        let reloaded = Config::from_toml_str(&text).unwrap();
        assert_eq!(reloaded.mode, Mode::Scheduled, "written to disk: {text}");
        assert_eq!(
            reloaded.schedule_times,
            vec!["06:00".to_string(), "14:30".to_string()]
        );
        assert!(!reloaded.schedule_catchup);

        // A follow-up GET reflects it, and so does the pending poll that drives
        // the To-Sync card (the page renders the whole strip off one fetch).
        let v = body_json(get(&app, "/api/send-mode").await).await;
        assert_eq!(v["mode"], "scheduled");
        assert_eq!(v["scheduleTimes"], serde_json::json!(["06:00", "14:30"]));
        let p = body_json(get(&app, "/api/pending").await).await;
        assert_eq!(p["mode"], "scheduled");
        assert_eq!(p["scheduleTimes"], serde_json::json!(["06:00", "14:30"]));
        assert_eq!(p["scheduleCatchup"], false);
    }

    /// A `scheduled` PUT with **no** times is a `422` carrying the validator's
    /// actionable message, and the config is left byte-identical — the mode is
    /// never written on its own into a state the validator rejects.
    #[tokio::test]
    async fn put_send_mode_scheduled_without_times_is_422_and_file_untouched() {
        let (state, _tmp) = test_state().await;
        let config_path = state.config_path.clone();
        let before = std::fs::read_to_string(&config_path).unwrap();
        let app = build_router(state, None);

        let body = serde_json::json!({
            "mode": "scheduled",
            "autoQuietSecs": 30,
            "scheduleTimes": [],
            "scheduleCatchup": true,
        });
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
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let msg = String::from_utf8(
            axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            msg.contains("at least one send time"),
            "the operator gets the validator's actionable message: {msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            before,
            "a rejected scheduled edit leaves the config byte-identical"
        );

        // A malformed time is refused the same way, naming the entry.
        let body = serde_json::json!({
            "mode": "scheduled", "autoQuietSecs": 30, "scheduleTimes": ["6h30"],
        });
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
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), before);
    }

    /// A schedule-blind PUT (`{mode, autoQuietSecs}` only — the shape a stale
    /// browser tab loaded before this task still sends) flips the mode and leaves
    /// the operator's `schedule_times` / `schedule_catchup` intact. Auto/manual
    /// behaviour is otherwise unchanged.
    #[tokio::test]
    async fn put_send_mode_without_schedule_fields_preserves_them() {
        let (state, _tmp) = test_state().await;
        let config_path = state.config_path.clone();
        let app = build_router(state, None);

        let put = |body: serde_json::Value| {
            let app = app.clone();
            async move {
                app.oneshot(
                    HttpRequest::builder()
                        .method("PUT")
                        .uri("/api/send-mode")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };

        // Seed a real schedule.
        let res = put(serde_json::json!({
            "mode": "scheduled", "autoQuietSecs": 30,
            "scheduleTimes": ["06:00"], "scheduleCatchup": false,
        }))
        .await;
        assert_eq!(res.status(), StatusCode::OK);

        // The old wire shape: mode + quiet only.
        let res = put(serde_json::json!({ "mode": "manual", "autoQuietSecs": 45 })).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["mode"], "manual");
        assert_eq!(v["autoQuietSecs"], 45);
        assert_eq!(
            v["scheduleTimes"],
            serde_json::json!(["06:00"]),
            "the times are still there — a schedule-blind edit never erases them"
        );
        assert_eq!(v["scheduleCatchup"], false);

        let reloaded = Config::from_toml_str(&std::fs::read_to_string(&config_path).unwrap())
            .expect("still parses");
        assert_eq!(reloaded.mode, Mode::Manual);
        assert_eq!(reloaded.schedule_times, vec!["06:00".to_string()]);
        assert!(!reloaded.schedule_catchup);

        // …and back to auto, still non-destructive.
        let res = put(serde_json::json!({ "mode": "auto", "autoQuietSecs": 20 })).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["mode"], "auto");
        assert_eq!(v["scheduleTimes"], serde_json::json!(["06:00"]));
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
            // No capture watcher in the harness: the forget fan-out is a no-op,
            // exactly as on a detached node.
            watcher_forget: RwLock::new(WatcherForget::none()),
            batcher: RwLock::new(Some(batcher)),
            batches: Arc::clone(&batches),
            send_cfg_tx: RwLock::new(send_cfg_tx),
            node: RwLock::new(None),
            free_space: Mutex::new(None),
            free_space_refresh: tokio::sync::Mutex::new(()),
            #[cfg(feature = "preview")]
            preview: crate::preview::PreviewCache::new(),
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

    // ── T4 (0.5.1 local library): GET /api/library ────────────────────────────

    /// A `WebState` whose capture root is a real, populated directory separate
    /// from the data dir, with both Perseus stores open on the data dir's db.
    /// Returns the state, the tempdir guard, and the capture root.
    async fn library_test_state() -> (Arc<WebState>, tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&cap).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let db = data.join("perseus.db");
        let store = Arc::new(StandaloneSyncStore::open(&db).unwrap());
        let seen = Arc::new(crate::seen::SeenStore::open(&db).unwrap());
        let batches = Arc::new(crate::batch_store::BatchStore::open(&db).unwrap());

        let toml_str = format!(
            "capture_dir = \"{}\"\ndata_dir = \"{}\"\npairing_ticket = \"t\"\nmode = \"manual\"\n[retention]\npolicy = \"keep_days\"\ndry_run = true\nkeep_days = 21\n",
            cap.display(),
            data.display()
        );
        let config_path = tmp.path().join("perseus.toml");
        std::fs::write(&config_path, &toml_str).unwrap();
        let config = Config::from_toml_str(&toml_str).unwrap();
        let (_state_tx, state_rx) = watch::channel(AgentState::Running { in_flight: 0 });
        let state = Arc::new(WebState::detached(
            store,
            seen,
            batches,
            config,
            config_path,
            state_rx,
            Arc::new(Notify::new()),
        ));
        (state, tmp, cap)
    }

    fn write_file(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, bytes).unwrap();
        std::fs::canonicalize(&p).unwrap()
    }

    /// The full status join over the wire: one file seeded into each derivable
    /// status (queued rides a live manual-mode batcher), camelCase keys, and a
    /// single-directory listing.
    #[tokio::test]
    async fn library_listing_reports_every_status() {
        let (state, _tmp, cap) = library_test_state().await;

        write_file(&cap, "unsent.fits", b"0123456789");
        let queued = write_file(&cap, "queued.fits", b"x");
        let sending = write_file(&cap, "sending.fits", b"x");
        let delivered = write_file(&cap, "delivered.fits", b"x");
        let declined = write_file(&cap, "declined.fits", b"x");
        let sent = write_file(&cap, "sent.fits", b"x");
        std::fs::create_dir_all(cap.join("M31")).unwrap();
        write_file(&cap, "M31/nested.fits", b"x");

        // Batch participations + their newest outbound rows.
        for (pkg, path) in [
            ("/pkg/sending", &sending),
            ("/pkg/delivered", &delivered),
            ("/pkg/declined", &declined),
        ] {
            state
                .batches
                .record_files(pkg, &[("f.fits".to_string(), path.clone())])
                .unwrap();
        }
        let id_s = state
            .store
            .enqueue("/pkg/sending", PEER, None, &[])
            .unwrap();
        state
            .store
            .set_state(id_s, OutboundState::Transferring)
            .unwrap();
        let id_d = state
            .store
            .enqueue("/pkg/delivered", PEER, None, &[])
            .unwrap();
        state.store.confirm(id_d, &[]).unwrap();
        let id_x = state
            .store
            .enqueue("/pkg/declined", PEER, None, &[])
            .unwrap();
        state
            .store
            .set_state(id_x, OutboundState::Cancelled)
            .unwrap();
        state
            .store
            .set_last_error(id_x, Some(CANCELLED_BY_RECEIVER_DETAIL))
            .unwrap();

        // Seen-only linkage → Sent.
        state
            .seen
            .mark_enqueued(&sent, 1, 1, "/pkg/legacy")
            .unwrap();

        // A live MANUAL-mode batcher never auto-flushes, so a fed path stays
        // pending for the whole test. Deliberately spawned with NO engine: the
        // seeded outbound rows above are the fixture, and a real engine's
        // crash-resume would re-drive them (a missing payload dir fails the
        // transferring row) out from under the assertions. Manual mode never
        // reaches the fan-out, so an empty target list is never consulted.
        let (stable_tx, stable_rx) = tokio::sync::mpsc::channel::<(PathBuf, PathBuf)>(8);
        let config = state.config.read().await.clone();
        let (send_cfg_tx, send_cfg_rx) = watch::channel(config.send_cfg());
        let (batcher, _task) = crate::batcher::spawn_batcher(
            stable_rx,
            Vec::new(),
            Arc::clone(&state.seen),
            Arc::clone(&state.batches),
            config,
            node_id_hex(&PEER),
            None,
            send_cfg_rx,
        );
        stable_tx.send((cap.clone(), queued.clone())).await.unwrap();
        // Wait for the batcher loop to absorb it (manual mode → it stays). The
        // budget is generous on purpose: the loop takes it on its first turn, so
        // a long ceiling costs nothing and cannot make this test time-sensitive.
        for _ in 0..1000 {
            if !batcher.pending_snapshot().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(batcher.pending_snapshot().len(), 1, "the file is pending");
        *state.batcher.write().await = Some(batcher);

        let app = build_router(Arc::clone(&state), None);
        let res = get(&app, "/api/library?root=0&path=").await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;

        assert_eq!(v["root"], 0);
        assert_eq!(v["path"], "");
        assert_eq!(
            v["dirs"].as_array().unwrap(),
            &vec![serde_json::json!("M31")],
            "subdirectories are listed, their contents are not"
        );

        let files = v["files"].as_array().unwrap();
        let by_name: HashMap<&str, &serde_json::Value> = files
            .iter()
            .map(|f| (f["name"].as_str().unwrap(), f))
            .collect();
        assert_eq!(files.len(), 6, "no nested file at root level: {by_name:?}");
        assert_eq!(by_name["unsent.fits"]["status"], "unsent");
        assert_eq!(by_name["queued.fits"]["status"], "queued");
        assert_eq!(by_name["sending.fits"]["status"], "sending");
        assert_eq!(by_name["delivered.fits"]["status"], "delivered");
        assert_eq!(by_name["declined.fits"]["status"], "declined");
        assert_eq!(by_name["sent.fits"]["status"], "sent");

        // camelCase keys + honest per-entry facts.
        assert_eq!(by_name["unsent.fits"]["size"], 10);
        assert!(by_name["unsent.fits"]["mtimeMs"].as_i64().unwrap() > 0);
        assert_eq!(by_name["unsent.fits"]["batches"], 0);
        assert_eq!(by_name["sending.fits"]["batches"], 1);
        assert!(
            by_name["unsent.fits"]["retention"].is_null(),
            "retention is T15's field; it ships null here"
        );

        // Descending into the subdirectory lists only its own contents.
        let sub = body_json(get(&app, "/api/library?root=0&path=M31").await).await;
        assert_eq!(sub["path"], "M31");
        assert_eq!(sub["files"].as_array().unwrap().len(), 1);
        assert_eq!(sub["files"][0]["name"], "nested.fits");

        // Both senders stay bound to here so the batcher loop — and with it the
        // pending set the assertions read — outlives the requests.
        drop(stable_tx);
        drop(send_cfg_tx);
    }

    /// A root index past the configured capture dirs is a `404`, not a panic.
    #[tokio::test]
    async fn library_unknown_root_is_404() {
        let (state, _tmp, _cap) = library_test_state().await;
        let app = build_router(state, None);
        let res = get(&app, "/api/library?root=7&path=").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    /// The T3 offline-at-boot case: a configured root that is not mounted is a
    /// `502` with the contract body, never a 404 that reads as "you asked for a
    /// path that does not exist".
    #[tokio::test]
    async fn library_offline_root_is_502_root_unavailable() {
        let (state, _tmp, cap) = library_test_state().await;
        std::fs::remove_dir_all(&cap).unwrap();
        let app = build_router(state, None);
        let res = get(&app, "/api/library?root=0&path=").await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&body), "root unavailable");
    }

    #[tokio::test]
    async fn library_missing_subdirectory_is_404() {
        let (state, _tmp, _cap) = library_test_state().await;
        let app = build_router(state, None);
        let res = get(&app, "/api/library?root=0&path=nope").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn library_hostile_path_is_400() {
        let (state, _tmp, _cap) = library_test_state().await;
        let app = build_router(state, None);
        for path in ["..", "%2e%2e", "a%2F..%2Fb"] {
            let res = get(&app, &format!("/api/library?root=0&path={path}")).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "path {path:?}");
        }
    }

    #[tokio::test]
    async fn library_listing_a_file_is_400() {
        let (state, _tmp, cap) = library_test_state().await;
        write_file(&cap, "a.fits", b"x");
        let app = build_router(state, None);
        let res = get(&app, "/api/library?root=0&path=a.fits").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // ── T6 (0.5.1 local library): GET /api/library/preview ────────────────────

    /// Without the `preview` feature the route still EXISTS — it just refuses
    /// with a greppable reason, so the UI can tell "this build has no renderer"
    /// apart from "this Perseus is too old to know the route".
    #[cfg(not(feature = "preview"))]
    #[tokio::test]
    async fn library_preview_is_a_404_stub_without_the_feature() {
        let (state, _tmp, _cap) = library_test_state().await;
        let app = build_router(state, None);
        let res = get(&app, "/api/library/preview?root=0&path=a.fits").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&body), "preview not built");
    }

    #[cfg(feature = "preview")]
    async fn get_with(app: &Router, uri: &str, header: (&str, &str)) -> Response {
        app.clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(uri)
                    .header(header.0, header.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// The whole conditional-request contract in one pass: a real frame renders
    /// to JPEG bytes with an ETag, and echoing that ETag back is answered `304`
    /// with no body — and, critically, WITHOUT re-rendering.
    #[cfg(feature = "preview")]
    #[tokio::test]
    async fn library_preview_renders_jpeg_then_revalidates_to_304() {
        let (state, _tmp, cap) = library_test_state().await;
        crate::preview::write_test_fits(&cap.join("light.fits"), 256, 192);
        let app = build_router(Arc::clone(&state), None);

        let res = get(&app, "/api/library/preview?root=0&path=light.fits&w=128").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/jpeg"
        );
        let etag = res
            .headers()
            .get(header::ETAG)
            .expect("a preview carries an ETag")
            .to_str()
            .unwrap()
            .to_string();
        assert!(etag.starts_with('"'), "strong ETag, quoted: {etag}");
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&bytes[0..3], &[0xFF, 0xD8, 0xFF], "JPEG magic bytes");
        assert_eq!(state.preview.render_count(), 1);

        let res = get_with(
            &app,
            "/api/library/preview?root=0&path=light.fits&w=128",
            ("if-none-match", &etag),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(res.headers().get(header::ETAG).unwrap(), etag.as_str());
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty(), "a 304 carries no body");
        assert_eq!(
            state.preview.render_count(),
            1,
            "a 304 must never reach the renderer"
        );
    }

    /// The stale case: the same path with a *different* ETag re-renders rather
    /// than 304-ing, so a rewritten sub is never served from the browser's cache.
    #[cfg(feature = "preview")]
    #[tokio::test]
    async fn library_preview_with_a_stale_etag_re_renders() {
        let (state, _tmp, cap) = library_test_state().await;
        crate::preview::write_test_fits(&cap.join("light.fits"), 256, 192);
        let app = build_router(Arc::clone(&state), None);
        let res = get_with(
            &app,
            "/api/library/preview?root=0&path=light.fits&w=128",
            ("if-none-match", "\"0000000000000000\""),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(state.preview.render_count(), 1);
    }

    /// A width past the cap is clamped, not rejected — and it shares the cap's
    /// cache entry, which is what the single clamp point in the key buys.
    #[cfg(feature = "preview")]
    #[tokio::test]
    async fn library_preview_clamps_an_absurd_width() {
        let (state, _tmp, cap) = library_test_state().await;
        crate::preview::write_test_fits(&cap.join("light.fits"), 256, 192);
        let app = build_router(Arc::clone(&state), None);

        let huge = get(&app, "/api/library/preview?root=0&path=light.fits&w=99999").await;
        assert_eq!(huge.status(), StatusCode::OK);
        let huge_etag = huge.headers().get(header::ETAG).unwrap().clone();

        let capped = get(
            &app,
            &format!(
                "/api/library/preview?root=0&path=light.fits&w={}",
                crate::preview::MAX_WIDTH
            ),
        )
        .await;
        assert_eq!(capped.status(), StatusCode::OK);
        assert_eq!(capped.headers().get(header::ETAG).unwrap(), &huge_etag);
        assert_eq!(
            state.preview.render_count(),
            1,
            "both requests are the same clamped render"
        );
    }

    /// A file inside the root that is not a frame is a `415` — the path was fine,
    /// the media type is not. The browser must not be able to point this route at
    /// `perseus.toml`.
    #[cfg(feature = "preview")]
    #[tokio::test]
    async fn library_preview_of_a_non_frame_is_415() {
        let (state, _tmp, cap) = library_test_state().await;
        write_file(&cap, "notes.txt", b"hello");
        write_file(&cap, "perseus.toml", b"secret = 1");
        // A DIRECTORY wearing a frame's extension must not reach the decoder.
        std::fs::create_dir_all(cap.join("trap.fits")).unwrap();
        let app = build_router(state, None);
        for rel in ["notes.txt", "perseus.toml", "trap.fits"] {
            let res = get(&app, &format!("/api/library/preview?root=0&path={rel}")).await;
            assert_eq!(
                res.status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "path {rel:?}"
            );
        }
    }

    /// The right extension over the wrong bytes is a `422`: we accepted the file
    /// as a frame and then could not decode it. Distinct from the `415` above.
    #[cfg(feature = "preview")]
    #[tokio::test]
    async fn library_preview_of_an_undecodable_frame_is_422() {
        let (state, _tmp, cap) = library_test_state().await;
        write_file(&cap, "broken.fits", b"not really a FITS file");
        let app = build_router(state, None);
        let res = get(&app, "/api/library/preview?root=0&path=broken.fits").await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// The preview route inherits the T1 path contract verbatim: the same inputs
    /// that a listing refuses, it refuses, with the same statuses.
    #[cfg(feature = "preview")]
    #[tokio::test]
    async fn library_preview_inherits_the_path_contract() {
        let (state, _tmp, cap) = library_test_state().await;
        crate::preview::write_test_fits(&cap.join("light.fits"), 128, 96);
        let app = build_router(state, None);

        let res = get(&app, "/api/library/preview?root=7&path=light.fits").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "unknown root");

        let res = get(&app, "/api/library/preview?root=0&path=gone.fits").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "missing file");

        for path in ["..", "%2e%2e", "a%2F..%2Fb"] {
            let res = get(&app, &format!("/api/library/preview?root=0&path={path}")).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "path {path:?}");
        }
    }

    /// An unmounted capture root is a `502` here too, never a `404` — the
    /// preview must not tell an operator their frames are gone because a share
    /// dropped.
    #[cfg(feature = "preview")]
    #[tokio::test]
    async fn library_preview_offline_root_is_502() {
        let (state, _tmp, cap) = library_test_state().await;
        std::fs::remove_dir_all(&cap).unwrap();
        let app = build_router(state, None);
        let res = get(&app, "/api/library/preview?root=0&path=light.fits").await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    // ── T8 (0.5.1 local library): POST /api/library/send ──────────────────────

    /// Write a minimal, parseable single-frame FITS. The send path builds a REAL
    /// package (parse → stat → hash → copy), so its fixtures must actually parse.
    fn write_fixture_fits(path: &Path, object: &str) {
        use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
        let cards = HeaderBuilder::new(FrameKind::Light)
            .object(object)
            .exptime(60.0)
            .filter("Ha")
            .instrume("TestCam")
            .build()
            .expect("build header");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        athenaeum_core::fits_writer::write_fits_f32(path, 8, 8, 1, &vec![0.0f32; 64], &cards)
            .expect("write fixture fits");
    }

    /// A live send harness: the library state above plus one loopback engine as
    /// the sole target and a real MANUAL-mode batcher (it never auto-flushes, so
    /// the pending accumulator is stable across the assertions).
    struct SendHarness {
        state: Arc<WebState>,
        cap: PathBuf,
        engine: Arc<SyncEngineHandle>,
        batcher: crate::batcher::BatcherHandle,
        stable_tx: tokio::sync::mpsc::Sender<(PathBuf, PathBuf)>,
        // Held so the batcher loop (and with it the pending set) outlives the test.
        _task: tokio::task::JoinHandle<()>,
        _cfg_tx: watch::Sender<SendCfg>,
        _tmp: tempfile::TempDir,
    }

    impl SendHarness {
        /// Feed one file into the batcher as a watcher would — the canonical
        /// `(capture_dir, file)` spelling — and wait for the loop to absorb it.
        async fn make_pending(&self, rel: &str) {
            let cap = std::fs::canonicalize(&self.cap).unwrap();
            let file = std::fs::canonicalize(self.cap.join(rel)).unwrap();
            self.stable_tx.send((cap, file)).await.unwrap();
            for _ in 0..1000 {
                if !self.batcher.pending_snapshot().is_empty() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            panic!("the batcher never took the fed file");
        }

        /// Is this capture file recorded in the seen store (i.e. did it ship)?
        fn is_seen(&self, rel: &str) -> bool {
            let p = std::fs::canonicalize(self.cap.join(rel)).unwrap();
            let m = std::fs::metadata(&p).unwrap();
            !self
                .state
                .seen
                .should_enqueue(&p, m.len(), crate::seen::mtime_millis(m.modified().ok()))
                .unwrap()
        }
    }

    async fn send_harness() -> SendHarness {
        let (state, tmp, cap) = library_test_state().await;
        let config = state.config.read().await.clone();
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let engine = Arc::new(SyncEngine::spawn(
            Arc::clone(&state.store) as Arc<dyn SyncStore>,
            transport,
            PEER,
        ));
        let (stable_tx, stable_rx) = tokio::sync::mpsc::channel::<(PathBuf, PathBuf)>(8);
        let (cfg_tx, cfg_rx) = watch::channel(config.send_cfg());
        let (batcher, task) = crate::batcher::spawn_batcher(
            stable_rx,
            vec![Arc::clone(&engine)],
            Arc::clone(&state.seen),
            Arc::clone(&state.batches),
            config,
            node_id_hex(&PEER),
            None,
            cfg_rx,
        );
        *state.engines.write().await = vec![(node_id_hex(&PEER), Arc::clone(&engine))];
        *state.engine.write().await = Some(Arc::clone(&engine));
        *state.batcher.write().await = Some(batcher.clone());
        SendHarness {
            state,
            cap,
            engine,
            batcher,
            stable_tx,
            _task: task,
            _cfg_tx: cfg_tx,
            _tmp: tmp,
        }
    }

    /// The whole happy path in one pass: a mixed file + directory selection is
    /// expanded (recursively, eligible-only), packaged as ONE `browser` batch,
    /// recorded seen, and — the spec §1a double-send guard — the selected file
    /// that was sitting in the batcher's pending set is taken OUT of it.
    #[tokio::test]
    async fn library_send_packages_the_selection_and_clears_it_from_pending() {
        let h = send_harness().await;
        write_fixture_fits(&h.cap.join("a.fits"), "M42");
        write_fixture_fits(&h.cap.join("M31/b.fits"), "M31");
        write_fixture_fits(&h.cap.join("M31/sub/c.fits"), "M31");
        std::fs::write(h.cap.join("M31/notes.txt"), b"not a frame").unwrap();
        h.make_pending("a.fits").await;

        let app = build_router(Arc::clone(&h.state), None);
        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({
                "items": [
                    { "root": 0, "rel": "a.fits" },
                    { "root": 0, "rel": "M31", "dir": true }
                ]
            }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(
            v["enqueued"], 3,
            "the picked file + both nested frames; notes.txt is not swept in"
        );
        let package_ref = v["packageRef"].as_str().unwrap().to_string();
        assert!(Path::new(&package_ref).is_dir(), "the package was written");

        let rows = h.state.batches.list().unwrap();
        assert_eq!(rows.len(), 1, "one batch row");
        assert_eq!(rows[0].mode, "browser", "recorded as a browser batch");
        assert_eq!(rows[0].file_count, 3);
        assert_eq!(rows[0].package_ref, package_ref);

        for rel in ["a.fits", "M31/b.fits", "M31/sub/c.fits"] {
            assert!(h.is_seen(rel), "{rel} recorded seen");
        }
        assert!(
            h.batcher.pending_snapshot().is_empty(),
            "the sent file left the pending set — no second send on the next flush"
        );
    }

    /// A selection that expands to no files is a `422`, whether it was empty or
    /// only held ineligible entries. Nothing is built and no row is recorded.
    #[tokio::test]
    async fn library_send_with_nothing_to_send_is_422() {
        let h = send_harness().await;
        std::fs::create_dir_all(h.cap.join("empty")).unwrap();
        std::fs::write(h.cap.join("empty/notes.txt"), b"x").unwrap();
        let app = build_router(Arc::clone(&h.state), None);

        for items in [
            serde_json::json!([]),
            serde_json::json!([{ "root": 0, "rel": "empty", "dir": true }]),
        ] {
            let body = serde_json::json!({ "items": items });
            let res = post_json(&app, "/api/library/send", body).await;
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{items}");
        }
        assert!(h.state.batches.list().unwrap().is_empty());
    }

    /// An explicitly picked file ships whatever its extension — but if NOTHING in
    /// the selection can be built into a manifest record, that is an honest `422`
    /// rather than a `200` over an empty package.
    #[tokio::test]
    async fn library_send_of_only_unbuildable_files_is_422() {
        let h = send_harness().await;
        std::fs::write(h.cap.join("notes.txt"), b"not a frame").unwrap();
        let app = build_router(Arc::clone(&h.state), None);
        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({ "items": [{ "root": 0, "rel": "notes.txt" }] }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(h.state.batches.list().unwrap().is_empty());
        assert!(!h.is_seen("notes.txt"), "nothing shipped, nothing seen");
    }

    /// Targets name the running send targets, by peer id or friendly device
    /// name. An unknown one is a `400` before anything is built.
    #[tokio::test]
    async fn library_send_resolves_targets_and_refuses_an_unknown_one() {
        let h = send_harness().await;
        write_fixture_fits(&h.cap.join("a.fits"), "M42");
        *h.state.device_names.write().await =
            HashMap::from([(node_id_hex(&PEER), "obs-pi".to_string())]);
        let app = build_router(Arc::clone(&h.state), None);
        let items = serde_json::json!([{ "root": 0, "rel": "a.fits" }]);

        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({ "targets": ["no-such-device"], "items": items }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(
            h.state.batches.list().unwrap().is_empty(),
            "an unknown target builds nothing"
        );

        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({ "targets": ["obs-pi"], "items": items }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK, "the friendly name resolves");
        assert_eq!(h.state.batches.list().unwrap().len(), 1);
    }

    /// Friendly device names are not unique. Two running targets called the same
    /// thing is a `400` naming both candidates — never a `200` for a batch that
    /// silently reached only whichever engine came first in the list.
    #[tokio::test]
    async fn library_send_refuses_an_ambiguous_target_name() {
        const PEER_B: [u8; 32] = [6u8; 32];
        let h = send_harness().await;
        write_fixture_fits(&h.cap.join("a.fits"), "M42");

        // A second running target, distinct peer id, SAME friendly name.
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let other = Arc::new(SyncEngine::spawn(
            Arc::clone(&h.state.store) as Arc<dyn SyncStore>,
            transport,
            PEER_B,
        ));
        h.state
            .engines
            .write()
            .await
            .push((node_id_hex(&PEER_B), other));
        *h.state.device_names.write().await = HashMap::from([
            (node_id_hex(&PEER), "obs-pi".to_string()),
            (node_id_hex(&PEER_B), "obs-pi".to_string()),
        ]);

        let app = build_router(Arc::clone(&h.state), None);
        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({
                "targets": ["obs-pi"],
                "items": [{ "root": 0, "rel": "a.fits" }]
            }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body).into_owned();
        assert!(body.starts_with("ambiguous target name"), "got {body:?}");
        assert!(
            body.contains(&node_id_hex(&PEER)) && body.contains(&node_id_hex(&PEER_B)),
            "the message names both candidates, so the operator can pick one: {body:?}"
        );
        assert!(
            h.state.batches.list().unwrap().is_empty(),
            "an ambiguous target builds nothing"
        );
    }

    /// Two engine entries for the SAME peer are not an ambiguity — a device
    /// configured under both its id and its friendly name reaches one machine, so
    /// naming it must resolve (and enqueue once), never `400`. Ambiguity is
    /// counted in devices, not in matching entries.
    #[tokio::test]
    async fn library_send_dedupes_repeated_entries_for_one_peer() {
        let h = send_harness().await;
        write_fixture_fits(&h.cap.join("a.fits"), "M42");

        // A second engine entry carrying the SAME peer id (a distinct handle, so
        // the collapse must key on the resolved hex, not on the Arc).
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let twin = Arc::new(SyncEngine::spawn(
            Arc::clone(&h.state.store) as Arc<dyn SyncStore>,
            transport,
            PEER,
        ));
        h.state
            .engines
            .write()
            .await
            .push((node_id_hex(&PEER), twin));
        *h.state.device_names.write().await =
            HashMap::from([(node_id_hex(&PEER), "obs-pi".to_string())]);

        let app = build_router(Arc::clone(&h.state), None);
        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({
                "targets": ["obs-pi"],
                "items": [{ "root": 0, "rel": "a.fits" }]
            }),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "one device listed twice still resolves"
        );
        let v = body_json(res).await;
        assert_eq!(v["enqueued"], 1);
        assert_eq!(
            h.state.batches.list().unwrap().len(),
            1,
            "one batch, sent once — the repeat did not fan out twice"
        );
    }

    /// A file that vanished between the listing and the send is dropped and the
    /// REST of the selection still ships (spec §1a, eligible subset): a browser
    /// listing is a snapshot, and one deleted frame must not fail the whole send.
    #[tokio::test]
    async fn library_send_skips_a_vanished_file_and_sends_the_rest() {
        let h = send_harness().await;
        write_fixture_fits(&h.cap.join("a.fits"), "M42");
        let app = build_router(Arc::clone(&h.state), None);

        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({
                "items": [
                    { "root": 0, "rel": "a.fits" },
                    { "root": 0, "rel": "gone.fits" }
                ]
            }),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "one vanished file is not fatal"
        );
        let v = body_json(res).await;
        assert_eq!(v["enqueued"], 1, "only the surviving file was packaged");
        assert_eq!(
            v["skipped"], 1,
            "the drop is reported, not left to be inferred from a short count"
        );

        let rows = h.state.batches.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file_count, 1);
        assert!(
            h.is_seen("a.fits"),
            "the file that shipped is recorded seen"
        );
    }

    /// The send route inherits the T1 path contract verbatim — the same inputs a
    /// listing refuses, with the same statuses — plus the two kind mismatches.
    ///
    /// One deliberate exception: a named FILE that vanished is skipped rather
    /// than refused (spec §1a — see
    /// [`library_send_skips_a_vanished_file_and_sends_the_rest`]), so the `404`
    /// here is carried by an unknown root and a missing *directory*, which stay
    /// hard failures.
    #[tokio::test]
    async fn library_send_inherits_the_path_contract() {
        let h = send_harness().await;
        write_fixture_fits(&h.cap.join("a.fits"), "M42");
        std::fs::create_dir_all(h.cap.join("M31")).unwrap();
        let app = build_router(Arc::clone(&h.state), None);

        let cases: Vec<(serde_json::Value, StatusCode)> = vec![
            (
                serde_json::json!({ "root": 7, "rel": "a.fits" }),
                StatusCode::NOT_FOUND,
            ),
            (
                serde_json::json!({ "root": 0, "rel": "gone", "dir": true }),
                StatusCode::NOT_FOUND,
            ),
            (
                serde_json::json!({ "root": 0, "rel": ".." }),
                StatusCode::BAD_REQUEST,
            ),
            (
                serde_json::json!({ "root": 0, "rel": "a\\b.fits" }),
                StatusCode::BAD_REQUEST,
            ),
            (
                serde_json::json!({ "root": 0, "rel": "M31" }),
                StatusCode::BAD_REQUEST,
            ),
            (
                serde_json::json!({ "root": 0, "rel": "a.fits", "dir": true }),
                StatusCode::BAD_REQUEST,
            ),
        ];
        for (item, want) in cases {
            let res = post_json(
                &app,
                "/api/library/send",
                serde_json::json!({ "items": [item.clone()] }),
            )
            .await;
            assert_eq!(res.status(), want, "item {item}");
        }

        // An unmounted root is a 502 ("root unavailable"), never a 404.
        std::fs::remove_dir_all(&h.cap).unwrap();
        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({ "items": [{ "root": 0, "rel": "a.fits" }] }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    /// A package that reaches ZERO targets is a `502`, its staged dir is gone,
    /// nothing is recorded seen — and the send is a no-op on the pending set: a
    /// selected file that was queued for the next flush is still queued.
    #[tokio::test]
    async fn library_send_that_reaches_no_target_is_502_and_leaves_no_package() {
        let h = send_harness().await;
        write_fixture_fits(&h.cap.join("a.fits"), "M42");
        h.make_pending("a.fits").await;
        // The sole target's worker is gone: every enqueue now fails.
        h.engine.shutdown().await;

        let app = build_router(Arc::clone(&h.state), None);
        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({ "items": [{ "root": 0, "rel": "a.fits" }] }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body),
            "no target accepted the package"
        );

        assert!(h.state.batches.list().unwrap().is_empty(), "no batch row");
        assert!(!h.is_seen("a.fits"), "nothing shipped, nothing seen");
        let packages = h.state.config.read().await.packages_dir();
        let leftovers: Vec<_> = std::fs::read_dir(&packages)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "the undelivered package dir was removed: {leftovers:?}"
        );
        let cap = std::fs::canonicalize(&h.cap).unwrap();
        assert_eq!(
            h.batcher.pending_snapshot(),
            vec![(cap.clone(), cap.join("a.fits"))],
            "a send that shipped nothing leaves the pending set as it found it"
        );
    }

    /// A detached page (engine still in setup) cannot send: an honest `503`, the
    /// same answer the other engine-dependent write routes give.
    #[tokio::test]
    async fn library_send_while_detached_is_503() {
        let (state, _tmp, cap) = library_test_state().await;
        write_fixture_fits(&cap.join("a.fits"), "M42");
        let app = build_router(state, None);
        let res = post_json(
            &app,
            "/api/library/send",
            serde_json::json!({ "items": [{ "root": 0, "rel": "a.fits" }] }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── T9 (0.5.1 local library): POST /api/library/delete, spec §2 matrix ─────
    //
    // One test per row of the spec's consequence table. The owner requirement
    // behind all of them: the operator may delete ANYTHING at any moment, and
    // the consequence is stated rather than the deletion forbidden.

    /// `{ items, confirm }` for the delete route.
    fn delete_body(items: serde_json::Value, confirm: bool) -> serde_json::Value {
        serde_json::json!({ "items": items, "confirm": confirm })
    }

    /// One item's preview row from a `confirm: false` response.
    fn preview_of<'a>(v: &'a serde_json::Value, rel: &str) -> &'a serde_json::Value {
        v["preview"]
            .as_array()
            .expect("preview list")
            .iter()
            .find(|p| p["rel"] == rel)
            .unwrap_or_else(|| panic!("{rel} not previewed: {v}"))
    }

    /// One item's outcome row from a `confirm: true` response.
    fn outcome_of<'a>(v: &'a serde_json::Value, rel: &str) -> &'a serde_json::Value {
        &v["outcomes"]
            .as_array()
            .expect("outcome list")
            .iter()
            .find(|p| p["rel"] == rel)
            .unwrap_or_else(|| panic!("{rel} has no outcome: {v}"))["outcome"]
    }

    /// Every `sync_history` row whose outcome is `outcome`.
    fn history_rows_with_outcome(
        store: &StandaloneSyncStore,
        outcome: &str,
    ) -> Vec<athenaeum_core::sync::HistoryRow> {
        store
            .search_history(athenaeum_core::sync::HistoryQuery {
                filename: None,
                object: None,
                direction: None,
                peer: None,
                project: None,
                package_id: None,
                limit: 1000,
            })
            .unwrap()
            .into_iter()
            .filter(|h| h.outcome == outcome)
            .collect()
    }

    /// Seed a batch that carries `source`: a package directory holding a payload
    /// copy, the `perseus_batch_files` linkage, and one outbound row in `state`.
    /// Returns the package ref.
    async fn seed_batch(
        st: &Arc<WebState>,
        dir: &Path,
        name: &str,
        source: &Path,
        state: OutboundState,
    ) -> String {
        let pkg = dir.join(name);
        std::fs::create_dir_all(&pkg).unwrap();
        // The payload COPY — what the upload actually reads, and what must
        // survive the original being deleted.
        std::fs::copy(source, pkg.join("payload.fits")).unwrap();
        let pkg_ref = pkg.display().to_string();
        st.batches
            .record_files(
                &pkg_ref,
                &[("payload.fits".to_string(), source.to_path_buf())],
            )
            .unwrap();
        let id = st.store.enqueue(&pkg_ref, PEER, None, &[]).unwrap();
        if state == OutboundState::Confirmed {
            st.store.confirm(id, &[]).unwrap();
        } else {
            st.store.set_state(id, state).unwrap();
        }
        pkg_ref
    }

    /// §2 row 1 — a **queued** file. The pending set is cleared BEFORE the
    /// unlink, so no scheduled or auto flush can ever see a file that is about
    /// to stop existing; a preview on the way there touches neither.
    ///
    /// The failure half of that promise — a pass that removes the pair and then
    /// cannot delete the file puts it back — is
    /// [`library_delete_that_fails_puts_the_file_back_in_the_pending_set`].
    #[tokio::test]
    async fn library_delete_queued_file_leaves_the_pending_set_first() {
        let h = send_harness().await;
        write_file(&h.cap, "a.fits", b"x");
        h.make_pending("a.fits").await;
        let app = build_router(Arc::clone(&h.state), None);

        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), false),
            )
            .await,
        )
        .await;
        assert_eq!(preview_of(&v, "a.fits")["queued"], true);
        assert!(
            h.cap.join("a.fits").exists() && h.batcher.pending_snapshot().len() == 1,
            "a preview touches neither disk nor the pending set"
        );

        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), true),
            )
            .await,
        )
        .await;
        assert_eq!(outcome_of(&v, "a.fits")["kind"], "deleted");
        assert!(!h.cap.join("a.fits").exists(), "the file is gone");
        assert!(
            h.batcher.pending_snapshot().is_empty(),
            "and it left the pending set — the next flush cannot send it"
        );

    }

    /// §2 row 1, failure half — a delete that does NOT happen puts the file
    /// back where it found it.
    ///
    /// The pending removal is a promise about a file that is about to stop
    /// existing. When the unlink then fails, the file is still on disk and still
    /// unsent, so leaving it out of the accumulator would silently cancel its
    /// send for the life of the process while the operator was told only "delete
    /// failed" — the watcher cannot repair it either, because its sweep skips
    /// paths it has already emitted. So the pair goes back, and the restore
    /// re-arms the quiet timer for it.
    #[cfg(unix)]
    #[tokio::test]
    async fn library_delete_that_fails_puts_the_file_back_in_the_pending_set() {
        use std::os::unix::fs::PermissionsExt;

        let h = send_harness().await;
        let app = build_router(Arc::clone(&h.state), None);
        let locked = h.cap.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        write_file(&h.cap, "locked/b.fits", b"x");
        h.make_pending("locked/b.fits").await;
        let pending_before = h.batcher.pending_snapshot();
        // A read-only parent is what makes the unlink itself fail.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(
                    serde_json::json!([{ "root": 0, "rel": "locked/b.fits" }]),
                    true,
                ),
            )
            .await,
        )
        .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(
            outcome_of(&v, "locked/b.fits")["kind"],
            "error",
            "the failure is still reported as a failure"
        );
        assert!(locked.join("b.fits").exists(), "the unlink really failed");
        assert_eq!(
            h.batcher.pending_snapshot(),
            pending_before,
            "and the file is back in the pending set, in the watcher's own \
             (capture_dir, file) spelling — the next flush still sends it"
        );
    }

    /// §2 row 2 — a file in an **in-flight** batch. The delete proceeds (the
    /// upload reads the package's payload copy, not the original), and the
    /// preview NAMES the batch so the operator is told, not blocked.
    #[tokio::test]
    async fn library_delete_names_the_in_flight_batch_and_leaves_the_transfer_alone() {
        let (state, tmp, cap) = library_test_state().await;
        let abs = write_file(&cap, "a.fits", b"x");
        let pkg_ref = seed_batch(
            &state,
            tmp.path(),
            "pkg-live",
            &abs,
            OutboundState::Transferring,
        )
        .await;
        let app = build_router(Arc::clone(&state), None);

        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), false),
            )
            .await,
        )
        .await;
        let p = preview_of(&v, "a.fits");
        assert_eq!(
            p["inFlightBatches"],
            serde_json::json!(["pkg-live"]),
            "the live batch is named in the confirm dialog: {p}"
        );
        assert_eq!(p["confirmedBatches"], 0);

        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), true),
            )
            .await,
        )
        .await;
        assert_eq!(
            outcome_of(&v, "a.fits")["kind"],
            "deleted",
            "an in-flight transfer never forbids the delete"
        );
        assert!(!abs.exists());
        assert!(
            Path::new(&pkg_ref).join("payload.fits").exists(),
            "the payload copy the upload reads is untouched"
        );
        let rows = state.store.all_outbound(u32::MAX).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].state,
            OutboundState::Transferring,
            "the transfer row is untouched — it completes from its copy"
        );
    }

    /// §2 row 3 — a file from a **confirmed** batch deletes freely, and the
    /// batch's own history survives: its source linkage still resolves, which is
    /// what makes a later re-send degrade honestly ("97 of 100") instead of
    /// forgetting the file was ever in it.
    #[tokio::test]
    async fn library_delete_of_a_confirmed_batch_file_keeps_the_batch_history() {
        let (state, tmp, cap) = library_test_state().await;
        let abs = write_file(&cap, "a.fits", b"x");
        let pkg_ref = seed_batch(
            &state,
            tmp.path(),
            "pkg-done",
            &abs,
            OutboundState::Confirmed,
        )
        .await;
        let app = build_router(Arc::clone(&state), None);

        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), false),
            )
            .await,
        )
        .await;
        let p = preview_of(&v, "a.fits");
        assert_eq!(p["confirmedBatches"], 1, "the consequence is stated: {p}");
        assert_eq!(p["inFlightBatches"], serde_json::json!([]));

        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), true),
            )
            .await,
        )
        .await;
        assert_eq!(outcome_of(&v, "a.fits")["kind"], "deleted");
        assert!(!abs.exists());
        assert_eq!(
            state
                .batches
                .batches_for_source(&abs.to_string_lossy())
                .unwrap(),
            vec![pkg_ref],
            "the batch still knows it carried this file"
        );
    }

    /// §2 row 4 — the **reappear** case, and the worst quiet failure in the
    /// matrix if it is wrong.
    ///
    /// A frame re-copied from the camera media reproduces the original
    /// `(size, mtime)` byte for byte. After a delete, the seen store must treat
    /// exactly that as NEW — otherwise the frame is silently never sent again.
    /// The chain is asserted end to end: delete → row stamped → identical
    /// re-creation → `should_enqueue`.
    #[tokio::test]
    async fn library_delete_lets_an_identical_recreation_be_sent_again() {
        let (state, _tmp, cap) = library_test_state().await;
        let abs = write_file(&cap, "a.fits", b"0123456789");
        let meta = std::fs::metadata(&abs).unwrap();
        let (size, mtime_ms) = (meta.len(), crate::seen::mtime_millis(meta.modified().ok()));
        let modified = meta.modified().unwrap();
        state
            .seen
            .mark_enqueued(&abs, size, mtime_ms, "/pkg/u1")
            .unwrap();
        assert!(
            !state.seen.should_enqueue(&abs, size, mtime_ms).unwrap(),
            "precondition: while it lives, the recorded file is deduped"
        );

        let app = build_router(Arc::clone(&state), None);
        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), true),
            )
            .await,
        )
        .await;
        assert_eq!(outcome_of(&v, "a.fits")["kind"], "deleted");
        assert!(
            !state.seen.is_recorded(&abs).unwrap(),
            "the seen row is stamped deleted, not live"
        );

        // Re-copied from the camera media: same bytes, same mtime.
        std::fs::write(&abs, b"0123456789").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&abs)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let again = std::fs::metadata(&abs).unwrap();
        assert_eq!(
            (
                again.len(),
                crate::seen::mtime_millis(again.modified().ok())
            ),
            (size, mtime_ms),
            "the re-creation really is stat-identical"
        );
        assert!(
            state.seen.should_enqueue(&abs, size, mtime_ms).unwrap(),
            "a file that reappears after a delete is a NEW capture and must be sent"
        );
    }

    /// §2 row 4, the other half — the same reappear case with a RUNNING WATCHER,
    /// which is the half that actually decides whether the frame is sent again.
    ///
    /// The stamped seen row proven by the test above is necessary and not
    /// sufficient: both of the watcher's discovery paths drop a path they have
    /// already emitted BEFORE consulting the store, so on a Pi that runs for
    /// weeks the re-copied frame would never be enqueued again. Here the delete
    /// goes through the route with the watcher's forget channel attached exactly
    /// as the supervisor attaches it, and the re-created file must be emitted a
    /// second time within the same process run.
    #[tokio::test]
    async fn library_delete_lets_a_running_watcher_send_the_recreation_again() {
        use std::time::Duration;

        let (state, _tmp, cap) = library_test_state().await;

        let (stable_tx, mut stable_rx) = tokio::sync::mpsc::channel::<(PathBuf, PathBuf)>(8);
        // Poll-only: discovery on the tick sweep alone is complete and
        // deterministic, and it keeps the test off a real FSEvents/inotify watch.
        let watcher = crate::watcher::spawn_watcher_with_options(
            cap.clone(),
            Duration::from_millis(50),
            Duration::from_millis(50),
            stable_tx,
            Arc::clone(&state.seen),
            /* force_poll_only = */ true,
        );
        // Exactly what `WebState::attach` does with the agent's aggregate.
        *state.watcher_forget.write().await = WatcherForget::new(vec![watcher.forget_sender()]);

        let abs = write_file(&cap, "a.fits", b"0123456789");
        let (_dir, first) = tokio::time::timeout(Duration::from_secs(5), stable_rx.recv())
            .await
            .expect("the watcher must emit the new capture")
            .expect("stable-file channel closed");
        assert_eq!(first, abs);

        // Record it as the enqueue path does: without the route's `mark_deleted`
        // the durable store would dedup the stat-identical recreation below, so
        // this keeps BOTH halves of the reappear row under test.
        let meta = std::fs::metadata(&abs).unwrap();
        state
            .seen
            .mark_enqueued(
                &abs,
                meta.len(),
                crate::seen::mtime_millis(meta.modified().ok()),
                "/pkg/u1",
            )
            .unwrap();

        let app = build_router(Arc::clone(&state), None);
        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), true),
            )
            .await,
        )
        .await;
        assert_eq!(outcome_of(&v, "a.fits")["kind"], "deleted");

        // Re-copied from the camera media, same bytes at the same path.
        std::fs::write(&abs, b"0123456789").unwrap();
        let (_dir, again) = tokio::time::timeout(Duration::from_secs(5), stable_rx.recv())
            .await
            .expect("a re-created capture must be enqueued again in the same run")
            .expect("stable-file channel closed");
        assert_eq!(again, abs, "the same path, discovered a second time");
        watcher.shutdown().await;
    }

    /// §2 row 7 — **directory delete**: recursive, one file's failure never
    /// strands its siblings, and a directory is removed only once it is empty.
    #[cfg(unix)]
    #[tokio::test]
    async fn library_delete_of_a_directory_is_recursive_and_keeps_going_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        let (state, _tmp, cap) = library_test_state().await;
        write_file(&cap, "M31/a.fits", b"x");
        write_file(&cap, "M31/notes.txt", b"x"); // not capture-eligible: still deleted
        write_file(&cap, "M31/sub/b.fits", b"x");
        write_file(&cap, "M31/locked/c.fits", b"x");
        let locked = cap.join("M31/locked");
        // Read-only parent: `c.fits` cannot be unlinked (the portable stand-in
        // for the spec's Windows sharing violation).
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        let app = build_router(Arc::clone(&state), None);
        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(
                    serde_json::json!([{ "root": 0, "rel": "M31", "dir": true }]),
                    true,
                ),
            )
            .await,
        )
        .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

        for rel in ["M31/a.fits", "M31/notes.txt", "M31/sub/b.fits"] {
            assert_eq!(outcome_of(&v, rel)["kind"], "deleted", "{rel}");
            assert!(!cap.join(rel).exists(), "{rel} is gone");
        }
        assert_eq!(
            outcome_of(&v, "M31/locked/c.fits")["kind"],
            "error",
            "the one that could not be removed is reported, not hidden: {v}"
        );
        assert!(locked.join("c.fits").exists());
        assert!(
            !cap.join("M31/sub").exists(),
            "an emptied subdirectory is removed"
        );
        assert!(
            cap.join("M31").exists() && locked.exists(),
            "a directory that still holds something survives"
        );
    }

    /// §2 — the only refusal: a Perseus-internal path. `data_dir` nested inside a
    /// capture root is a misconfiguration, not an invitation to delete the
    /// agent's own database; the rest of the selection still goes.
    #[tokio::test]
    async fn library_delete_refuses_a_perseus_internal_path() {
        // A capture root with the data dir INSIDE it — the case the guard alone
        // cannot catch, since the data dir really is under the root.
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let data = cap.join(".perseus");
        std::fs::create_dir_all(&data).unwrap();
        let db = data.join("perseus.db");
        let store = Arc::new(StandaloneSyncStore::open(&db).unwrap());
        let seen = Arc::new(crate::seen::SeenStore::open(&db).unwrap());
        let batches = Arc::new(crate::batch_store::BatchStore::open(&db).unwrap());
        let toml_str = format!(
            "capture_dir = \"{}\"\ndata_dir = \"{}\"\npairing_ticket = \"t\"\nmode = \"manual\"\n[retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
            cap.display(),
            data.display()
        );
        let config_path = tmp.path().join("perseus.toml");
        std::fs::write(&config_path, &toml_str).unwrap();
        let config = Config::from_toml_str(&toml_str).unwrap();
        let (_state_tx, state_rx) = watch::channel(AgentState::Running { in_flight: 0 });
        let state = Arc::new(WebState::detached(
            store,
            seen,
            batches,
            config,
            config_path,
            state_rx,
            Arc::new(Notify::new()),
        ));
        let own = write_file(&cap, "a.fits", b"x");

        let app = build_router(Arc::clone(&state), None);
        // Naming it directly.
        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(
                    serde_json::json!([{ "root": 0, "rel": ".perseus/perseus.db" }]),
                    true,
                ),
            )
            .await,
        )
        .await;
        let outcome = outcome_of(&v, ".perseus/perseus.db");
        assert_eq!(outcome["kind"], "refused");
        assert_eq!(outcome["reason"], crate::library::INTERNAL_PATH_REASON);
        assert!(db.exists(), "the agent's own database is untouched");

        // And swept up by a whole-root delete: the internal subtree is refused
        // while everything else still goes.
        let v = body_json(
            post_json(
                &app,
                "/api/library/delete",
                delete_body(
                    serde_json::json!([{ "root": 0, "rel": "", "dir": true }]),
                    true,
                ),
            )
            .await,
        )
        .await;
        assert_eq!(outcome_of(&v, "a.fits")["kind"], "deleted");
        assert_eq!(outcome_of(&v, ".perseus")["kind"], "refused");
        assert!(!own.exists(), "the operator's own file went");
        assert!(db.exists(), "the database did not");
        assert!(cap.exists(), "a capture root itself is never removed");
    }

    /// §2 — **audit**. Every manual deletion writes the retention-style history
    /// row with the `manual-web` actor, and the pass shows up in the retention
    /// log beside the automatic passes' `retention_deleted` rows.
    #[tokio::test]
    async fn library_delete_writes_an_audit_row_and_shows_in_the_retention_log() {
        let (state, _tmp, cap) = library_test_state().await;
        let abs = write_file(&cap, "a.fits", b"0123456789");
        let app = build_router(Arc::clone(&state), None);

        // A preview writes NOTHING.
        post_json(
            &app,
            "/api/library/delete",
            delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), false),
        )
        .await;
        assert!(
            history_rows_with_outcome(&state.store, "deleted_manual-web").is_empty(),
            "a preview leaves no audit trail — it deleted nothing"
        );
        assert!(body_json(get(&app, "/api/retention/log").await)
            .await
            .as_array()
            .unwrap()
            .is_empty());

        post_json(
            &app,
            "/api/library/delete",
            delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), true),
        )
        .await;

        let audit = history_rows_with_outcome(&state.store, "deleted_manual-web");
        assert_eq!(audit.len(), 1, "one audit row per deleted file");
        assert_eq!(audit[0].filename, "a.fits");
        assert_eq!(audit[0].bytes, 10, "the size it had when it was deleted");

        let log = body_json(get(&app, "/api/retention/log").await).await;
        let entry = &log.as_array().unwrap()[0];
        assert_eq!(
            entry["policy"], "manual-web",
            "the pass is labelled by its actor: {entry}"
        );
        assert_eq!(entry["dryRun"], false);
        assert_eq!(
            entry["deleted"],
            serde_json::json!([abs.display().to_string()]),
            "the deleted path is visible beside the automatic passes"
        );
    }

    /// §2 — a path that **vanished** between the listing and the delete is that
    /// item's own honest `not found`, never the whole request's failure: the
    /// outcome the operator asked for already holds for it, and the rest of the
    /// selection still goes.
    #[tokio::test]
    async fn library_delete_of_a_vanished_path_reports_it_and_proceeds() {
        let (state, _tmp, cap) = library_test_state().await;
        let survivor = write_file(&cap, "a.fits", b"x");
        let app = build_router(Arc::clone(&state), None);

        let body = delete_body(
            serde_json::json!([
                { "root": 0, "rel": "gone.fits" },
                { "root": 0, "rel": "gone-dir", "dir": true },
                { "root": 0, "rel": "a.fits" }
            ]),
            false,
        );
        let v = body_json(post_json(&app, "/api/library/delete", body).await).await;
        assert_eq!(preview_of(&v, "gone.fits")["blocked"], "not found");
        assert_eq!(
            preview_of(&v, "gone-dir")["blocked"],
            "not found",
            "a vanished DIRECTORY is absorbed too — unlike a send, the asked-for \
             outcome already holds"
        );
        assert!(preview_of(&v, "a.fits")["blocked"].is_null());

        let body = delete_body(
            serde_json::json!([
                { "root": 0, "rel": "gone.fits" },
                { "root": 0, "rel": "a.fits" }
            ]),
            true,
        );
        let res = post_json(&app, "/api/library/delete", body).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(outcome_of(&v, "gone.fits")["kind"], "error");
        assert_eq!(outcome_of(&v, "gone.fits")["reason"], "not found");
        assert_eq!(outcome_of(&v, "a.fits")["kind"], "deleted");
        assert!(!survivor.exists(), "the rest of the selection proceeded");
    }

    /// The path contract is inherited whole: a hostile or mistyped selection is
    /// the request's own failure (nothing is a "partial delete" there), and an
    /// unmounted root is a `502` rather than a `404` that reads as "your file is
    /// gone".
    #[tokio::test]
    async fn library_delete_inherits_the_path_contract() {
        let (state, _tmp, cap) = library_test_state().await;
        write_file(&cap, "a.fits", b"x");
        let app = build_router(Arc::clone(&state), None);

        for (items, want) in [
            (
                serde_json::json!([{ "root": 0, "rel": "../escape" }]),
                StatusCode::BAD_REQUEST,
            ),
            (
                serde_json::json!([{ "root": 0, "rel": "a.fits", "dir": true }]),
                StatusCode::BAD_REQUEST,
            ),
            (
                serde_json::json!([{ "root": 9, "rel": "a.fits" }]),
                StatusCode::NOT_FOUND,
            ),
        ] {
            let res = post_json(
                &app,
                "/api/library/delete",
                delete_body(items.clone(), true),
            )
            .await;
            assert_eq!(res.status(), want, "{items}");
        }
        assert!(
            cap.join("a.fits").exists(),
            "a refused request deletes nothing"
        );

        std::fs::remove_dir_all(&cap).unwrap();
        let res = post_json(
            &app,
            "/api/library/delete",
            delete_body(serde_json::json!([{ "root": 0, "rel": "a.fits" }]), true),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY, "offline root");
    }

    // ── T11 (0.5.1 §6): POST /api/transfers/send-to ───────────────────────────

    /// A delivered batch as the Transfers history holds it — a confirmed row
    /// over a cleaned (manifest-only) package dir, its `perseus_batch_files`
    /// linkage intact — plus two running engines: the batch's own peer and a
    /// SECOND device it never went to.
    struct SendToHarness {
        state: Arc<WebState>,
        cap: PathBuf,
        pkg: PathBuf,
        row_id: i64,
        sources: Vec<PathBuf>,
        _engines: Vec<Arc<SyncEngineHandle>>,
        _tmp: tempfile::TempDir,
    }

    const PEER_B: [u8; 32] = [7u8; 32];

    async fn send_to_harness(rels: &[&str]) -> SendToHarness {
        use athenaeum_core::package::write_package;

        let (state, tmp, cap) = library_test_state().await;
        let sources: Vec<PathBuf> = rels
            .iter()
            .enumerate()
            .map(|(i, rel)| write_file(&cap, rel, format!("payload-{i}-{rel}").as_bytes()))
            .collect();

        // A real package (real sizes + xxh3, so a rebuild can verify its
        // sources), then stripped to its manifest the way post-confirm cleanup
        // leaves it.
        let pkg = tmp.path().join("packages").join("sent-uuid");
        let records: Vec<(PathBuf, ManifestRecord)> = sources
            .iter()
            .zip(rels.iter())
            .enumerate()
            .map(|(i, (src, rel))| {
                (
                    src.clone(),
                    ManifestRecord {
                        v: MANIFEST_VERSION,
                        frame_uuid: format!("uuid-{i}"),
                        origin_catalog_uuid: format!("uuid-{i}"),
                        origin_device: "self-node".to_string(),
                        payload_kind: PayloadKind::RawFrame,
                        rel_path: (*rel).to_string(),
                        byte_size: std::fs::metadata(src).unwrap().len(),
                        xxh3: athenaeum_core::package::xxh3_full_file(src).unwrap(),
                        frame_meta: serde_json::json!({}),
                        analysis: None,
                        app_version: "test".to_string(),
                        project: None,
                    },
                )
            })
            .collect();
        write_package(&pkg, records).unwrap();
        for entry in std::fs::read_dir(&pkg).unwrap().flatten() {
            if entry.file_name() != std::ffi::OsStr::new(MANIFEST_FILENAME) {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }

        let pkg_ref = pkg.display().to_string();
        let row_id = state.store.enqueue(&pkg_ref, PEER, None, &[]).unwrap();
        state.store.confirm(row_id, &[]).unwrap();
        state
            .batches
            .record_files(
                &pkg_ref,
                &sources
                    .iter()
                    .zip(rels.iter())
                    .map(|(src, rel)| ((*rel).to_string(), src.clone()))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        state
            .batches
            .record(&pkg_ref, "auto", "2026-07-26T00:00:00.000Z", rels.len())
            .unwrap();

        let mut engines = Vec::new();
        let mut registry = Vec::new();
        for peer in [PEER, PEER_B] {
            let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
            let engine = Arc::new(SyncEngine::spawn(
                Arc::clone(&state.store) as Arc<dyn SyncStore>,
                transport,
                peer,
            ));
            registry.push((node_id_hex(&peer), Arc::clone(&engine)));
            engines.push(engine);
        }
        *state.engines.write().await = registry;
        *state.device_names.write().await = HashMap::from([
            (node_id_hex(&PEER), "obs-a".to_string()),
            (node_id_hex(&PEER_B), "obs-b".to_string()),
        ]);

        SendToHarness {
            state,
            cap,
            pkg,
            row_id,
            sources,
            _engines: engines,
            _tmp: tmp,
        }
    }

    fn send_to_body(id: i64, target: &str, confirm: bool) -> serde_json::Value {
        serde_json::json!({ "id": id, "target": target, "confirm": confirm })
    }

    /// The §6 round trip over the wire: the confirm step's preview counts every
    /// file and builds nothing, then the send mints a new transfer on the SECOND
    /// device (fresh batch_uuid, its own history row) while the source batch
    /// keeps its state, its manifest and its cleaned dir.
    #[tokio::test]
    async fn send_to_previews_then_sends_the_batch_to_another_device() {
        let h = send_to_harness(&["a.fits", "b.fits"]).await;
        let app = build_router(Arc::clone(&h.state), None);

        let v = body_json(
            post_json(&app, "/api/transfers/send-to", send_to_body(h.row_id, "obs-b", false)).await,
        )
        .await;
        assert_eq!(v["confirmed"], false);
        assert_eq!(v["newId"], serde_json::Value::Null, "a preview builds nothing");
        assert_eq!((v["sent"].as_u64(), v["skipped"].as_u64()), (Some(2), Some(0)));
        assert_eq!(
            h.state.store.all_outbound(u32::MAX).unwrap().len(),
            1,
            "no transfer was minted by the preview"
        );

        let res = post_json(&app, "/api/transfers/send-to", send_to_body(h.row_id, "obs-b", true)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["confirmed"], true);
        assert_eq!((v["sent"].as_u64(), v["skipped"].as_u64()), (Some(2), Some(0)));
        let new_id = v["newId"].as_i64().expect("a new transfer id");
        assert_ne!(new_id, h.row_id);

        let new_row = h.state.store.get_outbound(new_id).unwrap().unwrap();
        assert_eq!(new_row.peer, PEER_B, "queued on the chosen device");
        assert_ne!(
            Path::new(&new_row.package_ref).file_name(),
            h.pkg.file_name(),
            "a fresh dir basename IS the fresh wire batch_uuid"
        );

        // The source batch is untouched…
        let old = h.state.store.get_outbound(h.row_id).unwrap().unwrap();
        assert_eq!(old.state, OutboundState::Confirmed);
        assert_eq!(old.last_error, None);
        assert!(!resend::package_has_payload(&h.pkg), "still cleaned");
        // …and both batches now show in the grouped transfers list.
        let refs: Vec<String> = body_json(get(&app, "/api/transfers").await)
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["packageRef"].as_str().unwrap().to_string())
            .collect();
        assert!(refs.contains(&h.pkg.display().to_string()));
        assert!(refs.contains(&new_row.package_ref));
    }

    /// Eligible subset over the wire: one source deleted since the batch shipped
    /// is reported by the preview (`2 of 3`) and dropped by the send — never a
    /// refusal of the whole batch.
    #[tokio::test]
    async fn send_to_reports_and_sends_the_eligible_subset() {
        let h = send_to_harness(&["a.fits", "b.fits", "c.fits"]).await;
        std::fs::remove_file(&h.sources[1]).unwrap();
        let app = build_router(Arc::clone(&h.state), None);

        let v = body_json(
            post_json(&app, "/api/transfers/send-to", send_to_body(h.row_id, "obs-b", false)).await,
        )
        .await;
        assert_eq!(
            (v["sent"].as_u64(), v["skipped"].as_u64()),
            (Some(2), Some(1)),
            "the confirm dialog's `sends 2 of 3 (1 deleted locally)`"
        );

        let v = body_json(
            post_json(&app, "/api/transfers/send-to", send_to_body(h.row_id, "obs-b", true)).await,
        )
        .await;
        assert_eq!((v["sent"].as_u64(), v["skipped"].as_u64()), (Some(2), Some(1)));
        let new_row = h
            .state
            .store
            .get_outbound(v["newId"].as_i64().unwrap())
            .unwrap()
            .unwrap();
        let new_dir = PathBuf::from(&new_row.package_ref);
        assert_eq!(read_manifest(&new_dir).unwrap().len(), 2);
        assert!(!new_dir.join("b.fits").exists());
        assert_eq!(
            read_manifest(&h.pkg).unwrap().len(),
            3,
            "the source batch still records the full delivery"
        );
    }

    /// Every source gone → `409` on BOTH steps, with the same sentence: there is
    /// nothing left to send, and nothing was built.
    #[tokio::test]
    async fn send_to_with_every_source_deleted_is_409() {
        let h = send_to_harness(&["a.fits", "b.fits"]).await;
        for src in &h.sources {
            std::fs::remove_file(src).unwrap();
        }
        let app = build_router(Arc::clone(&h.state), None);

        for confirm in [false, true] {
            let res = post_json(
                &app,
                "/api/transfers/send-to",
                send_to_body(h.row_id, "obs-b", confirm),
            )
            .await;
            assert_eq!(res.status(), StatusCode::CONFLICT, "confirm={confirm}");
            let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
            assert_eq!(
                String::from_utf8_lossy(&body),
                "all source files deleted locally",
                "confirm={confirm}"
            );
        }
        assert_eq!(
            h.state.store.all_outbound(u32::MAX).unwrap().len(),
            1,
            "nothing was minted"
        );
    }

    /// The request's two ways of naming something that is not there: an unknown
    /// row id is a `404`, and a device with no engine on THIS node — every
    /// account device that is not a configured, running target — is a `400` that
    /// says where to fix it. Neither builds anything.
    #[tokio::test]
    async fn send_to_refuses_an_unknown_row_and_a_target_without_an_engine() {
        let h = send_to_harness(&["a.fits"]).await;
        let app = build_router(Arc::clone(&h.state), None);

        let res = post_json(
            &app,
            "/api/transfers/send-to",
            send_to_body(h.row_id + 4242, "obs-b", true),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let res = post_json(
            &app,
            "/api/transfers/send-to",
            send_to_body(h.row_id, "obs-elsewhere", true),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("Settings → Send Targets"),
            "the 400 says how to make that device sendable"
        );
        assert_eq!(h.state.store.all_outbound(u32::MAX).unwrap().len(), 1);
        assert!(h.cap.join("a.fits").exists(), "sources are never touched");
    }

    /// A still-running transfer is not history yet: its package dir is being
    /// served and its own terminal can free the payload mid-copy, so the send is
    /// refused (`409`) until it settles.
    #[tokio::test]
    async fn send_to_refuses_a_source_transfer_that_is_still_in_flight() {
        let h = send_to_harness(&["a.fits"]).await;
        h.state
            .store
            .set_state(h.row_id, OutboundState::Transferring)
            .unwrap();
        let app = build_router(Arc::clone(&h.state), None);
        let res = post_json(
            &app,
            "/api/transfers/send-to",
            send_to_body(h.row_id, "obs-b", true),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(h.state.store.all_outbound(u32::MAX).unwrap().len(), 1);
    }

    /// A detached node has nothing to send through: `503`, the same answer every
    /// other engine-dependent write route gives mid-setup.
    #[tokio::test]
    async fn send_to_while_detached_is_503() {
        let (state, _tmp, _cap) = library_test_state().await;
        let id = state.store.enqueue("pkg-x", PEER, None, &[]).unwrap();
        state.store.confirm(id, &[]).unwrap();
        let app = build_router(state, None);
        let res = post_json(&app, "/api/transfers/send-to", send_to_body(id, "obs-b", false)).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
