//! The scheduler's effect executor (collab v3 wave 3, Task 15; spec §7.1,
//! §7.4, §8, plan P18): it feeds the pure scheduler core
//! ([`crate::collab::scheduler::core::Core`]) the need sets, provider lists,
//! fetch results, pool events, lane grants and timers, and performs the
//! commands that come out — one live assignment run (`run_live`) on ONE
//! collab-class `ReceiveGate` permit, a work unit per frame, and the landing
//! of every verified frame.
//!
//! - **Need set** = `wanted` rows only (quarantined, idle, declined, awaiting
//!   a choice or the GC are never fetched), published ∧ accepted ∧ replica ∧
//!   policy ∧ byte budget, empty while the role, the toggle or the storage
//!   forbid fetching ([`need_wants`]).
//! - **Providers** are derived on every change (holder map ∩ presence ∩
//!   members, the manifest's CURRENT version and hash) and fed for every
//!   frame that entered the need set, and whenever a list changed.
//! - **Fetch ids** name each run item (`<frame_uuid>#<fetch_id>`), so a late
//!   result of a cancelled fetch is forwarded harmlessly (the core ignores
//!   it) and never lands over the fetch that replaced it.
//! - **Every pool event goes in**: a dial as `DialOk`, a close by our own
//!   idle reaper as `ConnectionIdle`, any other close as `ConnectionClosed` —
//!   unless the pool already holds a newer connection to that node (a stale
//!   close).
//! - **The lane**: a `RequestLane` queues on `acquire_collab`; the permit is
//!   kept until the core's `ReleaseLane`. Every rise of the gate's yield
//!   signal is fed as `Lane { admitted: false }`; the signal's `borrow()` is
//!   never held across a permit drop or an acquire (the gate publishes under
//!   its lock).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, EndpointId};
use iroh_blobs::api::Store;
use iroh_blobs::Hash;
use tokio::sync::{mpsc, watch};

use crate::api::collab_exchange::{
    drop_tag, read_policy, role_allows_replication, ReplicationPolicy,
};
use crate::api::collab_live::holdings::member_devices;
use crate::api::collab_live::landing::{
    identical_landed, land_frame, link_identical, project_frame_in_flight_tag, Landed, LandingEnv,
};
use crate::api::db;
use crate::collab::live::holders::{providers, FrameRef};
use crate::collab::live::meter::{device_id_of, ExchangeMeter, FlowDirection};
use crate::collab::live::presence::PresenceBook;
use crate::collab::scheduler::core::{
    CancelReason, Command, Core, FetchResult, FrameKey, Input, ProviderList, ProviderRef, Want,
    WORK_UNIT_MAX_BYTES,
};
use crate::collab::storage::marker::StoreGuard;
use crate::db::collab::CollabProjectRow;
use crate::db::collab_frames::{self as frames_db, LocalFrameRow, LocalState};
use crate::services::ServiceContext;
use crate::sharing::iroh::assign::{
    is_refused_by_every_provider, run_live, Dialer, FetchItem, ItemOutcome, LiveItem,
    LiveRunOptions, LiveVerdict, ProviderSet, STALL_HARD_LIMIT,
};
use crate::sharing::iroh::collab_pool::{CollabPool, PoolEvent};
use crate::sharing::iroh::node::{BlobHealth, SharedIrohNode};
use crate::sync::receiver::{InboundControl, ReceivePermit};

/// Items the live run may have queued at once (a Start is one item; the
/// core never has more than `collab.max_receive_streams` in flight).
const RUN_QUEUE: usize = 256;

/// The executor's fixed context: the catalog, the node and its mounted
/// collab store, the Collaboration root and its storage guard, the receive
/// gate.
pub(crate) struct ExecEnv {
    pub ctx: Arc<ServiceContext>,
    pub node: Arc<SharedIrohNode>,
    pub store: Store,
    pub root: PathBuf,
    pub guard: Arc<StoreGuard>,
    pub control: Arc<InboundControl>,
    /// The node's exchange meter (spec 2026-09-29 §6): every fetch is
    /// registered here and the fetch loop's per-provider deltas land on it.
    pub(crate) meter: Arc<ExchangeMeter>,
    /// Each live project's "to go" (the scheduler's want count), published
    /// by [`Executor::step`] whenever it can change; the runtime's shared
    /// copy, read by the `get_collab_exchange` snapshot (spec §6.2).
    pub(crate) to_go: Arc<RwLock<HashMap<String, usize>>>,
}

/// What the provider derivation reads (spec §6.1, I4/I5): the holder maps,
/// the presence book and this device's id.
pub(crate) struct Derive<'a> {
    /// The live holder maps (read under the runtime's short read lock).
    pub maps: &'a HashMap<String, crate::collab::live::holders::ProjectHolders>,
    pub presence: &'a PresenceBook,
    pub me: &'a str,
}

/// A side effect the runtime applies after a step of the executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Note {
    /// A frame landed (the outbox holds its claim `add`).
    Landed(String),
    Failed(String),
    AwaitingGc(String),
    /// The landing found the storage unavailable: re-check it now.
    StorageCheck(String, String),
}

/// One frame's need-set entry as last fed to the core.
#[derive(Debug, Clone)]
struct NeedEntry {
    content_version: i32,
    blake3: String,
    frame_seq: Option<i32>,
    /// The provider list last fed for this version (`None`: never fed).
    fed: Option<Vec<ProviderRef>>,
}

/// Where a started fetch stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The local shortcuts run (dead entry, own file, identical content).
    Preparing,
    /// Handed to the live run `run`.
    Sent { run: u64 },
    /// Its bytes are complete; the landing runs.
    Landing,
}

struct Item {
    key: FrameKey,
    fetch_id: u64,
    content_version: i32,
    blake3: String,
    hash: Hash,
    phase: Phase,
    providers: watch::Sender<Arc<Vec<EndpointId>>>,
    cancel: watch::Sender<bool>,
    /// Set when the core cancelled this fetch.
    cancelled: Option<CancelReason>,
    /// The row the fetch was prepared from (the landing carries it).
    row: Option<LocalFrameRow>,
    started_at: String,
}

/// What a prepare task decided.
pub(crate) enum Prepared {
    /// Nothing local answers it: fetch.
    Fetch(LocalFrameRow),
    /// A local shortcut settled the frame.
    Settled(Landed),
    /// The row no longer describes this fetch.
    Stale,
}

/// Results of the executor's own tasks, one channel.
pub(crate) enum ExecEvent {
    Prepared {
        name: String,
        outcome: Prepared,
    },
    Landed {
        name: String,
        landed: Landed,
    },
    Lane(ReceivePermit),
    RunEnded {
        run: u64,
    },
    /// Parked rows the GC probe released (their catalog write done off the
    /// loop, final fix A-I2): these projects' need sets are re-read.
    GcReleased(Vec<String>),
}

/// One catalog write the executor hands off the loop (final fix A-I2).
type WriteJob = Box<dyn FnOnce() + Send + 'static>;

/// The executor's catalog writer: its jobs run one after another on a
/// blocking thread, in the order the loop queued them — a write that waits
/// out another writer's lock (up to the busy timeout) never holds the loop.
async fn run_writer(mut jobs: mpsc::UnboundedReceiver<WriteJob>) {
    while let Some(job) = jobs.recv().await {
        if let Err(e) = tokio::task::spawn_blocking(job).await {
            tracing::error!(error = %e, "a collab catalog write task failed");
        }
    }
}

struct Run {
    id: u64,
    items: mpsc::Sender<LiveItem>,
}

/// What one of the executor's tasks was doing, so a task that panicked is
/// reaped as its fetch's failure or its run's end (Task 15 fix round 1,
/// I2) — never a leaked core slot or a lane held forever.
enum TaskKind {
    /// A prepare or landing task of the item named so.
    Item(String),
    /// The live run with this id.
    Run(u64),
    /// A tag cleanup.
    Cleanup,
}

pub(crate) struct Executor {
    env: Arc<ExecEnv>,
    core: Core,
    slots: Arc<AtomicUsize>,
    slots_tx: watch::Sender<usize>,
    pool: Arc<CollabPool>,
    /// Dial addresses by node id — what the live run's dialer asks.
    book: Arc<RwLock<HashMap<EndpointId, EndpointAddr>>>,
    /// Device (standard base64) by node id — how pool events and verdicts
    /// name a provider to the core.
    devices: HashMap<EndpointId, String>,
    items: HashMap<String, Item>,
    need: HashMap<String, BTreeMap<String, NeedEntry>>,
    permit: Option<ReceivePermit>,
    requesting: Option<tokio::task::JoinHandle<()>>,
    run: Option<Run>,
    run_seq: u64,
    /// Every task this executor runs; the runtime reaps them
    /// ([`Executor::on_task_ended`]).
    pub(crate) tasks: tokio::task::JoinSet<()>,
    task_kinds: HashMap<tokio::task::Id, TaskKind>,
    /// Where the next GC probe continues (round-robin, fix round 2).
    gc_cursor: Option<(String, String)>,
    events_tx: mpsc::UnboundedSender<ExecEvent>,
    pub(crate) events_rx: mpsc::UnboundedReceiver<ExecEvent>,
    done_tx: mpsc::UnboundedSender<(String, ItemOutcome)>,
    pub(crate) done_rx: mpsc::UnboundedReceiver<(String, ItemOutcome)>,
    verdicts_tx: mpsc::UnboundedSender<LiveVerdict>,
    pub(crate) verdicts_rx: mpsc::UnboundedReceiver<LiveVerdict>,
    pub(crate) pool_rx: mpsc::UnboundedReceiver<PoolEvent>,
    pub(crate) yield_rx: watch::Receiver<bool>,
    notes: Vec<Note>,
    /// Projects whose need set must be re-read (a stale landing, a local
    /// change).
    pub(crate) dirty: BTreeSet<String>,
    /// The catalog writer's queue (`None` once shut down: the writer drains
    /// what it has and ends).
    writer: Option<mpsc::UnboundedSender<WriteJob>>,
    /// Test only: every result a fetch finished with, in order.
    #[cfg(test)]
    finished: Vec<(FrameKey, FetchResult)>,
}

/// At most this many parked rows per GC probe (one local store call each).
pub(crate) const GC_PROBE_BATCH: usize = 200;

/// Wall-clock milliseconds — the core's clock: `Want::since_ms` comes from
/// the catalog's `state_changed_at`, so the starvation rule and the
/// back-offs share one time base.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn item_name(key: &FrameKey, fetch_id: u64) -> String {
    format!("{}#{fetch_id}", key.1)
}

