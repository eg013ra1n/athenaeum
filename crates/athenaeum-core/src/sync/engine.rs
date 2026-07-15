//! The sync engine: a tokio worker task driving the outbound state machine over
//! a [`SharingTransport`], plus the [`SyncEngineHandle`] the app talks to.
//!
//! # Ownership of the transport
//!
//! Each [`SyncEngine`] owns exactly one transport endpoint and takes its
//! single-consumer [`events`](SharingTransport::events) stream exactly once (at
//! worker start). Crash-resume therefore constructs a *new* engine over a *new*
//! endpoint — never re-subscribing a spent stream. The peer relearns the new
//! endpoint's node id from the re-announce it receives, so no identity has to be
//! persisted.
//!
//! # Transitions
//!
//! - `enqueue → Queued` (the worker inserts the row) → the worker `serve`s +
//!   announces → `Announced` → `Transferring`.
//! - A **first-attempt** build / `serve` / `announce` failure — the normal
//!   "peer is asleep when we queue" case — is retryable, never fatal. The package
//!   keeps a pending retry slot with a deadline and stays `Queued` until an
//!   announce succeeds, so [`handle_timeouts`](Worker::handle_timeouts) re-drives
//!   it on a capped exponential backoff ([`retry_backoff`], spec §2). It
//!   eventually announces when the peer comes online; a network-unreachable peer
//!   never terminalizes it — delivery is retried forever, never left non-terminal
//!   with no retry slot.
//! - The sender observes no `FetchProgress` on loopback (fetch is
//!   receiver-driven), so `Transferring` is marked on the first *successful*
//!   announce — the in-flight window during which we await the ack. A
//!   `FetchProgress` arm remains for real transports and is a no-op here.
//! - `AckReceived → Confirmed` (idempotent: a second ack for a package no longer
//!   in the in-flight map is logged at debug and dropped).
//! - Per-package ack/retry timeout → climb one backoff rung and wait it out,
//!   then re-announce ([`retry_backoff`]). A network condition (no ack, offline
//!   peer, serve/announce error) never marks a package `Failed`; it backs off and
//!   retries indefinitely. A persistently-erroring re-announce re-arms its
//!   deadline every time, so it advances the backoff rather than busy-spinning.
//! - [`cancel`](SyncEngineHandle::cancel) → `Failed` with a `cancelled` history
//!   outcome.
//! - Spec §1's one non-network terminal path: if the package dir/payload has
//!   vanished from disk, [`attempt`](Worker::attempt) fails it immediately via
//!   [`fail_package`](Worker::fail_package) — re-announcing can never succeed no
//!   matter how long it backs off, so this (and `cancel`) are the only ways to
//!   reach `Failed`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::events::{emit_event, ProgressEmitter};
use crate::package::{self, ManifestRecord, MANIFEST_FILENAME};
use crate::sharing::iroh::node::BoxFuture;
use crate::sharing::iroh::proto::OfferEntry;
use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, ReceiptOutcome, TransportEvent,
};
use crate::sharing::SharingTransport;

use super::models::{Direction, HistoryRow, OutboundRow, OutboundState};
use super::receiver::{SyncFinishedEvent, SyncProgressEvent};
use super::store::SyncStore;
use super::{node_id_hex, now_iso};

/// Retry-time peer-address re-resolver (iroh hardening T8). Given the engine's
/// peer, yields the peer's CURRENT dialable [`EndpointAddr`](iroh::EndpointAddr)
/// — the app re-fetches the peer's hub-reported address + fresh relays, Perseus
/// re-resolves its target — or `None` when it can't be refreshed right now (hub
/// blip / peer gone). [`handle_timeouts`](Worker::handle_timeouts) awaits it
/// before a re-attempt so a stale cached address doesn't doom every retry to the
/// same dead path; on `Some(addr)` the engine re-registers it on the transport
/// ([`SharingTransport::add_peer_addr`]). `None` for tests + single-shot spawners.
pub type AddrRefresher =
    Arc<dyn Fn(NodeId) -> BoxFuture<Option<iroh::EndpointAddr>> + Send + Sync>;

/// Default per-attempt wait for the peer's ack before retrying.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Far-future sleep target used when nothing is in flight (any command/event
/// wakes the worker before it elapses).
const IDLE_SLEEP: Duration = Duration::from_secs(3600);

/// Spec §2: capped exponential backoff, expressed as multiples of the base
/// rung (ack_timeout) so tests with short timeouts scale down naturally.
/// 30s → 1m → 5m → 15m → 30m with the default 30s base.
const BACKOFF_MULTIPLIERS: [u32; 5] = [1, 2, 10, 30, 60];

/// The backoff window for a given `rung`, as `base * multiplier[rung]` with the
/// last multiplier held as the cap. Delivery is retried forever (spec §2): a
/// network-unreachable peer never terminalizes a package, it just sits at the
/// 30-minute cap. `base` is the engine's [`SyncConfig::ack_timeout`] so a test
/// with a millisecond timeout gets a proportionally short schedule.
pub fn retry_backoff(base: Duration, rung: u32) -> Duration {
    let m = BACKOFF_MULTIPLIERS[(rung as usize).min(BACKOFF_MULTIPLIERS.len() - 1)];
    base * m
}

/// Wall-clock rendering of a retry deadline `delay` from now for the persisted
/// `OutboundRow::next_retry_at` (Task 2): `Utc::now() + delay`, formatted exactly
/// like the store's `created_at` (RFC3339 UTC, millisecond precision, `Z`) so the
/// UI reads one timestamp shape across the whole sync surface. A `delay` that
/// overflows `chrono`'s range degrades to `now` rather than panicking.
fn retry_deadline_stamp(delay: Duration) -> String {
    let at = chrono::Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default();
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// What the per-package deadline means when it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NextAction {
    /// Announce succeeded; deadline = ack wait. Firing = ack timed out.
    AwaitAck,
    /// Waiting out a backoff window. Firing = attempt the announce now.
    Retry,
}

/// Tunables for the engine worker.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// How long to wait for an ack before backing off. Doubles as the base rung
    /// of the retry backoff schedule ([`retry_backoff`]).
    pub ack_timeout: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            ack_timeout: DEFAULT_ACK_TIMEOUT,
        }
    }
}

/// Commands the handle sends to the worker.
enum Command {
    /// Enqueue + drive a package directory. The worker inserts the `Queued` row
    /// itself (so a fresh enqueue and the startup crash-resume enumeration can
    /// never both drive the same row) and replies with the new id.
    Process {
        dir: PathBuf,
        reply: oneshot::Sender<Result<i64>>,
    },
    /// Cancel an in-flight package → `Failed` (`cancelled`).
    Cancel(i64),
    /// Stop the worker loop.
    Shutdown,
}

/// In-flight bookkeeping for one package. Keyed in the worker's map by the
/// durable row `id` (not the per-session `package_id`) so a slot can exist even
/// before an announce has been successfully minted — the retry safety net for a
/// first-attempt failure against an offline peer.
struct Pending {
    id: i64,
    dir: PathBuf,
    /// The announce minted for this package. `None` until the first successful
    /// build (a prior build failure leaves it `None` and the next attempt
    /// rebuilds); once `Some`, its `package_id` is stable across retries and is
    /// what an [`AckReceived`](TransportEvent::AckReceived) correlates against.
    announce: Option<PackageAnnounce>,
    /// Whether the transfer-start milestone (`→ Transferring` + the `sent`
    /// history rows) has been recorded. `false` while the first announce has not
    /// yet succeeded (peer offline); the first successful announce records it.
    started: bool,
    /// When the transfer-start history row was written, reused as `started_at`
    /// on the confirm/terminal rows.
    started_at: String,
    /// When the next deadline fires (an ack wait, or a backoff window).
    deadline: Instant,
    /// Current backoff rung (spec §2). Climbs on every ack timeout / failed
    /// announce, never resets; indexes [`retry_backoff`]'s capped schedule.
    rung: u32,
    /// What [`deadline`](Self::deadline) firing means: waiting for the peer's ack
    /// ([`NextAction::AwaitAck`]) vs. waiting out a backoff window before the next
    /// announce ([`NextAction::Retry`]).
    next_action: NextAction,
    /// Frame uuids the most recent ack rejected, if any (task A7 fix-review).
    /// Empty until a partial ack arrives; overwritten (not accumulated) by each
    /// subsequent partial ack so it always reflects the latest verdict. Named
    /// in the terminal history outcome if the package is eventually `Failed`.
    last_rejected: Vec<String>,
    /// The dedup-negotiated want subset (Sync Phase 3): `None` = full send (the
    /// pre-dedup behavior + the best-effort fallback when the handshake is
    /// unavailable or errors), `Some(rel_paths)` = only the frames the peer
    /// still wants. Decided ONCE on the first build (announce `None`) and reused
    /// across every retry so a retry re-serves the same subset instead of
    /// re-negotiating. Also filters the started-history + the `on_ack`
    /// completeness check to exactly the frames actually sent.
    want: Option<HashSet<String>>,
    /// Frames actually sent to the peer this batch (`new`) vs. dropped as the
    /// peer's duplicates (`duplicate`), fixed at negotiate time and reported on
    /// the sender's `sync-finished` `{new, duplicate}` outcome.
    new_count: u32,
    duplicate_count: u32,
    /// Collab exchange (slice 4): the project id / HUB package uuid read from the
    /// manifest's [`ProjectStamp`](crate::package::ProjectStamp) on the first
    /// build. `Some` marks this as a PROJECT package — the announce goes out via
    /// [`announce_project`](SharingTransport::announce_project) and the Offer/Want
    /// dedup negotiation is skipped (full send). Both `None` for personal sync.
    project_id: Option<String>,
    hub_package_id: Option<String>,
    /// Snapshot of the manifest records read on the first successful build,
    /// cached so a terminal-fail epilogue (`fail_package`) can still name every
    /// frame in the `failed` history outcome even when the package dir has since
    /// vanished from disk (the missing-payload terminal path) and a live
    /// [`package::read_manifest`] re-read is no longer possible. `None` until the
    /// first successful manifest read; never re-read after that (same "decided
    /// once, reused across retries" discipline as `want`).
    manifest_records: Option<Vec<ManifestRecord>>,
}

