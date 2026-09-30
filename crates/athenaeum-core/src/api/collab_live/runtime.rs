//! The live exchange runtime (collab v3 wave 3, Task 15; spec §4, §6, §7,
//! §9, plan P18, P26–P28): one task per catalog that owns the feed applier,
//! the holder side, the storage engine and the scheduler's executor, and one
//! event session beside it. Everything is event-driven (L3): a hub event, a
//! disk signal, a fetch result, a lane grant or a due timer wakes it; there
//! is no pass and no poll.
//!
//! Armed once per catalog by [`spawn_collab_live`] (from every
//! `ensure_started` site); [`shutdown`] (app exit) and [`on_sign_out`] stop
//! it within [`STOP_BOUND`], sending `DELETE /me/presence` on the way out.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use crate::api::collab_exchange::{
    CollabFramesLanded, CollabPeersChanged, COLLAB_FRAMES_LANDED_EVENT, COLLAB_PEERS_CHANGED_EVENT,
};
use crate::api::collab_live::executor::{Derive, ExecEnv, Executor, Note};
use crate::api::collab_live::feed::FeedEffect;
use crate::api::collab_live::holdings::{member_devices, HolderMaps};
use crate::api::collab_live::storage_task::{
    HolderView, StorageEngine, StorageEvent, StorageTimings,
};
use crate::api::collab_live::surface::{project_flows, CollabExchangeProgress};
use crate::api::collab_live::workers::{
    FeedOut, FeedWork, FeedWorker, SharedHolders, StorageOut, StorageWork,
};
use crate::api::collab_live::{
    CollabAttentionChanged, CollabDeletionChoice, CollabFrameChanged, CollabFrameLost,
    CollabLiveStatus, LiveState, StorageStateView, COLLAB_ATTENTION_EVENT,
    COLLAB_DELETION_CHOICE_EVENT, COLLAB_EXCHANGE_PROGRESS_EVENT, COLLAB_FRAME_CHANGED_EVENT,
    COLLAB_FRAME_LOST_EVENT, COLLAB_LIVE_STATUS_EVENT,
};
use crate::api::{db, ApiError};
use crate::collab::hub_client::CollabClient;
use crate::collab::live::holders::{redundancy, FrameRef, Redundancy};
use crate::collab::live::meter::{ExchangeMeter, ProgressGate, PROGRESS_PERIOD};
use crate::collab::live::presence::PresenceBook;
use crate::collab::live::wire::LiveEvent;
use crate::collab::scheduler::core::Input;
use crate::collab::storage::marker::{StoreGuard, StoreState, UnavailableReason};
use crate::db::collab_frames::LocalState;
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;
use crate::sharing::iroh::node::SharedIrohNode;
use crate::sync::receiver::InboundControl;

/// How long the runtime waits for its session to leave presence and end,
/// beyond the leave's own bound.
const SESSION_MARGIN: Duration = Duration::from_millis(500);
/// How long the runtime waits for an aborted worker to end.
const WORKER_STOP: Duration = Duration::from_millis(250);
/// `shutdown` / `on_sign_out` wait at most this long for the runtime to
/// leave presence and stop (P28): the leave's bound plus a margin (M1).
///
/// The runtime's own stop ([`Runtime::finish`]) waits for its workers and
/// its session AT ONCE, never one after another, so it takes at most
/// `LEAVE_TIMEOUT + SESSION_MARGIN` (2.5 s); the remaining
/// [`STOP_DISPATCH_MARGIN`] is for the stop command to reach the runtime's
/// loop. (Task 18, flake-5: the waits used to run in sequence — 250 ms per
/// worker, then up to 2.5 s for the session — which added up to this whole
/// bound and left the dispatch no room: under double CPU load a stop hit
/// the bound at 3.0016 s.)
pub const STOP_BOUND: Duration =
    Duration::from_secs(crate::api::collab_live::session::LEAVE_TIMEOUT.as_secs() + 1);
/// What [`STOP_BOUND`] leaves for the stop command to reach the loop (the
/// loop finishes the arm it is in first — all short, Task 15 C1).
pub const STOP_DISPATCH_MARGIN: Duration = Duration::from_millis(500);
const _: () = assert!(
    crate::api::collab_live::session::LEAVE_TIMEOUT.as_millis()
        + SESSION_MARGIN.as_millis()
        + STOP_DISPATCH_MARGIN.as_millis()
        <= STOP_BOUND.as_millis()
);
/// How often an armed runtime re-checks for a bound node, a mounted collab
/// store and a started receiver before it can run.
pub const READY_POLL: Duration = Duration::from_secs(5);
/// How often rows parked for the collab GC are re-checked (one collab GC
/// interval).
pub const GC_PROBE_EVERY: Duration = crate::sharing::iroh::COLLAB_GC_INTERVAL;
/// At most one `collab-frames-landed` per project per this long (a burst,
/// never per frame).
pub const LANDED_BURST: Duration = Duration::from_secs(1);
/// Events queued from the stream before the pump waits for the runtime.
pub(super) const EVENT_QUEUE: usize = 256;

// ── the shared state (runtime ↔ session ↔ handle) ───────────────────────

/// State the runtime, its session and the command surface share.
pub(crate) struct Shared {
    pub ctx: Arc<ServiceContext>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    status: watch::Sender<CollabLiveStatus>,
    reconnect: watch::Sender<u64>,
    session_id: watch::Sender<Option<String>>,
    /// The event stream connection whose `hello` may set the session id
    /// (Task 15 fix round 2). Held while the id is written.
    connection: Mutex<u64>,
    serving: watch::Sender<BTreeMap<String, bool>>,
    credentials: Mutex<Option<(String, String)>>,
    /// A copy of the feed's presence book, for [`live_presence`].
    presence: RwLock<PresenceBook>,
    me: RwLock<Option<String>>,
    /// A runtime (its loop and both workers) runs right now — false while
    /// `supervise` waits to start one, restarts it, or after it stopped.
    running: std::sync::atomic::AtomicBool,
    /// The bound node's exchange meter, set by every runtime start (a
    /// rebound node brings its own), read by `get_collab_exchange`.
    meter: RwLock<Option<Arc<ExchangeMeter>>>,
    /// Each live project's "to go", published by `Executor::step` whenever
    /// the scheduler's wants change (spec §6.2); the snapshot reads it.
    to_go: Arc<RwLock<HashMap<String, usize>>>,
    /// Test only (final fix A-I3): how many runtimes this handle started.
    #[cfg(test)]
    starts: AtomicUsize,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl Shared {
    fn new(ctx: Arc<ServiceContext>, emitter: Option<Arc<dyn ProgressEmitter>>) -> Self {
        Self {
            ctx,
            emitter,
            status: watch::channel(CollabLiveStatus {
                state: LiveState::Connecting,
                retry_in_secs: None,
                since: now_rfc3339(),
                storage: StorageStateView::NotSet,
                storage_reason: None,
                watcher_degraded: false,
                network_volume: false,
            })
            .0,
            reconnect: watch::channel(0).0,
            session_id: watch::channel(None).0,
            connection: Mutex::new(0),
            serving: watch::channel(BTreeMap::new()).0,
            credentials: Mutex::new(None),
            presence: RwLock::new(PresenceBook::default()),
            me: RwLock::new(None),
            running: std::sync::atomic::AtomicBool::new(false),
            meter: RwLock::new(None),
            to_go: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(test)]
            starts: AtomicUsize::new(0),
        }
    }

    fn update_status(&self, f: impl FnOnce(&mut CollabLiveStatus) -> bool) {
        let changed = self.status.send_if_modified(f);
        if changed {
            let status = self.status.borrow().clone();
            tracing::debug!(state = ?status.state, retry_in_ms = status.retry_in_secs.map(|s| s * 1000), "collab live status changed");
            if let Some(em) = &self.emitter {
                emit_event(em.as_ref(), COLLAB_LIVE_STATUS_EVENT, &status);
            }
        }
    }

    /// Move the connection state (deduplicated; an emitted
    /// `collab-live-status` per change).
    pub(crate) fn set_state(&self, state: LiveState, retry: Option<Duration>) {
        let retry_in_secs = retry.map(|d| d.as_secs_f64().ceil() as u64);
        self.update_status(|s| {
            if s.state == state && s.retry_in_secs == retry_in_secs {
                return false;
            }
            if s.state != state {
                s.since = now_rfc3339();
            }
            s.state = state;
            s.retry_in_secs = retry_in_secs;
            true
        });
    }

    fn set_storage(&self, view: StorageStateView, reason: Option<String>) {
        self.update_status(|s| {
            if s.storage == view && s.storage_reason == reason {
                return false;
            }
            s.storage = view;
            s.storage_reason = reason;
            true
        });
    }

    fn set_watcher(&self, degraded: bool, network: bool) {
        self.update_status(|s| {
            if s.watcher_degraded == degraded && s.network_volume == network {
                return false;
            }
            s.watcher_degraded = degraded;
            s.network_volume = network;
            true
        });
    }

    pub(crate) fn status(&self) -> CollabLiveStatus {
        self.status.borrow().clone()
    }

    /// Drop the stream and reconnect at once (never the session's stop).
    pub(crate) fn reconnect_now(&self) {
        self.reconnect.send_modify(|n| *n = n.wrapping_add(1));
    }

    pub(crate) fn reconnect_signal(&self) -> watch::Receiver<u64> {
        let mut rx = self.reconnect.subscribe();
        rx.mark_unchanged();
        rx
    }

    pub(crate) fn set_session_id(&self, id: Option<String>) {
        self.session_id.send_replace(id);
    }

    /// A new event stream connection: the session id is cleared until its
    /// own `hello` names one. Returns the connection's generation.
    pub(crate) fn begin_connection(&self) -> u64 {
        let mut current = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        *current = current.wrapping_add(1);
        self.session_id.send_replace(None);
        *current
    }

