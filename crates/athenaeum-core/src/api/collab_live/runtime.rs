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
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use crate::api::collab_exchange::{CollabFramesLanded, COLLAB_FRAMES_LANDED_EVENT};
use crate::api::collab_live::executor::{Derive, ExecEnv, Executor, Note};
use crate::api::collab_live::feed::{FeedApplier, FeedEffect};
use crate::api::collab_live::holdings::{member_devices, Holdings};
use crate::api::collab_live::storage_task::{
    HolderView, StorageEngine, StorageEvent, StorageTimings,
};
use crate::api::collab_live::{
    CollabAttentionChanged, CollabDeletionChoice, CollabFrameChanged, CollabFrameLost,
    CollabLiveStatus, LiveState, StorageStateView, COLLAB_ATTENTION_EVENT,
    COLLAB_DELETION_CHOICE_EVENT, COLLAB_FRAME_CHANGED_EVENT, COLLAB_FRAME_LOST_EVENT,
    COLLAB_LIVE_STATUS_EVENT,
};
use crate::api::{db, ApiError};
use crate::collab::hub_client::CollabClient;
use crate::collab::live::holders::{redundancy, FrameRef, Redundancy};
use crate::collab::live::presence::PresenceBook;
use crate::collab::live::wire::LiveEvent;
use crate::collab::scheduler::core::Input;
use crate::collab::storage::marker::{StoreGuard, StoreState, UnavailableReason};
use crate::db::collab_frames::LocalState;
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;
use crate::sharing::iroh::node::SharedIrohNode;
use crate::sync::receiver::InboundControl;

/// `shutdown` / `on_sign_out` wait at most this long for the runtime to
/// leave presence and stop (P28).
pub const STOP_BOUND: Duration = Duration::from_secs(2);
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
const EVENT_QUEUE: usize = 256;

// ── the shared state (runtime ↔ session ↔ handle) ───────────────────────

/// State the runtime, its session and the command surface share.
pub(crate) struct Shared {
    pub ctx: Arc<ServiceContext>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    status: watch::Sender<CollabLiveStatus>,
    reconnect: watch::Sender<u64>,
    session_id: watch::Sender<Option<String>>,
    serving: watch::Sender<BTreeMap<String, bool>>,
    credentials: Mutex<Option<(String, String)>>,
    /// A copy of the feed's presence book, for [`holder_view`].
    presence: RwLock<PresenceBook>,
    me: RwLock<Option<String>>,
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
            serving: watch::channel(BTreeMap::new()).0,
            credentials: Mutex::new(None),
            presence: RwLock::new(PresenceBook::default()),
            me: RwLock::new(None),
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