/// Outcome of the first-build dedup handshake
/// ([`negotiate_and_build`](Worker::negotiate_and_build)).
enum Negotiated {
    /// Serve + announce this (subset-or-full) announce; `want` is the negotiated
    /// subset (`None` = full send / best-effort fallback).
    Send {
        announce: PackageAnnounce,
        want: Option<HashSet<String>>,
    },
    /// The peer already has every frame — the package was terminalized in place
    /// (confirmed) with no announce; the caller returns without serving.
    AllDuplicate,
    /// A manifest/announce build error already logged + armed a retry; the
    /// caller returns and lets `handle_timeouts` re-drive the slot.
    Deferred,
}

/// Optional host sink notified when a package reaches a **terminal** state
/// (confirmed, failed, or cancelled) on this engine.
///
/// # Why this exists
///
/// Perseus's multi-target send builds ONE package directory and fans it out to N
/// independent [`SyncEngine`]s (one per target, each its own peer-scoped store).
/// The payload copies live in that **one shared** dir. Without coordination the
/// first engine to reach `Confirmed` would delete the shared payload out from
/// under every other target still mid-transfer — so a target that was offline
/// when the first confirmed would silently never receive the frame (its retry
/// re-serves a manifest-only collection). Gating the shared-payload cleanup
/// behind this sink lets a single coordinator
/// ([`SharedPackageCleanup`](super::cleanup_coord::SharedPackageCleanup)) clean
/// the dir exactly once, only after **every** target that received the package is
/// terminal (confirmed OR failed OR cancelled — a dead/offline target must not
/// block cleanup forever).
///
/// When the sink is `None` (the app and single-target Perseus — no fan-out, no
/// sharing) the engine keeps its original in-line cleanup behavior byte-for-byte.
///
/// The coordinator keys on `dir` (the shared fan-out identity): each engine mints
/// its own per-session announce `PackageId`, so those are **not** shared across
/// targets and cannot be the key. `dir` is also the only identity available at
/// every terminal site (a pre-announce failure has no `PackageId`).
pub trait PackageCleanupSink: Send + Sync {
    /// One target's engine has reached a terminal state for the package served
    /// from `dir`. Implementations MUST be idempotent and cheap; this is called
    /// on the synchronous engine worker (confirm/fail/cancel path).
    fn on_terminal(&self, dir: &Path);
}

/// Factory + namespace for the sync engine. Holds no state itself — [`spawn`]
/// moves everything into the worker task and returns a [`SyncEngineHandle`].
///
/// [`spawn`]: SyncEngine::spawn
pub struct SyncEngine;

impl SyncEngine {
    /// Spawn the engine with default [`SyncConfig`].
    pub fn spawn(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
    ) -> SyncEngineHandle {
        Self::spawn_with_config(store, transport, peer, SyncConfig::default())
    }

    /// Spawn the engine with an explicit [`SyncConfig`] (tests use short
    /// timeouts) and no progress emitter (log-only).
    pub fn spawn_with_config(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        config: SyncConfig,
    ) -> SyncEngineHandle {
        Self::spawn_with_config_and_emitter(store, transport, peer, config, None)
    }

    /// Spawn with an optional host [`ProgressEmitter`] (task M3). The app-side
    /// sender passes one so each package's coarse state transitions surface as
    /// `sync-progress` / `sync-finished` events for the Transfers UI; Perseus
    /// and every test pass `None`, keeping the transport-agnostic engine
    /// UI-agnostic (log-only, exactly as before). Events are discrete per
    /// package-state change — never per byte.
    pub fn spawn_with_emitter(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        emitter: Option<Arc<dyn ProgressEmitter>>,
    ) -> SyncEngineHandle {
        Self::spawn_with_config_and_emitter(store, transport, peer, SyncConfig::default(), emitter)
    }

    /// The public constructor the config/emitter convenience spawners delegate
    /// to. Passes no [`PackageCleanupSink`], so payload cleanup stays exactly as
    /// it was before the sink existed (app + single-target Perseus path).
    pub fn spawn_with_config_and_emitter(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        config: SyncConfig,
        emitter: Option<Arc<dyn ProgressEmitter>>,
    ) -> SyncEngineHandle {
        Self::spawn_inner(store, transport, peer, config, emitter, None, None)
    }

    /// Spawn with a host [`ProgressEmitter`] AND a retry-time [`AddrRefresher`]
    /// (iroh hardening T8) — the app's personal-sync sender path. On a timed-out
    /// re-attempt the engine re-resolves the peer's current address through the
    /// refresher so a relay-map change (or the peer moving relays) can't strand
    /// every retry on a dead cached path.
    pub fn spawn_with_emitter_and_refresher(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        emitter: Option<Arc<dyn ProgressEmitter>>,
        addr_refresher: Option<AddrRefresher>,
    ) -> SyncEngineHandle {
        Self::spawn_inner(
            store,
            transport,
            peer,
            SyncConfig::default(),
            emitter,
            None,
            addr_refresher,
        )
    }

    /// Spawn with a retry-time [`AddrRefresher`] and no sink/emitter — Perseus's
    /// single-target path (T8). Mirrors [`spawn`](Self::spawn) plus the refresher.
    pub fn spawn_with_refresher(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        addr_refresher: Option<AddrRefresher>,
    ) -> SyncEngineHandle {
        Self::spawn_inner(
            store,
            transport,
            peer,
            SyncConfig::default(),
            None,
            None,
            addr_refresher,
        )
    }

    /// Spawn with a shared [`PackageCleanupSink`] AND a retry-time
    /// [`AddrRefresher`] — Perseus's multi-target fan-out path (T8). Mirrors
    /// [`spawn_with_sink`](Self::spawn_with_sink) plus the refresher.
    pub fn spawn_with_sink_and_refresher(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        cleanup_sink: Arc<dyn PackageCleanupSink>,
        addr_refresher: Option<AddrRefresher>,
    ) -> SyncEngineHandle {
        Self::spawn_inner(
            store,
            transport,
            peer,
            SyncConfig::default(),
            None,
            Some(cleanup_sink),
            addr_refresher,
        )
    }

    /// Spawn with a shared [`PackageCleanupSink`] and default [`SyncConfig`] /
    /// no emitter — Perseus's multi-target fan-out path. Every engine sharing one
    /// package dir is given the SAME coordinator so the shared payload is cleaned
    /// exactly once, only after all targets are terminal. The app and
    /// single-target Perseus never take this path (they spawn without a sink and
    /// keep the original in-line cleanup).
    pub fn spawn_with_sink(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        cleanup_sink: Arc<dyn PackageCleanupSink>,
    ) -> SyncEngineHandle {
        Self::spawn_inner(
            store,
            transport,
            peer,
            SyncConfig::default(),
            None,
            Some(cleanup_sink),
            None,
        )
    }

    /// Spawn with BOTH a shared [`PackageCleanupSink`] and a host
    /// [`ProgressEmitter`], default [`SyncConfig`] — the collab request-to-serve
    /// map (slice 4). Where [`spawn_with_sink`] hard-codes `emitter: None`, the
    /// collab sender needs both halves: the sink (a
    /// [`CollabCleanupSink`](crate::api::collab_exchange::CollabCleanupSink)) so a
    /// reconstructed `collab_serve` dir is cleaned on terminal while a retained
    /// `collab_pub` publication survives confirm (Д4), AND an emitter so a project
    /// serve still surfaces `sync-progress` / `sync-finished` for the Transfers UI.
    pub fn spawn_with_sink_and_emitter(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        cleanup_sink: Arc<dyn PackageCleanupSink>,
        emitter: Option<Arc<dyn ProgressEmitter>>,
    ) -> SyncEngineHandle {
        Self::spawn_inner(
            store,
            transport,
            peer,
            SyncConfig::default(),
            emitter,
            Some(cleanup_sink),
            None,
        )
    }