    /// Connection `conn`'s `hello` named session `id` — set unless a newer
    /// connection began since (a stale `hello` never replaces a newer id).
    pub(crate) fn set_session_for(&self, conn: u64, id: String) {
        let current = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        if *current != conn {
            tracing::debug!("a stale connection's hello ignored");
            return;
        }
        self.session_id.send_replace(Some(id));
    }

    pub(crate) fn session_signal(&self) -> watch::Receiver<Option<String>> {
        self.session_id.subscribe()
    }

    pub(crate) fn serving_signal(&self) -> watch::Receiver<BTreeMap<String, bool>> {
        self.serving.subscribe()
    }

    fn set_serving(&self, map: BTreeMap<String, bool>) {
        self.serving.send_if_modified(|cur| {
            if *cur == map {
                return false;
            }
            tracing::debug!(
                count = map.values().filter(|s| **s).count(),
                "serving map changed"
            );
            *cur = map;
            true
        });
    }

    pub(crate) fn set_credentials(&self, creds: Option<(String, String)>) {
        *self.credentials.lock().unwrap_or_else(|p| p.into_inner()) = creds;
    }

    pub(crate) fn emitter(&self) -> Option<Arc<dyn ProgressEmitter>> {
        self.emitter.clone()
    }

    /// The feed's presence book, copied after every applied event.
    pub(crate) fn set_presence(&self, presence: &PresenceBook) {
        match self.presence.write() {
            Ok(mut p) => *p = presence.clone(),
            Err(e) => tracing::error!(error = %e, "presence copy lock poisoned; not updated"),
        }
    }

    pub(crate) fn presence_copy(&self) -> PresenceBook {
        match self.presence.read() {
            Ok(p) => p.clone(),
            Err(e) => {
                tracing::error!(error = %e, "presence copy lock poisoned; read as left");
                e.into_inner().clone()
            }
        }
    }

    pub(crate) fn credentials(&self) -> Option<(String, String)> {
        self.credentials
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// `DELETE /me/presence` for the current session, bounded (P28) — the
    /// bound covers building the request's client too (Task 18, flake-5).
    pub(crate) async fn leave(&self, hub_url: &str) {
        let Some(id) = self.session_id.borrow().clone() else {
            return;
        };
        let leave = async {
            let client = match CollabClient::new(hub_url.to_string()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "presence leave client could not be built");
                    return None;
                }
            };
            Some(client.presence_leave(&id).await)
        };
        match tokio::time::timeout(crate::api::collab_live::session::LEAVE_TIMEOUT, leave).await {
            Ok(None) => return,
            Ok(Some(Ok(()))) => tracing::info!("left presence"),
            Ok(Some(Err(e))) => {
                tracing::warn!(error = %e, "presence leave failed; the hub's timeouts take the device offline")
            }
            Err(_) => tracing::warn!(
                duration_ms = crate::api::collab_live::session::LEAVE_TIMEOUT.as_millis() as u64,
                "presence leave timed out; the hub's timeouts take the device offline"
            ),
        }
        self.set_session_id(None);
    }
}

// ── the handle registry ─────────────────────────────────────────────────

pub(crate) enum LiveCommand {
    /// Sync now (L10, P26).
    Reconcile,
    /// A command changed a project's local state.
    LocalChange(String),
    SetStreams(usize),
    /// The publishing binding moved here: the re-announce check, on the
    /// feed worker (final fix A-I1).
    Rebind(String),
    /// Test only (final fix A-I2): re-derive this project's scope.
    #[cfg(test)]
    PolicyDirty(String),
    /// Test only (final fix A-I3): make one of the workers panic.
    #[cfg(test)]
    PanicWorker(TestWorker),
    Stop {
        sign_out: bool,
        done: oneshot::Sender<()>,
    },
    /// Test only (Task 18, spec §12 "B is killed"): stop as a crashed
    /// process does — no presence leave, no oracle change, nothing flushed.
    #[cfg(test)]
    Crash {
        done: oneshot::Sender<()>,
    },
}

/// Which worker a test makes panic.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum TestWorker {
    Feed,
    Storage,
}

struct LiveHandle {
    commands: mpsc::UnboundedSender<LiveCommand>,
    shared: Arc<Shared>,
    abort: tokio::task::AbortHandle,
}

static ARMED: OnceLock<Mutex<HashMap<String, LiveHandle>>> = OnceLock::new();

fn registry() -> std::sync::MutexGuard<'static, HashMap<String, LiveHandle>> {
    ARMED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// The registry key: one runtime per catalog (the in-process tests run
/// several instances in one process).
fn scope(ctx: &ServiceContext) -> Option<String> {
    match db(ctx) {
        Ok(d) => Some(d.path().to_string_lossy().to_string()),
        Err(e) => {
            tracing::warn!(error = %e, "collab live exchange: the catalog is not open");
            None
        }
    }
}

fn handle_shared(ctx: &ServiceContext) -> Option<Arc<Shared>> {
    let key = scope(ctx)?;
    registry().get(&key).map(|h| Arc::clone(&h.shared))
}

/// The running live exchange's meter and its last "to go" counts, for the
/// `get_collab_exchange` snapshot. `None` when no runtime runs (signed out,
/// waiting for its folder or storage, just started).
pub(crate) fn exchange_view(
    ctx: &ServiceContext,
) -> Option<(Arc<ExchangeMeter>, HashMap<String, usize>)> {
    let shared = handle_shared(ctx)?;
    if !shared.running.load(Ordering::SeqCst) {
        return None;
    }
    let meter = shared
        .meter
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    let to_go = match shared.to_go.read() {
        Ok(g) => g.clone(),
        Err(p) => {
            tracing::warn!("exchange to-go cache poisoned; continuing with its data");
            p.into_inner().clone()
        }
    };
    Some((meter, to_go))
}

fn send(ctx: &ServiceContext, cmd: LiveCommand) -> bool {
    let Some(key) = scope(ctx) else {
        return false;
    };
    match registry().get(&key) {
        Some(h) => h.commands.send(cmd).is_ok(),
        None => false,
    }
}

/// Where the runtime takes its receive gate from.
pub(crate) enum GateSource {
    /// The started sync receiver's (production).
    #[cfg_attr(test, allow(dead_code))]
    Sync(Arc<crate::sync::SyncRuntime>),
    /// A gate the caller owns (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    Fixed(Arc<InboundControl>),
}

/// Per-runtime configuration (Task 15 R1: never a process global).
#[derive(Clone, Copy)]
pub(crate) struct LiveConfig {
    pub timings: StorageTimings,
    pub ready_poll: Duration,
    /// The presence beat interval (the spec's 15 s; a test against a fake
    /// hub with a shortened silence rule beats faster).
    pub beat: Duration,
    /// How often rows parked for the collab GC are re-checked
    /// ([`GC_PROBE_EVERY`]; a test whose store GC runs every 100 ms probes
    /// as often).
    pub gc_probe: Duration,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            timings: StorageTimings::default(),
            ready_poll: READY_POLL,
            beat: crate::collab::live::presence::BEAT_INTERVAL,
            gc_probe: GC_PROBE_EVERY,
        }
    }
}