    fn credentials(&self) -> Option<(String, String)> {
        self.credentials
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// `DELETE /me/presence` for the current session, bounded (P28).
    pub(crate) async fn leave(&self, hub_url: &str) {
        let Some(id) = self.session_id.borrow().clone() else {
            return;
        };
        let client = match CollabClient::new(hub_url.to_string()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "presence leave client could not be built");
                return;
            }
        };
        match tokio::time::timeout(
            crate::api::collab_live::session::LEAVE_TIMEOUT,
            client.presence_leave(&id),
        )
        .await
        {
            Ok(Ok(())) => tracing::info!("left presence"),
            Ok(Err(e)) => {
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
    Stop {
        sign_out: bool,
        done: oneshot::Sender<()>,
    },
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
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            timings: StorageTimings::default(),
            ready_poll: READY_POLL,
            beat: crate::collab::live::presence::BEAT_INTERVAL,
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
/// stat sweep).
pub fn sync_now(ctx: &ServiceContext) -> Result<(), ApiError> {
    let Some(shared) = handle_shared(ctx) else {
        let e = ApiError::Invalid(
            "The live exchange is not running (signed out, or no Collaboration folder yet).".into(),
        );
        tracing::warn!(error = %e, "sync now refused");
        return Err(e);
    };
    tracing::info!("collab sync now requested");
    crate::collab::live::backoff::reset_all();
    shared.reconnect_now();
    if !send(ctx, LiveCommand::Reconcile) {
        let e = ApiError::Internal("the live exchange stopped".into());
        tracing::error!(error = %e, "sync now: reconciliation not queued");
        return Err(e);
    }
    Ok(())
}

/// A command changed a project's local state: its need set is re-read.
pub fn notify_local_change(ctx: &ServiceContext, project_id: &str) {
    if !send(ctx, LiveCommand::LocalChange(project_id.to_string())) {
        tracing::debug!(
            project_id,
            "no live exchange runs; the change applies when it starts"
        );
    }
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

/// Who else holds a frame's current version, for the user-facing commands
/// (Task 16: the last-copy warning): the persisted holder map and the live
/// presence. `None` when no live exchange runs.
#[allow(dead_code)] // read by the Task 16 commands (the last-copy warning)
pub(crate) fn holder_view(ctx: &ServiceContext) -> Option<Arc<dyn HolderView>> {
    let shared = handle_shared(ctx)?;
    let me = shared.me.read().ok()?.clone()?;
    let presence = shared.presence.read().ok()?.clone();
    Some(Arc::new(PersistedView {
        ctx: Arc::clone(&shared.ctx),
        presence,
        me,
    }))
}

/// The holder view over the persisted holder map (see [`holder_view`]).
struct PersistedView {
    ctx: Arc<ServiceContext>,
    presence: PresenceBook,
    me: String,
}

impl HolderView for PersistedView {
    fn other_holders(&self, project_id: &str, frame_uuid: &str) -> Redundancy {
        let read = db(&self.ctx).and_then(|d| {
            let conn = d.conn();
            let (devices, claims) = crate::db::collab_live::load_holders(&conn, project_id)?;
            let row = crate::db::collab_frames::get(&conn, project_id, frame_uuid)?;
            let project = crate::db::collab::get_project(&conn, project_id)?;
            Ok((devices, claims, row, project))
        });
        match read {
            Ok((devices, claims, Some(row), Some(project))) => {
                let map =
                    crate::collab::live::holders::ProjectHolders::from_rows(&devices, &claims);
                redundancy_of(&map, &self.presence, &project, &self.me, &row)
            }
            Ok(_) => Redundancy::default(),
            Err(e) => {
                tracing::warn!(project_id, frame_uuid, error = %e, "holders could not be read; counted none");
                Redundancy::default()
            }
        }
    }
}

fn redundancy_of(
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

/// The runtime's own holder view: the in-memory holder maps.
struct LiveView<'a> {
    ctx: &'a ServiceContext,
    holdings: Option<&'a Holdings>,
    presence: &'a PresenceBook,
    me: &'a str,
}

impl HolderView for LiveView<'_> {
    fn other_holders(&self, project_id: &str, frame_uuid: &str) -> Redundancy {
        let Some(map) = self.holdings.and_then(|h| h.map(project_id)) else {
            return Redundancy::default();
        };
        let read = db(self.ctx).and_then(|d| {
            let conn = d.conn();
            Ok((
                crate::db::collab_frames::get(&conn, project_id, frame_uuid)?,
                crate::db::collab::get_project(&conn, project_id)?,
            ))
        });
        match read {
            Ok((Some(row), Some(project))) => {
                redundancy_of(map, self.presence, &project, self.me, &row)
            }
            Ok(_) => Redundancy::default(),
            Err(e) => {
                tracing::warn!(project_id, frame_uuid, error = %e, "holders could not be read; counted none");
                Redundancy::default()
            }
        }
    }
}

// ── the runtime ─────────────────────────────────────────────────────────

/// What the runtime needs before it can run.
struct Ready {
    node: Arc<SharedIrohNode>,
    store: iroh_blobs::api::Store,
    root: PathBuf,
    control: Arc<InboundControl>,
}

async fn ready(ctx: &ServiceContext, gate: &GateSource) -> Result<Ready, &'static str> {
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
    let store = match node.collab_store() {
        Some(s) => s,
        None => match crate::api::collab_exchange::ensure_collab_store(ctx).await {
            Some(s) => s,
            None => return Err("collab store not mounted"),
        },
    };
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
    })
}