    /// The single full constructor every spawner delegates to. Starts a tokio
    /// task running the worker loop and returns a handle to it. `cleanup_sink`
    /// is `None` for the app and single-target Perseus (unchanged in-line
    /// cleanup) and `Some(coordinator)` for the multi-target fan-out.
    /// `addr_refresher` is `None` for every test + single-shot spawner and
    /// `Some(..)` for the app/Perseus production paths that re-resolve a peer's
    /// address on a timed-out retry (T8).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_inner(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        config: SyncConfig,
        emitter: Option<Arc<dyn ProgressEmitter>>,
        cleanup_sink: Option<Arc<dyn PackageCleanupSink>>,
        addr_refresher: Option<AddrRefresher>,
    ) -> SyncEngineHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(64);
        let worker = Worker {
            store: Arc::clone(&store),
            transport,
            peer,
            config,
            pending: HashMap::new(),
            emitter,
            cleanup_sink,
            addr_refresher,
        };
        let join = tokio::spawn(async move {
            if let Err(e) = worker.run(cmd_rx).await {
                tracing::error!(error = %e, "sync worker exited with error");
            }
        });
        SyncEngineHandle {
            store,
            cmd_tx,
            join: Mutex::new(Some(join)),
        }
    }
}

/// Handle to a running [`SyncEngine`]. Cheap to hold; dropping it closes the
/// command channel, which stops the worker (crash-resume relies on exactly this).
pub struct SyncEngineHandle {
    store: Arc<dyn SyncStore>,
    cmd_tx: mpsc::Sender<Command>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl SyncEngineHandle {
    /// Enqueue a package directory for sending and return its durable row id
    /// (valid for [`cancel`](Self::cancel)). The worker inserts the row and
    /// begins driving it; this awaits the worker's id reply.
    pub async fn enqueue_package(&self, dir: impl AsRef<Path>) -> Result<i64> {
        let dir = dir.as_ref().to_path_buf();
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Process {
                dir,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        match reply_rx.await {
            Ok(res) => res,
            Err(_) => Err(anyhow!("sync engine worker dropped enqueue reply")),
        }
    }

    /// Cancel an in-flight package. Terminal (`Failed`) once processed; a no-op
    /// if the package is already terminal.
    pub async fn cancel(&self, id: i64) -> Result<()> {
        self.cmd_tx
            .send(Command::Cancel(id))
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        Ok(())
    }

    /// The current non-terminal outbound rows (live in-flight picture). Reads
    /// the store directly — no round-trip through the worker.
    pub fn status_snapshot(&self) -> Result<Vec<OutboundRow>> {
        self.store.non_terminal()
    }

    /// Ask the worker to stop and await its exit. (Dropping the handle without
    /// calling this also stops the worker, just without the join.)
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown).await;
        let join = self.join.lock().expect("join mutex poisoned").take();
        if let Some(j) = join {
            let _ = j.await;
        }
    }
}

/// The worker: owns the transport endpoint, the in-flight map, and the store
/// handle. All state mutation happens on this single task, so ack idempotence
/// and history de-duplication fall out of sequential processing for free.
struct Worker {
    store: Arc<dyn SyncStore>,
    transport: Arc<dyn SharingTransport>,
    peer: NodeId,
    config: SyncConfig,
    pending: HashMap<i64, Pending>,
    /// Optional host sink for the send-side `sync-progress` / `sync-finished`
    /// events (task M3). `None` for Perseus + tests → the engine is log-only.
    emitter: Option<Arc<dyn ProgressEmitter>>,
    /// Optional coordinator for shared-payload cleanup on the multi-target
    /// fan-out (Sync 2C). `Some` only when Perseus fans one package dir out to
    /// N engines; the confirmed/failed/cancelled terminal paths route through it
    /// so the shared dir is cleaned exactly once, after every target is
    /// terminal. `None` for the app + single-target Perseus → the original
    /// in-line cleanup runs unchanged.
    cleanup_sink: Option<Arc<dyn PackageCleanupSink>>,
    /// Optional retry-time peer-address re-resolver (T8). `Some` on the
    /// app/Perseus production paths: on a timed-out retry the worker awaits it and,
    /// on `Some(addr)`, re-registers the peer's current address on the transport
    /// before re-attempting. `None` for tests + single-shot spawners → retries use
    /// the address already known to the transport (unchanged behavior).
    addr_refresher: Option<AddrRefresher>,
}