/// Arm the live exchange once per catalog (every `ensure_started` site
/// calls it; the second and later calls are no-ops). Also arms the
/// auto-publish worker, once per process.
///
/// In the crate's unit tests the `ensure_started` sites arm nothing: a test
/// that starts the sync receiver against a mock hub must not get a live
/// session talking to it behind its back (the wave-2 worker's 90 s start
/// delay kept it out of every test the same way). The live tests arm it
/// explicitly (`spawn_with`).
pub fn spawn_collab_live(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Option<tokio::task::JoinHandle<()>> {
    #[cfg(test)]
    {
        let _ = (ctx, sync, emitter);
        tracing::debug!("collab live exchange not armed in a unit test");
        None
    }
    #[cfg(not(test))]
    {
        let armed = spawn_with(
            Arc::clone(&ctx),
            GateSource::Sync(sync),
            emitter.clone(),
            LiveConfig::default(),
        );
        arm_auto_publish_once(ctx, emitter);
        armed
    }
}

/// The coalesced auto-publish worker shares the live exchange's arming
/// sites, once per PROCESS (its dirty state is process-global) — a sign-out
/// and sign-in re-arm the live exchange, never a second worker.
#[cfg(not(test))]
fn arm_auto_publish_once(ctx: Arc<ServiceContext>, emitter: Option<Arc<dyn ProgressEmitter>>) {
    static ARMED_PUBLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !ARMED_PUBLISH.swap(true, std::sync::atomic::Ordering::SeqCst) {
        crate::api::collab_autopublish::spawn_auto_publish_worker(ctx, emitter);
    }
}

pub(crate) fn spawn_with(
    ctx: Arc<ServiceContext>,
    gate: GateSource,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    cfg: LiveConfig,
) -> Option<tokio::task::JoinHandle<()>> {
    let key = scope(&ctx)?;
    let mut reg = registry();
    if reg.contains_key(&key) {
        tracing::debug!("collab live exchange already armed");
        return None;
    }
    let (commands, cmd_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared::new(ctx, emitter));
    let task = tokio::spawn(supervise(Arc::clone(&shared), cmd_rx, gate, cfg));
    reg.insert(
        key,
        LiveHandle {
            commands,
            shared,
            abort: task.abort_handle(),
        },
    );
    drop(reg);
    tracing::info!("collab live exchange armed");
    Some(task)
}

/// The live status (or `off` when no live exchange runs for this catalog).
pub fn status(ctx: &ServiceContext) -> CollabLiveStatus {
    match handle_shared(ctx) {
        Some(s) => s.status(),
        None => CollabLiveStatus {
            state: LiveState::Off,
            retry_in_secs: None,
            since: now_rfc3339(),
            storage: StorageStateView::NotSet,
            storage_reason: None,
            watcher_degraded: false,
            network_volume: false,
        },
    }
}

/// Sync now (L10, P26): every back-off cleared, the stream dropped and
/// reopened at once, then reconciliation (a digest check per project, a
/// stat sweep). Armed but not running yet (waiting for its folder, its
/// node, or restarting): queued, applied when the runtime starts (final
/// fix A-M4) — never a silent no-op.
pub fn sync_now(ctx: &ServiceContext) -> Result<(), ApiError> {
    let Some(shared) = handle_shared(ctx) else {
        let e = ApiError::Invalid(
            "The live exchange is not running (signed out, or no Collaboration folder yet).".into(),
        );
        tracing::warn!(error = %e, "sync now refused");
        return Err(e);
    };
    if shared.running.load(Ordering::SeqCst) {
        tracing::info!("collab sync now requested");
    } else {
        tracing::info!("collab sync now requested; queued until the live exchange runs");
    }
    crate::collab::live::backoff::reset_all();
    shared.reconnect_now();
    if !send(ctx, LiveCommand::Reconcile) {
        let e = ApiError::Internal("the live exchange stopped".into());
        tracing::error!(error = %e, "sync now: reconciliation not queued");
        return Err(e);
    }
    Ok(())
}

/// A command changed a project's local state: its need set is re-read
/// (queued while the runtime is not running yet, final fix A-M4).
pub fn notify_local_change(ctx: &ServiceContext, project_id: &str) {
    if !send(ctx, LiveCommand::LocalChange(project_id.to_string())) {
        tracing::debug!(
            project_id,
            "no live exchange runs; the change applies when it starts"
        );
    }
}

/// Whether a live runtime runs for this catalog right now (its feed worker
/// takes work).
pub(crate) fn live_running(ctx: &ServiceContext) -> bool {
    handle_shared(ctx).is_some_and(|s| s.running.load(Ordering::SeqCst))
}

/// Hand the re-announce check after a binding move to the feed worker
/// (final fix A-I1). `false`: no live exchange took it — the caller runs it.
pub(crate) fn request_rebind(ctx: &ServiceContext, project_id: &str) -> bool {
    send(ctx, LiveCommand::Rebind(project_id.to_string()))
}

/// `collab.max_receive_streams` changed (L11): applied live.
pub fn set_receive_streams(ctx: &ServiceContext, n: usize) {
    if !send(ctx, LiveCommand::SetStreams(n)) {
        tracing::debug!(
            streams = n,
            "no live exchange runs; the limit applies when it starts"
        );
    }
}

/// Clean exit (P28): `DELETE /me/presence` and stop, bounded by
/// [`STOP_BOUND`]. The catalog-backed serve oracle stays installed.
pub async fn shutdown(ctx: &ServiceContext) {
    stop(ctx, false).await;
}

/// Sign-out: as [`shutdown`], and the node serves nothing more (its serve
/// oracle is removed).
pub async fn on_sign_out(ctx: &ServiceContext) {
    stop(ctx, true).await;
    // A runtime armed again meanwhile (a sign-in during the stop window)
    // owns the oracle now (final fix A-M1).
    if handle_shared(ctx).is_some() {
        tracing::debug!(
            "a new live exchange was armed during the sign-out; its serve oracle stays"
        );
        return;
    }
    if let Some(node) = crate::api::collab_exchange::bound_node(ctx).await {
        node.set_collab_serve_oracle(None);
    }
}

async fn stop(ctx: &ServiceContext, sign_out: bool) {
    let Some(key) = scope(ctx) else {
        return;
    };
    let Some(handle) = registry().remove(&key) else {
        return;
    };
    let (done, rx) = oneshot::channel();
    if handle
        .commands
        .send(LiveCommand::Stop { sign_out, done })
        .is_err()
    {
        tracing::debug!("collab live exchange already stopped");
        return;
    }
    match tokio::time::timeout(STOP_BOUND, rx).await {
        Ok(_) => tracing::info!(
            outcome = if sign_out { "signed_out" } else { "shutdown" },
            "collab live exchange stopped"
        ),
        Err(_) => {
            tracing::warn!(
                duration_ms = STOP_BOUND.as_millis() as u64,
                "collab live exchange did not stop in time; aborted"
            );
            handle.abort.abort();
        }
    }
    handle.shared.set_state(LiveState::Off, None);
}

/// Test only (Task 18): kill this catalog's live exchange as a crash would
/// — no presence leave (see [`LiveCommand::Crash`]).
#[cfg(test)]
pub(crate) async fn crash_for_test(ctx: &ServiceContext) {
    let Some(key) = scope(ctx) else {
        return;
    };
    let Some(handle) = registry().remove(&key) else {
        return;
    };
    let (done, rx) = oneshot::channel();
    if handle.commands.send(LiveCommand::Crash { done }).is_ok() {
        let _ = tokio::time::timeout(STOP_BOUND, rx).await;
    }
    handle.abort.abort();
}

/// Test only (final fix A-I3): how many runtimes this catalog's handle has
/// started.
#[cfg(test)]
pub(crate) fn runtime_starts(ctx: &ServiceContext) -> usize {
    handle_shared(ctx).map_or(0, |s| s.starts.load(Ordering::SeqCst))
}

/// Test only (final fix A-I3): make one of the runtime's workers panic.
#[cfg(test)]
pub(crate) fn panic_worker_for_test(ctx: &ServiceContext, worker: TestWorker) {
    assert!(
        send(ctx, LiveCommand::PanicWorker(worker)),
        "no live exchange armed"
    );
}

/// Test only (final fix A-I2): re-derive `project_id`'s replication scope.
#[cfg(test)]
pub(crate) fn mark_policy_dirty_for_test(ctx: &ServiceContext, project_id: &str) {
    assert!(
        send(ctx, LiveCommand::PolicyDirty(project_id.to_string())),
        "no live exchange armed"
    );
}

/// The live presence and this device's id, for the command surface's holder
/// counts (Task 16: the frames list, the attention lists, the last-copy
/// warning) — read against the persisted holder map, one project at a time.
/// `None` when no live exchange runs.
pub(crate) fn live_presence(ctx: &ServiceContext) -> Option<(PresenceBook, String)> {
    let shared = handle_shared(ctx)?;
    // M2 (fix round 1): a poisoned lock is logged, never a silent `None`.
    let me = match shared.me.read() {
        Ok(me) => me.clone()?,
        Err(e) => {
            tracing::error!(error = %e, "live device id lock poisoned; no holder view");
            return None;
        }
    };
    Some((shared.presence_copy(), me))
}

pub(crate) fn redundancy_of(
    map: &crate::collab::live::holders::ProjectHolders,
    presence: &PresenceBook,
    project: &crate::db::collab::CollabProjectRow,
    me: &str,
    row: &crate::db::collab_frames::LocalFrameRow,
) -> Redundancy {
    let Some(seq) = row.frame_seq else {
        return Redundancy::default();
    };
    let none = HashSet::new();
    redundancy(
        map,
        presence,
        &member_devices(project),
        me,
        &FrameRef {
            project_id: &row.project_id,
            frame_seq: seq,
            content_version: row.content_version,
            publisher_devices: &none,
        },
    )
}

// ── the runtime ─────────────────────────────────────────────────────────

/// What the runtime needs before it can run.
struct Ready {
    node: Arc<SharedIrohNode>,
    store: iroh_blobs::api::Store,
    root: PathBuf,
    control: Arc<InboundControl>,
    /// The node's collab mount generation, seen at this store (I1).
    mount_gen: watch::Receiver<u64>,
}

/// `lazy_mount` (the first start): mount the designated folder's store when
/// the node has none, and adopt a folder whose marker was never recorded.
/// After a remount neither is done here — the designation in progress mounts
/// the store and records the marker itself, and either would race it.
async fn ready(
    ctx: &ServiceContext,
    gate: &GateSource,
    lazy_mount: bool,
) -> Result<Ready, &'static str> {
    let Some(node) = crate::api::collab_exchange::bound_node(ctx).await else {
        return Err("no bound node");
    };
    let root = match crate::api::scan_roots::get_collaboration_dir(ctx) {
        Ok(Some(r)) => PathBuf::from(r),
        Ok(None) => return Err("no Collaboration folder"),
        Err(e) => {
            tracing::warn!(error = %e, "collaboration root could not be read");
            return Err("collaboration root unreadable");
        }
    };
    // A designation in progress (its mount done, its marker not recorded
    // yet): the runtime waits for the marker, or its own adoption would race
    // the designation's (I1).
    // After a remount the record must name THIS folder: a record cleared by
    // the folder change (`None`) would let this runtime adopt a marker the
    // designation is about to write. The first start adopts a root never
    // recorded (a wave-2 root) — even while a first designation of it is
    // in flight: every adoption of a marker (the designation's, this lazy
    // mount's, the session guard's) is decided under one lock
    // (`marker::check_and_adopt`), so they all adopt the ONE store id the
    // first of them wrote.
    match db(ctx).and_then(|d| Ok(crate::db::collab_live::store_marker_path(&d.conn())?)) {
        Ok(Some(at)) if at != root.to_string_lossy() => {
            return Err("storage marker not recorded for this folder yet")
        }
        Ok(None) if !lazy_mount => return Err("storage marker not recorded for this folder yet"),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "storage marker record could not be read");
            return Err("storage marker record unreadable");
        }
    }
    if node.collab_store().is_none()
        && (!lazy_mount
            || crate::api::collab_exchange::ensure_collab_store(ctx)
                .await
                .is_none())
    {
        return Err("collab store not mounted");
    }
    // Seen BEFORE the store is read (a mount bumps after its swap): any
    // later mount, unmount or swap restarts the runtime (Task 15 fix
    // round 1, I1).
    let mut mount_gen = node.collab_mount_signal();
    mount_gen.borrow_and_update();
    let Some((mounted, store)) = node.collab_mounted() else {
        return Err("collab store not mounted");
    };
    if !crate::sharing::iroh::node::same_dir(&mounted, &root) {
        return Err("collab store mounted at another folder");
    }
    let control = match gate {
        GateSource::Sync(sync) => match sync.inbound_control().await {
            Some(c) => c,
            None => return Err("receiver not started"),
        },
        GateSource::Fixed(c) => Arc::clone(c),
    };
    Ok(Ready {
        node,
        store,
        root,
        control,
        mount_gen,
    })
}