fn cancel_reason_str(r: CancelReason) -> &'static str {
    match r {
        CancelReason::NewVersion => "new_version",
        CancelReason::NotWanted => "not_wanted",
        CancelReason::ProjectGone => "project_gone",
        CancelReason::StorageUnavailable => "storage_unavailable",
        CancelReason::NoProvider => "no_provider",
    }
}

fn result_str(r: FetchResult) -> &'static str {
    match r {
        FetchResult::Landed => "landed",
        FetchResult::Failed => "failed",
        FetchResult::Cancelled => "cancelled",
        FetchResult::AwaitingGc => "awaiting_gc",
    }
}

/// The live need set's rows (spec §7.1, Task 15), pure: every `wanted`
/// replica row that is published, accepted, not awaiting the GC and matched
/// by the policy, oldest first, inside the byte budget. Quarantined, idle,
/// declined, missing and awaiting-choice rows are never fetched (Task 15
/// R2); the role, the auto-replicate toggle and the storage are the
/// caller's gates (`api::collab_live::executor::need_wants`). The scheduler
/// ranks the result itself (rarest first).
pub(crate) fn frame_need(rows: &[LocalFrameRow], policy: &ReplicationPolicy) -> Vec<LocalFrameRow> {
    let mut need: Vec<&LocalFrameRow> = rows
        .iter()
        .filter(|r| {
            r.state == "published"
                && r.accepted
                && r.origin == crate::db::collab_frames::FrameOrigin::Replica
                && r.local_state == crate::db::collab_frames::LocalState::Wanted
                && !r.awaiting_gc
                && crate::api::collab_exchange::policy_matches(r, policy)
        })
        .collect();
    need.sort_by(|a, b| {
        crate::api::collab_exchange::manifest_str(a, "createdAt")
            .unwrap_or_default()
            .cmp(&crate::api::collab_exchange::manifest_str(b, "createdAt").unwrap_or_default())
            .then_with(|| a.frame_uuid.cmp(&b.frame_uuid))
    });
    let Some(budget) = policy.byte_budget else {
        return need.into_iter().cloned().collect();
    };
    let mut held: i64 = rows
        .iter()
        .filter(|r| r.origin == crate::db::collab_frames::FrameOrigin::Replica && r.on_disk)
        .map(|r| r.byte_size)
        .sum();
    let mut out = Vec::new();
    for r in need {
        if held.saturating_add(r.byte_size) > budget {
            break;
        }
        held += r.byte_size;
        out.push(r.clone());
    }
    out
}

/// A frame of the need set with its hub ordinal (the provider derivation's
/// key).
pub(crate) struct NeedRow {
    pub want: Want,
    pub frame_seq: Option<i32>,
}

/// The project's need set (spec §7.1): empty unless the storage may be
/// written for a fetch, the role allows replication and the project's
/// auto-replicate toggle is on; else every `wanted` replica row that is
/// published, accepted, not awaiting the GC, matched by the replication
/// policy and inside its byte budget ([`frame_need`]).
#[cfg_attr(not(test), allow(dead_code))] // the executor reads `need_rows` (it also needs the frame ordinals)
pub(crate) fn need_wants(
    conn: &rusqlite::Connection,
    project: &CollabProjectRow,
    storage_fetching: bool,
) -> anyhow::Result<Vec<Want>> {
    Ok(need_rows(conn, project, storage_fetching)?
        .into_iter()
        .map(|r| r.want)
        .collect())
}

fn need_rows(
    conn: &rusqlite::Connection,
    project: &CollabProjectRow,
    storage_fetching: bool,
) -> anyhow::Result<Vec<NeedRow>> {
    if !storage_fetching
        || !role_allows_replication(&project.data_role, project.is_coordinator)
        || !project.auto_replicate
    {
        return Ok(Vec::new());
    }
    let pid = &project.project_id;
    let rows = frames_db::list_for_project(conn, pid)?;
    let need = frame_need(&rows, &read_policy(project));
    if need.is_empty() {
        return Ok(Vec::new());
    }
    let since = frames_db::wanted_since(conn, pid)?;
    let now = now_ms();
    Ok(need
        .into_iter()
        .map(|r| NeedRow {
            want: Want {
                key: (pid.clone(), r.frame_uuid.clone()),
                content_version: r.content_version,
                blake3: r.blake3.clone(),
                byte_size: r.byte_size,
                since_ms: since.get(&r.frame_uuid).copied().unwrap_or(now),
            },
            frame_seq: r.frame_seq,
        })
        .collect())
}

impl Executor {
    pub(crate) fn new(env: ExecEnv, slots: usize, seed: u64) -> Self {
        let slots = slots.max(1);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let (verdicts_tx, verdicts_rx) = mpsc::unbounded_channel();
        let (pool_tx, pool_rx) = mpsc::unbounded_channel();
        let pool = CollabPool::new(env.node.endpoint(), pool_tx);
        let yield_rx = env.control.receive_gate.yield_signal();
        let (slots_tx, _) = watch::channel(slots);
        let (writer, jobs) = mpsc::unbounded_channel();
        tokio::spawn(run_writer(jobs));
        Self {
            env: Arc::new(env),
            core: Core::new(seed, slots),
            slots: Arc::new(AtomicUsize::new(slots)),
            slots_tx,
            pool,
            book: Arc::new(RwLock::new(HashMap::new())),
            devices: HashMap::new(),
            items: HashMap::new(),
            need: HashMap::new(),
            permit: None,
            requesting: None,
            run: None,
            run_seq: 0,
            tasks: tokio::task::JoinSet::new(),
            task_kinds: HashMap::new(),
            gc_cursor: None,
            events_tx,
            events_rx,
            done_tx,
            done_rx,
            verdicts_tx,
            verdicts_rx,
            pool_rx,
            yield_rx,
            notes: Vec::new(),
            dirty: BTreeSet::new(),
            writer: Some(writer),
            #[cfg(test)]
            finished: Vec::new(),
        }
    }

    /// Queue a catalog write on the executor's writer (final fix A-I2).
    fn write(&self, job: WriteJob) {
        let sent = match &self.writer {
            Some(w) => w.send(job).is_ok(),
            None => false,
        };
        if !sent {
            tracing::error!("the collab catalog writer is gone; a write was dropped");
        }
    }

    /// A fetch's failure on its row, off the loop — only while the row is
    /// still `wanted` (a write that waited out another writer never marks a
    /// row that landed meanwhile).
    fn write_frame_error(&self, project_id: &str, frame_uuid: &str, error: &str) {
        let (ctx, pid, uuid, error) = (
            Arc::clone(&self.env.ctx),
            project_id.to_string(),
            frame_uuid.to_string(),
            error.to_string(),
        );
        self.write(Box::new(move || {
            let written = db(&ctx).and_then(|d| {
                Ok(frames_db::set_fetch_error(&d.conn(), &pid, &uuid, &error)?)
            });
            if let Err(e) = written {
                tracing::warn!(project_id = %pid, frame_uuid = %uuid, error = %e, "record frame error failed");
            }
        }));
    }

    /// Test only: wait until every queued catalog write ran, then apply the
    /// events they sent.
    #[cfg(test)]
    pub(crate) async fn settle_writes(&mut self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.write(Box::new(move || {
            let _ = tx.send(());
        }));
        let _ = rx.await;
        while let Ok(ev) = self.events_rx.try_recv() {
            self.on_event(ev).await;
        }
    }

    pub(crate) fn take_notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    /// Whether a back-off of the core has ended by now (a `Tick` is due).
    pub(crate) fn tick_due(&self) -> bool {
        self.core.next_wake_ms().is_some_and(|w| w <= now_ms())
    }