/// The armed task: wait until the runtime can run, then run it.
async fn supervise(
    shared: Arc<Shared>,
    mut commands: mpsc::UnboundedReceiver<LiveCommand>,
    gate: GateSource,
    cfg: LiveConfig,
) {
    let ctx = Arc::clone(&shared.ctx);
    loop {
        match ready(&ctx, &gate).await {
            Ok(r) => match Runtime::start(Arc::clone(&shared), r, cfg).await {
                Ok(rt) => {
                    rt.run(commands).await;
                    return;
                }
                Err(e) => {
                    tracing::error!(error = %e, "collab live exchange could not start; retried");
                }
            },
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
        tokio::select! {
            _ = tokio::time::sleep(cfg.ready_poll) => {}
            cmd = commands.recv() => match cmd {
                None => return,
                Some(LiveCommand::Stop { done, .. }) => {
                    let _ = done.send(());
                    return;
                }
                Some(_) => {}
            },
        }
    }
}

/// Frames landed / failed / parked since the last burst, per project.
#[derive(Default)]
struct Burst {
    counts: HashMap<String, CollabFramesLanded>,
    last: HashMap<String, Instant>,
}

struct Runtime {
    shared: Arc<Shared>,
    ctx: Arc<ServiceContext>,
    node: Arc<SharedIrohNode>,
    me: String,
    feed: Option<FeedApplier>,
    holdings: Option<Holdings>,
    creds: Option<(String, String)>,
    storage: StorageEngine,
    storage_state: StoreState,
    exec: Executor,
    events_rx: mpsc::Receiver<LiveEvent>,
    events_open: bool,
    checks_rx: mpsc::UnboundedReceiver<(String, String)>,
    reset: watch::Receiver<u64>,
    stop_tx: watch::Sender<bool>,
    session: tokio::task::JoinHandle<()>,
    refused: HashSet<String>,
    next_gc_probe: Instant,
    burst: Burst,
    serving_dirty: bool,
    attention: BTreeSet<String>,
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
        let storage = StorageEngine::start_with(
            Arc::clone(&ctx),
            Arc::clone(&r.node),
            Arc::clone(&guard),
            cfg.timings,
        );
        let (checks_tx, checks_rx) = mpsc::unbounded_channel();
        r.node.set_collab_serve_oracle(Some(Arc::new(
            crate::api::collab_live::serve_oracle::DbServeOracle::new(
                Arc::clone(&ctx),
                Arc::clone(&guard),
                checks_tx,
            ),
        )));
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
        let exec = Executor::new(
            ExecEnv {
                ctx: Arc::clone(&ctx),
                node: Arc::clone(&r.node),
                store: r.store,
                root: r.root,
                guard,
                control: r.control,
            },
            receive,
            seed,
        );
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let (stop_tx, stop_rx) = watch::channel(false);
        let session = tokio::spawn(crate::api::collab_live::session::run_session(
            Arc::clone(&shared),
            Arc::clone(&r.node),
            events_tx,
            stop_rx,
            cfg.beat,
        ));
        let mut rt = Self {
            ctx,
            node: r.node,
            me,
            feed: None,
            holdings: None,
            creds: None,
            storage,
            storage_state: state.clone(),
            exec,
            events_rx,
            events_open: true,
            checks_rx,
            reset: crate::collab::live::backoff::reset_signal(),
            stop_tx,
            session,
            refused: HashSet::new(),
            next_gc_probe: Instant::now() + GC_PROBE_EVERY,
            burst: Burst::default(),
            serving_dirty: true,
            attention: BTreeSet::new(),
            shared,
        };
        rt.publish_storage();
        rt.ensure_feed(crate::api::account::hub_credentials(&rt.ctx).ok().flatten());
        rt.exec.storage(state.fetching());
        for pid in rt.live_projects() {
            rt.exec.dirty.insert(pid);
        }
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

    /// Build the feed applier and the holder side for `creds` (again when
    /// the account's token changed).
    fn ensure_feed(&mut self, creds: Option<(String, String)>) {
        let Some(creds) = creds else {
            return;
        };
        if self.creds.as_ref() == Some(&creds) && self.feed.is_some() {
            return;
        }
        let (hub, token) = creds.clone();
        let clients = CollabClient::new(hub.clone()).and_then(|a| Ok((a, CollabClient::new(hub)?)));
        let (feed_client, holdings_client) = match clients {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "hub client could not be built; the feed waits");
                return;
            }
        };
        match Holdings::load(
            Arc::clone(&self.ctx),
            holdings_client,
            token.clone(),
            self.me.clone(),
        ) {
            Ok(h) => self.holdings = Some(h),
            Err(e) => {
                tracing::error!(error = %e, "holder maps could not be loaded; the feed waits");
                return;
            }
        }
        self.feed = Some(FeedApplier::new(
            Arc::clone(&self.ctx),
            feed_client,
            token,
            self.shared.emitter.clone(),
        ));
        self.creds = Some(creds);
    }

    fn emit<T: serde::Serialize>(&self, name: &str, payload: &T) {
        if let Some(em) = &self.shared.emitter {
            emit_event(em.as_ref(), name, payload);
        }
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
        self.shared
            .set_watcher(self.storage.degraded(), self.storage.network());
        self.serving_dirty = true;
    }

    fn deadline(&self) -> Instant {
        let mut d = self.storage.next_deadline().min(self.next_gc_probe);
        if let Some(h) = self.holdings.as_ref().and_then(Holdings::next_deadline) {
            d = d.min(h);
        }
        if let Some(w) = self.exec.next_wake() {
            d = d.min(w);
        }
        for pid in self.burst.counts.keys() {
            if let Some(last) = self.burst.last.get(pid) {
                d = d.min(*last + LANDED_BURST);
            }
        }
        d
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<LiveCommand>) {
        let mut stop: Option<(bool, oneshot::Sender<()>)> = None;
        loop {
            self.flush_dirty();
            let deadline = self.deadline();
            tokio::select! {
                biased;
                cmd = commands.recv() => match cmd {
                    None => break,
                    Some(LiveCommand::Stop { sign_out, done }) => {
                        stop = Some((sign_out, done));
                        break;
                    }
                    Some(LiveCommand::Reconcile) => self.reconcile().await,
                    Some(LiveCommand::LocalChange(p)) => {
                        self.exec.dirty.insert(p.clone());
                        self.attention.insert(p);
                    }
                    Some(LiveCommand::SetStreams(n)) => self.exec.set_slots(n),
                },
                ev = self.events_rx.recv(), if self.events_open => match ev {
                    Some(ev) => self.on_event(ev).await,
                    None => self.events_open = false,
                },
                changed = self.reset.changed() => {
                    if changed.is_ok() {
                        self.exec.step(Input::ClearBackoffs);
                    }
                }
                ev = self.exec.events_rx.recv() => {
                    if let Some(ev) = ev {
                        self.exec.on_event(ev).await;
                        self.apply_notes().await;
                    }
                }
                done = self.exec.done_rx.recv() => {
                    if let Some((name, outcome)) = done {
                        self.exec.on_done(name, outcome).await;
                        self.apply_notes().await;
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
                changed = self.exec.yield_rx.changed() => {
                    if changed.is_ok() {
                        self.exec.on_yield_changed();
                    }
                }
                sig = self.storage.recv_signal() => {
                    if let Some(sig) = sig {
                        self.storage.on_signal(sig, Instant::now());
                    }
                }
                check = self.checks_rx.recv() => {
                    if let Some((p, u)) = check {
                        let evs = self.storage.local_check(&p, &u).await;
                        self.on_storage_events(evs);
                    }
                }
                _ = tokio::time::sleep_until(deadline.into()) => self.on_timers().await,
            }
        }
        self.finish(stop).await;
    }

    async fn finish(mut self, stop: Option<(bool, oneshot::Sender<()>)>) {
        let sign_out = stop.as_ref().is_some_and(|(s, _)| *s);
        let _ = self.stop_tx.send(true);
        self.exec.shutdown();
        // The session leaves presence (≤ LEAVE_TIMEOUT) and ends.
        match tokio::time::timeout(STOP_BOUND, &mut self.session).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(error = %e, "event session task panicked"),
            Err(_) => {
                tracing::warn!("event session did not end in time; aborted");
                self.session.abort();
            }
        }
        if sign_out {
            self.node.set_collab_serve_oracle(None);
        } else {
            // Without a runtime the catalog-backed oracle serves (Task 15 R4).
            crate::api::collab_live::serve_oracle::install_catalog_oracle(&self.ctx, &self.node);
        }
        self.shared.set_state(LiveState::Off, None);
        if let Some((_, done)) = stop {
            let _ = done.send(());
        }
    }

    // ── feed events ─────────────────────────────────────────────────────

    async fn on_event(&mut self, ev: LiveEvent) {
        let project = match &ev {
            LiveEvent::Project(p) => Some(p.project_id.clone()),
            LiveEvent::Holders(h) => Some(h.project_id.clone()),
            LiveEvent::Resync(r) => Some(r.project_id.clone()),
            LiveEvent::Account(a) => Some(a.project_id.clone()),
            _ => None,
        };
        let presence_changed = matches!(ev, LiveEvent::Hello(_) | LiveEvent::Presence(_));
        if let LiveEvent::Hello(h) = &ev {
            self.shared.set_session_id(Some(h.session_id.clone()));
            self.ensure_feed(self.shared.credentials());
            if !self.refused.is_empty() {
                tracing::info!(
                    count = self.refused.len(),
                    "a new session retries the refused projects"
                );
                self.refused.clear();
                self.serving_dirty = true;
            }
        }
        let (Some(feed), Some(holdings)) = (self.feed.as_mut(), self.holdings.as_mut()) else {
            tracing::debug!(
                "feed event before the account was loaded; skipped (the next hello catches up)"
            );
            return;
        };
        let applied = feed.apply(ev, holdings).await;
        if presence_changed {
            if let Ok(mut p) = self.shared.presence.write() {
                *p = feed.presence.clone();
            }
        }
        match applied {
            Ok(effects) => {
                for e in effects {
                    self.on_effect(e);
                }
            }
            Err(ApiError::Conflict(m)) if m == "epoch_changed" => {
                tracing::warn!("the hub's epoch changed under the session; reconnecting to reload");
                self.shared.reconnect_now();
            }
            Err(ApiError::Forbidden(e)) => match project {
                Some(p) => self.refuse(p, &e),
                None => tracing::error!(error = %e, "feed event refused by the hub"),
            },
            Err(e) => {
                tracing::warn!(error = %e, "feed event could not be applied; the next event or the versions vector catches up")
            }
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

    fn on_effect(&mut self, e: FeedEffect) {
        match e {
            FeedEffect::NeedSetChanged(p) | FeedEffect::ProjectJoined(p) => {
                self.exec.dirty.insert(p);
                self.serving_dirty = true;
            }
            FeedEffect::ProvidersChanged(p) => self.refresh_providers(&p),
            FeedEffect::MembersChanged(p) => {
                // I11: a device that may no longer connect is closed on both
                // sides; the policy is re-derived (a caps change).
                let inbound = self.node.close_collab_connections_not_admitted();
                let outbound = self.exec.close_not_admitted();
                if inbound + outbound > 0 {
                    tracing::info!(project_id = %p, count = inbound + outbound, "connections of devices no longer admitted closed");
                }
                if let Err(e) = crate::api::collab_live::storage_task::apply_policy(&self.ctx, &p) {
                    tracing::error!(project_id = %p, error = %e, "the replication scope was not re-derived after a membership change");
                }
                self.exec.dirty.insert(p.clone());
                self.refresh_providers(&p);
                self.attention.insert(p);
            }
            FeedEffect::ProjectGone(p) => {
                self.exec.project_gone(&p);
                self.serving_dirty = true;
            }
            FeedEffect::EpochChanged => {
                // Every need set AND every provider list in one pass (a
                // reused version number must not keep an old-epoch list).
                let derive = Derive {
                    holdings: self.holdings.as_ref(),
                    presence: self
                        .feed
                        .as_ref()
                        .map(|f| &f.presence)
                        .unwrap_or(&EMPTY_PRESENCE),
                    me: &self.me,
                };
                let fetching = self.storage_state.fetching();
                for p in self.live_projects() {
                    let refused = self.refused.contains(&p);
                    self.exec.refresh_need(&p, fetching, refused, &derive, true);
                }
                self.serving_dirty = true;
            }
        }
    }

    fn refresh_providers(&mut self, project_id: &str) {
        let derive = Derive {
            holdings: self.holdings.as_ref(),
            presence: self
                .feed
                .as_ref()
                .map(|f| &f.presence)
                .unwrap_or(&EMPTY_PRESENCE),
            me: &self.me,
        };
        self.exec.refresh_providers(project_id, &derive);
    }

    /// Re-read every dirty need set, publish the serving map and the
    /// attention changes.
    fn flush_dirty(&mut self) {
        if !self.exec.dirty.is_empty() {
            let dirty = std::mem::take(&mut self.exec.dirty);
            let derive = Derive {
                holdings: self.holdings.as_ref(),
                presence: self
                    .feed
                    .as_ref()
                    .map(|f| &f.presence)
                    .unwrap_or(&EMPTY_PRESENCE),
                me: &self.me,
            };
            let fetching = self.storage_state.fetching();
            for p in dirty {
                let refused = self.refused.contains(&p);
                self.exec
                    .refresh_need(&p, fetching, refused, &derive, false);
            }
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

    // ── storage ─────────────────────────────────────────────────────────

    fn on_storage_events(&mut self, evs: Vec<StorageEvent>) {
        let now = Instant::now();
        for ev in evs {
            match ev {
                StorageEvent::StateChanged {
                    project_id,
                    from,
                    to,
                    ..
                } => {
                    if let Some(h) = self.holdings.as_mut() {
                        h.note_append(&project_id, now);
                    }
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
                StorageEvent::DeletionChoice { count, project_ids } => {
                    for p in &project_ids {
                        self.attention.insert(p.clone());
                    }
                    self.emit(
                        COLLAB_DELETION_CHOICE_EVENT,
                        &CollabDeletionChoice { count, project_ids },
                    );
                }
                StorageEvent::FrameLost {
                    project_id,
                    frame_uuid,
                    file_name,
                } => {
                    self.attention.insert(project_id.clone());
                    self.emit(
                        COLLAB_FRAME_LOST_EVENT,
                        &CollabFrameLost {
                            project_id,
                            frame_uuid,
                            file_name,
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

    async fn storage_tick(&mut self, now: Instant) {
        let view = LiveView {
            ctx: &self.ctx,
            holdings: self.holdings.as_ref(),
            presence: self
                .feed
                .as_ref()
                .map(|f| &f.presence)
                .unwrap_or(&EMPTY_PRESENCE),
            me: &self.me,
        };
        let evs = self.storage.tick(now, &view).await;
        self.on_storage_events(evs);
    }

    async fn apply_notes(&mut self) {
        let now = Instant::now();
        for note in self.exec.take_notes() {
            match note {
                Note::Landed(p) => {
                    if let Some(h) = self.holdings.as_mut() {
                        h.note_append(&p, now);
                    }
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
                Note::StorageCheck(p, u) => {
                    let evs = self.storage.local_check(&p, &u).await;
                    self.on_storage_events(evs);
                }
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

    // ── timers and reconciliation ───────────────────────────────────────

    async fn on_timers(&mut self) {
        let now = Instant::now();
        if now >= self.storage.next_deadline() {
            self.storage_tick(now).await;
        }
        let mut refused = Vec::new();
        if let Some(h) = self.holdings.as_mut() {
            // Every wake (Task 15 R1): the due flushes AND the hourly check.
            for (pid, res) in h.flush_due(now).await {
                if let Err(ApiError::Forbidden(e)) = res {
                    refused.push((pid, e));
                }
            }
            h.hourly_digest_checks(now).await;
        }
        for (pid, e) in refused {
            self.refuse(pid, &e);
        }
        if now >= self.next_gc_probe {
            self.next_gc_probe = now + GC_PROBE_EVERY;
            self.exec.gc_probe().await;
        }
        if self.exec.tick_due() {
            self.exec.step(Input::Tick);
        }
        self.flush_bursts(now);
    }

    /// Sync now's reconciliation (P26): the refused projects are retried, a
    /// digest check per project, a stat sweep, every need set re-read (the
    /// stream itself was reopened by the caller, so `hello` catches every
    /// project up).
    async fn reconcile(&mut self) {
        if !self.refused.is_empty() {
            self.refused.clear();
            self.serving_dirty = true;
        }
        let projects = self.live_projects();
        if let Some(h) = self.holdings.as_mut() {
            for p in &projects {
                if let Err(e) = h.digest_check(p).await {
                    tracing::warn!(project_id = %p, error = %e, "sync now: claim digest check failed; retried on the next flush");
                }
            }
        }
        let view = LiveView {
            ctx: &self.ctx,
            holdings: self.holdings.as_ref(),
            presence: self
                .feed
                .as_ref()
                .map(|f| &f.presence)
                .unwrap_or(&EMPTY_PRESENCE),
            me: &self.me,
        };
        let evs = self.storage.sweep(&view).await;
        self.on_storage_events(evs);
        for p in projects {
            self.exec.dirty.insert(p);
        }
        tracing::info!("sync now reconciled");
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

static EMPTY_PRESENCE: std::sync::LazyLock<PresenceBook> =
    std::sync::LazyLock::new(PresenceBook::default);