/// Commands that arrived while no runtime ran (final fix A-M4): applied
/// when the next one starts, deduplicated.
#[derive(Default)]
struct Pending(Vec<LiveCommand>);

impl Pending {
    fn push(&mut self, cmd: LiveCommand) {
        let dup = self.0.iter().any(|c| match (c, &cmd) {
            (LiveCommand::Reconcile, LiveCommand::Reconcile) => true,
            (LiveCommand::LocalChange(a), LiveCommand::LocalChange(b)) => a == b,
            (LiveCommand::Rebind(a), LiveCommand::Rebind(b)) => a == b,
            _ => false,
        });
        if dup {
            return;
        }
        if matches!(cmd, LiveCommand::SetStreams(_)) {
            self.0.retain(|c| !matches!(c, LiveCommand::SetStreams(_)));
        }
        tracing::debug!(
            count = self.0.len() + 1,
            "live command queued until the runtime starts"
        );
        self.0.push(cmd);
    }
}

/// Await `fut` while serving the command channel (final fix A-M2): a stop
/// is answered at once and ends the wait (`None`, the future dropped at its
/// await point); every other command is queued for the next runtime.
async fn or_stop<F: std::future::Future>(
    fut: F,
    commands: &mut mpsc::UnboundedReceiver<LiveCommand>,
    pending: &mut Pending,
) -> Option<F::Output> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            biased;
            out = &mut fut => return Some(out),
            cmd = commands.recv() => match cmd {
                None => return None,
                Some(LiveCommand::Stop { done, .. }) => {
                    let _ = done.send(());
                    return None;
                }
                #[cfg(test)]
                Some(LiveCommand::Crash { done }) => {
                    let _ = done.send(());
                    return None;
                }
                Some(other) => pending.push(other),
            },
        }
    }
}

/// The armed task: wait until the runtime can run, then run it — again on
/// its new store after the Collaboration folder moved (I1), or after one of
/// its workers died (final fix A-I3). A stop is observed at every wait
/// (final fix A-M2); every other command that arrives while no runtime runs
/// is queued for the next one (final fix A-M4).
async fn supervise(
    shared: Arc<Shared>,
    mut commands: mpsc::UnboundedReceiver<LiveCommand>,
    gate: GateSource,
    cfg: LiveConfig,
) {
    let ctx = Arc::clone(&shared.ctx);
    let mut lazy_mount = true;
    let mut pending = Pending::default();
    loop {
        let Some(ready) =
            or_stop(ready(&ctx, &gate, lazy_mount), &mut commands, &mut pending).await
        else {
            return;
        };
        match ready {
            Ok(r) => {
                let Some(started) = or_stop(
                    Runtime::start(Arc::clone(&shared), r, cfg),
                    &mut commands,
                    &mut pending,
                )
                .await
                else {
                    return;
                };
                match started {
                    Ok(rt) => match rt.run(&mut commands, std::mem::take(&mut pending)).await {
                        RunEnd::Stopped => return,
                        RunEnd::Remount => {
                            tracing::info!(
                                "collaboration store changed; the live exchange restarts"
                            );
                            lazy_mount = false;
                            continue;
                        }
                        RunEnd::Restart => {
                            tracing::warn!(
                                retry_in_ms = cfg.ready_poll.as_millis() as u64,
                                "the live exchange restarts after a worker ended"
                            );
                        }
                    },
                    Err(e) => {
                        tracing::error!(error = %e, "collab live exchange could not start; retried");
                    }
                }
            }
            Err(why) => {
                let view = if why == "no Collaboration folder" {
                    StorageStateView::NotSet
                } else {
                    StorageStateView::Unavailable
                };
                shared.set_storage(view, Some(why.to_string()));
                tracing::debug!(reason = why, "collab live exchange waits to start");
            }
        }
        if or_stop(
            tokio::time::sleep(cfg.ready_poll),
            &mut commands,
            &mut pending,
        )
        .await
        .is_none()
        {
            return;
        }
    }
}

/// Frames landed / failed / parked since the last burst, per project.
#[derive(Default)]
struct Burst {
    counts: HashMap<String, CollabFramesLanded>,
    last: HashMap<String, Instant>,
}

/// Per-project throttle for [`COLLAB_PEERS_CHANGED_EVENT`]: at most one
/// emission per project per [`LANDED_BURST`], mirroring [`Burst`] — a
/// project noted inside the window is not dropped, it flushes once the
/// window ends. Unlike `Burst` there is nothing to accumulate (the payload
/// is just the project id), so a note only schedules a due time; a note that
/// arrives while one is already scheduled changes nothing — it still fires
/// once, at the already-scheduled time.
#[derive(Default)]
struct PeerBurst {
    /// Project → when its next `collab-peers-changed` may fire.
    due_at: HashMap<String, Instant>,
    /// Project → when it last fired (the throttle anchor).
    last: HashMap<String, Instant>,
}

impl PeerBurst {
    /// Note a providers change (presence or holders) for `project_id` at
    /// `now`. The first note since the last emission schedules one —
    /// immediately if outside the throttle window, otherwise at the
    /// window's end.
    fn note(&mut self, project_id: &str, now: Instant) {
        if self.due_at.contains_key(project_id) {
            return;
        }
        let earliest = self
            .last
            .get(project_id)
            .map(|t| *t + LANDED_BURST)
            .filter(|t| *t > now)
            .unwrap_or(now);
        self.due_at.insert(project_id.to_string(), earliest);
    }

    /// Projects whose scheduled emission is due at `now` — removed from the
    /// schedule and stamped as last-fired.
    fn due(&mut self, now: Instant) -> Vec<String> {
        let ready: Vec<String> = self
            .due_at
            .iter()
            .filter(|(_, at)| now >= **at)
            .map(|(p, _)| p.clone())
            .collect();
        for p in &ready {
            self.due_at.remove(p);
            self.last.insert(p.clone(), now);
        }
        ready
    }

    /// The earliest scheduled emission, for the loop's `deadline()`.
    fn next_deadline(&self) -> Option<Instant> {
        self.due_at.values().min().copied()
    }
}

/// How a runtime's loop ended.
enum Exit {
    /// `shutdown` / `on_sign_out` (or the handle dropped: `None`).
    Stop(Option<(bool, oneshot::Sender<()>)>),
    /// The collab store was mounted, unmounted or swapped (I1).
    Remount,
    /// A worker ended (a panic): the whole runtime is rebuilt — a loop
    /// whose feed or storage worker is gone applies nothing more (final fix
    /// A-I3).
    Restart,
}

enum RunEnd {
    Stopped,
    Remount,
    Restart,
}

struct Runtime {
    shared: Arc<Shared>,
    ctx: Arc<ServiceContext>,
    node: Arc<SharedIrohNode>,
    me: String,
    /// The holder maps the feed worker keeps current (read here).
    maps: HolderMaps,
    feed_tx: mpsc::UnboundedSender<FeedWork>,
    feed_rx: mpsc::UnboundedReceiver<FeedOut>,
    feed_open: bool,
    feed_task: tokio::task::JoinHandle<()>,
    /// Stream events handed to the feed worker and not applied yet.
    feed_pending: Arc<AtomicUsize>,
    storage_tx: mpsc::UnboundedSender<StorageWork>,
    storage_rx: mpsc::UnboundedReceiver<StorageOut>,
    storage_open: bool,
    storage_task: tokio::task::JoinHandle<()>,
    storage_state: StoreState,
    degraded: bool,
    network: bool,
    exec: Executor,
    events_rx: mpsc::Receiver<LiveEvent>,
    events_open: bool,
    checks_rx: mpsc::UnboundedReceiver<(String, String)>,
    /// A closed channel answers `None` at once, forever: its arm is
    /// switched off instead of spinning the loop (the serve oracle replaced
    /// by another).
    checks_open: bool,
    mount_gen: watch::Receiver<u64>,
    mount_open: bool,
    reset: watch::Receiver<u64>,
    stop_tx: watch::Sender<bool>,
    session: tokio::task::JoinHandle<()>,
    refused: HashSet<String>,
    next_gc_probe: Instant,
    gc_probe: Duration,
    burst: Burst,
    /// Throttles `collab-peers-changed` (presence or holders changed).
    peer_burst: PeerBurst,
    serving_dirty: bool,
    attention: BTreeSet<String>,
    /// Projects whose replication scope (`apply_policy`) is re-derived at
    /// the end of this turn.
    policy_dirty: BTreeSet<String>,
    /// Re-derives running on a blocking thread (final fix A-I2: never a
    /// catalog write on the loop); their need sets wait for the result.
    policy_pending: BTreeSet<String>,
    /// Marked dirty again while a re-derive was running: once more after it.
    policy_again: BTreeSet<String>,
    /// Failed re-derives, retried after their back-off (never dropped).
    policy_retry: BTreeMap<String, (Instant, crate::collab::live::backoff::Backoff)>,
    policy_tx: mpsc::UnboundedSender<(String, Result<usize, ApiError>)>,
    policy_rx: mpsc::UnboundedReceiver<(String, Result<usize, ApiError>)>,
    /// The serve oracle this runtime installed: `finish` replaces only it
    /// (final fix A-M1).
    oracle: Arc<dyn crate::collab::serve::ServeOracle>,
    /// The node's exchange meter (the executor's fetches and the node's
    /// serves write it; `flush_exchange` reads it).
    meter: Arc<ExchangeMeter>,
    /// Throttles `collab-exchange-progress` (spec 2026-09-29 §6.4).
    exchange_gate: ProgressGate,
    /// When the progress gate is polled next.
    next_exchange: Instant,
}