    /// When the core wants a `Tick` (a back-off ends).
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        let wake = self.core.next_wake_ms()?;
        let wait = (wake - now_ms()).max(0) as u64;
        Some(Instant::now() + Duration::from_millis(wait))
    }

    /// How many frames of a project are still to fetch (the exchange's
    /// "to go", spec 2026-09-29 §6.4) — in memory, no catalog read. The
    /// core's want set, not `need`: a landed frame leaves the core's at once,
    /// while `need` keeps it until the project's need set is next re-read.
    pub(crate) fn need_len(&self, project_id: &str) -> usize {
        self.core.wants_of(project_id)
    }

    /// Step the core and perform what comes out. The ONE place the core's
    /// wants change (a need set, a finished fetch, a gone project): the
    /// project's "to go" is published here (spec §6.2), pruned at zero.
    pub(crate) fn step(&mut self, input: Input) {
        let touched = match &input {
            Input::NeedSet { project_id, .. } | Input::ProjectGone { project_id } => {
                Some(project_id.clone())
            }
            Input::Finished { key, .. } => Some(key.0.clone()),
            _ => None,
        };
        let cmds = self.core.step(now_ms(), input);
        if let Some(pid) = touched {
            self.publish_to_go(&pid);
        }
        self.execute(cmds);
    }

    fn publish_to_go(&self, project_id: &str) {
        let n = self.core.wants_of(project_id);
        let mut g = match self.env.to_go.write() {
            Ok(g) => g,
            Err(p) => {
                tracing::warn!(
                    project_id,
                    "exchange to-go cache poisoned; continuing with its data"
                );
                p.into_inner()
            }
        };
        if n == 0 {
            g.remove(project_id);
        } else {
            g.insert(project_id.to_string(), n);
        }
    }

    pub(crate) fn set_slots(&mut self, n: usize) {
        let n = n.max(1);
        self.slots.store(n, Ordering::Relaxed);
        self.slots_tx.send_replace(n);
        tracing::info!(streams = n, "collab receive stream limit changed");
        self.step(Input::Slots(n));
    }

    pub(crate) fn storage(&mut self, fetching: bool) {
        self.step(Input::Storage { fetching });
    }

    pub(crate) fn project_gone(&mut self, project_id: &str) {
        if self.need.remove(project_id).is_some() {
            tracing::info!(project_id, "project gone; its fetches stop");
        }
        self.step(Input::ProjectGone {
            project_id: project_id.to_string(),
        });
    }

    // ── need set and providers ──────────────────────────────────────────

    /// Re-read one project's need set and feed it, then the provider list
    /// of every frame that entered it (or changed version); `refeed` feeds
    /// every frame's list again (an epoch change: a reused version number
    /// could otherwise keep a list derived under the old epoch).
    pub(crate) fn refresh_need(
        &mut self,
        project_id: &str,
        storage_fetching: bool,
        refused: bool,
        derive: &Derive<'_>,
        refeed: bool,
    ) {
        let read = db(&self.env.ctx).and_then(|d| {
            let conn = d.conn();
            let Some(project) = crate::db::collab::get_live_project(&conn, project_id)? else {
                return Ok(None);
            };
            let rows = need_rows(&conn, &project, storage_fetching && !refused)
                .map_err(crate::api::ApiError::from)?;
            Ok(Some((project, rows)))
        });
        let (project, rows) = match read {
            Ok(Some(x)) => x,
            Ok(None) => {
                self.project_gone(project_id);
                return;
            }
            Err(e) => {
                tracing::error!(project_id, error = %e, "need set could not be read; retried on the next change");
                return;
            }
        };
        let old = self.need.remove(project_id).unwrap_or_default();
        let mut next: BTreeMap<String, NeedEntry> = BTreeMap::new();
        let mut entered: HashSet<String> = HashSet::new();
        for r in &rows {
            let uuid = r.want.key.1.clone();
            let kept = old.get(&uuid).filter(|e| {
                e.content_version == r.want.content_version && e.blake3 == r.want.blake3
            });
            let fed = if refeed {
                None
            } else {
                kept.and_then(|e| e.fed.clone())
            };
            if fed.is_none() {
                entered.insert(uuid.clone());
            }
            next.insert(
                uuid,
                NeedEntry {
                    content_version: r.want.content_version,
                    blake3: r.want.blake3.clone(),
                    frame_seq: r.frame_seq,
                    fed,
                },
            );
        }
        let wants: Vec<Want> = rows.into_iter().map(|r| r.want).collect();
        tracing::debug!(
            project_id,
            count = wants.len(),
            outcome = if entered.is_empty() {
                "unchanged"
            } else {
                "entered"
            },
            "need set fed"
        );
        self.need.insert(project_id.to_string(), next);
        self.step(Input::NeedSet {
            project_id: project_id.to_string(),
            wants,
        });
        self.feed_lists(&project, derive, Some(&entered));
    }

    /// Re-derive the provider lists of a project's need set and feed those
    /// that changed.
    pub(crate) fn refresh_providers(&mut self, project_id: &str, derive: &Derive<'_>) {
        if !self.need.contains_key(project_id) {
            return;
        }
        let project = match db(&self.env.ctx)
            .and_then(|d| Ok(crate::db::collab::get_live_project(&d.conn(), project_id)?))
        {
            Ok(Some(p)) => p,
            Ok(None) => {
                self.project_gone(project_id);
                return;
            }
            Err(e) => {
                tracing::error!(project_id, error = %e, "project could not be read for its providers; retried on the next change");
                return;
            }
        };
        self.feed_lists(&project, derive, None);
    }

    /// Derive and feed lists for `only` (or every frame), skipping those
    /// equal to what was last fed — in ONE core step.
    fn feed_lists(
        &mut self,
        project: &CollabProjectRow,
        derive: &Derive<'_>,
        only: Option<&HashSet<String>>,
    ) {
        let pid = project.project_id.clone();
        let members = member_devices(project);
        let none = HashSet::new();
        let map = derive.maps.get(&pid);
        let mut lists = Vec::new();
        let Some(entries) = self.need.get_mut(&pid) else {
            return;
        };
        for (uuid, e) in entries.iter_mut() {
            if only.is_some_and(|o| !o.contains(uuid)) {
                continue;
            }
            let derived: Vec<ProviderRef> = match (map, e.frame_seq) {
                (Some(map), Some(seq)) => providers(
                    map,
                    derive.presence,
                    &members,
                    derive.me,
                    &FrameRef {
                        project_id: &pid,
                        frame_seq: seq,
                        content_version: e.content_version,
                        publisher_devices: &none,
                    },
                )
                .into_iter()
                .map(|p| ProviderRef {
                    device: p.device,
                    relay_url: p.relay_url,
                })
                .collect(),
                _ => Vec::new(),
            };
            if e.fed.as_ref() == Some(&derived) {
                continue;
            }
            e.fed = Some(derived.clone());
            lists.push(ProviderList {
                key: (pid.clone(), uuid.clone()),
                content_version: e.content_version,
                blake3: e.blake3.clone(),
                providers: derived,
            });
        }
        if lists.is_empty() {
            return;
        }
        for l in &lists {
            self.learn_addresses(&l.providers);
        }
        tracing::debug!(project_id = %pid, count = lists.len(), "provider lists fed");
        self.step(Input::ProvidersBatch { lists });
    }

    /// Remember how to dial each provider (spec §7.3, S1: across accounts
    /// only the relay — the live presence relay, else the holder map's).
    fn learn_addresses(&mut self, providers: &[ProviderRef]) {
        for p in providers {
            let node = match crate::sync::pairing::node_id_from_pubkey_b64(&p.device) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(device = %p.device, error = %format!("{e:#}"), "provider device id does not parse; not dialled");
                    continue;
                }
            };
            let report = crate::account::EndpointAddrReport {
                home_relay_url: p.relay_url.clone(),
                direct_addrs: Vec::new(),
                reported_at: None,
            };
            match crate::sync::pairing::peer_dial_addr(
                node,
                Some(&report),
                &self.env.node.relay_urls(),
                true,
            ) {
                Ok(addr) => {
                    self.devices.insert(addr.id, p.device.clone());
                    match self.book.write() {
                        Ok(mut book) => {
                            book.insert(addr.id, addr);
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "collab dial book poisoned; provider not recorded")
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(device = %p.device, error = %format!("{e:#}"), "provider address could not be built; not dialled")
                }
            }
        }
    }

    fn endpoints(&mut self, providers: &[ProviderRef]) -> Arc<Vec<EndpointId>> {
        self.learn_addresses(providers);
        Arc::new(
            providers
                .iter()
                .filter_map(|p| {
                    let node = crate::sync::pairing::node_id_from_pubkey_b64(&p.device).ok()?;
                    EndpointId::from_bytes(&node).ok()
                })
                .collect(),
        )
    }

    // ── commands ─────────────────────────────────────────────────────────

    fn execute(&mut self, cmds: Vec<Command>) {
        for cmd in cmds {
            match cmd {
                Command::Start {
                    key,
                    fetch_id,
                    content_version,
                    blake3,
                    byte_size,
                    providers,
                } => self.start(key, fetch_id, content_version, blake3, byte_size, providers),
                Command::UpdateProviders {
                    key,
                    fetch_id,
                    providers,
                } => {
                    #[cfg(test)]
                    test_hooks::record_sighting(self.env.node.node_id(), &key.1, &providers, false);
                    let list = self.endpoints(&providers);
                    match self.items.get(&item_name(&key, fetch_id)) {
                        Some(item) => {
                            item.providers.send_replace(list);
                        }
                        None => {
                            tracing::debug!(project_id = %key.0, frame_uuid = %key.1, "provider update for a fetch no longer here")
                        }
                    }
                }
                Command::Cancel {
                    key,
                    fetch_id,
                    reason,
                } => {
                    let name = item_name(&key, fetch_id);
                    match self.items.get_mut(&name) {
                        Some(item) => {
                            item.cancelled = Some(reason);
                            item.cancel.send_replace(true);
                            tracing::debug!(project_id = %key.0, frame_uuid = %key.1, reason = cancel_reason_str(reason), "live fetch cancelled");
                        }
                        None => {
                            tracing::debug!(project_id = %key.0, frame_uuid = %key.1, "cancel for a fetch no longer here")
                        }
                    }
                }
                Command::RequestLane => self.request_lane(),
                Command::ReleaseLane => {
                    // The core has nothing in flight: close the run (it ends
                    // once idle) and give the permit back (spec §8).
                    self.run = None;
                    if self.permit.take().is_some() {
                        tracing::debug!("collab lane released");
                    }
                }
            }
        }
    }

    fn start(
        &mut self,
        key: FrameKey,
        fetch_id: u64,
        content_version: i32,
        blake3: String,
        byte_size: i64,
        providers: Vec<ProviderRef>,
    ) {
        let name = item_name(&key, fetch_id);
        let hash: Hash = match blake3.parse() {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(project_id = %key.0, frame_uuid = %key.1, blake3 = %blake3, error = %e, "frame hash does not parse; fetch failed");
                self.step(Input::Finished {
                    key,
                    fetch_id,
                    result: FetchResult::Failed,
                });
                return;
            }
        };
        #[cfg(test)]
        test_hooks::record_sighting(self.env.node.node_id(), &key.1, &providers, true);
        let list = self.endpoints(&providers);
        let (providers_tx, _) = watch::channel(list);
        let (cancel_tx, _) = watch::channel(false);
        tracing::debug!(
            project_id = %key.0,
            frame_uuid = %key.1,
            content_version,
            bytes = byte_size,
            count = providers.len(),
            "live fetch started"
        );
        self.items.insert(
            name.clone(),
            Item {
                key: key.clone(),
                fetch_id,
                content_version,
                blake3: blake3.clone(),
                hash,
                phase: Phase::Preparing,
                providers: providers_tx,
                cancel: cancel_tx,
                cancelled: None,
                row: None,
                started_at: crate::sync::now_iso(),
            },
        );
        let env = Arc::clone(&self.env);
        let tx = self.events_tx.clone();
        let kind = TaskKind::Item(name.clone());
        let task = self.tasks.spawn(async move {
            let outcome = prepare(&env, &key, content_version, &blake3, hash).await;
            let _ = tx.send(ExecEvent::Prepared { name, outcome });
        });
        self.task_kinds.insert(task.id(), kind);
    }

    fn request_lane(&mut self) {
        if self.permit.is_some() || self.requesting.is_some() {
            tracing::debug!("collab lane already held or requested");
            return;
        }
        let control = Arc::clone(&self.env.control);
        let tx = self.events_tx.clone();
        tracing::debug!("collab lane requested");
        self.requesting = Some(tokio::spawn(async move {
            let permit = control.receive_gate.acquire_collab().await;
            let _ = tx.send(ExecEvent::Lane(permit));
        }));
    }

    /// The run's item queue, starting a run when none is live (the lane
    /// must be held).
    fn run_queue(&mut self) -> Option<(u64, mpsc::Sender<LiveItem>)> {
        self.permit.as_ref()?;
        if self.run.is_none() {
            self.run_seq += 1;
            let id = self.run_seq;
            let (tx, rx) = mpsc::channel(RUN_QUEUE);
            let book = Arc::clone(&self.book);
            let dialer = Dialer::Collab {
                pool: Arc::clone(&self.pool),
                addrs: Arc::new(move |id: &EndpointId| {
                    book.read().ok().and_then(|b| b.get(id).cloned())
                }),
            };
            let opts = LiveRunOptions {
                stall_hard_limit: STALL_HARD_LIMIT,
                hedging: true,
                telemetry: crate::sharing::noop_provider_telemetry(),
                max_in_flight: Arc::clone(&self.slots),
                limit_changed: Some(self.slots_tx.subscribe()),
                unit_cap_bytes: WORK_UNIT_MAX_BYTES,
                // Per-provider deltas, straight from the fetch loop, onto the
                // item registered in `on_prepared` (same key: `item_name`).
                delivered: Some({
                    let meter = Arc::clone(&self.env.meter);
                    Arc::new(move |key: &str, provider: [u8; 32], delta: u64| {
                        meter.delivered(key, &device_id_of(&provider), delta, Instant::now())
                    }) as crate::sharing::iroh::assign::DeliveredSink
                }),
            };
            let store = self.env.store.clone();
            let done = self.done_tx.clone();
            let verdicts = self.verdicts_tx.clone();
            let yield_now = self.env.control.receive_gate.yield_signal();
            let ev = self.events_tx.clone();
            #[cfg(test)]
            let root = self.env.root.clone();
            let task = self.tasks.spawn(async move {
                #[cfg(test)]
                if test_hooks::take_run_panic(&root) {
                    panic!("injected run panic (test hook)");
                }
                run_live(&store, dialer, rx, opts, done, verdicts, yield_now).await;
                let _ = ev.send(ExecEvent::RunEnded { run: id });
            });
            self.task_kinds.insert(task.id(), TaskKind::Run(id));
            tracing::debug!(count = id, "collab live run started");
            self.run = Some(Run { id, items: tx });
        }
        self.run.as_ref().map(|r| (r.id, r.items.clone()))
    }

    // ── results ──────────────────────────────────────────────────────────

    pub(crate) async fn on_event(&mut self, ev: ExecEvent) {
        match ev {
            ExecEvent::Lane(permit) => {
                self.requesting = None;
                self.permit = Some(permit);
                tracing::debug!("collab lane admitted");
                self.step(Input::Lane { admitted: true });
            }
            ExecEvent::Prepared { name, outcome } => self.on_prepared(name, outcome).await,
            ExecEvent::Landed { name, landed } => self.on_landed(name, landed),
            // Items still queued at a yield got no outcome: re-derived (a
            // cancel the core re-queues at once).
            ExecEvent::RunEnded { run } => self.end_run(run, FetchResult::Cancelled).await,
            ExecEvent::GcReleased(projects) => self.dirty.extend(projects),
        }
    }

    /// A live run ended: every outcome it sent first (it sends each before
    /// it returns), then its items that got none finish as `orphans`.
    async fn end_run(&mut self, run: u64, orphans: FetchResult) {
        while let Ok((n, o)) = self.done_rx.try_recv() {
            self.on_done(n, o).await;
        }
        if self.run.as_ref().is_some_and(|r| r.id == run) {
            self.run = None;
        }
        let names: Vec<String> = self
            .items
            .iter()
            .filter(|(_, i)| i.phase == Phase::Sent { run })
            .map(|(n, _)| n.clone())
            .collect();
        for name in names {
            if orphans == FetchResult::Failed {
                if let Some((pid, uuid)) = self.items.get(&name).map(|i| i.key.clone()) {
                    self.write_frame_error(&pid, &uuid, "internal error: the transfer run failed");
                    self.notes.push(Note::Failed(pid));
                }
            }
            self.finish(&name, orphans).await;
        }
    }

    async fn on_prepared(&mut self, name: String, outcome: Prepared) {
        let Some((cancelled, pid)) = self
            .items
            .get(&name)
            .map(|i| (i.cancelled.is_some(), i.key.0.clone()))
        else {
            return;
        };
        if cancelled {
            self.finish(&name, FetchResult::Cancelled).await;
            return;
        }
        match outcome {
            Prepared::Stale => {
                self.dirty.insert(pid);
                self.finish(&name, FetchResult::Cancelled).await;
            }
            Prepared::Settled(landed) => self.on_landed(name, landed),
            Prepared::Fetch(row) => {
                let Some((run, queue)) = self.run_queue() else {
                    // The lane went back meanwhile: again when it returns.
                    self.finish(&name, FetchResult::Cancelled).await;
                    return;
                };
                let item = self.items.get_mut(&name).expect("checked above");
                self.env.meter.register(
                    &name,
                    FlowDirection::Recv,
                    &item.key.0,
                    &item.key.1,
                    &row.file_name,
                    row.byte_size.max(0) as u64,
                );
                let live = LiveItem {
                    item: FetchItem {
                        key: name.clone(),
                        request: iroh_blobs::protocol::GetRequest::blob(item.hash),
                        hash: item.hash,
                        size: row.byte_size.max(0) as u64,
                        providers: ProviderSet::Live(item.providers.subscribe()),
                    },
                    cancel: item.cancel.subscribe(),
                };
                item.row = Some(row);
                item.phase = Phase::Sent { run };
                match queue.try_send(live) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // The run is gone and its end is not reaped yet: it
                        // ended at a yield (`RunEnded` on its way) or it
                        // panicked (its task's reaping on its way). The item
                        // stays `Sent` to that run and the run's end rules on
                        // it with every other item it left — cancelled (no
                        // strike, no back-off) at a yield, failed and backed
                        // off after a panic, never re-queued at once into the
                        // same dead queue (fix round 2; final fix A-M3).
                        tracing::debug!(frame_uuid = %name, "live run ended before the fetch was queued; its end rules on it");
                        if self.run.as_ref().is_some_and(|r| r.id == run) {
                            self.run = None;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(frame_uuid = %name, error = %e, "live run queue refused the fetch; re-queued");
                        self.finish(&name, FetchResult::Cancelled).await;
                    }
                }
            }
        }
    }

    /// One live run result.
    pub(crate) async fn on_done(&mut self, name: String, outcome: ItemOutcome) {
        let Some((cancelled, hash, pid, uuid)) = self.items.get(&name).map(|i| {
            (
                i.cancelled.is_some(),
                i.hash,
                i.key.0.clone(),
                i.key.1.clone(),
            )
        }) else {
            tracing::debug!(frame_uuid = %name, "live fetch result for a fetch no longer here");
            return;
        };
        match outcome {
            ItemOutcome::Done if !cancelled => self.land(&name),
            ItemOutcome::Cancelled if !cancelled => {
                // A cut at a yield. In the hedge-win window its blob may be
                // complete already: then it lands (Task 15 R2).
                let complete = matches!(
                    self.env.store.blobs().status(hash).await,
                    Ok(iroh_blobs::api::proto::BlobStatus::Complete { .. })
                );
                if complete {
                    self.land(&name);
                } else {
                    self.finish(&name, FetchResult::Cancelled).await;
                }
            }
            ItemOutcome::Failed(e) if !cancelled => {
                let msg = format!("{e:#}");
                if is_refused_by_every_provider(&e) {
                    tracing::debug!(project_id = %pid, frame_uuid = %uuid, error = %msg, "every provider refused the frame; retried after its back-off");
                } else {
                    tracing::warn!(project_id = %pid, frame_uuid = %uuid, error = %msg, "frame fetch failed; retried after its back-off");
                }
                self.write_frame_error(&pid, &uuid, &msg);
                self.notes.push(Note::Failed(pid));
                self.finish(&name, FetchResult::Failed).await;
            }
            _ => self.finish(&name, FetchResult::Cancelled).await,
        }
    }

    fn land(&mut self, name: &str) {
        let Some(item) = self.items.get_mut(name) else {
            return;
        };
        let Some(row) = item.row.clone() else {
            tracing::error!(frame_uuid = %name, "a fetched frame has no row to land; dropped");
            return;
        };
        item.phase = Phase::Landing;
        // The fetch is over: its per-device bytes, largest first (the top
        // device is credited the completion) — what the landing records as
        // the frame's sources.
        let sources = self.env.meter.finish(name, true, Instant::now());
        let env = Arc::clone(&self.env);
        let hash = item.hash;
        let started_at = item.started_at.clone();
        let name = name.to_string();
        let tx = self.events_tx.clone();
        let kind = TaskKind::Item(name.clone());
        let task = self.tasks.spawn(async move {
            let landed = land(&env, &row, hash, &started_at, &sources).await;
            let _ = tx.send(ExecEvent::Landed { name, landed });
        });
        self.task_kinds.insert(task.id(), kind);
    }

    fn on_landed(&mut self, name: String, landed: Landed) {
        let Some(item) = self.items.remove(&name) else {
            return;
        };
        let (pid, uuid) = (item.key.0.clone(), item.key.1.clone());
        let result = match landed {
            Landed::Yes(_) => {
                self.notes.push(Note::Landed(pid));
                FetchResult::Landed
            }
            Landed::AwaitingGc => {
                self.notes.push(Note::AwaitingGc(pid));
                FetchResult::AwaitingGc
            }
            Landed::Stale => {
                self.dirty.insert(pid);
                FetchResult::Cancelled
            }
            Landed::Unavailable => {
                self.notes.push(Note::StorageCheck(pid, uuid));
                FetchResult::Cancelled
            }
            Landed::Failed(_) => {
                self.notes.push(Note::Failed(pid));
                FetchResult::Failed
            }
        };
        tracing::debug!(project_id = %item.key.0, frame_uuid = %item.key.1, outcome = result_str(result), "live fetch finished");
        self.step(Input::Finished {
            key: item.key,
            fetch_id: item.fetch_id,
            result,
        });
    }

    /// End a fetch without a landing: forward the result (the core ignores
    /// it for a fetch it cancelled) and drop its in-flight tag when nothing
    /// will resume it — the row moved to another version or content, left
    /// `wanted`, or is gone, and no newer fetch of the same version runs.
    async fn finish(&mut self, name: &str, result: FetchResult) {
        let Some(item) = self.items.remove(name) else {
            return;
        };
        // Nothing lands: its in-flight row goes, nothing is completed (a
        // fetch never registered — cancelled before it was sent — is a
        // no-op). Its delivered bytes stay on their flows.
        self.env.meter.finish(name, false, Instant::now());
        let same_version_runs = self
            .items
            .values()
            .any(|i| i.key == item.key && i.content_version == item.content_version);
        if !same_version_runs && self.resumable(&item).is_some_and(|r| !r) {
            let tag = project_frame_in_flight_tag(&item.key.0, &item.key.1, item.content_version);
            drop_tag(&self.env.store, &tag).await;
        }
        #[cfg(test)]
        self.finished.push((item.key.clone(), result));
        self.step(Input::Finished {
            key: item.key,
            fetch_id: item.fetch_id,
            result,
        });
    }

    /// Whether a fetch's partial bytes may still be resumed: its row is
    /// still `wanted` at this version and content. `None`: unknown (the
    /// catalog read failed, logged) — the bytes are kept.
    fn resumable(&self, item: &Item) -> Option<bool> {
        match db(&self.env.ctx)
            .and_then(|d| Ok(frames_db::get(&d.conn(), &item.key.0, &item.key.1)?))
        {
            Ok(Some(r)) => Some(
                r.content_version == item.content_version
                    && r.blake3 == item.blake3
                    && r.local_state == LocalState::Wanted,
            ),
            Ok(None) => Some(false),
            Err(e) => {
                tracing::warn!(project_id = %item.key.0, frame_uuid = %item.key.1, error = %e, "fetch cleanup: the frame could not be read; its partial bytes are kept");
                None
            }
        }
    }

    pub(crate) fn on_verdict(&mut self, v: LiveVerdict) {
        match v {
            LiveVerdict::DialFailed { provider, error } => match self.devices.get(&provider) {
                Some(device) => {
                    tracing::debug!(provider = %provider.fmt_short(), error = %error, "provider dial failed; it backs off");
                    let device = device.clone();
                    self.step(Input::DialFailed { device });
                }
                None => {
                    tracing::debug!(provider = %provider.fmt_short(), "dial failure of an unknown provider")
                }
            },
            // The engine excluded it for that hash (and logged it).
            LiveVerdict::Refused { .. }
            | LiveVerdict::Corrupt { .. }
            | LiveVerdict::Busy { .. } => {}
        }
    }

    pub(crate) fn on_pool(&mut self, ev: PoolEvent) {
        match ev {
            PoolEvent::Dialed { node, .. } => {
                if let Some(device) = self.devices.get(&node).cloned() {
                    self.step(Input::DialOk { device });
                }
            }
            PoolEvent::Closed {
                node,
                conn_id,
                idle,
                reason,
            } => {
                if self
                    .pool
                    .current_conn_id(&node)
                    .is_some_and(|current| current != conn_id)
                {
                    tracing::debug!(provider = %node.fmt_short(), connection_id = conn_id, "stale close of a re-dialled provider ignored");
                    return;
                }
                let Some(device) = self.devices.get(&node).cloned() else {
                    return;
                };
                if idle {
                    self.step(Input::ConnectionIdle { device });
                } else {
                    tracing::debug!(provider = %node.fmt_short(), connection_id = conn_id, reason = %reason, "provider connection closed; it backs off");
                    self.step(Input::ConnectionClosed { device });
                }
            }
        }
    }

    /// The gate's yield signal changed: every rise is fed as a yield (the
    /// core ignores it without a permit). The value is copied out before the
    /// step — no `borrow()` is held across a permit drop (Task 15 R3).
    pub(crate) fn on_yield_changed(&mut self) {
        let rose = *self.yield_rx.borrow_and_update();
        if rose {
            tracing::debug!("a personal transfer waits; the collab lane yields");
            self.step(Input::Lane { admitted: false });
        }
    }

    /// A frame was quarantined (Task 15 R2): the partial bytes of its
    /// current version's fetch go with their in-flight tag — nothing lands
    /// over the quarantined file, so nothing resumes them. A fetch still
    /// running for it drops its own tag when the core's cancel ends it.
    pub(crate) fn forget_partial(&mut self, project_id: &str, frame_uuid: &str) {
        if self
            .items
            .values()
            .any(|i| i.key.0 == project_id && i.key.1 == frame_uuid)
        {
            return;
        }
        let cv = match db(&self.env.ctx)
            .and_then(|d| Ok(frames_db::get(&d.conn(), project_id, frame_uuid)?))
        {
            Ok(Some(r)) => r.content_version,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(project_id, frame_uuid, error = %e, "quarantined frame could not be read; its partial bytes are kept");
                return;
            }
        };
        let store = self.env.store.clone();
        let tag = project_frame_in_flight_tag(project_id, frame_uuid, cv);
        let task = self
            .tasks
            .spawn(async move { drop_tag(&store, &tag).await });
        self.task_kinds.insert(task.id(), TaskKind::Cleanup);
    }

    /// One of this executor's tasks ended (the runtime reaps the join set).
    /// A task that panicked is its item's failure (the core frees the slot
    /// and backs the frame off) or its run's end (items still queued are
    /// cancelled and re-queued), so a panic never leaks a slot or the lane
    /// (Task 15 fix round 1, I2). A task aborted by the shutdown is not an
    /// error.
    pub(crate) async fn on_task_ended(
        &mut self,
        ended: Result<(tokio::task::Id, ()), tokio::task::JoinError>,
    ) {
        let (id, failure) = match ended {
            Ok((id, ())) => (id, None),
            Err(e) => (e.id(), Some(e)),
        };
        let kind = self.task_kinds.remove(&id);
        let Some(e) = failure else {
            return;
        };
        if e.is_cancelled() {
            return;
        }
        match kind {
            Some(TaskKind::Item(name)) => {
                let key = self.items.get(&name).map(|i| i.key.clone());
                tracing::error!(frame_uuid = %name, error = %e, "a fetch task panicked; the fetch failed");
                if let Some((pid, uuid)) = key {
                    self.write_frame_error(&pid, &uuid, &format!("internal error: {e}"));
                    self.notes.push(Note::Failed(pid));
                }
                self.finish(&name, FetchResult::Failed).await;
            }
            Some(TaskKind::Run(run)) => {
                // Failed, never cancelled (fix round 2): a panic that repeats
                // backs its fetches off instead of re-queuing them at once.
                tracing::error!(error = %e, "the collab live run panicked; its fetches failed and back off");
                self.end_run(run, FetchResult::Failed).await;
            }
            Some(TaskKind::Cleanup) | None => {
                tracing::error!(error = %e, "a collab executor task panicked");
            }
        }
    }

    /// I11: close every pooled connection to a node the connect gate no
    /// longer admits.
    pub(crate) fn close_not_admitted(&self) -> usize {
        let node = Arc::clone(&self.env.node);
        self.pool.close_where(move |id| !node.admits(id.as_bytes()))
    }

    /// Rows parked for the GC (a landing whose store data vanished, P20)
    /// whose entry is gone or partial by now: released into the need set.
    ///
    /// At most [`GC_PROBE_BATCH`] rows per probe (fix round 2: the probe
    /// runs in the loop, one local store call per row), round-robin: each
    /// probe continues after the last row the previous one looked at and
    /// wraps at the end, so every parked row is eventually probed.
    pub(crate) async fn gc_probe(&mut self) {
        self.gc_probe_batch(GC_PROBE_BATCH).await;
    }

    async fn gc_probe_batch(&mut self, batch: usize) {
        let all = match db(&self.env.ctx)
            .and_then(|d| Ok(frames_db::awaiting_gc_released(&d.conn())?))
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "awaiting-GC rows could not be read; retried at the next probe");
                return;
            }
        };
        // Sorted by (project_id, frame_uuid): the rows after the cursor.
        let start = match &self.gc_cursor {
            Some(c) => all.partition_point(|(p, u, _)| (p, u) <= (&c.0, &c.1)),
            None => 0,
        };
        let rows: Vec<(String, String, String)> =
            all[start..].iter().take(batch.max(1)).cloned().collect();
        self.gc_cursor = if start + rows.len() >= all.len() {
            None
        } else {
            rows.last().map(|(p, u, _)| (p.clone(), u.clone()))
        };
        let mut released: Vec<(String, String)> = Vec::new();
        for (pid, uuid, blake3) in rows {
            let Ok(hash) = blake3.parse::<Hash>() else {
                continue;
            };
            match self.env.node.collab_blob_health(hash).await {
                Ok(BlobHealth::Missing | BlobHealth::Partial) => released.push((pid, uuid)),
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(project_id = %pid, frame_uuid = %uuid, error = %format!("{e:#}"), "blob health unknown; kept awaiting GC")
                }
            }
        }
        if released.is_empty() {
            return;
        }
        // Final fix A-I2: the release is a catalog write — off the loop; the
        // need sets are re-read when it is done (`ExecEvent::GcReleased`).
        let (ctx, events) = (Arc::clone(&self.env.ctx), self.events_tx.clone());
        self.write(Box::new(move || {
            let mut projects = Vec::new();
            for (pid, uuid) in released {
                let done = db(&ctx).and_then(|d| {
                    Ok(frames_db::set_awaiting_gc(&d.conn(), &pid, &uuid, false)?)
                });
                match done {
                    Ok(_) => projects.push(pid),
                    Err(e) => {
                        tracing::warn!(project_id = %pid, frame_uuid = %uuid, error = %e, "release from awaiting GC failed")
                    }
                }
            }
            if projects.is_empty() {
                return;
            }
            tracing::info!(count = projects.len(), "frames released from awaiting GC");
            projects.sort();
            projects.dedup();
            if events.send(ExecEvent::GcReleased(projects)).is_err() {
                tracing::debug!("the executor is gone; released frames wait for its next start");
            }
        }));
    }

    /// Stop everything: the lane request, the live run, every task; the
    /// permit goes back.
    pub(crate) fn shutdown(&mut self) {
        if let Some(r) = self.requesting.take() {
            r.abort();
        }
        // The writer finishes what is queued, then ends.
        self.writer = None;
        self.run = None;
        self.tasks.abort_all();
        self.task_kinds.clear();
        self.permit = None;
        // The meter is the node's and outlives this executor (a remount, a
        // restart): every fetch dropped here leaves it, or its in-flight row
        // and the next runtime's progress wake would stay forever.
        let now = Instant::now();
        for (name, item) in &self.items {
            item.cancel.send_replace(true);
            self.env.meter.finish(name, false, now);
        }
    }
}