impl Worker {
    async fn run(mut self, mut cmd_rx: mpsc::Receiver<Command>) -> Result<()> {
        // Bring the endpoint online (idempotent) so the peer can ack back to us,
        // then take the single-consumer event stream exactly once.
        self.transport
            .start()
            .await
            .context("start sync transport")?;
        let mut events = self.transport.events().await;

        // Crash-resume: re-drive the non-terminal rows left by a prior engine —
        // but ONLY the ones bound to THIS engine's peer. The store is shared
        // (Perseus fans one package out to N per-target engines over one
        // `perseus.db`; the app holds one engine per peer over one catalog store),
        // so `non_terminal()` returns rows for every peer. An engine re-driving
        // another peer's row would announce that package to the WRONG peer and let
        // its own ack confirm a row destined elsewhere — cross-delivery + a
        // corrupted per-peer confirmation. Scoping the resume to `row.peer ==
        // self.peer` keeps each engine's recovery to its own outbound rows.
        match self.store.non_terminal() {
            Ok(rows) => {
                for row in rows {
                    if row.peer != self.peer {
                        continue;
                    }
                    let dir = PathBuf::from(&row.package_ref);
                    if let Err(e) = self.start_package(row.id, dir, row.state).await {
                        tracing::error!(package_id = row.id, error = %e, "resume re-announce failed");
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "crash-resume enumeration failed"),
        }

        // Startup heal: reclaim payload copies left behind by any confirmed
        // package a prior engine cleaned up incompletely (crashed after confirm
        // but before cleanup, or confirmed by a pre-cleanup build). `confirmed()`
        // returns EVERY confirmed row ever, so an already-clean (manifest-only)
        // dir frees 0 bytes and is not counted. Best-effort and non-fatal: a
        // per-dir error warns and continues; startup is never blocked.
        //
        // With a `cleanup_sink` (multi-target fan-out) the package dirs are
        // SHARED across N engines: a per-engine heal here would let one engine
        // delete a dir another target still needs (that target's row may be
        // non-terminal and about to resume). The shared restart reconciliation is
        // done ONCE, centrally, by Perseus (`reconcile_shared_cleanup`) which has
        // the full per-dir picture across every target, so a sinked engine skips
        // its own heal entirely. The no-sink heal below is unchanged.
        if self.cleanup_sink.is_some() {
            tracing::debug!("startup payload heal delegated to the shared cleanup coordinator");
        } else {
            match self.store.confirmed() {
                Ok(rows) => {
                    let mut count: u64 = 0;
                    let mut freed_bytes: u64 = 0;
                    for row in rows {
                        let dir = PathBuf::from(&row.package_ref);
                        match cleanup_package_payloads(&dir) {
                            Ok(0) => {}
                            Ok(bytes) => {
                                count += 1;
                                freed_bytes = freed_bytes.saturating_add(bytes);
                            }
                            Err(e) => tracing::warn!(
                                package_id = row.id,
                                error = %format!("{e:#}"),
                                "package payload heal failed"
                            ),
                        }
                    }
                    if count > 0 {
                        tracing::info!(count, freed_bytes, "package payload heal");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "confirmed enumeration for payload heal failed")
                }
            }
        }

        loop {
            let next = self.next_deadline();
            let sleep = tokio::time::sleep_until(next);
            tokio::pin!(sleep);

            tokio::select! {
                _ = &mut sleep => {
                    if let Err(e) = self.handle_timeouts().await {
                        tracing::error!(error = %e, "sync timeout handling failed");
                    }
                }
                event = events.recv() => match event {
                    Some(ev) => {
                        if let Err(e) = self.handle_event(ev) {
                            tracing::error!(error = %e, "sync event handling failed");
                        }
                    }
                    None => {
                        tracing::info!("sync transport event stream closed; worker stopping");
                        break;
                    }
                },
                cmd = cmd_rx.recv() => match cmd {
                    Some(Command::Process { dir, reply }) => {
                        match self.store.enqueue(&dir.to_string_lossy(), self.peer) {
                            Ok(id) => {
                                tracing::info!(package_id = id, state = "queued", "sync state");
                                self.emit_progress(id, "queued", 0);
                                let _ = reply.send(Ok(id));
                                if let Err(e) = self.start_package(id, dir, OutboundState::Queued).await {
                                    tracing::error!(package_id = id, error = %e, "sync start package failed");
                                }
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "sync enqueue failed");
                                let _ = reply.send(Err(e));
                            }
                        }
                    }
                    Some(Command::Cancel(id)) => {
                        if let Err(e) = self.cancel_package(id) {
                            tracing::error!(package_id = id, error = %e, "sync cancel failed");
                        }
                    }
                    Some(Command::Shutdown) => {
                        tracing::info!("sync worker shutting down");
                        break;
                    }
                    None => {
                        tracing::info!("sync command channel closed; worker stopping");
                        break;
                    }
                },
            }
        }
        Ok(())
    }

    /// Earliest ack deadline across in-flight packages, or a far-future instant
    /// when idle.
    fn next_deadline(&self) -> Instant {
        self.pending
            .values()
            .map(|p| p.deadline)
            .min()
            .unwrap_or_else(|| Instant::now() + IDLE_SLEEP)
    }

    /// Emit a coarse send-side `sync-progress` tick (task M3). No-op without a
    /// host emitter. Keyed on the durable row `id` (stable across the package's
    /// lifecycle) so the Transfers UI can correlate it to the Active-tab row.
    fn emit_progress(&self, id: i64, stage: &str, frame_count: u32) {
        if let Some(em) = &self.emitter {
            // Collab exchange (slice 4): tag the tick with the project id if this
            // package is a project exchange. Read from the live slot (present for
            // every progress tick); `None` for personal sync.
            let project_id = self.pending.get(&id).and_then(|p| p.project_id.clone());
            emit_event(em.as_ref(), "sync-progress", &SyncProgressEvent {
                package_id: id.to_string(),
                direction: Direction::Sent,
                stage: stage.to_string(),
                peer_device: node_id_hex(&self.peer),
                frame_count,
                project_id,
            });
        }
    }

    /// Emit the single send-side `sync-finished` event for a package (task M3).
    /// `new_count` / `duplicate_count` are the Sync Phase 3 dedup outcome — how
    /// many frames were actually sent vs. dropped as the peer's duplicates.
    fn emit_finished(
        &self,
        id: i64,
        outcome: &str,
        ok_count: u32,
        failed: Vec<String>,
        new_count: u32,
        duplicate_count: u32,
        project_id: Option<String>,
    ) {
        if let Some(em) = &self.emitter {
            emit_event(em.as_ref(), "sync-finished", &SyncFinishedEvent {
                package_id: id.to_string(),
                direction: Direction::Sent,
                outcome: outcome.to_string(),
                peer_device: node_id_hex(&self.peer),
                ok_count,
                failed,
                new_count,
                duplicate_count,
                project_id,
            });
        }
    }

    /// Begin driving a package: install its pending slot, then make the first
    /// serve+announce attempt.
    ///
    /// The slot is inserted **before** the attempt so that a first-attempt
    /// build/serve/announce failure (typically the peer being offline at send
    /// time) can never leave the row non-terminal with no retry slot — the C1
    /// wedge. [`attempt`](Self::attempt) arms the real deadline (ack-wait on
    /// success, retry on failure); until then the slot is due immediately.
    async fn start_package(
        &mut self,
        id: i64,
        dir: PathBuf,
        prior_state: OutboundState,
    ) -> Result<()> {
        // A prior engine already recorded the transfer-start milestone iff the
        // row is past Queued/Announced (crash-resume of a `Transferring` row).
        let started = !matches!(prior_state, OutboundState::Queued | OutboundState::Announced);
        self.pending.insert(
            id,
            Pending {
                id,
                dir,
                announce: None,
                started,
                started_at: String::new(),
                deadline: Instant::now(),
                rung: 0,
                // A fresh enqueue's first deadline is an immediate attempt,
                // matching today's flow (start_package calls `attempt` directly).
                next_action: NextAction::Retry,
                last_rejected: Vec::new(),
                want: None,
                new_count: 0,
                duplicate_count: 0,
                project_id: None,
                hub_package_id: None,
                manifest_records: None,
            },
        );
        self.attempt(id).await
    }

    /// (Re)serve + (re)announce the pending package `id`, then arm its next
    /// deadline. This is the single attempt path shared by the first drive
    /// ([`start_package`](Self::start_package)) and every retry
    /// ([`handle_timeouts`](Self::handle_timeouts)).
    ///
    /// On success it records the transfer-start milestone the first time and
    /// arms the ack-wait deadline ([`NextAction::AwaitAck`]). On a
    /// build/serve/announce failure it logs and arms a *backoff* deadline instead
    /// of returning `Err`, so the row keeps its slot and `handle_timeouts`
    /// re-drives it on a capped exponential backoff (spec §2) rather than wedging
    /// (C1) — delivery is retried forever, never terminalized from a network
    /// condition. Because it always re-arms a deadline, a persistently failing
    /// re-announce cannot busy-spin (M1).
    ///
    /// Spec §1's one exception: if the package dir has vanished from disk —
    /// genuinely local, re-announcing can never succeed no matter how long we
    /// back off — this fails the package terminally via [`fail_package`]
    /// instead of arming another retry.
    async fn attempt(&mut self, id: i64) -> Result<()> {
        // One announce attempt is being made now: bump the persisted attempt
        // counter (announce attempts made) up front, before any early return, so
        // it reflects every serve/announce try — success or failure.
        let attempts = self.store.bump_attempts(id).context("bump attempts")?;
        tracing::debug!(package_id = id, attempts, "sync announce attempt");

        // Snapshot from the slot; drop the borrow before any await / mutation.
        let Some((dir, existing, started, cached_want)) = self
            .pending
            .get(&id)
            .map(|p| (p.dir.clone(), p.announce.clone(), p.started, p.want.clone()))
        else {
            // Slot gone (cancelled/confirmed concurrently) — nothing to do.
            return Ok(());
        };

        // Spec §1: a missing package dir means the payload is gone and
        // re-announcing can never succeed — the ONE local-unrecoverable case
        // that stays terminal under delivery-forever semantics (spec §2 governs
        // every *network* condition, not this one).
        if !dir.exists() {
            tracing::error!(
                package_id = id,
                path = %dir.display(),
                "package payload missing; failing terminally"
            );
            if let Err(se) = self
                .store
                .set_last_error(id, Some("package payload missing on disk"))
            {
                tracing::warn!(package_id = id, error = %se, "record last_error (missing payload) failed");
            }
            self.fail_package(id)?;
            return Ok(());
        }

        // Reuse the minted announce + negotiated want across retries (stable
        // `package_id` for ack correlation, no re-negotiation), or run the dedup
        // handshake once and build a fresh subset/full announce on the first
        // build. An empty want short-circuits to an all-duplicate terminal.
        let (announce, want) = match existing {
            Some(a) => (a, cached_want),
            None => match self.negotiate_and_build(id, &dir).await? {
                // The handshake found the peer already has every frame: no
                // announce, no serve — terminalize as all-duplicate and return.
                Negotiated::AllDuplicate => return Ok(()),
                // A manifest/announce build error already logged + armed a retry.
                Negotiated::Deferred => return Ok(()),
                Negotiated::Send { announce, want } => (announce, want),
            },
        };

        // Collab exchange (slice 4): read the project routing captured by
        // `negotiate_and_build` (set on the first build, persisted across
        // retries). `Some((project_id, hub_package_id))` ⇒ the announce goes out
        // as a project advertisement; `None` ⇒ the personal-sync announce.
        let project = self.pending.get(&id).and_then(|p| {
            match (&p.project_id, &p.hub_package_id) {
                (Some(pid), Some(hub)) => Some((pid.clone(), hub.clone())),
                _ => None,
            }
        });

        // Provider side: register the served dir (the negotiated want-subset when
        // `Some`, the full package when `None`), then advertise it to the peer. A
        // failure here (e.g. the peer is offline) is retryable, not fatal:
        // remember the announce + want and arm a retry deadline.
        let serve_announce = async {
            self.transport
                .serve(&announce, &dir, want.as_ref())
                .await
                .context("serve package")?;
            match &project {
                Some((pid, hub)) => self
                    .transport
                    .announce_project(self.peer, pid, hub, &announce)
                    .await
                    .context("announce project package"),
                None => self
                    .transport
                    .announce(self.peer, &announce)
                    .await
                    .context("announce package"),
            }
        }
        .await;
        if let Err(e) = serve_announce {
            // `{e:#}` — same rationale as the build-announce branch above: the
            // bare `.context("announce package")` layer alone hid the real
            // cause (e.g. "peer not started: <hex>") in production logs.
            let reason = format!("{e:#}");
            tracing::error!(package_id = id, error = %reason, "sync serve/announce failed; will retry");
            // Record the attempt-error reason for the Perseus status page (Task 9).
            // Best-effort: a diagnostic write must never turn a retryable transfer
            // into a failure, so a store error here is logged, not propagated.
            if let Err(se) = self.store.set_last_error(id, Some(&reason)) {
                tracing::warn!(package_id = id, error = %se, "record last_error (serve/announce) failed");
            }
            if let Some(p) = self.pending.get_mut(&id) {
                p.announce = Some(announce);
                p.want = want;
            }
            self.arm_retry(id);
            return Ok(());
        }

        // Success. Record the transfer-start milestone the first time only.
        let record_started = !started;
        let started_at = if record_started {
            now_iso()
        } else {
            let existing_ts = self
                .pending
                .get(&id)
                .map(|p| p.started_at.clone())
                .unwrap_or_default();
            if existing_ts.is_empty() {
                now_iso()
            } else {
                existing_ts
            }
        };
        if record_started {
            self.store.set_state(id, OutboundState::Announced)?;
            tracing::info!(package_id = id, state = "announced", "sync state");
            self.store.set_state(id, OutboundState::Transferring)?;
            tracing::info!(package_id = id, state = "transferring", "sync state");
            // Started history covers only the frames actually sent (the want
            // subset), so a resend of the peer's duplicates leaves no orphan
            // `sent` rows that never get a matching confirm.
            self.append_started_history(id, &dir, &started_at, want.as_ref())?;
            // One coarse in-flight tick per package (the first successful
            // announce); retries re-announce but do NOT re-emit — bounded, never
            // per-byte.
            self.emit_progress(id, "transferring", announce.frame_count);
        } else {
            tracing::info!(package_id = id, state = "transferring", "sync resume/retry re-announce");
        }

        // Arm the ack-wait deadline and persist the announce/want/milestone. The
        // deadline now means "waiting for the peer's ack"; if it fires,
        // `handle_timeouts` climbs a backoff rung rather than failing (spec §2).
        if let Some(p) = self.pending.get_mut(&id) {
            p.announce = Some(announce);
            p.want = want;
            p.started = true;
            p.started_at = started_at;
            p.next_action = NextAction::AwaitAck;
            p.deadline = Instant::now() + self.config.ack_timeout;
        }
        // The serve/announce succeeded — clear any stale attempt-error so a package
        // now awaiting its ack no longer shows the previous failure (Task 9).
        // Best-effort: a diagnostic write must never fail the send.
        if let Err(se) = self.store.set_last_error(id, None) {
            tracing::warn!(package_id = id, error = %se, "clear last_error on announce success failed");
        }
        // No retry is pending while we await the ack — clear the persisted
        // countdown deadline (Task 2). Best-effort.
        self.clear_next_retry(id);
        Ok(())
    }

    /// First-build dedup handshake (Sync Phase 3), run once when a package has no
    /// minted announce yet. Reads the manifest, mints the announce, offers the
    /// peer the frames' sampling hashes, and `negotiate_want`s the subset it
    /// still needs. Best-effort throughout: a `compute_xxhash` error or any
    /// `negotiate_want` failure abandons the handshake and falls back to a full
    /// send (`want = None`) rather than ever failing the send. An empty want is
    /// terminalized here as all-duplicate (no announce). The served
    /// `{new, duplicate}` counts are stashed on the pending slot for the eventual
    /// finished event.
    async fn negotiate_and_build(&mut self, id: i64, dir: &Path) -> Result<Negotiated> {
        let records = match package::read_manifest(dir) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(package_id = id, error = %format!("{e:#}"), "sync build announce failed; will retry");
                self.arm_retry(id);
                return Ok(Negotiated::Deferred);
            }
        };
        // Cache the manifest snapshot (once, on the first successful read) so a
        // later terminal-fail epilogue can still name every frame in history even
        // if the package dir has since vanished from disk and a live re-read of
        // the manifest is no longer possible.
        if let Some(p) = self.pending.get_mut(&id) {
            p.manifest_records = Some(records.clone());
        }
        let full_announce = match announce_for_dir(dir) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(package_id = id, error = %format!("{e:#}"), "sync build announce failed; will retry");
                self.arm_retry(id);
                return Ok(Negotiated::Deferred);
            }
        };

        // Collab exchange (slice 4): a manifest carrying a project stamp marks a
        // PROJECT package. Record `(project_id, hub_package_id)` on the slot so
        // the announce site routes through `announce_project`, and skip the
        // Offer/Want dedup negotiation entirely (Д2/audit B2 — project packages
        // always full-send). Persisted on the slot so retries keep the routing.
        let stamp = records.iter().find_map(|r| r.project.clone());
        if let Some(s) = &stamp {
            if let Some(p) = self.pending.get_mut(&id) {
                p.project_id = Some(s.project_id.clone());
                p.hub_package_id = Some(s.package_id.clone());
            }
        }

        // Build the offer (sampling hashes). ANY hashing error abandons the
        // handshake for the whole package → full send (`want = None`). A project
        // package skips the handshake entirely and full-sends.
        let want: Option<HashSet<String>> = if stamp.is_some() {
            tracing::debug!(package_id = id, "project package; skipping dedup negotiation (full send)");
            None
        } else {
            match build_offer(dir, &records) {
                Err(e) => {
                    tracing::debug!(package_id = id, error = %format!("{e:#}"), "dedup offer hashing failed; full send");
                    None
                }
                Ok((offer, full_by_rel)) => {
                    match self
                        .transport
                        .negotiate_want(self.peer, full_announce.package_id.clone(), offer, full_by_rel)
                        .await
                    {
                        Ok(w) => Some(w),
                        Err(e) => {
                            tracing::debug!(error = %e, package_id = %full_announce.package_id.0, "dedup negotiate failed; full send");
                            None
                        }
                    }
                }
            }
        };

        // All-duplicate: the peer already has every frame — terminalize the
        // package to a confirmed terminal WITHOUT announcing or serving.
        if matches!(&want, Some(w) if w.is_empty()) {
            self.terminalize_all_duplicate(id, &records)?;
            return Ok(Negotiated::AllDuplicate);
        }

        // Compute the announce actually served + the outcome counts: the full
        // announce for a fallback (`None`), or a subset announce (adjusted
        // byte_size/frame_count, same package_id/root_hash) for a want-subset.
        let total = records.len();
        let (announce, new_count, duplicate_count) = match &want {
            None => (full_announce, total as u32, 0u32),
            Some(w) => {
                let byte_size: u64 = records
                    .iter()
                    .filter(|r| w.contains(&r.rel_path))
                    .map(|r| r.byte_size)
                    .sum();
                let subset = PackageAnnounce {
                    package_id: full_announce.package_id.clone(),
                    root_hash: full_announce.root_hash.clone(),
                    byte_size,
                    frame_count: w.len() as u32,
                };
                (subset, w.len() as u32, total.saturating_sub(w.len()) as u32)
            }
        };
        if let Some(p) = self.pending.get_mut(&id) {
            p.new_count = new_count;
            p.duplicate_count = duplicate_count;
            p.want = want.clone();
        }
        Ok(Negotiated::Send { announce, want })
    }

    /// Drive a package straight to a confirmed terminal because the dedup
    /// handshake found the peer already holds every frame (empty want). Reuses
    /// the exact confirmed-terminal mechanics of [`on_ack`](Self::on_ack) —
    /// per-frame `Duplicate` receipts, confirmed history, payload cleanup routed
    /// through the [`cleanup_sink`] (so a multi-target coordinator counts this
    /// engine terminal), and the confirmed state stamp — but skips announce /
    /// serve / blob release entirely (nothing was ever served).
    fn terminalize_all_duplicate(&mut self, id: i64, records: &[ManifestRecord]) -> Result<()> {
        let receipts: Vec<FrameReceipt> = records
            .iter()
            .map(|r| FrameReceipt {
                frame_uuid: r.frame_uuid.clone(),
                xxh3: r.xxh3.clone(),
                outcome: ReceiptOutcome::Duplicate,
            })
            .collect();

        let mut pending = self
            .pending
            .remove(&id)
            .ok_or_else(|| anyhow!("all-duplicate terminal for a vanished slot {id}"))?;
        // No transfer-start milestone was ever recorded (we never announced), so
        // stamp a start time now for a coherent confirmed history row.
        if pending.started_at.is_empty() {
            pending.started_at = now_iso();
        }

        self.store
            .confirm(pending.id, &receipts)
            .context("confirm all-duplicate outbound")?;
        // Terminal: no retry is pending — clear the persisted countdown (Task 2).
        self.clear_next_retry(pending.id);
        self.append_confirmed_history(&pending, &receipts)?;
        // Same shared-payload discipline as the ack path: defer to the
        // coordinator when fanned out, else clean the payload copies in line.
        match &self.cleanup_sink {
            Some(sink) => sink.on_terminal(&pending.dir),
            None => match cleanup_package_payloads(&pending.dir) {
                Ok(freed_bytes) => {
                    tracing::info!(package_id = pending.id, freed_bytes, "package payloads cleaned");
                }
                Err(e) => {
                    tracing::warn!(package_id = pending.id, error = %format!("{e:#}"), "package payload cleanup failed");
                }
            },
        }
        tracing::info!(package_id = pending.id, state = "confirmed", reason = "all_duplicate", "sync state");
        self.emit_finished(
            pending.id,
            "confirmed",
            receipts.len() as u32,
            Vec::new(),
            0,
            records.len() as u32,
            pending.project_id.clone(),
        );
        Ok(())
    }

    /// Arm a backoff deadline on a still-pending package after a failed
    /// build/serve/announce attempt: climb one rung and wait out
    /// [`retry_backoff`] before the next announce (spec §2). Leaves the milestone
    /// untouched — the row stays `Queued` until an announce actually succeeds, and
    /// is never terminalized from a network condition.
    fn arm_retry(&mut self, id: i64) {
        let delay = if let Some(p) = self.pending.get_mut(&id) {
            p.rung = p.rung.saturating_add(1);
            p.next_action = NextAction::Retry;
            let delay = retry_backoff(self.config.ack_timeout, p.rung);
            p.deadline = Instant::now() + delay;
            Some(delay)
        } else {
            None
        };
        // Persist the wall-clock retry deadline (Task 2) once the `&mut pending`
        // borrow is dropped, so the UI can render a countdown and a restart re-arms
        // honestly.
        if let Some(delay) = delay {
            self.persist_next_retry(id, delay);
        }
    }

    /// Persist the wall-clock deadline (`now + delay`) of a just-armed `Retry`
    /// window as `OutboundRow::next_retry_at` (Task 2). Best-effort (spec §2): a
    /// store error is logged and dropped — a failed diagnostic write must never
    /// break scheduling.
    fn persist_next_retry(&self, id: i64, delay: Duration) {
        let stamp = retry_deadline_stamp(delay);
        if let Err(e) = self.store.set_next_retry_at(id, Some(&stamp)) {
            tracing::warn!(package_id = id, error = %e, "persist next_retry_at failed");
        }
    }

    /// Clear `OutboundRow::next_retry_at` (Task 2) — no retry is pending: on a
    /// successful announce (now awaiting the ack) and on every terminal
    /// transition. Best-effort, same rationale as [`persist_next_retry`].
    fn clear_next_retry(&self, id: i64) {
        if let Err(e) = self.store.set_next_retry_at(id, None) {
            tracing::warn!(package_id = id, error = %e, "clear next_retry_at failed");
        }
    }

    /// Fire-and-forget blob release for a package that has reached a terminal
    /// state (confirmed / failed / cancelled). Runs on a detached task over a
    /// clone of the transport `Arc`, so a release failure can never block or
    /// fail the synchronous state transition ([`handle_event`](Self::handle_event)
    /// is deliberately non-async) that triggered it — it only logs. `release`
    /// is idempotent, so a double-fire (e.g. a resumed-then-cancelled row) is
    /// harmless.
    fn spawn_release(&self, package_id: PackageId) {
        let transport = Arc::clone(&self.transport);
        tokio::spawn(async move {
            if let Err(e) = transport.release(&package_id).await {
                tracing::warn!(package_id = %package_id.0, error = %format!("{e:#}"), "blob release failed");
            }
        });
    }

    /// Dispatch a transport event. Synchronous — no `.await` — so a package can
    /// never be confirmed twice by interleaving.
    fn handle_event(&mut self, ev: TransportEvent) -> Result<()> {
        match ev {
            TransportEvent::AckReceived {
                from,
                package_id,
                receipts,
            } => self.on_ack(from, package_id, receipts),
            TransportEvent::FetchProgress {
                package_id,
                bytes_done,
                bytes_total,
            } => {
                // Loopback delivers this only to the fetcher, so the sender
                // rarely sees it; kept for real transports that surface
                // provider-side progress. Transferring is already set.
                tracing::debug!(
                    package_id = %package_id.0,
                    bytes = bytes_done,
                    total = bytes_total,
                    "sync fetch progress"
                );
                Ok(())
            }
            // Inbound announcements are the receiver's concern (task A7), not
            // this sender-side engine's.
            TransportEvent::AnnounceReceived { .. } => Ok(()),
            // Inbound project advertisements / pull requests (collab exchange,
            // slice 4) are handled by the receive side, not this sender engine.
            TransportEvent::ProjectAnnounceReceived { .. }
            | TransportEvent::ProjectRequestReceived { .. } => Ok(()),
        }
    }

    /// Handle an ack: confirm the package ONLY if every receipt is non-`Rejected`
    /// (task A7 fix-review — `Confirmed` means "all frames ingested-or-duplicate").
    /// An ack carrying any `Rejected` receipt is a partial delivery: log the
    /// rejected frame uuids and leave the package's pending slot untouched (no
    /// confirm, no history, deadline unchanged) — the existing ack-timeout
    /// deadline elapses normally and `handle_timeouts`' ordinary backoff/retry
    /// path re-announces (redelivery) until the peer accepts every frame — a
    /// partial ack, like any other network condition, never terminalizes the
    /// package (spec §2).
    ///
    /// Idempotent for a fully-accepted ack: one for a package no longer in the
    /// in-flight map (already confirmed, or unknown) is dropped at debug.
    fn on_ack(&mut self, from: NodeId, package_id: PackageId, receipts: Vec<FrameReceipt>) -> Result<()> {
        // Peer-binding (finding M3): an ack is only trustworthy from the
        // package's intended destination. The remote node id is cryptographically
        // authenticated by the transport (iroh QUIC / loopback), so a `from` that
        // is not `self.peer` is a forged or misdirected ack — drop it with no
        // state change, so it can never drive a package to `Confirmed` (which
        // would make retention eligible to delete the capture originals).
        if from != self.peer {
            tracing::warn!(
                package_id = %package_id.0,
                from = %node_id_hex(&from),
                expected = %node_id_hex(&self.peer),
                "ignoring sync ack from a node other than the paired peer"
            );
            return Ok(());
        }

        // The map is keyed by row id, so locate the slot whose minted announce
        // carries this `package_id`.
        let key = self.pending.iter().find_map(|(k, p)| match &p.announce {
            Some(a) if a.package_id == package_id => Some(*k),
            _ => None,
        });
        let Some(key) = key else {
            tracing::debug!(package_id = %package_id.0, "duplicate/late ack ignored");
            return Ok(());
        };

        let rejected: Vec<&str> = receipts
            .iter()
            .filter_map(|r| matches!(r.outcome, ReceiptOutcome::Rejected(_)).then_some(r.frame_uuid.as_str()))
            .collect();

        if !rejected.is_empty() {
            tracing::warn!(
                package_id = key,
                rejected = %rejected.join(","),
                "sync ack has rejected frame(s); package stays in flight for retry"
            );
            if let Some(p) = self.pending.get_mut(&key) {
                p.last_rejected = rejected.into_iter().map(str::to_string).collect();
            }
            return Ok(());
        }

        // Completeness (finding M3): confirm ONLY when every frame we actually
        // SENT is acked with a non-`Rejected` receipt. An empty or partial ack
        // (fewer frames than we sent) must NOT confirm — otherwise retention
        // could delete a source whose frame the peer never actually stored.
        //
        // "Sent" is the negotiated want subset (Sync Phase 3), not the whole
        // manifest: a want-subset send never transfers the peer's duplicates, so
        // the ack legitimately covers only the subset. `want = None` (full send /
        // fallback) expects the whole manifest, exactly as before dedup. All
        // receipts here are already non-`Rejected` (guarded above), so `acked` is
        // exactly the set of accepted frame uuids.
        let (dir, want) = self
            .pending
            .get(&key)
            .map(|p| (p.dir.clone(), p.want.clone()))
            .expect("key from live find");
        let expected: Vec<String> = match crate::package::read_manifest(&dir) {
            Ok(records) => records
                .into_iter()
                .filter(|r| match &want {
                    Some(w) => w.contains(&r.rel_path),
                    None => true,
                })
                .map(|r| r.frame_uuid)
                .collect(),
            Err(e) => {
                tracing::warn!(
                    package_id = key,
                    error = %format!("{e:#}"),
                    "cannot read manifest to verify ack completeness; not confirming"
                );
                return Ok(());
            }
        };
        let acked: std::collections::HashSet<&str> =
            receipts.iter().map(|r| r.frame_uuid.as_str()).collect();
        let missing: Vec<&str> = expected
            .iter()
            .map(String::as_str)
            .filter(|u| !acked.contains(u))
            .collect();
        if !missing.is_empty() {
            tracing::warn!(
                package_id = key,
                missing = %missing.join(","),
                "sync ack does not cover every announced frame; package stays in flight for retry"
            );
            if let Some(p) = self.pending.get_mut(&key) {
                p.last_rejected = missing.into_iter().map(str::to_string).collect();
            }
            return Ok(());
        }

        let pending = self.pending.remove(&key).expect("key from live find");
        self.store
            .confirm(pending.id, &receipts)
            .context("confirm outbound")?;
        // Terminal: no retry is pending — clear the persisted countdown (Task 2).
        self.clear_next_retry(pending.id);
        self.append_confirmed_history(&pending, &receipts)?;
        // Terminal + confirmed: free the payload copies `write_package` made in
        // the package dir. They are dead weight once confirmed (the package is
        // never re-served), and without this an observatory keeps a full
        // duplicate of every night it sends. MUST run AFTER
        // `append_confirmed_history` (which reads the manifest); the manifest is
        // deliberately kept so retention/audit can still read it. Cleanup failure
        // never fails the confirm — log and continue.
        //
        // On the multi-target fan-out (`cleanup_sink` set) the payload dir is
        // SHARED across N engines, so this engine must NOT delete it on its own
        // confirm — that would strip a still-offline target's retry to a
        // manifest-only collection (silent data loss). Route the terminal signal
        // to the coordinator, which cleans exactly once after every target is
        // terminal. Without a sink (app / single-target) the original in-line
        // cleanup runs unchanged.
        match &self.cleanup_sink {
            Some(sink) => sink.on_terminal(&pending.dir),
            None => match cleanup_package_payloads(&pending.dir) {
                Ok(freed_bytes) => {
                    tracing::info!(package_id = pending.id, freed_bytes, "package payloads cleaned");
                }
                Err(e) => {
                    tracing::warn!(package_id = pending.id, error = %format!("{e:#}"), "package payload cleanup failed");
                }
            },
        }
        // Terminal: the package is confirmed; drop its served blobs so they do
        // not outlive the transfer. `package_id` is the id the ack correlated
        // against (== pending.announce.package_id). Fire-and-forget, never fails
        // the confirm.
        self.spawn_release(package_id);
        tracing::info!(package_id = pending.id, state = "confirmed", "sync state");
        self.emit_finished(
            pending.id,
            "confirmed",
            receipts.len() as u32,
            Vec::new(),
            pending.new_count,
            pending.duplicate_count,
            pending.project_id.clone(),
        );
        Ok(())
    }

    /// Handle every in-flight package whose deadline has elapsed. The deadline
    /// means one of two things ([`NextAction`]): an ack that never arrived
    /// ([`AwaitAck`](NextAction::AwaitAck)), or a backoff window that has elapsed
    /// so it is time to re-announce ([`Retry`](NextAction::Retry)). A network
    /// condition (no ack, offline peer, serve/announce error) never terminalizes
    /// a package — it climbs one backoff rung and is retried forever (spec §2).
    async fn handle_timeouts(&mut self) -> Result<()> {
        let now = Instant::now();
        let due: Vec<i64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(k, _)| *k)
            .collect();

        // Before re-attempting any timed-out package, re-resolve the peer's CURRENT
        // dialable address once per timeout pass (T8): a relay-map change or the
        // peer moving relays can strand the cached address so every retry hits the
        // same dead path. All this engine's packages go to `self.peer`, so one
        // refresh covers the whole `due` set; done lazily (only when something is
        // actually due) and only when a refresher is wired. `None` (hub blip / peer
        // gone) leaves the existing address in place — no worse than before.
        let mut refreshed = false;
        for id in due {
            // Copy the action out so no `&mut pending` borrow is held across the
            // store / attempt calls below.
            let Some(action) = self.pending.get(&id).map(|p| p.next_action) else {
                continue;
            };
            match action {
                NextAction::AwaitAck => {
                    // The ack never came: record it, climb one backoff rung, and
                    // wait it out. Never terminal (spec §2). Best-effort diagnostic
                    // write — a store error must not fail the timeout pass.
                    if let Err(se) = self
                        .store
                        .set_last_error(id, Some("no ack from peer within timeout"))
                    {
                        tracing::warn!(package_id = id, error = %se, "record last_error (ack timeout) failed");
                    }
                    let delay = if let Some(p) = self.pending.get_mut(&id) {
                        p.rung = p.rung.saturating_add(1);
                        p.next_action = NextAction::Retry;
                        let delay = retry_backoff(self.config.ack_timeout, p.rung);
                        p.deadline = Instant::now() + delay;
                        tracing::info!(package_id = id, rung = p.rung, "ack timeout, backing off");
                        Some(delay)
                    } else {
                        None
                    };
                    // Persist the wall-clock retry deadline (Task 2) after the
                    // borrow drops so the UI countdown reflects the new backoff.
                    if let Some(delay) = delay {
                        self.persist_next_retry(id, delay);
                    }
                }
                NextAction::Retry => {
                    if !refreshed {
                        refreshed = true;
                        if let Some(refresher) = self.addr_refresher.clone() {
                            if let Some(addr) = refresher(self.peer).await {
                                tracing::info!(
                                    peer = %node_id_hex(&self.peer),
                                    "retry: re-resolved peer address"
                                );
                                self.transport.add_peer_addr(addr);
                            }
                        }
                    }
                    // `attempt` bumps the counter, re-announces, and re-arms its own
                    // deadline (ack-wait on success, backoff on failure) so it can
                    // never busy-spin (M1). Guard the rare store-error path with a
                    // backoff arm so the deadline is never left stale.
                    if let Err(e) = self.attempt(id).await {
                        tracing::warn!(package_id = id, error = %e, "attempt errored, backing off");
                        self.arm_retry(id);
                    }
                }
            }
        }
        Ok(())
    }

    /// Mark a package `Failed` and record a `failed` history outcome. When the
    /// package's last known ack rejected one or more frames, the outcome names
    /// them (task A7 fix-review) instead of the bare `failed` string — the
    /// recorded reason for terminal failure was the receiver's rejection.
    ///
    /// No longer reached from ack/announce timeouts: under delivery-forever
    /// semantics (spec §2) a network condition never terminalizes a package.
    /// Spec §1: `Failed` stays reachable ONLY for the genuinely-unrecoverable
    /// *local* case — the package dir/payload has vanished from disk, so
    /// re-announcing can never succeed (see the `!dir.exists()` check at the top
    /// of [`attempt`](Self::attempt)) — plus `cancel_package`'s own direct
    /// `Failed` transition (not routed through here).
    fn fail_package(&mut self, id: i64) -> Result<()> {
        let removed = self.pending.remove(&id);
        let (dir, last_rejected, pkg_id, project_id, manifest_records) = match removed {
            Some(p) => (
                Some(p.dir),
                p.last_rejected,
                p.announce.map(|a| a.package_id),
                p.project_id,
                p.manifest_records,
            ),
            None => (None, Vec::new(), None, None, None),
        };
        self.store.set_state(id, OutboundState::Failed)?;
        // Terminal: no retry is pending — clear the persisted countdown (Task 2).
        self.clear_next_retry(id);
        // Terminal: release any served blobs (fire-and-forget). `pkg_id` is
        // `None` when the package never minted+served an announce (a pre-serve
        // failure) — nothing to release in that case.
        if let Some(pid) = pkg_id {
            self.spawn_release(pid);
        }
        tracing::error!(package_id = id, state = "failed", rejected = ?last_rejected, "sync state");
        let outcome = if last_rejected.is_empty() {
            "failed".to_string()
        } else {
            format!("failed: rejected frame(s) {}", last_rejected.join(","))
        };
        if let Some(dir) = dir {
            // The dir may itself be the thing that's gone (missing-payload
            // terminal path) — fall back to the cached first-build manifest
            // snapshot so the `failed` history outcome is still recorded per
            // frame instead of silently dropped.
            self.append_terminal_history(id, &dir, &outcome, manifest_records.as_deref())?;
            // Multi-target fan-out: a `Failed` target is terminal too. Notify the
            // coordinator so a permanently-unreachable peer does not block the
            // shared payload's cleanup forever (spec: terminal = confirmed OR
            // failed OR cancelled). No-op without a sink — a failed package keeps
            // its payloads there (Task 2: retry depends on them), unchanged.
            if let Some(sink) = &self.cleanup_sink {
                sink.on_terminal(&dir);
            }
        }
        self.emit_finished(id, &outcome, 0, last_rejected, 0, 0, project_id);
        Ok(())
    }

    /// Cancel a package → `Failed` with a `cancelled` outcome. Idempotent: a
    /// no-op if the package is already terminal / unknown.
    fn cancel_package(&mut self, id: i64) -> Result<()> {
        // Resolve the package dir: prefer the in-flight entry, else a live row.
        // Removing the slot also ensures no later timeout fires for this id. The
        // in-flight entry also carries the minted announce whose blobs need
        // releasing; a row resolved only from the store was never served this
        // session, so there is nothing to release (`pkg_id` stays `None`).
        let (dir, pkg_id, project_id) = if let Some(p) = self.pending.remove(&id) {
            (Some(p.dir), p.announce.map(|a| a.package_id), p.project_id)
        } else {
            (
                self.store
                    .non_terminal()?
                    .into_iter()
                    .find(|r| r.id == id)
                    .map(|r| PathBuf::from(r.package_ref)),
                None,
                None,
            )
        };

        let Some(dir) = dir else {
            tracing::debug!(package_id = id, "cancel ignored (already terminal or unknown)");
            return Ok(());
        };

        self.store.set_state(id, OutboundState::Cancelled)?;
        // Terminal: no retry is pending — clear the persisted countdown (Task 2).
        self.clear_next_retry(id);
        // Terminal: release any served blobs (fire-and-forget).
        if let Some(pid) = pkg_id {
            self.spawn_release(pid);
        }
        tracing::info!(
            package_id = id,
            state = "cancelled",
            "sync state"
        );
        self.append_terminal_history(id, &dir, "cancelled", None)?;
        // Multi-target fan-out: a cancelled target is terminal — notify the
        // coordinator so it counts toward the all-targets-terminal cleanup gate.
        // No-op without a sink (unchanged single-target behavior).
        if let Some(sink) = &self.cleanup_sink {
            sink.on_terminal(&dir);
        }
        self.emit_finished(id, "cancelled", 0, Vec::new(), 0, 0, project_id);
        Ok(())
    }

    /// Append one `sent` (transfer-started) history row per frame actually sent.
    /// `want` is the negotiated subset (Sync Phase 3): `Some(w)` records only the
    /// frames in `w` (the peer's duplicates were never transferred), `None`
    /// records every manifest frame (full send / fallback).
    fn append_started_history(
        &self,
        id: i64,
        dir: &Path,
        started_at: &str,
        want: Option<&HashSet<String>>,
    ) -> Result<()> {
        let records = package::read_manifest(dir)
            .with_context(|| format!("read manifest for started history {}", dir.display()))?;
        let records: Vec<&ManifestRecord> = records
            .iter()
            .filter(|r| want.map(|w| w.contains(&r.rel_path)).unwrap_or(true))
            .collect();
        let peer_device = node_id_hex(&self.peer);
        for r in &records {
            self.store.append_history(HistoryRow {
                frame_uuid: r.frame_uuid.clone(),
                filename: filename_of(&r.rel_path),
                object: object_of(r),
                peer_device: peer_device.clone(),
                direction: Direction::Sent,
                bytes: r.byte_size,
                started_at: started_at.to_string(),
                finished_at: None,
                outcome: "sent".to_string(),
                project: project_of(r),
            })?;
        }
        tracing::debug!(package_id = id, count = records.len(), "sync history: transfer started");
        Ok(())
    }

    /// Append one confirm history row per receipt, joining the peer's verdict to
    /// this package's manifest by `frame_uuid`.
    fn append_confirmed_history(&self, pending: &Pending, receipts: &[FrameReceipt]) -> Result<()> {
        let records = package::read_manifest(&pending.dir).with_context(|| {
            format!("read manifest for confirm history {}", pending.dir.display())
        })?;
        let by_uuid: HashMap<&str, &ManifestRecord> =
            records.iter().map(|r| (r.frame_uuid.as_str(), r)).collect();
        let peer_device = node_id_hex(&self.peer);
        let finished = now_iso();

        for rec in receipts {
            let (filename, object, bytes, project) = match by_uuid.get(rec.frame_uuid.as_str()) {
                Some(m) => (filename_of(&m.rel_path), object_of(m), m.byte_size, project_of(m)),
                None => (rec.frame_uuid.clone(), None, 0, None),
            };
            self.store.append_history(HistoryRow {
                frame_uuid: rec.frame_uuid.clone(),
                filename,
                object,
                peer_device: peer_device.clone(),
                direction: Direction::Sent,
                bytes,
                started_at: pending.started_at.clone(),
                finished_at: Some(finished.clone()),
                outcome: receipt_outcome_str(&rec.outcome),
                project,
            })?;
        }
        Ok(())
    }

    /// Append one terminal history row (`failed` / `cancelled`) per manifest
    /// frame. Prefers a live re-read of the manifest from `dir` (the freshest
    /// state); when that fails AND `fallback_records` is `Some` (the missing-
    /// payload terminal path, where the dir itself is what vanished), falls back
    /// to the cached snapshot from the package's first successful build instead
    /// of silently dropping the terminal history row. A missing/unreadable
    /// manifest with no fallback available is logged, not fatal (unchanged).
    fn append_terminal_history(
        &self,
        id: i64,
        dir: &Path,
        outcome: &str,
        fallback_records: Option<&[ManifestRecord]>,
    ) -> Result<()> {
        let records = match package::read_manifest(dir) {
            Ok(r) => r,
            Err(e) => match fallback_records {
                Some(cached) => {
                    tracing::info!(
                        package_id = id,
                        error = %e,
                        "sync history: manifest unreadable at terminal, using cached records"
                    );
                    cached.to_vec()
                }
                None => {
                    tracing::warn!(package_id = id, error = %e, "sync history: manifest unreadable at terminal");
                    return Ok(());
                }
            },
        };
        let peer_device = node_id_hex(&self.peer);
        let ts = now_iso();
        for r in &records {
            self.store.append_history(HistoryRow {
                frame_uuid: r.frame_uuid.clone(),
                filename: filename_of(&r.rel_path),
                object: object_of(r),
                peer_device: peer_device.clone(),
                direction: Direction::Sent,
                bytes: r.byte_size,
                started_at: ts.clone(),
                finished_at: Some(ts.clone()),
                outcome: outcome.to_string(),
                project: project_of(r),
            })?;
        }
        Ok(())
    }
}