impl Runtime {
    async fn start(shared: Arc<Shared>, r: Ready, cfg: LiveConfig) -> Result<Self, ApiError> {
        let ctx = Arc::clone(&shared.ctx);
        let me = crate::api::account::own_device_id(&ctx)?;
        *shared.me.write().unwrap_or_else(|p| p.into_inner()) = Some(me.clone());
        let recorded = {
            let d = db(&ctx)?;
            let conn = d.conn();
            let at = crate::db::collab_live::store_marker_path(&conn)?;
            if at.as_deref() == Some(r.root.to_string_lossy().as_ref()) {
                crate::db::collab_live::recorded_store_marker(&conn)?
            } else {
                None
            }
        };
        // The ONE storage guard of this session (Task 15 R4).
        let guard = Arc::new(StoreGuard::new(r.root.clone(), me.clone(), recorded));
        let state = guard.check_now();
        if let Some(adopted) = guard.take_adoption() {
            let d = db(&ctx)?;
            crate::db::collab_live::record_store_marker(
                &d.conn(),
                &adopted,
                &r.root.to_string_lossy(),
            )?;
        }
        // Landings of this session never meet a temp file of an earlier one.
        let root = r.root.clone();
        match tokio::task::spawn_blocking(move || {
            crate::api::collab_live::landing::sweep_orphaned_athtmp(&root)
        })
        .await
        {
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "landing temp sweep task failed"),
        }
        // Registering the folder watcher blocks until the OS stream is up
        // (macOS FSEvents: measured 1.6–3.2 s) — off the async path, so the
        // start stays an await point `or_stop` can end at once (final fix
        // A-M2); a stop meanwhile drops the engine when the thread returns.
        let engine = {
            let (ctx, node, guard, timings) = (
                Arc::clone(&ctx),
                Arc::clone(&r.node),
                Arc::clone(&guard),
                cfg.timings,
            );
            tokio::task::spawn_blocking(move || {
                StorageEngine::start_with(ctx, node, guard, timings)
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "collaboration storage engine start panicked");
                ApiError::Internal(format!("storage engine start failed: {e}"))
            })?
        };
        let (degraded, network) = (engine.degraded(), engine.network());
        let maps = HolderMaps::default();
        let holders: Arc<dyn HolderView> = Arc::new(SharedHolders {
            ctx: Arc::clone(&ctx),
            maps: Arc::clone(&maps),
            shared: Arc::clone(&shared),
            me: me.clone(),
        });
        let (storage_tx, storage_work) = mpsc::unbounded_channel();
        let (storage_out, storage_rx) = mpsc::unbounded_channel();
        let storage_task = tokio::spawn(crate::api::collab_live::workers::run_storage(
            engine,
            holders,
            storage_work,
            storage_out,
        ));
        let (checks_tx, checks_rx) = mpsc::unbounded_channel();
        let oracle: Arc<dyn crate::collab::serve::ServeOracle> =
            Arc::new(crate::api::collab_live::serve_oracle::DbServeOracle::new(
                Arc::clone(&ctx),
                Arc::clone(&guard),
                checks_tx,
            ));
        r.node.set_collab_serve_oracle(Some(Arc::clone(&oracle)));
        let (upload, receive) = {
            let d = db(&ctx)?;
            let conn = d.conn();
            (
                ctx.settings
                    .get_collab_max_upload_streams(&conn)
                    .map_err(ApiError::from)?,
                ctx.settings
                    .get_collab_max_receive_streams(&conn)
                    .map_err(ApiError::from)?,
            )
        };
        r.node.set_collab_upload_limit(upload);
        let seed = uuid::Uuid::new_v4().as_u128() as u64;
        let meter = r.node.exchange_meter();
        *shared.meter.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&meter));
        shared
            .to_go
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        let exec = Executor::new(
            ExecEnv {
                ctx: Arc::clone(&ctx),
                node: Arc::clone(&r.node),
                store: r.store,
                root: r.root,
                guard,
                control: r.control,
                meter: Arc::clone(&meter),
                to_go: Arc::clone(&shared.to_go),
            },
            receive,
            seed,
        );
        let (feed_tx, feed_work) = mpsc::unbounded_channel();
        let (feed_out, feed_rx) = mpsc::unbounded_channel();
        let feed_pending = Arc::new(AtomicUsize::new(0));
        let feed_task = tokio::spawn(
            FeedWorker::new(
                Arc::clone(&shared),
                me.clone(),
                Arc::clone(&maps),
                feed_out,
                Arc::clone(&feed_pending),
            )
            .run(feed_work),
        );
        // M2 (fix round 1): an unreadable account is logged, never swallowed
        // — the feed waits for the next hello's credentials.
        let creds = match crate::api::account::hub_credentials(&ctx) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "account could not be read; the feed waits for the next hello");
                None
            }
        };
        let _ = feed_tx.send(FeedWork::Creds(creds));
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let (stop_tx, stop_rx) = watch::channel(false);
        let session = tokio::spawn(crate::api::collab_live::session::run_session(
            Arc::clone(&shared),
            Arc::clone(&r.node),
            events_tx,
            stop_rx,
            cfg.beat,
        ));
        let (policy_tx, policy_rx) = mpsc::unbounded_channel();
        let mut rt = Self {
            ctx,
            node: r.node,
            me,
            maps,
            feed_tx,
            feed_rx,
            feed_open: true,
            feed_task,
            feed_pending,
            storage_tx,
            storage_rx,
            storage_open: true,
            storage_task,
            storage_state: state.clone(),
            degraded,
            network,
            exec,
            events_rx,
            events_open: true,
            checks_rx,
            checks_open: true,
            mount_gen: r.mount_gen,
            mount_open: true,
            reset: crate::collab::live::backoff::reset_signal(),
            stop_tx,
            session,
            refused: HashSet::new(),
            next_gc_probe: Instant::now() + cfg.gc_probe,
            gc_probe: cfg.gc_probe,
            burst: Burst::default(),
            peer_burst: PeerBurst::default(),
            serving_dirty: true,
            attention: BTreeSet::new(),
            policy_dirty: BTreeSet::new(),
            policy_pending: BTreeSet::new(),
            policy_again: BTreeSet::new(),
            policy_retry: BTreeMap::new(),
            policy_tx,
            policy_rx,
            oracle,
            meter,
            exchange_gate: ProgressGate::default(),
            next_exchange: Instant::now(),
            shared,
        };
        rt.publish_storage();
        rt.exec.storage(state.fetching());
        for pid in rt.live_projects() {
            rt.exec.dirty.insert(pid);
        }
        rt.shared.running.store(true, Ordering::SeqCst);
        #[cfg(test)]
        rt.shared.starts.fetch_add(1, Ordering::SeqCst);
        tracing::info!(state = ?state, "collab live exchange running");
        Ok(rt)
    }

    fn live_projects(&self) -> Vec<String> {
        match db(&self.ctx).and_then(|d| Ok(crate::db::collab::list_projects(&d.conn())?)) {
            Ok(ps) => ps.into_iter().map(|p| p.project_id).collect(),
            Err(e) => {
                tracing::error!(error = %e, "live projects could not be listed");
                Vec::new()
            }
        }
    }

    fn emit<T: serde::Serialize>(&self, name: &str, payload: &T) {
        if let Some(em) = &self.shared.emitter {
            emit_event(em.as_ref(), name, payload);
        }
    }

    /// Hand work to the feed worker (it is gone only after a panic, logged
    /// where it is reaped).
    fn to_feed(&self, work: FeedWork) {
        if self.feed_tx.send(work).is_err() {
            tracing::error!("the feed worker is gone; feed work dropped");
        }
    }

    fn to_storage(&self, work: StorageWork) {
        if self.storage_tx.send(work).is_err() {
            tracing::error!("the storage task is gone; storage work dropped");
        }
    }

    fn note_append(&self, project_id: &str) {
        self.to_feed(FeedWork::NoteAppend(project_id.to_string(), Instant::now()));
    }

    fn publish_storage(&mut self) {
        let (view, reason) = match &self.storage_state {
            StoreState::Available => (StorageStateView::Available, None),
            StoreState::ReadOnly => (StorageStateView::ReadOnly, None),
            StoreState::Unavailable(r) => (
                StorageStateView::Unavailable,
                Some(
                    match r {
                        UnavailableReason::PathMissing => "path missing",
                        UnavailableReason::NotADirectory => "not a directory",
                        UnavailableReason::MarkerMissing => "marker missing",
                        UnavailableReason::MarkerMismatch => "marker id mismatch",
                        UnavailableReason::OtherDevice { .. } => "another device's folder",
                    }
                    .to_string(),
                ),
            ),
        };
        self.shared.set_storage(view, reason);
        self.shared.set_watcher(self.degraded, self.network);
        self.serving_dirty = true;
    }

    fn deadline(&self) -> Instant {
        let mut d = self.next_gc_probe;
        if let Some(w) = self.exec.next_wake() {
            d = d.min(w);
        }
        for (at, _) in self.policy_retry.values() {
            d = d.min(*at);
        }
        for pid in self.burst.counts.keys() {
            if let Some(last) = self.burst.last.get(pid) {
                d = d.min(*last + LANDED_BURST);
            }
        }
        if let Some(next) = self.peer_burst.next_deadline() {
            d = d.min(next);
        }
        // Something in flight or moving, or a quiet payload owed: the
        // progress gate is polled once a period (a flow that starts moving
        // wakes the loop through `flow_started`).
        if self.exchange_gate.armed() || self.meter.needs_progress(Instant::now()) {
            d = d.min(self.next_exchange);
        }
        d
    }

    /// The loop. Every arm is short: hub HTTP and sweeps run on the two
    /// workers (Task 15 fix round 1, C1), and catalog writes on blocking
    /// threads (final fix A-I2), so executor results, lane grants, pool
    /// events, the yield signal, commands and stop are serviced within
    /// milliseconds. `pending`: commands queued while no runtime ran (final
    /// fix A-M4), applied first.
    async fn run(
        mut self,
        commands: &mut mpsc::UnboundedReceiver<LiveCommand>,
        pending: Pending,
    ) -> RunEnd {
        if !pending.0.is_empty() {
            tracing::info!(
                count = pending.0.len(),
                "commands queued before the live exchange ran applied"
            );
        }
        for cmd in pending.0 {
            if let Some(exit) = self.on_command(cmd) {
                return self.finish(exit).await;
            }
        }
        let meter = Arc::clone(&self.meter);
        let exit = loop {
            self.flush_dirty();
            let mut deadline = self.deadline();
            // M6: a due timer runs first, whatever else is ready (a cheap
            // starvation guard; the timer work is short).
            if Instant::now() >= deadline {
                self.on_timers().await;
                self.flush_dirty();
                deadline = self.deadline();
            }
            let feed_room = self.feed_pending.load(Ordering::SeqCst) < EVENT_QUEUE;
            tokio::select! {
                biased;
                cmd = commands.recv() => match cmd {
                    None => break Exit::Stop(None),
                    Some(cmd) => {
                        #[cfg(test)]
                        if let LiveCommand::Crash { done } = cmd {
                            self.crash();
                            let _ = done.send(());
                            return RunEnd::Stopped;
                        }
                        if let Some(exit) = self.on_command(cmd) {
                            break exit;
                        }
                    }
                },
                changed = self.mount_gen.changed(), if self.mount_open => {
                    if changed.is_ok() {
                        break Exit::Remount;
                    }
                    self.mount_open = false;
                }
                changed = self.exec.yield_rx.changed() => {
                    if changed.is_ok() {
                        self.exec.on_yield_changed();
                    }
                }
                done = self.exec.done_rx.recv() => {
                    if let Some((name, outcome)) = done {
                        self.exec.on_done(name, outcome).await;
                        self.apply_notes();
                    }
                }
                ev = self.exec.events_rx.recv() => {
                    if let Some(ev) = ev {
                        self.exec.on_event(ev).await;
                        self.apply_notes();
                    }
                }
                ended = self.exec.tasks.join_next_with_id(), if !self.exec.tasks.is_empty() => {
                    if let Some(ended) = ended {
                        self.exec.on_task_ended(ended).await;
                        self.apply_notes();
                    }
                }
                v = self.exec.verdicts_rx.recv() => {
                    if let Some(v) = v {
                        self.exec.on_verdict(v);
                    }
                }
                ev = self.exec.pool_rx.recv() => {
                    if let Some(ev) = ev {
                        self.exec.on_pool(ev);
                    }
                }
                changed = self.reset.changed() => {
                    if changed.is_ok() {
                        self.exec.step(Input::ClearBackoffs);
                    }
                }
                applied = self.policy_rx.recv() => {
                    if let Some((p, res)) = applied {
                        self.on_policy_applied(p, res);
                    }
                }
                out = self.feed_rx.recv(), if self.feed_open => match out {
                    Some(out) => self.on_feed_out(out),
                    None => {
                        // Final fix A-I3: a loop without its feed worker
                        // applies no hub event more — rebuild the runtime
                        // (the session leaves presence on the way out).
                        tracing::error!("the feed worker ended; the live exchange restarts");
                        self.feed_open = false;
                        break Exit::Restart;
                    }
                },
                out = self.storage_rx.recv(), if self.storage_open => match out {
                    Some(out) => {
                        let watcher = (out.degraded, out.network);
                        if watcher != (self.degraded, self.network) {
                            (self.degraded, self.network) = watcher;
                            self.publish_storage();
                        }
                        self.on_storage_events(out.events);
                    }
                    None => {
                        tracing::error!("the storage task ended; the live exchange restarts");
                        self.storage_open = false;
                        break Exit::Restart;
                    }
                },
                ev = self.events_rx.recv(), if self.events_open && feed_room => match ev {
                    Some(ev) => {
                        // The session set the id when its pump read the
                        // hello (fix round 2): never gated by this queue.
                        self.feed_pending.fetch_add(1, Ordering::SeqCst);
                        self.to_feed(FeedWork::Event(ev));
                    }
                    None => self.events_open = false,
                },
                check = self.checks_rx.recv(), if self.checks_open => match check {
                    Some((p, u)) => self.to_storage(StorageWork::Check(p, u)),
                    None => {
                        tracing::warn!("the serve oracle's check channel closed; mismatches wait for the sweep");
                        self.checks_open = false;
                    }
                },
                // A flow started moving: nothing to do but re-read the
                // deadline (the progress gate is due now).
                _ = meter.flow_started() => {}
                _ = tokio::time::sleep_until(deadline.into()) => self.on_timers().await,
            }
        };
        self.finish(exit).await
    }

    /// One command (from the channel, or queued before the runtime ran).
    /// `Some`: the loop ends.
    fn on_command(&mut self, cmd: LiveCommand) -> Option<Exit> {
        match cmd {
            LiveCommand::Stop { sign_out, done } => {
                return Some(Exit::Stop(Some((sign_out, done))))
            }
            LiveCommand::Reconcile => self.reconcile(),
            LiveCommand::LocalChange(p) => {
                // The command may have appended claim changes: they flush
                // after the usual delay, not at the next wake.
                self.note_append(&p);
                self.exec.dirty.insert(p.clone());
                self.attention.insert(p);
            }
            LiveCommand::SetStreams(n) => self.exec.set_slots(n),
            LiveCommand::Rebind(p) => self.to_feed(FeedWork::Rebind(p)),
            #[cfg(test)]
            LiveCommand::PolicyDirty(p) => {
                self.policy_dirty.insert(p);
            }
            #[cfg(test)]
            LiveCommand::PanicWorker(TestWorker::Feed) => self.to_feed(FeedWork::Panic),
            #[cfg(test)]
            LiveCommand::PanicWorker(TestWorker::Storage) => self.to_storage(StorageWork::Panic),
            #[cfg(test)]
            LiveCommand::Crash { done } => {
                // Queued before the runtime ran: nothing to kill yet.
                let _ = done.send(());
                return Some(Exit::Stop(None));
            }
        }
        None
    }

    /// Test only: end as a killed process — every task aborted where it
    /// stands. The event stream's connection drops with the session task
    /// (the hub sees a closed stream, never a `DELETE /me/presence`), and
    /// its beat task ends with it.
    #[cfg(test)]
    fn crash(&mut self) {
        self.session.abort();
        self.feed_task.abort();
        self.storage_task.abort();
        self.exec.shutdown();
        tracing::warn!("collab live exchange crashed (test hook)");
    }

    async fn finish(mut self, exit: Exit) -> RunEnd {
        let (stop, remount, restart) = match exit {
            Exit::Stop(stop) => (stop, false, false),
            Exit::Remount => (None, true, false),
            Exit::Restart => (None, false, true),
        };
        let (stop, remount, restart) = (stop, remount, restart);
        let sign_out = stop.as_ref().is_some_and(|(s, _)| *s);
        self.shared.running.store(false, Ordering::SeqCst);
        if remount || restart {
            // Final fix A-I3: nothing is advertised as served while no
            // runtime runs — the next one publishes its own map.
            self.shared.set_serving(BTreeMap::new());
        }
        // The session leaves presence (≤ LEAVE_TIMEOUT) while the rest stops.
        let _ = self.stop_tx.send(true);
        self.exec.shutdown();
        self.feed_task.abort();
        self.storage_task.abort();
        // The workers and the session are waited for AT ONCE: the whole stop
        // takes at most `LEAVE_TIMEOUT + SESSION_MARGIN`, inside STOP_BOUND
        // with the dispatch margin to spare (see STOP_BOUND).
        let (feed, storage, session) = (
            &mut self.feed_task,
            &mut self.storage_task,
            &mut self.session,
        );
        let feed = async {
            match tokio::time::timeout(WORKER_STOP, feed).await {
                Ok(Err(e)) if e.is_panic() => tracing::error!(error = %e, "feed worker panicked"),
                Ok(_) => {}
                Err(_) => tracing::warn!(
                    duration_ms = WORKER_STOP.as_millis() as u64,
                    "feed worker did not end in time"
                ),
            }
        };
        let storage = async {
            match tokio::time::timeout(WORKER_STOP, storage).await {
                Ok(Err(e)) if e.is_panic() => tracing::error!(error = %e, "storage task panicked"),
                Ok(_) => {}
                Err(_) => tracing::warn!(
                    duration_ms = WORKER_STOP.as_millis() as u64,
                    "storage task did not end in time"
                ),
            }
        };
        let session_bound = crate::api::collab_live::session::LEAVE_TIMEOUT + SESSION_MARGIN;
        let session = async {
            match tokio::time::timeout(session_bound, &mut *session).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!(error = %e, "event session task panicked"),
                Err(_) => {
                    tracing::warn!(
                        duration_ms = session_bound.as_millis() as u64,
                        "event session did not end in time; aborted"
                    );
                    session.abort();
                }
            }
        };
        tokio::join!(feed, storage, session);
        // Only while the oracle is still this runtime's (final fix A-M1): a
        // runtime armed again during this stop installed its own already.
        let next = if sign_out {
            None
        } else {
            // Without a runtime the catalog-backed oracle serves (Task 15 R4).
            crate::api::collab_live::serve_oracle::catalog_oracle(&self.ctx)
        };
        self.node.replace_collab_serve_oracle_if(&self.oracle, next);
        self.shared.set_state(LiveState::Off, None);
        if let Some((_, done)) = stop {
            let _ = done.send(());
        }
        if remount {
            RunEnd::Remount
        } else if restart {
            RunEnd::Restart
        } else {
            RunEnd::Stopped
        }
    }

    // ── feed results ────────────────────────────────────────────────────

    fn on_feed_out(&mut self, out: FeedOut) {
        match out {
            FeedOut::Effects(effects) => {
                for e in effects {
                    self.on_effect(e);
                }
            }
            FeedOut::NewSession => {
                if !self.refused.is_empty() {
                    tracing::info!(
                        count = self.refused.len(),
                        "a new session retries the refused projects"
                    );
                    for p in std::mem::take(&mut self.refused) {
                        self.exec.dirty.insert(p);
                    }
                    self.serving_dirty = true;
                }
            }
            FeedOut::Refused { project_id, error } => self.refuse(project_id, &error),
        }
    }

    /// Spec §4.6: a 403 marks that project refused — nothing is fetched or
    /// served for it until the next session — and the session goes on for
    /// the others.
    fn refuse(&mut self, project_id: String, error: &str) {
        tracing::error!(project_id = %project_id, error, refused = true, "the hub refused this project");
        if self.refused.insert(project_id.clone()) {
            self.exec.dirty.insert(project_id);
            self.serving_dirty = true;
        }
    }

    /// The provider derivation's inputs: the shared maps (a short read
    /// lock) and the presence copy.
    fn with_derive<T>(&mut self, f: impl FnOnce(&mut Executor, &Derive<'_>) -> T) -> T {
        let presence = self.shared.presence_copy();
        let maps = self.maps.read().unwrap_or_else(|p| {
            tracing::error!("holder maps lock poisoned; read as left");
            p.into_inner()
        });
        let derive = Derive {
            maps: &maps,
            presence: &presence,
            me: &self.me,
        };
        f(&mut self.exec, &derive)
    }

    fn on_effect(&mut self, e: FeedEffect) {
        match e {
            FeedEffect::NeedSetChanged(p) | FeedEffect::ProjectJoined(p) => {
                // A manifest apply: new rows start `wanted` whatever the
                // policy — the scope is re-derived (Task 15 R1, the T9
                // carry). The feed worker started the claim flush wait.
                self.policy_dirty.insert(p.clone());
                self.exec.dirty.insert(p);
                self.serving_dirty = true;
            }
            FeedEffect::ProvidersChanged(p) => {
                self.with_derive(|exec, d| exec.refresh_providers(&p, d));
                let now = Instant::now();
                self.peer_burst.note(&p, now);
                self.flush_peer_bursts(now);
            }
            FeedEffect::MembersChanged(p) => {
                // I11: a device that may no longer connect is closed on both
                // sides; the policy is re-derived (a caps change).
                let inbound = self.node.close_collab_connections_not_admitted();
                let outbound = self.exec.close_not_admitted();
                if inbound + outbound > 0 {
                    tracing::info!(project_id = %p, count = inbound + outbound, "connections of devices no longer admitted closed");
                }
                self.policy_dirty.insert(p.clone());
                self.exec.dirty.insert(p.clone());
                self.with_derive(|exec, d| exec.refresh_providers(&p, d));
                // A member add/remove is a providers change too (the
                // Members tab's roster): a member added while offline, or
                // removed, would otherwise never nudge an open tab. Same
                // throttle as ProvidersChanged.
                let now = Instant::now();
                self.peer_burst.note(&p, now);
                self.flush_peer_bursts(now);
                self.attention.insert(p);
            }
            FeedEffect::ProjectGone(p) => {
                self.exec.project_gone(&p);
                self.serving_dirty = true;
            }
            FeedEffect::EpochChanged => {
                // Every need set AND every provider list in one pass (a
                // reused version number must not keep an old-epoch list).
                let fetching = self.storage_state.fetching();
                let projects: Vec<(String, bool)> = self
                    .live_projects()
                    .into_iter()
                    .map(|p| {
                        let refused = self.refused.contains(&p);
                        (p, refused)
                    })
                    .collect();
                self.with_derive(|exec, d| {
                    for (p, refused) in &projects {
                        exec.refresh_need(p, fetching, *refused, d, true);
                    }
                });
                // Every provider list was refreshed above — a reused epoch
                // affects every live project, not just one, so every one of
                // them is noted (same throttle as ProvidersChanged).
                let now = Instant::now();
                for (p, _) in &projects {
                    self.peer_burst.note(p, now);
                }
                self.flush_peer_bursts(now);
                self.serving_dirty = true;
            }
        }
    }

    /// Re-read every dirty need set, publish the serving map and the
    /// attention changes.
    fn flush_dirty(&mut self) {
        // Final fix A-I2: the re-derive is a catalog write (IMMEDIATE, up to
        // the busy timeout under another writer) — it runs on a blocking
        // thread and its result comes back to the loop; the project's need
        // set is re-read once it did.
        for p in std::mem::take(&mut self.policy_dirty) {
            if self.policy_pending.contains(&p) {
                self.policy_again.insert(p);
                continue;
            }
            self.policy_pending.insert(p.clone());
            let (ctx, tx) = (Arc::clone(&self.ctx), self.policy_tx.clone());
            tokio::task::spawn_blocking(move || {
                let res = crate::api::collab_live::storage_task::apply_policy(&ctx, &p);
                if tx.send((p, res)).is_err() {
                    tracing::debug!("the live runtime is gone; a replication scope result dropped");
                }
            });
        }
        if !self.exec.dirty.is_empty() {
            let dirty: Vec<(String, bool)> = std::mem::take(&mut self.exec.dirty)
                .into_iter()
                // Re-read once its re-derive is back (a scope move may take
                // rows out of the need set): the result re-dirties it.
                .filter(|p| !self.policy_pending.contains(p))
                .map(|p| {
                    let refused = self.refused.contains(&p);
                    (p, refused)
                })
                .collect();
            let fetching = self.storage_state.fetching();
            self.with_derive(|exec, d| {
                for (p, refused) in &dirty {
                    exec.refresh_need(p, fetching, *refused, d, false);
                }
            });
        }
        if self.serving_dirty {
            self.serving_dirty = false;
            let serving = self.storage_state.serving() && self.node.collab_store().is_some();
            let map: BTreeMap<String, bool> = self
                .live_projects()
                .into_iter()
                .map(|p| {
                    let on = serving && !self.refused.contains(&p);
                    (p, on)
                })
                .collect();
            self.shared.set_serving(map);
        }
        for p in std::mem::take(&mut self.attention) {
            self.emit(
                COLLAB_ATTENTION_EVENT,
                &CollabAttentionChanged { project_id: p },
            );
        }
    }

    /// A re-derive came back (final fix A-I2). A failure is retried after
    /// its back-off — never dropped; the need set is re-read either way (a
    /// failed re-derive keeps the old scope meanwhile).
    fn on_policy_applied(&mut self, p: String, res: Result<usize, ApiError>) {
        self.policy_pending.remove(&p);
        match res {
            Ok(moved) => {
                if self.policy_retry.remove(&p).is_some() {
                    tracing::info!(project_id = %p, count = moved, "replication scope re-derived after a retry");
                }
                if moved > 0 {
                    // Scope moves are claim changes and attention changes.
                    self.note_append(&p);
                    self.attention.insert(p.clone());
                }
            }
            Err(ApiError::NotFound(e)) => {
                self.policy_retry.remove(&p);
                tracing::warn!(project_id = %p, error = %e, "replication scope not re-derived: the project is gone");
            }
            Err(e) => {
                let entry = self.policy_retry.entry(p.clone()).or_insert_with(|| {
                    (Instant::now(), crate::collab::live::backoff::Backoff::new())
                });
                let delay = entry.1.next_delay();
                entry.0 = Instant::now() + delay;
                tracing::error!(project_id = %p, error = %e, retry_in_ms = delay.as_millis() as u64, "the replication scope was not re-derived after a manifest or membership change; retried");
            }
        }
        if self.policy_again.remove(&p) {
            self.policy_dirty.insert(p.clone());
        }
        self.exec.dirty.insert(p);
    }

    // ── storage ─────────────────────────────────────────────────────────

    fn on_storage_events(&mut self, evs: Vec<StorageEvent>) {
        for ev in evs {
            match ev {
                StorageEvent::StateChanged {
                    project_id,
                    from,
                    to,
                    ..
                } => {
                    self.note_append(&project_id);
                    let listed = |s: LocalState| {
                        matches!(
                            s,
                            LocalState::Quarantined
                                | LocalState::AwaitingChoice
                                | LocalState::NotKept
                                | LocalState::Missing
                        )
                    };
                    if listed(from) || listed(to) {
                        self.attention.insert(project_id.clone());
                    }
                    self.exec.dirty.insert(project_id);
                }
                StorageEvent::DeletionChoice {
                    count,
                    project_ids,
                    batch_id,
                } => {
                    for p in &project_ids {
                        self.attention.insert(p.clone());
                    }
                    self.emit(
                        COLLAB_DELETION_CHOICE_EVENT,
                        &CollabDeletionChoice::new(count, project_ids, batch_id),
                    );
                }
                StorageEvent::FrameLost {
                    project_id,
                    frame_uuid,
                    file_name,
                    previous_path,
                } => {
                    self.attention.insert(project_id.clone());
                    self.emit(
                        COLLAB_FRAME_LOST_EVENT,
                        &CollabFrameLost {
                            project_id,
                            frame_uuid,
                            file_name,
                            in_previous_folder: previous_path.is_some(),
                            previous_path,
                        },
                    );
                }
                StorageEvent::Quarantined {
                    project_id,
                    frame_uuid,
                    file_name,
                } => {
                    self.attention.insert(project_id.clone());
                    self.exec.dirty.insert(project_id.clone());
                    self.exec.forget_partial(&project_id, &frame_uuid);
                    self.emit(
                        COLLAB_FRAME_CHANGED_EVENT,
                        &CollabFrameChanged {
                            project_id,
                            frame_uuid,
                            file_name,
                        },
                    );
                }
                StorageEvent::Availability(state) => {
                    let was = self.storage_state.fetching();
                    self.storage_state = state;
                    if was != self.storage_state.fetching() {
                        self.exec.storage(self.storage_state.fetching());
                        for p in self.live_projects() {
                            self.exec.dirty.insert(p);
                        }
                    }
                    self.publish_storage();
                }
                StorageEvent::WatcherDegraded(_) => self.publish_storage(),
            }
        }
    }

    fn apply_notes(&mut self) {
        let now = Instant::now();
        for note in self.exec.take_notes() {
            match note {
                Note::Landed(p) => {
                    self.note_append(&p);
                    self.burst
                        .counts
                        .entry(p.clone())
                        .or_insert_with(|| blank(&p))
                        .landed += 1;
                }
                Note::Failed(p) => {
                    self.burst
                        .counts
                        .entry(p.clone())
                        .or_insert_with(|| blank(&p))
                        .failed += 1;
                }
                Note::AwaitingGc(p) => {
                    self.burst
                        .counts
                        .entry(p.clone())
                        .or_insert_with(|| blank(&p))
                        .awaiting_gc += 1;
                }
                Note::StorageCheck(p, u) => self.to_storage(StorageWork::Check(p, u)),
            }
        }
        self.flush_bursts(now);
    }

    /// Emit each project's landed burst at most once per [`LANDED_BURST`].
    fn flush_bursts(&mut self, now: Instant) {
        let due: Vec<String> = self
            .burst
            .counts
            .keys()
            .filter(|p| {
                self.burst
                    .last
                    .get(*p)
                    .is_none_or(|t| now >= *t + LANDED_BURST)
            })
            .cloned()
            .collect();
        for p in due {
            if let Some(counts) = self.burst.counts.remove(&p) {
                self.emit(COLLAB_FRAMES_LANDED_EVENT, &counts);
                self.burst.last.insert(p, now);
            }
        }
    }

    /// Emit each project's due `collab-peers-changed` (at most one per
    /// project per [`LANDED_BURST`] — see [`PeerBurst`]).
    fn flush_peer_bursts(&mut self, now: Instant) {
        for p in self.peer_burst.due(now) {
            self.emit(
                COLLAB_PEERS_CHANGED_EVENT,
                &CollabPeersChanged { project_id: p },
            );
        }
    }

    // ── timers and reconciliation ───────────────────────────────────────

    /// The loop's own due work: the GC probe, the core's `Tick`, the
    /// landed bursts. (The holder side's and the storage engine's timers
    /// run on their workers.)
    async fn on_timers(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .policy_retry
            .iter()
            .filter(|(p, (at, _))| now >= *at && !self.policy_pending.contains(*p))
            .map(|(p, _)| p.clone())
            .collect();
        for p in due {
            if let Some((at, _)) = self.policy_retry.get_mut(&p) {
                // Not due again until its result is back (and backs off).
                *at = now + crate::collab::live::backoff::BACKOFF_CAP;
            }
            self.policy_dirty.insert(p);
        }
        if now >= self.next_gc_probe {
            self.next_gc_probe = now + self.gc_probe;
            self.exec.gc_probe().await;
        }
        if self.exec.tick_due() {
            self.exec.step(Input::Tick);
        }
        self.flush_bursts(now);
        self.flush_peer_bursts(now);
        self.flush_exchange(now);
    }

    /// Spec 2026-09-29 §6.4: at most once a second while something moves,
    /// then one quiet payload per project. No catalog access here: flows
    /// come from the in-memory meter, "to go" from the scheduler's want set.
    fn flush_exchange(&mut self, now: Instant) {
        if now < self.next_exchange {
            return;
        }
        self.next_exchange = now + PROGRESS_PERIOD;
        let flows = self.meter.snapshot(now);
        let moving: BTreeSet<String> = flows
            .iter()
            .filter(|f| f.moving)
            .map(|f| f.project_id.clone())
            .collect();
        let Some(emit) = self.exchange_gate.poll(now, moving) else {
            return;
        };
        let listed: Vec<String> = emit
            .projects
            .iter()
            .chain(emit.quiet.iter())
            .cloned()
            .collect();
        // "To go" straight from the scheduler (the snapshot's shared copy is
        // published by `Executor::step`, never here).
        let payload = CollabExchangeProgress {
            projects: project_flows(&flows, &listed, &|p| self.exec.need_len(p)),
        };
        self.emit(COLLAB_EXCHANGE_PROGRESS_EVENT, &payload);
    }

    /// Sync now's reconciliation (P26): the refused projects are retried, a
    /// digest check per project and a stat sweep (both on their workers),
    /// every need set re-read (the stream itself was reopened by the
    /// caller, so `hello` catches every project up).
    fn reconcile(&mut self) {
        if !self.refused.is_empty() {
            self.refused.clear();
            self.serving_dirty = true;
        }
        let projects = self.live_projects();
        self.to_feed(FeedWork::DigestAll(projects.clone()));
        self.to_storage(StorageWork::Sweep);
        for p in projects {
            self.exec.dirty.insert(p);
        }
        tracing::info!("sync now reconciliation queued");
    }
}