/// The local shortcuts before a fetch (Task 15 S5): the row as it is now, a
/// dead store entry, the row's own file already holding the frame, another
/// frame of the project already holding the content; else the in-flight tag
/// and a fetch.
async fn prepare(
    env: &ExecEnv,
    key: &FrameKey,
    content_version: i32,
    blake3: &str,
    hash: Hash,
) -> Prepared {
    let (pid, uuid) = (key.0.as_str(), key.1.as_str());
    let row = match db(&env.ctx).and_then(|d| Ok(frames_db::get(&d.conn(), pid, uuid)?)) {
        Ok(Some(r))
            if r.content_version == content_version
                && r.blake3 == blake3
                && r.local_state == LocalState::Wanted
                && !r.awaiting_gc =>
        {
            r
        }
        Ok(_) => return Prepared::Stale,
        Err(e) => {
            tracing::warn!(project_id = pid, frame_uuid = uuid, error = %e, "the frame could not be read before its fetch");
            return Prepared::Settled(Landed::Failed(e.to_string()));
        }
    };
    // A complete entry whose data is gone (P20): the GC must drop it first.
    match env.node.collab_blob_health(hash).await {
        Ok(BlobHealth::Dead) => {
            let _ = env.node.unseed_project_frame(pid, uuid).await;
            if let Err(e) = db(&env.ctx)
                .and_then(|d| Ok(frames_db::set_awaiting_gc(&d.conn(), pid, uuid, true)?))
            {
                tracing::warn!(project_id = pid, frame_uuid = uuid, error = %e, "mark frame awaiting GC failed");
            }
            tracing::info!(
                project_id = pid,
                frame_uuid = uuid,
                "frame's store entry is dead; waiting for the GC"
            );
            return Prepared::Settled(Landed::AwaitingGc);
        }
        Ok(_) => {}
        Err(e) => {
            tracing::debug!(project_id = pid, frame_uuid = uuid, error = %format!("{e:#}"), "blob health unknown before a fetch")
        }
    }
    // The row's own file already holds this version (its bytes put back):
    // re-adopted, no transfer. A file of the previous version (`prev_stamp`
    // set) never does — no hash spent on it. A replica file outside the
    // CURRENT Collaboration root never does either: after a re-designation
    // it counts as gone and the frame is fetched into the new root (owner
    // rule, Task 15 fix round 1, I1).
    if let Some(path) = row
        .landed_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| crate::api::collab_exchange::inside_root(Some(&env.root), &row, p))
    {
        if own_file_holds(env, &row, &path).await {
            match crate::api::collab_live::replace::adopt_by_hash(
                &env.ctx, &env.node, &env.root, &path,
            )
            .await
            {
                Ok(adopted) if adopted.iter().any(|(p, u)| p == pid && u == uuid) => {
                    tracing::info!(project_id = pid, frame_uuid = uuid, path = %path.display(), "frame re-adopted from its own file; no fetch");
                    return Prepared::Settled(Landed::Yes(path));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(project_id = pid, frame_uuid = uuid, error = %e, "re-adopting the frame's own file failed; fetching")
                }
            }
        }
    }
    // Identical content already landed by another frame (P24): linked.
    match identical_landed(&env.ctx, &row) {
        Ok(Some(src)) => {
            let project = match landing_project(env, &row) {
                Ok(p) => p,
                Err(landed) => return Prepared::Settled(landed),
            };
            let hooks = crate::sharing::iroh::blobs::ExportHooks::default();
            let started_at = crate::sync::now_iso();
            let landing = LandingEnv {
                ctx: &env.ctx,
                node: &env.node,
                store: &env.store,
                project: &project,
                collab_root: &env.root,
                guard: &env.guard,
                started_at: &started_at,
                hooks: &hooks,
                // Linked, not fetched: no peer delivered anything.
                sources: &[],
            };
            return Prepared::Settled(link_identical(&landing, &row, &src).await);
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(project_id = pid, frame_uuid = uuid, error = %e, "identical-content lookup failed; fetching")
        }
    }
    let tag = project_frame_in_flight_tag(pid, uuid, content_version);
    if let Err(e) = env
        .store
        .tags()
        .set(&tag, iroh_blobs::HashAndFormat::raw(hash))
        .await
    {
        tracing::warn!(project_id = pid, frame_uuid = uuid, error = %e, "in-flight tag not set; the partial bytes are not protected from the GC");
    }
    Prepared::Fetch(row)
}