/// Basename of a forward-slash manifest `rel_path`.
fn filename_of(rel_path: &str) -> String {
    rel_path
        .rsplit('/')
        .next()
        .unwrap_or(rel_path)
        .to_string()
}

/// Extract `object` from a manifest record's opaque `frame_meta`, if present.
fn object_of(r: &ManifestRecord) -> Option<String> {
    r.frame_meta
        .get("object")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// The `project_id` this record moved for (Stage II collab stamp), or `None` for
/// a personal-sync record. Populates `sync_history.project` (Task 11).
fn project_of(r: &ManifestRecord) -> Option<String> {
    r.project.as_ref().map(|p| p.project_id.clone())
}

/// Short outcome tag for a receipt verdict, stored in `sync_history.outcome`.
fn receipt_outcome_str(o: &ReceiptOutcome) -> String {
    match o {
        ReceiptOutcome::Ingested => "ingested",
        ReceiptOutcome::Duplicate => "duplicate",
        ReceiptOutcome::Rejected(_) => "rejected",
    }
    .to_string()
}

/// Reconstruct a [`PackageAnnounce`] from an on-disk package directory.
///
/// `byte_size`/`frame_count` come from the manifest; `root_hash` is the same
/// order-independent digest of payload hashes the package writer uses (loopback
/// ignores it; A5's iroh transport substitutes the real collection hash behind
/// this opaque field). The `package_id` is fresh per call — correlation of the
/// eventual ack is done in-memory via the worker's pending map, so it need not
/// persist across restarts.
fn announce_for_dir(dir: &Path) -> Result<PackageAnnounce> {
    let records = package::read_manifest(dir)?;
    let byte_size = records.iter().map(|r| r.byte_size).sum();
    let frame_count = records.len() as u32;

    let mut hashes: Vec<&str> = records.iter().map(|r| r.xxh3.as_str()).collect();
    hashes.sort_unstable();
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    for h in hashes {
        hasher.update(h.as_bytes());
        hasher.update(b"\n");
    }

    Ok(PackageAnnounce {
        package_id: PackageId(uuid::Uuid::new_v4().to_string()),
        root_hash: format!("{:016x}", hasher.digest()),
        byte_size,
        frame_count,
    })
}

/// Build the dedup [`Offer`](crate::sharing::iroh::proto::Msg::Offer) for a
/// package: one [`OfferEntry`] per manifest record keyed by `rel_path` + the
/// SAMPLING xxh3 (`duplicates::compute_xxhash`, matching `files.content_hash`),
/// plus a `rel_path → full xxh3` map for the second (full-hash) handshake round.
///
/// Returns `Err` on the first unhashable payload; the caller treats ANY error as
/// "abandon the handshake for the whole package" and falls back to a full send —
/// the dedup path is a best-effort optimization, never a correctness gate.
fn build_offer(
    dir: &Path,
    records: &[ManifestRecord],
) -> Result<(Vec<OfferEntry>, HashMap<String, String>)> {
    let mut offer = Vec::with_capacity(records.len());
    let mut full_by_rel = HashMap::with_capacity(records.len());
    for r in records {
        let sampling_hash = crate::duplicates::compute_xxhash(&dir.join(&r.rel_path))
            .with_context(|| format!("sampling-hash payload {}", r.rel_path))?;
        offer.push(OfferEntry {
            rel_path: r.rel_path.clone(),
            sampling_hash,
            byte_size: r.byte_size,
        });
        full_by_rel.insert(r.rel_path.clone(), r.xxh3.clone());
    }
    Ok((offer, full_by_rel))
}

/// Free the payload copies a confirmed package's directory holds, keeping only
/// `manifest.ndjson`, and return the number of bytes reclaimed.
///
/// [`write_package`](crate::package::write_package) *copies* every source file
/// into the package dir; once a package is `Confirmed` it is terminal and never
/// re-served, so those copies are pure dead weight — without this an observatory
/// keeps a full duplicate of every night it sends. The manifest is deliberately
/// preserved: Perseus's retention audit (`build_retention_history_rows`) and the
/// Sent-row naming both read it long after confirmation.
///
/// Packages are flat, but a manifest `rel_path` *may* carry a subdirectory (the
/// writer creates it), so this also walks subdirectories and removes the emptied
/// dirs. Cleaning an already-clean (manifest-only) dir is a cheap no-op that
/// frees 0 bytes. Errors are propagated so the caller can log-and-continue — a
/// cleanup failure must never fail the confirm transition or block startup.
///
/// `pub(super)` so the shared-payload coordinator
/// ([`SharedPackageCleanup`](super::cleanup_coord::SharedPackageCleanup)) reuses
/// this exact routine when it fires the once-only cleanup for a fanned-out dir.
pub(super) fn cleanup_package_payloads(dir: &Path) -> Result<u64> {
    let mut freed = 0u64;
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("read package dir {}", dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("read package dir entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat package entry {}", path.display()))?;
        if file_type.is_dir() {
            freed = freed.saturating_add(remove_tree_counting(&path)?);
        } else if file_type.is_file() {
            // The manifest is the one file that must survive cleanup.
            if entry.file_name() == std::ffi::OsStr::new(MANIFEST_FILENAME) {
                continue;
            }
            freed = freed.saturating_add(remove_file_counting(&path)?);
        }
        // Symlinks / other node types are never written into a package; skip.
    }
    Ok(freed)
}

/// Recursively remove every file and subdirectory under `dir`, then `dir`
/// itself, summing the bytes of the files removed. Only reached for the unusual
/// case of a package whose manifest `rel_path` carried a subdirectory.
fn remove_tree_counting(dir: &Path) -> Result<u64> {
    let mut freed = 0u64;
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("read package subdir {}", dir.display()))?
    {
        let entry = entry.with_context(|| format!("read subdir entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat subdir entry {}", path.display()))?;
        if file_type.is_dir() {
            freed = freed.saturating_add(remove_tree_counting(&path)?);
        } else {
            freed = freed.saturating_add(remove_file_counting(&path)?);
        }
    }
    std::fs::remove_dir(dir)
        .with_context(|| format!("remove emptied package subdir {}", dir.display()))?;
    Ok(freed)
}

/// Remove one file, returning its size in bytes (0 if it can't be stat'd).
fn remove_file_counting(path: &Path) -> Result<u64> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    std::fs::remove_file(path)
        .with_context(|| format!("remove package payload {}", path.display()))?;
    Ok(size)
}