fn blank(project_id: &str) -> CollabFramesLanded {
    CollabFramesLanded {
        project_id: project_id.to_string(),
        landed: 0,
        failed: 0,
        awaiting_gc: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fix round 2 (I-1): a connection's `hello` sets the session id; one
    /// read by an older connection never replaces a newer connection's.
    #[test]
    fn a_stale_hello_never_replaces_a_newer_session_id() {
        let (_tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let shared = Shared::new(Arc::new(ctx), None);
        let old = shared.begin_connection();
        shared.set_session_for(old, "s-old".into());
        assert_eq!(shared.session_signal().borrow().as_deref(), Some("s-old"));
        let new = shared.begin_connection();
        assert_eq!(
            *shared.session_signal().borrow(),
            None,
            "cleared per connection"
        );
        shared.set_session_for(new, "s-new".into());
        shared.set_session_for(old, "s-stale".into());
        assert_eq!(shared.session_signal().borrow().as_deref(), Some("s-new"));
    }

    /// Three `ProvidersChanged` for one project within 200ms produce exactly
    /// one immediate emission; the two coalesced inside the window are never
    /// dropped — polling again ~1.2s after the first (as `on_timers` would)
    /// flushes them as a second, distinct emission.
    #[test]
    fn peer_burst_coalesces_rapid_changes_and_flushes_at_window_end() {
        let mut b = PeerBurst::default();
        let t0 = Instant::now();
        // Three ProvidersChanged for one project within 200ms, each followed
        // by an immediate flush attempt (mirrors the runtime's
        // on_effect → flush_peer_bursts pattern).
        b.note("p1", t0);
        assert_eq!(
            b.due(t0),
            vec!["p1".to_string()],
            "the first change fires immediately"
        );
        b.note("p1", t0 + Duration::from_millis(50));
        assert!(
            b.due(t0 + Duration::from_millis(50)).is_empty(),
            "coalesced inside the throttle window"
        );
        // Cheap pin: the coalesced note schedules exactly the window's end —
        // this is what `Runtime::deadline()` folds in via `next_deadline()`
        // to actually wake the loop and flush it.
        assert_eq!(
            b.next_deadline(),
            Some(t0 + LANDED_BURST),
            "the coalesced change is scheduled for exactly the window's end"
        );
        b.note("p1", t0 + Duration::from_millis(200));
        assert!(
            b.due(t0 + Duration::from_millis(200)).is_empty(),
            "still inside the throttle window — exactly one event so far"
        );
        // A second change ~1.2s later: the coalesced note is never dropped —
        // it flushes as a second event once the window ends.
        let t1 = t0 + Duration::from_millis(1200);
        assert_eq!(
            b.due(t1),
            vec!["p1".to_string()],
            "the coalesced change flushed as a second event"
        );
        assert!(b.due(t1).is_empty(), "nothing left pending after the flush");
    }

    /// After a quiet period well past the throttle window, a genuinely new
    /// change fires immediately again — the throttle never gets stuck.
    #[test]
    fn peer_burst_fires_again_after_a_quiet_period() {
        let mut b = PeerBurst::default();
        let t0 = Instant::now();
        b.note("p1", t0);
        assert_eq!(b.due(t0), vec!["p1".to_string()]);
        let t1 = t0 + Duration::from_secs(3);
        b.note("p1", t1);
        assert_eq!(b.due(t1), vec!["p1".to_string()]);
    }

    /// Two different projects are throttled independently: a change on one
    /// mid-window never holds back a first change on the other.
    #[test]
    fn peer_burst_tracks_projects_independently() {
        let mut b = PeerBurst::default();
        let t0 = Instant::now();
        b.note("p1", t0);
        assert_eq!(b.due(t0), vec!["p1".to_string()]);
        // p1 is now inside its throttle window...
        b.note("p1", t0 + Duration::from_millis(50));
        assert!(b.due(t0 + Duration::from_millis(50)).is_empty());
        // ...but p2's own first-ever change fires immediately regardless.
        b.note("p2", t0 + Duration::from_millis(50));
        assert_eq!(
            b.due(t0 + Duration::from_millis(50)),
            vec!["p2".to_string()],
            "p2's own first change is not held back by p1's window"
        );
    }
}