/// Does the row's own landed file hold the frame: the recorded stamp, else
/// size + xxh3 — never for a previous version's file.
async fn own_file_holds(env: &ExecEnv, row: &LocalFrameRow, path: &std::path::Path) -> bool {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return false;
    };
    if !meta.is_file() || meta.len() as i64 != row.byte_size {
        return false;
    }
    if let Some(stamp) = row
        .size_mtime_seen
        .as_deref()
        .and_then(crate::collab::storage::sweep::Stamp::parse)
    {
        return matches!(
            crate::collab::storage::sweep::stat_verdict(path, Some(stamp)),
            crate::collab::storage::sweep::StatVerdict::Same
        );
    }
    let previous = db(&env.ctx).and_then(|d| {
        Ok(frames_db::prev_stamp(
            &d.conn(),
            &row.project_id,
            &row.frame_uuid,
        )?)
    });
    if !matches!(previous, Ok(None)) {
        return false;
    }
    matches!(crate::api::collab_exchange::xxh3_on_blocking(path).await, Ok(h) if h == row.xxh3)
}

/// Land one fetched frame through the moved wave-2 landing (Task 11).
async fn land(
    env: &ExecEnv,
    row: &LocalFrameRow,
    hash: Hash,
    started_at: &str,
    sources: &[(String, u64)],
) -> Landed {
    #[cfg(test)]
    if test_hooks::take_landing_panic(&env.root) {
        panic!("injected landing panic (test hook)");
    }
    let project = match landing_project(env, row) {
        Ok(p) => p,
        Err(landed) => return landed,
    };
    let hooks = crate::sharing::iroh::blobs::ExportHooks::default();
    #[cfg(test)]
    if test_hooks::take_landing_fault(&env.root) {
        // Whichever export this landing takes (DIRECT or TEMP) fails once.
        hooks.fail_direct_export_once();
        hooks.fail_after_export_once();
    }
    let landing = LandingEnv {
        ctx: &env.ctx,
        node: &env.node,
        store: &env.store,
        project: &project,
        collab_root: &env.root,
        guard: &env.guard,
        started_at,
        hooks: &hooks,
        sources,
    };
    let landed = land_frame(&landing, row, hash).await;
    if matches!(landed, Landed::Failed(_)) {
        match crate::api::db(&env.ctx) {
            Ok(db) => {
                if let Err(e) = crate::db::collab_sessions::record_failure(
                    &db.conn(),
                    &row.project_id,
                    &crate::sync::now_iso(),
                ) {
                    tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "receive session failure not recorded");
                }
            }
            Err(e) => {
                tracing::warn!(project_id = %row.project_id, error = %e, "receive session failure not recorded")
            }
        }
    }
    landed
}

/// Test hooks (Task 15 fix round 1, I2): the next landing under a root
/// panics, once.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static PANIC_NEXT_LANDING: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

    pub(crate) fn panic_next_landing(root: &Path) {
        PANIC_NEXT_LANDING
            .lock()
            .unwrap()
            .get_or_insert_with(HashSet::new)
            .insert(root.to_path_buf());
    }

    static PANIC_RUNS: Mutex<Option<std::collections::HashMap<PathBuf, usize>>> = Mutex::new(None);

    /// Every live run under `root` panics at its start, until
    /// [`stop_run_panics`]; returns nothing, counts them.
    pub(crate) fn panic_every_run(root: &Path) {
        PANIC_RUNS
            .lock()
            .unwrap()
            .get_or_insert_with(Default::default)
            .insert(root.to_path_buf(), 0);
    }

    /// Stop the run panics under `root`; how many runs panicked.
    pub(crate) fn stop_run_panics(root: &Path) -> usize {
        PANIC_RUNS
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|m| m.remove(root))
            .unwrap_or(0)
    }

    pub(super) fn take_run_panic(root: &Path) -> bool {
        match PANIC_RUNS
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|m| m.get_mut(root))
        {
            Some(n) => {
                *n += 1;
                true
            }
            None => false,
        }
    }

    pub(crate) fn landing_panic_pending(root: &Path) -> bool {
        PANIC_NEXT_LANDING
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.contains(root))
    }

    pub(super) fn take_landing_panic(root: &Path) -> bool {
        PANIC_NEXT_LANDING
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|s| s.remove(root))
    }

    static FAULT_NEXT_LANDING: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

    /// Task 18 (spec §12 "v2 landing interrupted"): the next landing under
    /// `root` fails inside its export, once — the DIRECT export before it
    /// touches the target, the TEMP one between its export and its rename.
    pub(crate) fn fail_next_landing(root: &Path) {
        FAULT_NEXT_LANDING
            .lock()
            .unwrap()
            .get_or_insert_with(HashSet::new)
            .insert(root.to_path_buf());
    }

    /// Whether the fault armed by [`fail_next_landing`] is still unspent.
    pub(crate) fn landing_fault_pending(root: &Path) -> bool {
        FAULT_NEXT_LANDING
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.contains(root))
    }

    pub(super) fn take_landing_fault(root: &Path) -> bool {
        FAULT_NEXT_LANDING
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|s| s.remove(root))
    }

    /// One `Start` or provider update the executor carried out (Task 18):
    /// spec §12 defines "starts fetching" as the first request, which the
    /// executor issues right after the core's `Start`.
    #[derive(Debug, Clone)]
    pub(crate) struct Sighting {
        pub at: std::time::Instant,
        pub frame_uuid: String,
        /// The provider devices the command named (hub device ids).
        pub providers: Vec<String>,
        /// `true` for a `Start`, `false` for an `UpdateProviders`.
        pub start: bool,
    }

    type Sightings = std::collections::HashMap<crate::sharing::types::NodeId, Vec<Sighting>>;
    static SIGHTINGS: Mutex<Option<Sightings>> = Mutex::new(None);

    pub(super) fn record_sighting(
        node: crate::sharing::types::NodeId,
        frame_uuid: &str,
        providers: &[crate::collab::scheduler::core::ProviderRef],
        start: bool,
    ) {
        SIGHTINGS
            .lock()
            .unwrap()
            .get_or_insert_with(Default::default)
            .entry(node)
            .or_default()
            .push(Sighting {
                at: std::time::Instant::now(),
                frame_uuid: frame_uuid.to_string(),
                providers: providers.iter().map(|p| p.device.clone()).collect(),
                start,
            });
    }

    /// Every sighting of `node`'s executor so far, in order.
    pub(crate) fn sightings(node: &crate::sharing::types::NodeId) -> Vec<Sighting> {
        SIGHTINGS
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|m| m.get(node).cloned())
            .unwrap_or_default()
    }
}

/// The frame's project row as it is now (a landing names its folder).
fn landing_project(env: &ExecEnv, row: &LocalFrameRow) -> Result<CollabProjectRow, Landed> {
    match db(&env.ctx).and_then(|d| Ok(crate::db::collab::get_project(&d.conn(), &row.project_id)?))
    {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err(Landed::Stale),
        Err(e) => {
            tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "the project could not be read for a landing");
            Err(Landed::Failed(e.to_string()))
        }
    }
}

#[cfg(all(test, feature = "render", feature = "solver"))]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;

    /// Task 15 R2: the live need set is `wanted` rows only, and nothing at
    /// all while the storage cannot be written or auto-replication is off.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_need_set_is_wanted_rows_only_behind_the_storage_and_the_toggle() {
        let (_tmp, ctx, hub) = ts::signed_in_rig().await;
        hub.seed_frames(ts::PID, "acc-o", &["w1", "w2", "q1", "i1"], "published");
        let conn = db(&ctx).unwrap().conn();
        for u in ["w1", "w2", "q1", "i1"] {
            let view = hub.frame(ts::PID, u).unwrap();
            frames_db::upsert_from_manifest(&conn, ts::PID, &view).unwrap();
        }
        frames_db::set_local_state(&conn, ts::PID, "q1", LocalState::Quarantined).unwrap();
        frames_db::set_local_state(&conn, ts::PID, "i1", LocalState::Idle).unwrap();
        frames_db::set_awaiting_gc(&conn, ts::PID, "w2", true).unwrap();
        let project = crate::db::collab::get_project(&conn, ts::PID)
            .unwrap()
            .unwrap();
        let uuids = |ws: Vec<Want>| ws.into_iter().map(|w| w.key.1).collect::<Vec<_>>();
        assert_eq!(
            uuids(need_wants(&conn, &project, true).unwrap()),
            vec!["w1"]
        );
        assert!(
            need_wants(&conn, &project, false).unwrap().is_empty(),
            "storage gate"
        );
        crate::db::collab::set_auto_replicate(&conn, ts::PID, false).unwrap();
        let off = crate::db::collab::get_project(&conn, ts::PID)
            .unwrap()
            .unwrap();
        assert!(
            need_wants(&conn, &off, true).unwrap().is_empty(),
            "toggle gate"
        );
        let mut send_only = project.clone();
        send_only.data_role = "send".into();
        send_only.is_coordinator = false;
        assert!(
            need_wants(&conn, &send_only, true).unwrap().is_empty(),
            "role gate: a send-only member never replicates"
        );
    }

    /// Task 15 (replaces the wave-2 `second_pass_after_delete_waits_for_gc_then_refetches`):
    /// a frame a landing parked for the GC (`awaiting_gc`, no stamp) is
    /// released into the need set once its store entry is gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_gc_probe_releases_a_frame_whose_dead_entry_went() {
        let (_tmp, ctx, hub) = ts::signed_in_rig().await;
        let ctx = Arc::new(ctx);
        hub.seed_frames(ts::PID, "acc-o", &["g1"], "published");
        {
            let conn = db(&ctx).unwrap().conn();
            let view = hub.frame(ts::PID, "g1").unwrap();
            frames_db::upsert_from_manifest(&conn, ts::PID, &view).unwrap();
            frames_db::set_awaiting_gc(&conn, ts::PID, "g1", true).unwrap();
        }
        let node = ctx.iroh_node.lock().await.clone().unwrap();
        let root = ts::collab_root(&ctx);
        // M4 (fix round 1): g2's entry is DEAD — complete per the store, its
        // referenced file gone — so the GC has not dropped it yet: it stays
        // parked.
        let big: Vec<u8> = (0..40 * 1024u32).map(|i| (i % 251) as u8).collect();
        let g2 = root.join("m31").join("g2.fits");
        ts::land_frame(&ctx, &hub, &node, "g2", &g2, &big).await;
        std::fs::remove_file(&g2).unwrap();
        let g2_hash: Hash = blake3::hash(&big).to_hex().parse().unwrap();
        assert_eq!(
            node.collab_blob_health(g2_hash).await.unwrap(),
            BlobHealth::Dead
        );
        {
            // as a landing parks it (P20): wanted, no stamp, awaiting the GC
            let conn = db(&ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET local_state = 'wanted', size_mtime_seen = NULL
                 WHERE project_id = ?1 AND frame_uuid = 'g2'",
                rusqlite::params![ts::PID],
            )
            .unwrap();
            frames_db::set_awaiting_gc(&conn, ts::PID, "g2", true).unwrap();
            assert_eq!(frames_db::awaiting_gc_released(&conn).unwrap().len(), 2);
        }
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        let mut exec = Executor::new(
            ExecEnv {
                ctx: Arc::clone(&ctx),
                store: node.collab_store().unwrap(),
                node,
                root: root.clone(),
                guard: Arc::new(StoreGuard::new(root, me, None)),
                control: Arc::new(InboundControl::new()),
                meter: Arc::new(ExchangeMeter::new()),
                to_go: Default::default(),
            },
            2,
            7,
        );
        exec.gc_probe().await;
        exec.settle_writes().await; // the release is written off the loop
        let row = frames_db::get(&db(&ctx).unwrap().conn(), ts::PID, "g1")
            .unwrap()
            .unwrap();
        assert!(!row.awaiting_gc, "released: its entry is gone");
        assert!(exec.dirty.contains(ts::PID), "its need set is re-read");
        let g2 = frames_db::get(&db(&ctx).unwrap().conn(), ts::PID, "g2")
            .unwrap()
            .unwrap();
        assert!(g2.awaiting_gc, "a dead entry keeps its frame parked");
        exec.shutdown();
    }

    /// Fix round 2 (M-2): the GC probe looks at a capped batch per probe,
    /// round-robin — two dead entries ahead of a released one never starve
    /// it: with a batch of one, the third probe reaches it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_gc_probe_is_capped_and_round_robin() {
        let (_tmp, ctx, hub) = ts::signed_in_rig().await;
        let ctx = Arc::new(ctx);
        let node = ctx.iroh_node.lock().await.clone().unwrap();
        let root = ts::collab_root(&ctx);
        for (u, seed) in [("r1", 7u32), ("r2", 11u32)] {
            let big: Vec<u8> = (0..40 * 1024u32)
                .map(|i| ((i * seed) % 251) as u8)
                .collect();
            let p = root.join("m31").join(format!("{u}.fits"));
            ts::land_frame(&ctx, &hub, &node, u, &p, &big).await;
            std::fs::remove_file(&p).unwrap(); // a dead entry: stays parked
        }
        hub.seed_frames(ts::PID, "acc-o", &["r3"], "published");
        {
            let conn = db(&ctx).unwrap().conn();
            let view = hub.frame(ts::PID, "r3").unwrap();
            frames_db::upsert_from_manifest(&conn, ts::PID, &view).unwrap();
            conn.execute(
                "UPDATE project_frames_local SET local_state = 'wanted', size_mtime_seen = NULL
                 WHERE project_id = ?1",
                rusqlite::params![ts::PID],
            )
            .unwrap();
            for u in ["r1", "r2", "r3"] {
                frames_db::set_awaiting_gc(&conn, ts::PID, u, true).unwrap();
            }
        }
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        let mut exec = Executor::new(
            ExecEnv {
                ctx: Arc::clone(&ctx),
                store: node.collab_store().unwrap(),
                node,
                root: root.clone(),
                guard: Arc::new(StoreGuard::new(root, me, None)),
                control: Arc::new(InboundControl::new()),
                meter: Arc::new(ExchangeMeter::new()),
                to_go: Default::default(),
            },
            2,
            7,
        );
        let parked = |u: &str| {
            frames_db::get(&db(&ctx).unwrap().conn(), ts::PID, u)
                .unwrap()
                .unwrap()
                .awaiting_gc
        };
        exec.gc_probe_batch(1).await; // r1: dead
        exec.gc_probe_batch(1).await; // r2: dead
        exec.settle_writes().await;
        assert!(parked("r3"), "not probed yet");
        exec.gc_probe_batch(1).await; // r3: its entry is gone
        exec.settle_writes().await; // the release is written off the loop
        assert!(!parked("r3"), "released on its turn");
        assert!(parked("r1") && parked("r2"));
        exec.gc_probe_batch(1).await; // wrapped: r1 again
        exec.settle_writes().await;
        assert!(parked("r1"));
        exec.shutdown();
    }

    /// Task 15 R2: a quarantined frame's partial bytes lose their
    /// in-flight tag (nothing will resume them).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quarantined_frame_loses_its_in_flight_tag() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, _) = rig.frames[0].clone();
        let store = rig.node.collab_store().unwrap();
        let tag = project_frame_in_flight_tag(&pid, &uuid, 1);
        store
            .tags()
            .set(&tag, iroh_blobs::HashAndFormat::raw(rig.hash_of(0)))
            .await
            .unwrap();
        let me = crate::api::account::own_device_id(&rig.ctx).unwrap();
        let mut exec = Executor::new(
            ExecEnv {
                ctx: Arc::clone(&rig.ctx),
                node: Arc::clone(&rig.node),
                store: store.clone(),
                root: rig.root.clone(),
                guard: Arc::new(StoreGuard::new(rig.root.clone(), me, None)),
                control: Arc::new(InboundControl::new()),
                meter: Arc::new(ExchangeMeter::new()),
                to_go: Default::default(),
            },
            2,
            7,
        );
        exec.forget_partial(&pid, &uuid);
        let deadline = Instant::now() + Duration::from_secs(5);
        while store.tags().get(tag.as_bytes()).await.unwrap().is_some() {
            assert!(Instant::now() < deadline, "the in-flight tag is dropped");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        exec.shutdown();
    }

    /// An executor on a signed-in rig holding the collab lane, whose live
    /// run 7 has already ended (its queue closed) with no end reaped yet,
    /// and one item `y1` being prepared.
    async fn a_run_that_ended_unreaped() -> (tempfile::TempDir, Executor, String, FrameKey) {
        let (tmp, ctx, _hub) = ts::signed_in_rig().await;
        let ctx = Arc::new(ctx);
        let node = ctx.iroh_node.lock().await.clone().unwrap();
        let root = ts::collab_root(&ctx);
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        let control = Arc::new(InboundControl::new());
        let permit = control.receive_gate.acquire_collab().await;
        let mut exec = Executor::new(
            ExecEnv {
                ctx: Arc::clone(&ctx),
                store: node.collab_store().unwrap(),
                node,
                root: root.clone(),
                guard: Arc::new(StoreGuard::new(root, me, None)),
                control,
                meter: Arc::new(ExchangeMeter::new()),
                to_go: Default::default(),
            },
            2,
            7,
        );
        exec.permit = Some(permit);
        let (items, dead) = mpsc::channel(1);
        drop(dead);
        exec.run_seq = 7;
        exec.run = Some(Run { id: 7, items });
        let key: FrameKey = (ts::PID.to_string(), "y1".to_string());
        let name = item_name(&key, 1);
        let hash = Hash::new(b"y1");
        exec.items.insert(
            name.clone(),
            Item {
                key: key.clone(),
                fetch_id: 1,
                content_version: 1,
                blake3: hash.to_hex().to_string(),
                hash,
                phase: Phase::Preparing,
                providers: watch::channel(Arc::new(Vec::new())).0,
                cancel: watch::channel(false).0,
                cancelled: None,
                row: None,
                started_at: crate::sync::now_iso(),
            },
        );
        (tmp, exec, name, key)
    }

    /// Final fix A-M3: a fetch prepared just after its run ended at a YIELD
    /// (the queue closed, `RunEnded` not reaped yet) is never struck: it
    /// waits for the run's end, which cancels it (re-queued at once, no
    /// back-off) with every other item the run left.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fetch_prepared_after_a_yield_ended_its_run_is_cancelled_not_struck() {
        let (_tmp, mut exec, name, key) = a_run_that_ended_unreaped().await;
        exec.on_prepared(
            name.clone(),
            Prepared::Fetch(replica("y1", "2026-01-01 00:00:00", 10)),
        )
        .await;
        assert!(
            exec.finished.is_empty(),
            "no verdict before the run's end: {:?}",
            exec.finished
        );
        assert_eq!(exec.items[&name].phase, Phase::Sent { run: 7 });
        assert!(exec.run.is_none(), "the next fetch starts a new run");
        exec.on_event(ExecEvent::RunEnded { run: 7 }).await;
        assert_eq!(exec.finished, vec![(key, FetchResult::Cancelled)]);
        exec.shutdown();
    }

    /// Fix round 2 kept: the same window after a run PANICKED fails the
    /// fetch (it backs off) — the panic's reaping rules on it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fetch_prepared_after_a_run_panicked_fails() {
        let (_tmp, mut exec, name, key) = a_run_that_ended_unreaped().await;
        exec.on_prepared(
            name.clone(),
            Prepared::Fetch(replica("y1", "2026-01-01 00:00:00", 10)),
        )
        .await;
        exec.end_run(7, FetchResult::Failed).await; // what the panic's reaping does
        assert_eq!(exec.finished, vec![(key, FetchResult::Failed)]);
        exec.shutdown();
    }

    /// Task 12 fix round 1: a shutdown (stop, remount, restart, crash)
    /// finishes every fetch it drops in the node-owned meter — no frozen
    /// in-flight row, no 1 Hz wake of the next runtime forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shutdown_leaves_nothing_in_flight_in_the_meter() {
        let (_tmp, mut exec, name, key) = a_run_that_ended_unreaped().await;
        let meter = Arc::clone(&exec.env.meter);
        meter.register(&name, FlowDirection::Recv, &key.0, &key.1, "y1.fits", 100);
        meter.delivered(&name, "DEV=", 10, Instant::now());
        assert!(meter.any_in_flight());
        exec.shutdown();
        let now = Instant::now();
        assert!(!meter.any_in_flight(), "the dropped fetch left the meter");
        assert!(
            !meter
                .needs_progress(now + crate::collab::live::meter::MOVING + Duration::from_secs(1)),
            "nothing keeps waking the next runtime"
        );
    }

    /// Task 12 fix round 1 (spec §6.2): "to go" is published from the
    /// scheduler whenever its wants change — a project with wanted frames
    /// and no traffic reads its count, a gone project is pruned.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn to_go_follows_the_schedulers_wants_without_any_traffic() {
        let (_tmp, mut exec, _name, _key) = a_run_that_ended_unreaped().await;
        let want = |u: &str| Want {
            key: (ts::PID.to_string(), u.to_string()),
            content_version: 1,
            blake3: format!("b-{u}"),
            byte_size: 10,
            since_ms: 0,
        };
        let to_go = Arc::clone(&exec.env.to_go);
        exec.step(Input::NeedSet {
            project_id: ts::PID.to_string(),
            wants: vec![want("w1"), want("w2")],
        });
        assert_eq!(to_go.read().unwrap().get(ts::PID), Some(&2));
        exec.step(Input::NeedSet {
            project_id: ts::PID.to_string(),
            wants: vec![want("w1")],
        });
        assert_eq!(to_go.read().unwrap().get(ts::PID), Some(&1));
        exec.step(Input::ProjectGone {
            project_id: ts::PID.to_string(),
        });
        assert!(to_go.read().unwrap().is_empty(), "a gone project is pruned");
        exec.shutdown();
    }

    // ── the pure need set (moved from the wave-2 `collab_exchange`) ─────

    fn replica(uuid: &str, created: &str, size: i64) -> LocalFrameRow {
        LocalFrameRow {
            project_id: "p1".into(),
            frame_uuid: uuid.into(),
            content_version: 1,
            origin: crate::db::collab_frames::FrameOrigin::Replica,
            publisher_account_id: "acc-o".into(),
            publisher_display: "Other".into(),
            file_name: format!("{uuid}.fits"),
            filter_canonical: "L".into(),
            state: "published".into(),
            accepted: true,
            byte_size: size,
            xxh3: "0".repeat(16),
            blake3: "0".repeat(64),
            manifest_version: 1,
            manifest_json: serde_json::json!({ "createdAt": created, "meta": {} }).to_string(),
            landed_path: None,
            size_mtime_seen: None,
            on_disk: false,
            awaiting_gc: false,
            source_frame_id: None,
            recipe_hash: None,
            last_error: None,
            updated_at: String::new(),
            local_state: LocalState::Wanted,
            frame_seq: None,
        }
    }

    fn uuids(v: &[LocalFrameRow]) -> Vec<&str> {
        v.iter().map(|r| r.frame_uuid.as_str()).collect()
    }

    fn all() -> ReplicationPolicy {
        ReplicationPolicy::default()
    }

    /// Spec §7.1 / Task 15 R2: only a published, accepted peer frame that is
    /// `wanted` and not waiting for the GC is needed — never an own, not
    /// kept, quarantined, idle, missing, awaiting-choice or held one.
    #[test]
    fn need_is_wanted_published_accepted_replicas_only() {
        let ok = replica("ok", "t", 10);
        let mut own = replica("own", "t", 10);
        own.origin = crate::db::collab_frames::FrameOrigin::Own;
        let mut unaccepted = replica("unaccepted", "t", 10);
        unaccepted.accepted = false;
        let mut pending = replica("pending", "t", 10);
        pending.state = "pending".into();
        let mut rejected = replica("rejected", "t", 10);
        rejected.state = "rejected".into();
        let mut awaiting = replica("awaiting", "t", 10);
        awaiting.awaiting_gc = true;
        let mut rows = vec![ok, own, unaccepted, pending, rejected, awaiting];
        for (u, st) in [
            ("not_kept", LocalState::NotKept),
            ("quarantined", LocalState::Quarantined),
            ("idle", LocalState::Idle),
            ("missing", LocalState::Missing),
            ("choice", LocalState::AwaitingChoice),
            ("held", LocalState::Held),
        ] {
            let mut r = replica(u, "t", 10);
            r.local_state = st;
            rows.push(r);
        }
        assert_eq!(uuids(&frame_need(&rows, &all())), vec!["ok"]);
    }

    /// Oldest first (`createdAt`), then by uuid — the order the byte budget
    /// is spent in (the scheduler ranks rarest first itself).
    #[test]
    fn need_is_oldest_first() {
        let rows = vec![
            replica("a", "2026-09-01T00:00:01Z", 10),
            replica("b", "2026-09-01T00:00:03Z", 10),
            replica("c", "2026-09-01T00:00:02Z", 10),
            replica("d", "2026-09-01T00:00:00Z", 10),
        ];
        assert_eq!(uuids(&frame_need(&rows, &all())), vec!["d", "a", "c", "b"]);
    }

    /// The budget counts the replicas already on disk (own frames are not
    /// replicas) and stops at the first frame that would cross it.
    #[test]
    fn byte_budget_counts_already_held_bytes() {
        let mut held = replica("held", "t0", 600);
        held.on_disk = true;
        held.local_state = LocalState::Held;
        let mut own = replica("own", "t0", 5000);
        own.origin = crate::db::collab_frames::FrameOrigin::Own;
        own.on_disk = true;
        let rows = vec![
            held,
            own,
            replica("a", "t1", 300),
            replica("b", "t2", 200),
            replica("c", "t3", 10),
        ];
        let policy = ReplicationPolicy {
            byte_budget: Some(1000),
            ..all()
        };
        assert_eq!(
            uuids(&frame_need(&rows, &policy)),
            vec!["a"],
            "600 held + 300 fits, + 200 would not — and the walk stops there"
        );
        let roomy = ReplicationPolicy {
            byte_budget: Some(1100),
            ..all()
        };
        assert_eq!(uuids(&frame_need(&rows, &roomy)), vec!["a", "b"]);
    }

    /// Filters, publishers, FWHM and star bounds each narrow the set; a
    /// frame without the measurement never matches a set bound.
    #[test]
    fn policy_filters_by_canonical_filter_publisher_fwhm_stars() {
        let with_meta = |uuid: &str, filter: &str, publisher: &str, meta: serde_json::Value| {
            let mut r = replica(uuid, uuid, 10);
            r.filter_canonical = filter.into();
            r.publisher_account_id = publisher.into();
            r.manifest_json = serde_json::json!({ "createdAt": uuid, "meta": meta }).to_string();
            r
        };
        let rows = vec![
            with_meta(
                "1",
                "L",
                "acc-o",
                serde_json::json!({ "fwhmArcsec": 2.0, "starsDetected": 100 }),
            ),
            with_meta(
                "2",
                "Ha",
                "acc-o",
                serde_json::json!({ "fwhmArcsec": 4.0, "starsDetected": 10 }),
            ),
            with_meta("3", "L", "acc-x", serde_json::json!({})),
        ];
        let need = |p: ReplicationPolicy| {
            uuids(&frame_need(&rows, &p))
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(need(all()), vec!["1", "2", "3"]);
        assert_eq!(
            need(ReplicationPolicy {
                filters: vec!["Ha".into()],
                ..all()
            }),
            vec!["2"]
        );
        assert_eq!(
            need(ReplicationPolicy {
                publishers: vec!["acc-x".into()],
                ..all()
            }),
            vec!["3"]
        );
        assert_eq!(
            need(ReplicationPolicy {
                max_fwhm_arcsec: Some(3.0),
                ..all()
            }),
            vec!["1"],
            "4.0 is over the bound and a missing FWHM never matches"
        );
        assert_eq!(
            need(ReplicationPolicy {
                min_stars: Some(50),
                ..all()
            }),
            vec!["1"]
        );
    }
}
