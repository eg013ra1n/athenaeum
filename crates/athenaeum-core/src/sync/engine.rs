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
//! - Fetch progress is receiver-driven and no longer rides the transport event
//!   stream (it flows through the fetch-time `FetchSink` instead), so
//!   `Transferring` is marked on the first *successful* announce — the in-flight
//!   window during which we await the ack.
//! - `AckReceived → Confirmed` (idempotent: a second ack for a package no longer
//!   in the in-flight map is logged at debug and dropped).
//! - Per-package ack/retry timeout → climb one backoff rung and wait it out,
//!   then re-announce ([`retry_backoff`]). A network condition (no ack, offline
//!   peer, serve/announce error) never marks a package `Failed`; it backs off and
//!   retries indefinitely. A persistently-erroring re-announce re-arms its
//!   deadline every time, so it advances the backoff rather than busy-spinning.
//! - [`cancel`](SyncEngineHandle::cancel) → `Cancelled` with a `cancelled`
//!   history outcome (a first-class terminal, distinct from `Failed`, since
//!   Task 3).
//! - Spec §1's one non-network terminal path: if the package dir/payload has
//!   vanished from disk, [`attempt`](Worker::attempt) fails it immediately via
//!   [`fail_package`](Worker::fail_package) — re-announcing can never succeed no
//!   matter how long it backs off, so this vanished-payload case is the only way
//!   to reach `Failed` (an explicit `cancel` reaches the distinct `Cancelled`).

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
    AnnounceFileEntry, FrameReceipt, NodeId, PackageAnnounce, PackageId, ReceiptOutcome,
    RevokeReason, TransportEvent,
};
use crate::sharing::SharingTransport;

use super::diagnostics::{classify_send_error, ConnectClass};
use super::models::{Direction, HistoryRow, OutboundFileState, OutboundRow, OutboundState};
use super::receiver::{SyncFileProgressEvent, SyncFinishedEvent, SyncProgressEvent};
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
pub type AddrRefresher = Arc<dyn Fn(NodeId) -> BoxFuture<Option<iroh::EndpointAddr>> + Send + Sync>;

/// Default per-attempt wait for the peer's ack before retrying.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// The exact `last_error` string the sender stamps on an outbound row when the
/// receiver DECLINES a transfer (a non-empty ack whose receipts are all
/// `Cancelled`). LOAD-BEARING STRING, single Rust source:
/// - the UI renders it as "Cancelled by the receiving device"
///   (`src/components/transfers/presentation.ts:83` — keep the TS literal in sync
///   by hand, it cannot share a Rust const);
/// - the api layer's declined-resend branch
///   (`crate::api::sync::resend_declined_as_new_transfer`) keys on it to divert a
///   Resend into a NEW batch identity (decline stays final per the old
///   `batch_uuid`; the re-ask mints a fresh one).
pub const CANCELLED_BY_RECEIVER_DETAIL: &str = "cancelled by receiver";

/// Far-future sleep target used when nothing is in flight (any command/event
/// wakes the worker before it elapses).
const IDLE_SLEEP: Duration = Duration::from_secs(3600);

/// Spec §2: capped exponential backoff, expressed as multiples of the base
/// rung (ack_timeout) so tests with short timeouts scale down naturally.
/// 30s → 1m → 5m → 15m → 30m with the default 30s base.
const BACKOFF_MULTIPLIERS: [u32; 5] = [1, 2, 10, 30, 60];

/// Flat retry interval while the peer looks ABSENT, as a multiple of the base
/// rung — 4 x the default 30 s `ack_timeout` = 2 minutes (D1 §3.3).
///
/// A multiple rather than an absolute for the same reason [`BACKOFF_MULTIPLIERS`]
/// is one: an engine configured with a millisecond timeout (every loopback test)
/// then observes the flat path in milliseconds instead of waiting two minutes.
const ABSENT_RETRY_MULTIPLIER: u32 = 4;

/// Whether this failure class says "the peer is not there", as opposed to "the
/// peer refuses us" or "our own side is broken" (D1 §3.3).
///
/// `NotStarted` counts as absent even though its doc reads "the local **or**
/// remote endpoint is not started": the remote reading IS an absent peer, and the
/// local one is transient — our own start fires the transport wake hook, which
/// releases the queue long before a flat interval matters. Escalating on it would
/// punish the commonest shape of "the other side's app is not running".
pub(crate) fn class_is_absent(class: Option<ConnectClass>) -> bool {
    matches!(
        class,
        Some(
            ConnectClass::NoRoute
                | ConnectClass::Timeout
                | ConnectClass::RelayUnreachable
                | ConnectClass::NotStarted
        )
    )
}

/// Which pending packages a reachability proof releases (D1 §3.4, hardened after
/// review): a BEACON releases everything armed, because the peer has just
/// (re)started and even an ack-timeout-parked package is waiting on exactly that;
/// ordinary CONTACT releases only what the absence parked, because a sibling whose
/// ack timed out while the peer was busy must keep its escalating ladder — zeroing
/// it would re-announce at a saturated peer every ack_timeout for the whole
/// transfer.
pub(crate) fn ids_to_release(
    proof: ReachabilityProof,
    pending: &[i64],
    absence_parked: &HashSet<i64>,
) -> Vec<i64> {
    match proof {
        ReachabilityProof::Beacon => pending.to_vec(),
        ReachabilityProof::Contact => pending
            .iter()
            .copied()
            .filter(|id| absence_parked.contains(id))
            .collect(),
    }
}

/// The backoff window for a given `rung` and failure `class`.
///
/// An ABSENT peer (see [`class_is_absent`]) gets a FLAT
/// `base * ABSENT_RETRY_MULTIPLIER` and never climbs: escalation exists to spare a
/// loaded peer, and an absent peer is not loaded — the only thing a 30-minute cap
/// buys against a switched-off laptop is half an hour of idleness after it is
/// already back. Everything else — a peer that is up and REFUSING us, an
/// unclassifiable failure, or an ack timeout that has no failed dial to classify
/// (`class = None`) — keeps `base * multiplier[rung]` with the last multiplier
/// held as the cap.
///
/// Delivery is retried forever either way (spec §2): a network condition never
/// terminalizes a package. `base` is the engine's [`SyncConfig::ack_timeout`] so
/// a test with a millisecond timeout gets a proportionally short schedule.
pub fn retry_backoff(base: Duration, rung: u32, class: Option<ConnectClass>) -> Duration {
    if class_is_absent(class) {
        return base * ABSENT_RETRY_MULTIPLIER;
    }
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
    /// never both drive the same row) and replies with the new id. `display_name`
    /// + `files` are the batch metadata (Transfers Status Model v2): the worker
    /// writes them in the SAME enqueue transaction as the row (kills the T3
    /// enqueue→announce race), so the row is never visible without its name +
    /// per-file manifest.
    Process {
        dir: PathBuf,
        display_name: Option<String>,
        files: Vec<AnnounceFileEntry>,
        reply: oneshot::Sender<Result<i64>>,
    },
    /// Cancel an in-flight package → `Cancelled` (`cancelled` history outcome).
    Cancel(i64),
    /// (Re)drive a resent transfer (Transfers Batch Model §D1/§D3). The API layer
    /// (`resend_transfer`) has already reset the SAME durable row to `Queued`
    /// (attempts++, fresh per-attempt wire id, per-file rows → `pending`) OUTSIDE
    /// the worker; the row is NOT in the in-memory `pending` map (its slot was
    /// dropped on the earlier terminal transition), so a plain [`Kick`](Self::Kick)
    /// cannot reach it. This tells the worker to read the fresh row and re-drive it
    /// exactly like a crash-resume.
    Resend(i64),
    /// Kick one in-flight package: collapse its retry deadline to now and reset
    /// its backoff rung so the next worker pass re-announces immediately (spec §2
    /// wake event / send-now). A no-op for an id with no pending slot.
    Kick(i64),
    /// This engine's peer announced itself online (D1 presence beacon). Clears
    /// the absence mark and releases every PARKED package — never touches one
    /// that is mid-pull awaiting its ack.
    PeerPresent,
    /// Kick **every** in-flight package (a relay-online / authorized-peer
    /// refresh nudge): the same immediate-retry reset applied to all pending
    /// slots.
    KickAll,
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
    /// Class of this package's most recent serve/announce dial failure (Task 3.1
    /// taxonomy), or `None` once an announce has succeeded (the address proved
    /// good) or before any classified failure. The retry path (Task 3.6) reads it
    /// to decide whether a freshly re-resolved address should REPLACE the peer's
    /// stale lookup entry (dead-addressing classes: `no_route` / `timeout` /
    /// `relay_unreachable`) rather than merge onto a possibly-dead relay URL.
    last_attempt_class: Option<ConnectClass>,
    /// Last per-file state we PERSISTED for each served `rel_path` (Transfers
    /// Status Model v2 §D4). In-memory transition tracker so a per-byte
    /// [`ServeFileProgress`](TransportEvent::ServeFileProgress) tick only writes
    /// the DB when the file's state actually changes (`pending → sending` on the
    /// first partial tick, `sending → uploaded` on completion) — never per byte.
    /// Rebuilt from scratch on a fresh engine (the DB holds the durable state).
    file_states: HashMap<String, OutboundFileState>,
    /// Whether the `serve_started` journal event has been recorded for this
    /// send session (§D7). The first [`ServeProgress`](TransportEvent::ServeProgress)
    /// tick (the peer began pulling) sets it; retries re-serve but do not re-log.
    serve_started_logged: bool,
    /// Set when a retry is armed (ack-timeout backoff or a failed serve/announce);
    /// cleared on the first [`ServeProgress`](TransportEvent::ServeProgress) tick
    /// after it, which also wipes the stale `last_error` (spec §D5: a transfer
    /// actually moving bytes must never carry a stale reason). Exactly one
    /// clear-write per retry cycle, only when bytes resume flowing.
    clear_error_on_next_serve: bool,
    /// Bytes already delivered by EARLIER pull sessions of this same transfer, the
    /// base the current session's figure is added to.
    ///
    /// The provider's byte counter is scoped to ONE GET request: the iroh consumer
    /// builds a fresh `UploadAccumulator` per `GetRequestReceivedNotify`. When a
    /// connection drops mid-transfer the receiver re-requests only the ranges it
    /// still lacks, so the next session's ticks start from ~0 — and emitting them
    /// raw made the sender's overall progress bar fall back to the start and count
    /// the remainder as if it were the whole batch (owner smoke 2026-07-25).
    ///
    /// Recomputed from the durable per-file rows (the transfer's real memory)
    /// whenever a new session is detected, never accumulated from the ticks
    /// themselves — so it is also correct after an app restart, and self-corrects
    /// if a peer genuinely re-pulls a file (that row walks back to `sending` and
    /// leaves the base).
    served_base: u64,
    /// The previous [`ServeProgress`](TransportEvent::ServeProgress) figure of the
    /// current session. A tick BELOW it means the provider started a new request,
    /// which is the only signal available that a session boundary was crossed.
    last_serve_tick: u64,
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
            peer_state: PeerReachability::default(),
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
    /// (valid for [`cancel`](Self::cancel)). The worker inserts the row —
    /// together with its `display_name` and per-file `pending` rows, in one
    /// transaction — and begins driving it; this awaits the worker's id reply.
    ///
    /// `display_name` is the human batch name (Transfers Status Model v2 §D1);
    /// `files` the per-payload `(rel_path, byte_size, frame_uuid)` manifest (§D4).
    /// Pass `None` / `Vec::new()` for a nameless / file-manifest-less send
    /// (Perseus, a collab project package, a re-enqueued retry) — the announce's
    /// basename fallback still applies.
    pub async fn enqueue_package(
        &self,
        dir: impl AsRef<Path>,
        display_name: Option<String>,
        files: Vec<AnnounceFileEntry>,
    ) -> Result<i64> {
        let dir = dir.as_ref().to_path_buf();
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Process {
                dir,
                display_name,
                files,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        match reply_rx.await {
            Ok(res) => res,
            Err(_) => Err(anyhow!("sync engine worker dropped enqueue reply")),
        }
    }

    /// Cancel an in-flight package. Terminal (`Cancelled`) once processed; a no-op
    /// if the package is already terminal.
    pub async fn cancel(&self, id: i64) -> Result<()> {
        self.cmd_tx
            .send(Command::Cancel(id))
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        Ok(())
    }

    /// (Re)drive a resent transfer (Transfers Batch Model §D1/§D3). The caller
    /// (`resend_transfer`) has already reset the SAME durable row back to `Queued`
    /// (attempts++, fresh per-attempt wire id, files → `pending`); this asks the
    /// worker to pick that row up and re-drive it like a crash-resume. Distinct
    /// from [`kick`](Self::kick), which only collapses the backoff of a row already
    /// in the worker's `pending` map — a resent row's slot was dropped when it went
    /// terminal, so kick alone would be a no-op.
    pub async fn resend(&self, id: i64) -> Result<()> {
        self.cmd_tx
            .send(Command::Resend(id))
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        Ok(())
    }

    /// Kick one in-flight package (send-now / spec §2 wake event): wake it out of
    /// its backoff so the worker re-announces on the next pass. A no-op on the
    /// worker side if the id has no pending slot (already terminal / unknown).
    /// Tasks 6/8/9 call this from the retry/send-now command surfaces.
    pub async fn kick(&self, id: i64) -> Result<()> {
        self.cmd_tx
            .send(Command::Kick(id))
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        Ok(())
    }

    /// Tell the worker its peer just announced itself online (D1 presence
    /// beacon). Clears the absence mark and releases every PARKED package; a
    /// package mid-pull is deliberately untouched. Idempotent and cheap — the
    /// host debounces per peer before calling.
    pub async fn peer_present(&self) -> Result<()> {
        self.cmd_tx
            .send(Command::PeerPresent)
            .await
            .map_err(|_| anyhow!("sync engine worker stopped"))?;
        Ok(())
    }

    /// Kick every in-flight package (a relay-online / authorized-peer refresh
    /// nudge): the immediate-retry wake applied to all pending slots. The worker
    /// resets each slot's deadline; delivery follows on the next pass.
    pub async fn kick_all(&self) -> Result<()> {
        self.cmd_tx
            .send(Command::KickAll)
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
/// The engine's in-memory view of ITS peer's reachability (D1 §3.4).
///
/// One engine is one peer, so nothing here needs keying. Not persisted on
/// purpose: after a restart the first attempt re-establishes the truth in
/// seconds, and a stale "absent since" restored from disk would be worse than
/// none — it would name a time the peer may have been back for hours.
#[derive(Debug, Default)]
struct PeerReachability {
    /// When the peer first looked absent, stamped only on the transition into
    /// absence (so it reads as "in this state since", not "last seen failing").
    absent_since: Option<Instant>,
    /// The package currently carrying the live probe deadline while absent. Every
    /// other pending package parks with no deadline and waits for a signal.
    head: Option<i64>,
    /// The packages THIS absence parked. Tracked explicitly because a parked
    /// package and an ack-timeout-armed one are both `NextAction::Retry`, and only
    /// the former may be released by ordinary contact — releasing the latter would
    /// zero the ladder of a package whose peer is up and simply busy.
    parked: HashSet<i64>,
}

/// What proved the peer reachable, and therefore how much of its queue to release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReachabilityProof {
    /// The peer announced itself (D1 beacon). It has just (re)started, so EVERY
    /// armed package goes — including one whose ack timed out because the peer died
    /// mid-ingest (audit F7), for which spec §6 makes the beacon the only rescue.
    Beacon,
    /// We reached the peer in the course of ordinary work: an announce accepted, a
    /// serve tick, an ack. This proves an absence is over, but says NOTHING about a
    /// sibling whose ack timed out while the peer was busy ingesting — that one
    /// keeps its ladder. Releasing it too would re-announce at a saturated peer
    /// every ack_timeout for the whole length of the transfer, which is exactly
    /// what §3.3 reserves the escalating schedule to prevent.
    Contact,
}

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
    /// This peer's reachability (D1). Drives the coalescing that keeps a flat
    /// retry affordable: while the peer is absent exactly one package probes.
    peer_state: PeerReachability,
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
            // Before deciding how long to sleep: if the absent peer's probing head
            // has left the queue, elect and wake its successor (D1 §3.4). Doing it
            // here rather than on each removal path means no future terminal path
            // can silently park a whole queue for an hour.
            self.ensure_absent_head_alive();
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
                    Some(Command::Process { dir, display_name, files, reply }) => {
                        // The enqueue writes the row + its display_name + per-file
                        // `pending` rows in one transaction, so the row is never
                        // visible (to this worker's own `start_package`, to
                        // `status_snapshot`, or to a concurrent reader) without its
                        // name + manifest — the T3 enqueue→announce race is gone.
                        match self.store.enqueue(&dir.to_string_lossy(), self.peer, display_name.as_deref(), &files) {
                            Ok(id) => {
                                tracing::info!(package_id = id, state = "queued", "sync state");
                                // §D6: stamp the collab `project_id` onto the row at
                                // the store enqueue path so collab transfers are
                                // distinguishable from personal ones. The stamp lives
                                // in the on-disk manifest (a project package's records
                                // carry it); read it once here, best-effort — a
                                // personal send finds no stamp and skips the write. No
                                // behavioral use this wave.
                                self.persist_project_id(id, &dir);
                                // Journal the batch's first lifecycle event (§D7).
                                let byte_size: u64 = files.iter().map(|f| f.byte_size).sum();
                                self.journal(id, "enqueued", Some(&format!("frames={} bytes={}", files.len(), byte_size)));
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
                    Some(Command::Resend(id)) => self.resend_package(id).await,
                    Some(Command::PeerPresent) => {
                        self.peer_reachable("presence", ReachabilityProof::Beacon)
                    }
                    Some(Command::Kick(id)) => self.kick_one(id),
                    Some(Command::KickAll) => {
                        // Snapshot the ids so no `&self.pending` borrow is held
                        // across the `&mut self` kick_one calls.
                        let ids: Vec<i64> = self.pending.keys().copied().collect();
                        for id in ids {
                            self.kick_one(id);
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
            emit_event(
                em.as_ref(),
                "sync-progress",
                &SyncProgressEvent {
                    package_id: id.to_string(),
                    direction: Direction::Sent,
                    stage: stage.to_string(),
                    peer_device: node_id_hex(&self.peer),
                    frame_count,
                    project_id,
                    // Sender-side ticks carry no byte figure (Task 11 fetch bytes are
                    // a receive-side concern).
                    bytes_done: None,
                    bytes_total: None,
                },
            );
        }
    }

    /// Emit a send-side `sync-progress` tick that carries byte figures (Task 13) —
    /// the byte-bearing sibling of [`emit_progress`](Self::emit_progress), used
    /// for the `transferring` stage driven by real upload progress from the
    /// transport's provider-upload events. `bytes_done` / `bytes_total` populate
    /// the [`SyncProgressEvent`] Option fields; everything else matches
    /// `emit_progress`. Never logs per tick (progress is UI data, not a log).
    fn emit_progress_bytes(
        &self,
        id: i64,
        stage: &str,
        frame_count: u32,
        bytes_done: u64,
        bytes_total: u64,
    ) {
        if let Some(em) = &self.emitter {
            let project_id = self.pending.get(&id).and_then(|p| p.project_id.clone());
            emit_event(
                em.as_ref(),
                "sync-progress",
                &SyncProgressEvent {
                    package_id: id.to_string(),
                    direction: Direction::Sent,
                    stage: stage.to_string(),
                    peer_device: node_id_hex(&self.peer),
                    frame_count,
                    project_id,
                    bytes_done: Some(bytes_done),
                    bytes_total: Some(bytes_total),
                },
            );
        }
    }

    /// Emit a send-side `sync-file-progress` tick for one file of a package we are
    /// serving (Task 2.2). The direction-agnostic
    /// [`SyncFileProgressEvent`](SyncFileProgressEvent) the receiver already emits
    /// (`sync/receiver.rs`) is reused verbatim — `package_id` is the outbound row id
    /// (matching the outbound Active row key), `file` the full forward-slash
    /// `rel_path`. No-op without a host emitter; never logs per tick (progress is UI
    /// data, not a log).
    fn emit_file_progress(&self, id: i64, file: String, bytes_done: u64, bytes_total: u64) {
        if let Some(em) = &self.emitter {
            emit_event(
                em.as_ref(),
                "sync-file-progress",
                &SyncFileProgressEvent {
                    package_id: id.to_string(),
                    peer_device: node_id_hex(&self.peer),
                    file,
                    bytes_done,
                    bytes_total,
                },
            );
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
            emit_event(
                em.as_ref(),
                "sync-finished",
                &SyncFinishedEvent {
                    package_id: id.to_string(),
                    direction: Direction::Sent,
                    outcome: outcome.to_string(),
                    peer_device: node_id_hex(&self.peer),
                    ok_count,
                    failed,
                    new_count,
                    duplicate_count,
                    project_id,
                },
            );
        }
    }

    /// Append one entry to this batch's `sync_events` journal (Transfers Status
    /// Model v2 §D7), `direction = sent`, `batch_key` = the outbound row id as
    /// text. Best-effort: a journal-write failure `warn!`s and never fails the
    /// transfer (§D7 — the log is a diagnostic, not delivery truth).
    fn journal(&self, id: i64, kind: &str, detail: Option<&str>) {
        if let Err(e) = self
            .store
            .append_sync_event(Direction::Sent, &id.to_string(), kind, detail)
        {
            tracing::warn!(package_id = id, kind, error = %format!("{e:#}"), "append sync_event failed");
        }
    }

    /// Settle each per-file row named in `receipts` (§D4): map `frame_uuid` →
    /// `rel_path` via `records`, write the receiver's verdict (same text encoding
    /// as `sync_receipts` — `ingested` / `duplicate` / `cancelled` /
    /// `rejected:<reason>`), state `done`. A `Rejected` receipt also stamps the
    /// per-file `error`. Best-effort per row (a write failure `warn!`s, never
    /// fails the ack). A receipt whose `frame_uuid` isn't in the manifest, or a
    /// row that doesn't exist (a Perseus batch with no per-file rows), is a no-op.
    fn settle_files_from_receipts(
        &self,
        id: i64,
        records: &[ManifestRecord],
        receipts: &[FrameReceipt],
    ) {
        let by_uuid: HashMap<&str, &ManifestRecord> =
            records.iter().map(|r| (r.frame_uuid.as_str(), r)).collect();
        for rec in receipts {
            let Some(m) = by_uuid.get(rec.frame_uuid.as_str()) else {
                continue;
            };
            let outcome = super::store::receipt_outcome_to_db(&rec.outcome);
            let error = match &rec.outcome {
                ReceiptOutcome::Rejected(msg) => Some(msg.as_str()),
                _ => None,
            };
            if let Err(e) = self.store.set_outbound_file_state(
                id,
                &m.rel_path,
                OutboundFileState::Done,
                m.byte_size,
                Some(&outcome),
                error,
            ) {
                tracing::warn!(package_id = id, rel_path = %m.rel_path, error = %format!("{e:#}"), "settle outbound file (receipt) failed");
            }
        }
    }

    /// Give every not-yet-settled per-file row of a terminal-`cancelled`/`failed`
    /// batch that terminal `outcome`, state `done` (§D4). Rows already `done` (an
    /// ack settled them) are left as they are — the receiver's verdict wins over
    /// the batch terminal. Best-effort throughout.
    fn settle_unsettled_files(&self, id: i64, outcome: &str) {
        let rows = match self.store.list_outbound_files(id) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(package_id = id, error = %format!("{e:#}"), "list outbound files for terminal settle failed");
                return;
            }
        };
        for row in rows {
            if row.state == OutboundFileState::Done {
                continue;
            }
            if let Err(e) = self.store.set_outbound_file_state(
                id,
                &row.rel_path,
                OutboundFileState::Done,
                row.bytes_done,
                Some(outcome),
                None,
            ) {
                tracing::warn!(package_id = id, rel_path = %row.rel_path, error = %format!("{e:#}"), "settle outbound file (terminal) failed");
            }
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
        let started = !matches!(
            prior_state,
            OutboundState::Queued | OutboundState::Announced
        );
        let delivered_base = self.delivered_bytes_base(id);
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
                last_attempt_class: None,
                file_states: HashMap::new(),
                serve_started_logged: false,
                clear_error_on_next_serve: false,
                // Seeded from the durable rows, not zero: a resurrected engine
                // installs this slot mid-transfer, and the peer then resumes with a
                // fresh per-request counter. A brand-new enqueue has no delivered
                // rows yet, so this reads 0 there.
                served_base: delivered_base,
                last_serve_tick: 0,
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
        let project =
            self.pending
                .get(&id)
                .and_then(|p| match (&p.project_id, &p.hub_package_id) {
                    (Some(pid), Some(hub)) => Some((pid.clone(), hub.clone())),
                    _ => None,
                });

        // The personal-sync announce goes out as v2 (batch name + file manifest).
        // Batch name = the outbound row's `display_name` (set by the send path),
        // falling back to the package-dir basename for a row that never got a name
        // (a re-enqueued retry, a foreign package). File manifest = the package's
        // manifest records, cached on the first build (`negotiate_and_build`), with
        // a fresh on-disk read as a defensive fallback so `files` is never empty
        // when payloads exist.
        // §D2/B3 (B1 seam): derive the basename through the SAME `package_dir_key`
        // helper the sender-side history writers and the `outbound_package_key`
        // reader use (`file_name().and_then(to_str)`), NOT `to_string_lossy` — so
        // the equivalence `batch_uuid == outbound_package_key(package_ref)` holds by
        // construction and the receiver keys ONE inbound row on exactly the value
        // the sent-history / detail joins recover from the outbound row.
        let pkg_basename = package_dir_key(&dir).unwrap_or_default();
        // Durable batch identity (spec §D2/B3): the package-dir basename IS the
        // final batch_uuid for an outbound transfer, stable across re-attempts, so
        // this is the real value the receiver keys ONE row on — not a placeholder.
        let batch_uuid = pkg_basename.clone();
        let batch_name = match self.store.get_outbound(id) {
            Ok(Some(row)) => row
                .display_name
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(pkg_basename),
            Ok(None) => pkg_basename,
            Err(e) => {
                tracing::warn!(package_id = id, error = %e, "read display_name for announce failed");
                pkg_basename
            }
        };
        // The announce describes THIS ATTEMPT, and every part of it must describe
        // the same thing. `announce.frame_count` and `announce.byte_size` are
        // already the negotiated subset (see `negotiate`), so the file list is
        // filtered to match: the receiver then writes one `sync_inbound_files` row
        // per file that will actually travel.
        //
        // Sending the FULL manifest alongside a subset count left the receiver with
        // rows for files the sender had already decided not to send. They could
        // never be settled — no fetch tick, no receipt — so they sat `announced`
        // forever and the receive-side counter could not reach its own total: a
        // delivered transfer read "3 of 5". The excluded files are duplicates the
        // peer already holds, so it loses nothing by not hearing about them; the
        // SENDER's own rows still carry them, settled `done`/`duplicate` (§D4), which
        // is where "38 sent, 35 already there" is legible.
        let announce_files: Vec<AnnounceFileEntry> = {
            let cached = self
                .pending
                .get(&id)
                .and_then(|p| p.manifest_records.clone());
            let records = match cached {
                Some(r) => r,
                None => package::read_manifest(&dir).unwrap_or_default(),
            };
            records
                .iter()
                .filter(|r| want.as_ref().is_none_or(|w| w.contains(&r.rel_path)))
                .map(|r| AnnounceFileEntry {
                    rel_path: r.rel_path.clone(),
                    byte_size: r.byte_size,
                    frame_uuid: r.frame_uuid.clone(),
                })
                .collect()
        };

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
                    .announce(
                        self.peer,
                        &announce,
                        &batch_name,
                        &batch_uuid,
                        &announce_files,
                    )
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
            // Task 3.1: classify the dial outcome (string-based — the engine only
            // sees an anyhow chain through the transport trait) into a stable
            // snake_case class. It rides the log as `class` and prefixes the
            // stored `last_error` so a later UI task maps it to human text.
            let class = classify_send_error(&reason);
            let class_tag = class.tag();
            tracing::error!(package_id = id, attempts, class = class_tag, error = %reason, "sync serve/announce failed; will retry");
            // Record the class-prefixed attempt-error reason for the Perseus
            // status page (Task 9). Best-effort: a diagnostic write must never turn
            // a retryable transfer into a failure, so a store error here is logged,
            // not propagated.
            let stored = format!("{class_tag}: {reason}");
            if let Err(se) = self.store.set_last_error(id, Some(&stored)) {
                tracing::warn!(package_id = id, error = %se, "record last_error (serve/announce) failed");
            }
            // Journal the class-tagged failure (§D7) before arming the retry (which
            // journals `retry_scheduled`).
            self.journal(id, "announce_failed", Some(&stored));
            if let Some(p) = self.pending.get_mut(&id) {
                p.announce = Some(announce);
                p.want = want;
                // Task 3.6: remember the dial-failure class so the retry path can
                // decide whether a fresh re-resolved address should replace (not
                // merge onto) the peer's stale lookup entry.
                p.last_attempt_class = Some(class);
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
            tracing::info!(
                package_id = id,
                state = "transferring",
                "sync resume/retry re-announce"
            );
        }
        // The announce reached the peer — journal it (§D7). Fires on every
        // successful announce (first + every resume/retry re-announce).
        self.journal(id, "announce_sent", None);

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
            // The serve/announce reached the peer — the cached address proved good,
            // so clear any stale dial-failure class (Task 3.6): a later retry after
            // an ack timeout must not treat proven-good addressing as dead.
            p.last_attempt_class = None;
        }
        // Reaching the peer IS proof it is back (D1 §3.4): release anything the
        // absence parked, so a queue behind the probing head drains at once
        // instead of waiting for a beacon that may never come (an older peer
        // never sends one).
        self.peer_reachable("announce", ReachabilityProof::Contact);
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
        let mut full_announce = match announce_for_dir(dir) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(package_id = id, error = %format!("{e:#}"), "sync build announce failed; will retry");
                self.arm_retry(id);
                return Ok(Negotiated::Deferred);
            }
        };

        // Stable wire package_id across sender restarts (zombie-inbound fix).
        // `announce_for_dir` mints a FRESH uuid on every call, but the row's wire
        // id must be stable so a crash-resume re-announce reuses the same id the
        // receiver's `sync_inbound` row was keyed on (else that row is orphaned as
        // a perpetual "fetching" zombie while delivery completes under a new id).
        // So: reuse the value persisted on the row if present, else persist the
        // freshly-minted one. Done BEFORE the dedup negotiate + subset build below,
        // which both derive from `full_announce.package_id`. A re-enqueue (retry)
        // is a NEW row with no persisted value → mints fresh (deliberate — see
        // `set_wire_package_id`). Persist is best-effort: a failed write warns and
        // still sends now; the worst case is a fresh id minted on the next restart.
        match self.store.get_outbound(id) {
            Ok(Some(row)) => match row.wire_package_id {
                Some(persisted) => full_announce.package_id = PackageId(persisted),
                None => {
                    if let Err(e) = self
                        .store
                        .set_wire_package_id(id, &full_announce.package_id.0)
                    {
                        tracing::warn!(package_id = id, error = %format!("{e:#}"), "persist wire_package_id failed");
                    }
                }
            },
            Ok(None) => {
                tracing::warn!(
                    package_id = id,
                    "outbound row gone while building announce; wire id not persisted"
                );
            }
            Err(e) => {
                tracing::warn!(package_id = id, error = %format!("{e:#}"), "read outbound row for wire id failed");
            }
        }

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
            tracing::debug!(
                package_id = id,
                "project package; skipping dedup negotiation (full send)"
            );
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
                        .negotiate_want(
                            self.peer,
                            full_announce.package_id.clone(),
                            offer,
                            full_by_rel,
                        )
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
        // package to a confirmed terminal WITHOUT announcing or serving. Pass the
        // just-minted wire id (== the persisted `wire_package_id` this build
        // resolved) so a superseded-race revoke can address the receiver's stuck
        // row (see `terminalize_all_duplicate`).
        if matches!(&want, Some(w) if w.is_empty()) {
            self.terminalize_all_duplicate(id, &records, &full_announce.package_id)?;
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
        // §D4: files EXCLUDED from the want subset — the peer already has them, so
        // they are never transferred. Settle them terminal now (`done`/`duplicate`)
        // so the UI never shows a duplicate as pending. Only applies to a real
        // subset (`want = Some`); a full send / fallback (`None`) excludes nothing.
        if let Some(w) = &want {
            for r in &records {
                if !w.contains(&r.rel_path) {
                    if let Err(e) = self.store.set_outbound_file_state(
                        id,
                        &r.rel_path,
                        OutboundFileState::Done,
                        0,
                        Some("duplicate"),
                        None,
                    ) {
                        tracing::warn!(package_id = id, rel_path = %r.rel_path, error = %format!("{e:#}"), "mark duplicate outbound file failed");
                    }
                }
            }
        }
        self.journal(
            id,
            "negotiated",
            Some(&format!("new={new_count} duplicate={duplicate_count}")),
        );
        Ok(Negotiated::Send { announce, want })
    }

    /// Drive a package straight to a confirmed terminal because the dedup
    /// handshake found the peer already holds every frame (empty want). Reuses
    /// the exact confirmed-terminal mechanics of [`on_ack`](Self::on_ack) —
    /// per-frame `Duplicate` receipts, confirmed history, payload cleanup routed
    /// through the [`cleanup_sink`] (so a multi-target coordinator counts this
    /// engine terminal), and the confirmed state stamp — but skips announce /
    /// serve / blob release entirely (nothing was ever served).
    fn terminalize_all_duplicate(
        &mut self,
        id: i64,
        records: &[ManifestRecord],
        wire_id: &PackageId,
    ) -> Result<()> {
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
        // Superseded-race revoke (spec §F3/§D2): all-duplicate normally runs on a
        // FIRST build (fresh enqueue / fresh resend — nothing announced yet, so no
        // receiver row exists for this wire id). But a re-drive that re-negotiates
        // AFTER a PRIOR attempt already announced (a crash-resume of a `Transferring`
        // row) can find the peer now holds every frame — and confirm all-duplicate
        // locally while the receiver's row for this same wire id sits `announced`
        // forever. `pending.started` is true exactly in that case (a prior announce
        // for this transfer reached the peer, at the same persisted wire id this
        // build resolved), so tell the receiver to drop its stuck row. A fresh
        // enqueue / fresh resend has `started == false` → no stray revoke.
        if pending.started {
            self.spawn_revoke(pending.id, wire_id, RevokeReason::Superseded);
        }
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
        // §D4/§D7: every file was a duplicate — settle each row `done`/`duplicate`
        // and journal the negotiated + confirmed outcome.
        self.journal(
            pending.id,
            "negotiated",
            Some(&format!("new=0 duplicate={}", records.len())),
        );
        self.settle_files_from_receipts(pending.id, records, &receipts);
        self.journal(pending.id, "confirmed", Some("all_duplicate"));
        self.append_confirmed_history(&pending, &receipts)?;
        // Same shared-payload discipline as the ack path: defer to the
        // coordinator when fanned out, else clean the payload copies in line.
        match &self.cleanup_sink {
            Some(sink) => sink.on_terminal(&pending.dir),
            None => match cleanup_package_payloads(&pending.dir) {
                Ok(freed_bytes) => {
                    tracing::info!(
                        package_id = pending.id,
                        freed_bytes,
                        "package payloads cleaned"
                    );
                }
                Err(e) => {
                    tracing::warn!(package_id = pending.id, error = %format!("{e:#}"), "package payload cleanup failed");
                }
            },
        }
        tracing::info!(
            package_id = pending.id,
            state = "confirmed",
            reason = "all_duplicate",
            "sync state"
        );
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

    /// The package that probes an absent peer: the lowest pending row id, so the
    /// choice is deterministic (no "whichever failed last" churn) and stable
    /// across passes. Re-elected whenever the previous head leaves `pending` —
    /// confirmed, cancelled, or terminalized by a local fault — so a queue whose
    /// probing package dies never goes silent.
    fn elect_absent_head(&mut self) -> Option<i64> {
        let head = self
            .peer_state
            .head
            .filter(|id| self.pending.contains_key(id))
            .or_else(|| self.pending.keys().min().copied());
        self.peer_state.head = head;
        head
    }

    /// Keep the absent peer's probe alive across the head leaving `pending`.
    ///
    /// The head can vanish without any failure of its own — it confirms, gets
    /// cancelled, or terminalizes on a local fault (a payload deleted off disk).
    /// Its queue-mates are parked with no deadline, so unless a new head is
    /// elected AND woken here, the whole queue would sleep out `IDLE_SLEEP` — an
    /// hour of silence for a peer we are supposed to be probing every two minutes.
    /// Called once per worker pass, so no removal path can forget it.
    fn ensure_absent_head_alive(&mut self) {
        if self.peer_state.absent_since.is_none() {
            return;
        }
        let head_alive = self
            .peer_state
            .head
            .is_some_and(|id| self.pending.contains_key(&id));
        if head_alive {
            return;
        }
        let Some(new_head) = self.elect_absent_head() else {
            return; // nothing pending for this peer at all
        };
        if let Some(p) = self.pending.get_mut(&new_head) {
            p.deadline = Instant::now();
            tracing::debug!(
                package_id = new_head,
                peer = %super::node_id_hex(&self.peer),
                "absent-peer probe re-elected after the previous head left the queue"
            );
        }
    }

    /// Mark this engine's peer absent (idempotent) and return whether `id` is the
    /// package that should carry the live probe deadline.
    ///
    /// Coalescing is what makes the flat schedule affordable (D1 §3.4): without
    /// it, fifty packages queued to one shut laptop would each dial every window.
    fn note_peer_absent(&mut self, id: i64) -> bool {
        if self.peer_state.absent_since.is_none() {
            self.peer_state.absent_since = Some(Instant::now());
            tracing::info!(
                peer = %super::node_id_hex(&self.peer),
                pending = self.pending.len(),
                "peer looks absent; parking its queue behind one probe"
            );
        }
        self.elect_absent_head() == Some(id)
    }

    /// The peer is reachable: forget the absence and release what this proof
    /// entitles us to release (see [`ReachabilityProof`]). `reason` names what
    /// proved it and rides the log line.
    ///
    /// Only packages in [`NextAction::Retry`] are ever touched. One in `AwaitAck`
    /// is mid-pull — collapsing its deadline would fire a pointless re-announce at
    /// a peer that is already reading our bytes.
    fn peer_reachable(&mut self, reason: &'static str, proof: ReachabilityProof) {
        let was_absent = self.peer_state.absent_since.take().is_some();
        self.peer_state.head = None;
        let parked = std::mem::take(&mut self.peer_state.parked);
        let all: Vec<i64> = self.pending.keys().copied().collect();
        let ids = ids_to_release(proof, &all, &parked);
        let mut released = 0usize;
        for id in ids {
            if let Some(p) = self.pending.get_mut(&id) {
                if p.next_action == NextAction::Retry {
                    p.deadline = Instant::now();
                    p.rung = 0;
                    released += 1;
                }
            }
        }
        if was_absent || released > 0 {
            tracing::info!(
                peer = %super::node_id_hex(&self.peer),
                reason,
                count = released,
                "peer reachable; releasing its parked queue"
            );
        }
    }

    /// Arm a backoff deadline on a still-pending package after a failed
    /// build/serve/announce attempt: climb one rung and wait out
    /// [`retry_backoff`] before the next announce (spec §2). Leaves the milestone
    /// untouched — the row stays `Queued` until an announce actually succeeds, and
    /// is never terminalized from a network condition.
    fn arm_retry(&mut self, id: i64) {
        // Read what the decision needs, then drop the borrow: electing an absent
        // head takes `&mut self`.
        let Some(class) = self.pending.get(&id).map(|p| p.last_attempt_class) else {
            return; // slot gone (confirmed/cancelled concurrently)
        };
        let absent = class_is_absent(class);
        // While the peer is absent exactly ONE package carries a live deadline;
        // the rest park and wait for a signal instead of a clock (D1 §3.4).
        let is_head = !absent || self.note_peer_absent(id);

        let armed = if let Some(p) = self.pending.get_mut(&id) {
            // An absent peer does not climb: the rung indexes an escalation that
            // only makes sense when repeated failure says something about US or
            // about a peer that is up and unhappy (D1 §3.3).
            if !absent {
                p.rung = p.rung.saturating_add(1);
            }
            p.next_action = NextAction::Retry;
            // §D5: a retry is now pending — the next serve tick (if bytes resume
            // flowing) must clear the stale reason.
            p.clear_error_on_next_serve = true;
            let delay = retry_backoff(self.config.ack_timeout, p.rung, class);
            if is_head {
                p.deadline = Instant::now() + delay;
                Some((p.rung, delay, class.map(|c| c.tag())))
            } else {
                // Parked behind the head: no deadline of its own. `IDLE_SLEEP`
                // keeps it out of `next_deadline()`'s way; what releases it is
                // `peer_reachable` — a presence beacon, or the head getting through.
                p.deadline = Instant::now() + IDLE_SLEEP;
                None
            }
        } else {
            None
        };
        // Record (or clear) the absence-parked marker outside the borrow: this is
        // what lets ordinary contact release exactly the packages the absence
        // parked, and nothing else.
        if absent && !is_head {
            self.peer_state.parked.insert(id);
        } else {
            self.peer_state.parked.remove(&id);
        }

        // Persist the wall-clock retry deadline (Task 2) + journal it (§D7) once the
        // `&mut pending` borrow is dropped, so the UI can render a countdown and a
        // restart re-arms honestly.
        match armed {
            Some((rung, delay, class)) => {
                self.persist_next_retry(id, delay);
                let detail = match class {
                    Some(c) => format!(
                        "rung={rung} delay_ms={} class={c}",
                        delay.as_millis() as u64
                    ),
                    None => format!("rung={rung} delay_ms={}", delay.as_millis() as u64),
                };
                self.journal(id, "retry_scheduled", Some(&detail));
            }
            None => {
                // A parked package has no countdown to show: clearing the persisted
                // deadline is what stops the UI rendering a number that means
                // nothing (it is waiting for a signal, not for that instant).
                self.clear_next_retry(id);
                self.journal(id, "parked_peer_absent", None);
            }
        }
    }

    /// Persist the wall-clock deadline (`now + delay`) of a just-armed `Retry`
    /// window as `OutboundRow::next_retry_at` (Task 2). Best-effort (spec §2): a
    /// store error is logged and dropped — a failed diagnostic write must never
    /// break scheduling.
    fn persist_next_retry(&self, id: i64, delay: Duration) {
        self.persist_next_retry_at(id, &retry_deadline_stamp(delay));
    }

    /// As [`persist_next_retry`] but for a caller that has already rendered the
    /// wall-clock stamp (the ack-timeout path logs it, so it reuses it here
    /// rather than recomputing `Utc::now() + delay` a second time).
    fn persist_next_retry_at(&self, id: i64, stamp: &str) {
        if let Err(e) = self.store.set_next_retry_at(id, Some(stamp)) {
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

    /// Kick one package (Task 5, spec §2 wake event / send-now): collapse its
    /// retry deadline to `now` and reset its backoff rung so the very next worker
    /// pass re-announces immediately instead of waiting out the current backoff
    /// window ([`next_deadline`](Self::next_deadline) re-reads the deadline every
    /// iteration, so no extra wakeup is needed). Sets [`NextAction::Retry`] so the
    /// collapsed deadline means "attempt now", and clears the persisted countdown
    /// (Task 2) since the retry is immediate. The attempts counter is left
    /// untouched — it is a diagnostic tally, not a scheduling input. A no-op
    /// (logged at debug) for an id with no pending slot: already terminal
    /// (confirmed/failed/cancelled) or unknown.
    fn kick_one(&mut self, id: i64) {
        match self.pending.get_mut(&id) {
            Some(p) => {
                p.rung = 0;
                p.next_action = NextAction::Retry;
                p.deadline = Instant::now();
            }
            None => {
                tracing::debug!(package_id = id, "kick ignored (no pending slot)");
                return;
            }
        }
        // The `&mut self.pending` borrow is dropped above; clear the persisted
        // retry countdown (never-swallow helper) now that the retry is immediate.
        self.clear_next_retry(id);
        tracing::info!(package_id = id, "kick: immediate retry");
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

    /// Fire-and-forget best-effort `Revoke` to the peer (spec §D2 / B3). Sent on a
    /// terminal transition whose CURRENT attempt still had an outstanding announce
    /// the peer may be acting on: user cancel ([`RevokeReason::Cancelled`]), an
    /// all-duplicate confirm that raced a prior attempt's announce
    /// ([`RevokeReason::Superseded`]), or a local terminal failure
    /// ([`RevokeReason::Failed`]). `wire_id` is that attempt's wire id — the id the
    /// receiver keyed its inbound row on and correlates acks by.
    ///
    /// Revoke IS the stop mechanism (B0 finding): iroh-blobs 0.103 providers serve
    /// purely by hash, so the sender cannot unilaterally abort an in-flight upload —
    /// the receiver acting on the revoke (drop fetch → close connection → provider
    /// write error) is what tears it down. Under the hood `revoke` is a
    /// delivery-acked `send_control` that waits up to ~30s with NO retry (B1); the
    /// terminal transition that triggers it MUST NOT block or fail on that, so this
    /// runs on a DETACHED task over a clone of the transport `Arc` and only ever
    /// logs on `Err`. The `revoke_sent` journal is written synchronously first (so
    /// it is ordered before the spawn's own logging and survives a slow send). A
    /// receiver that never had a row for `wire_id` ignores the revoke — harmless.
    fn spawn_revoke(&self, id: i64, wire_id: &PackageId, reason: RevokeReason) {
        let tag = revoke_reason_tag(reason);
        // Journal on the synchronous worker BEFORE detaching (never-swallow §D7).
        self.journal(id, "revoke_sent", Some(tag));
        let transport = Arc::clone(&self.transport);
        let peer = self.peer;
        let wire = wire_id.clone();
        tokio::spawn(async move {
            if let Err(e) = transport.revoke(peer, &wire, reason).await {
                tracing::warn!(
                    package_id = %wire.0,
                    reason = tag,
                    error = %format!("{e:#}"),
                    "sync revoke send failed"
                );
            }
        });
    }

    /// (Re)drive a resent transfer ([`Command::Resend`]). `resend_transfer` (API
    /// layer) has already reset the SAME durable row back to `Queued` — attempts++,
    /// a fresh per-attempt wire id, per-file rows → `pending` — while the row was
    /// terminal, so its slot is NOT in `pending` and a fresh
    /// [`start_package`](Self::start_package) is what picks the new attempt up
    /// (reading `wire_package_id` fresh; nothing is cached across the reset). This
    /// mirrors the crash-resume drive, scoped to this engine's own peer.
    async fn resend_package(&mut self, id: i64) {
        // Already live (a fresh-built engine's crash-resume beat this command to the
        // row): just collapse its backoff so it re-announces now.
        if self.pending.contains_key(&id) {
            self.kick_one(id);
            return;
        }
        let row = match self.store.get_outbound(id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::warn!(package_id = id, "resend: outbound row vanished; ignoring");
                return;
            }
            Err(e) => {
                tracing::error!(package_id = id, error = %format!("{e:#}"), "resend: read outbound row failed");
                return;
            }
        };
        // Scope to this engine's peer (the shared store returns every peer's rows).
        if row.peer != self.peer {
            tracing::warn!(
                package_id = id,
                "resend: row targets another peer; ignoring"
            );
            return;
        }
        // The API layer guards terminal-state eligibility + does the reset; if the
        // row is still terminal here the reset never ran — do not re-drive.
        if row.state.is_terminal() {
            tracing::warn!(
                package_id = id,
                state = row.state.as_str(),
                "resend: row still terminal; not re-driving"
            );
            return;
        }
        let dir = PathBuf::from(&row.package_ref);
        if let Err(e) = self.start_package(id, dir, row.state).await {
            tracing::error!(package_id = id, error = %format!("{e:#}"), "resend: start_package failed");
        }
    }

    /// Persist the collab `project_id` onto a freshly-enqueued outbound row (§D6),
    /// read from the package's on-disk manifest stamp. Best-effort and skipped
    /// entirely for a personal-sync package (no stamp): so collab rows become
    /// distinguishable in `sync_outbound` without any behavioral use this wave. A
    /// manifest-read or store-write failure only `warn!`s — it must never fail the
    /// enqueue.
    fn persist_project_id(&self, id: i64, dir: &Path) {
        let project_id = package::read_manifest(dir).ok().and_then(|records| {
            records
                .iter()
                .find_map(|r| r.project.as_ref().map(|p| p.project_id.clone()))
        });
        if let Some(pid) = project_id {
            if let Err(e) = self.store.set_outbound_project_id(id, Some(&pid)) {
                tracing::warn!(package_id = id, error = %format!("{e:#}"), "persist project_id failed");
            }
        }
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
            // Inbound announcements are the receiver's concern (task A7), not
            // this sender-side engine's.
            TransportEvent::AnnounceReceived { .. } => Ok(()),
            // A sender's revoke is likewise the receiver's concern (spec §D2 / B4);
            // this sender-side engine never consumes its own revoke.
            TransportEvent::RevokeReceived { .. } => Ok(()),
            // Inbound project advertisements / pull requests (collab exchange,
            // slice 4) are handled by the receive side, not this sender engine.
            TransportEvent::ProjectAnnounceReceived { .. }
            | TransportEvent::ProjectRequestReceived { .. } => Ok(()),
            // Upload progress for a package we are serving (Task 13): fold it into
            // a send-side `transferring` tick carrying byte figures.
            TransportEvent::ServeProgress {
                package_id,
                bytes_sent,
            } => {
                self.on_serve_progress(package_id, bytes_sent);
                Ok(())
            }
            // Upload finished serving what this peer asked for (Task 2.1): surface
            // the honest "uploaded — awaiting confirmation" stage. No store write,
            // no state transition — the ack remains the only delivery truth.
            TransportEvent::ServeComplete { package_id } => {
                self.on_serve_complete(package_id);
                Ok(())
            }
            // Per-file upload progress for a package we are serving (Task 2.2): fold
            // it into a send-side `sync-file-progress` so the expanded outbound row
            // shows a live per-file bar during the upload.
            TransportEvent::ServeFileProgress {
                package_id,
                file,
                bytes_done,
                bytes_total,
            } => {
                self.on_serve_file_progress(package_id, file, bytes_done, bytes_total);
                Ok(())
            }
        }
    }

    /// Record that the peer is actively pulling this package's payload and push the
    /// ack-timeout ladder out to one [`ack_timeout`](SyncConfig::ack_timeout) from
    /// *this* instant. Called on every serve tick ([`on_serve_progress`](Self::on_serve_progress),
    /// [`on_serve_file_progress`](Self::on_serve_file_progress),
    /// [`on_serve_complete`](Self::on_serve_complete)) after the caller has resolved
    /// the slot `id` from the tick's `package_id`.
    ///
    /// Fixes the "waiting · retry in N min" at 7.7 MB/s bug: without this, an
    /// actively-pulled multi-GB batch hits the ack timeout mid-pull, so
    /// [`handle_timeouts`](Self::handle_timeouts) climbs a backoff rung, persists
    /// `next_retry_at` (the UI renders "waiting" + a countdown while bytes are still
    /// flowing), and the armed [`Retry`](NextAction::Retry) re-announces
    /// MID-TRANSFER. Resetting the deadline on every tick means the ladder can only
    /// fire once the peer has genuinely gone quiet for a full `ack_timeout`.
    ///
    /// The in-memory deadline reset runs on every tick — serve ticks are throttled
    /// ≥300ms upstream (`SERVE_PROGRESS_THROTTLE`), so in-memory writes are free. The
    /// DB write ([`clear_next_retry`](Self::clear_next_retry)) happens ONLY on the
    /// `Retry → AwaitAck` flip (once per armed cycle), never per tick.
    ///
    /// Interactions this deliberately produces:
    /// - **Post-`ServeComplete` window**: `on_serve_complete` also notes activity, so
    ///   the ack window stays one `ack_timeout` from the *last* activity. A long
    ///   receiver ingest can still outlast that window and fire the ladder — its
    ///   re-announce is harmless: the receiver answers it from its receipt log (the
    ///   pure-replay guard), no duplicate ingest.
    /// - **send-now kick during an active pull**: [`kick_one`](Self::kick_one)
    ///   collapses the deadline to `now` and arms a `Retry`; the very next serve tick
    ///   deliberately undoes that (flips back to `AwaitAck`, deadline pushed out).
    ///   Re-announcing to a peer that is already pulling is pointless.
    /// - **cancelling an ANNOUNCE-failure-armed `Retry`** ([`arm_retry`](Self::arm_retry)):
    ///   the flip also disarms a retry that was armed by an announce/build/serve
    ///   failure. Correct by construction — a serve tick proves the peer holds a
    ///   valid announce for the CURRENT wire id and is pulling it, so a re-announce
    ///   is pointless; if the ack never comes, the ladder resumes from the last tick.
    fn note_serve_activity(&mut self, id: i64) {
        let flipped = if let Some(p) = self.pending.get_mut(&id) {
            p.deadline = Instant::now() + self.config.ack_timeout;
            if p.next_action == NextAction::Retry {
                p.next_action = NextAction::AwaitAck;
                p.rung = 0;
                true
            } else {
                false
            }
        } else {
            false
        };
        // The `&mut self.pending` borrow is dropped above; the persisted-countdown
        // clear + log run ONLY on the flip (once per armed cycle), never per tick.
        if flipped {
            self.clear_next_retry(id);
            tracing::info!(
                package_id = id,
                "armed retry cancelled; peer is actively pulling"
            );
        }
    }

    /// Turn a transport [`ServeProgress`](TransportEvent::ServeProgress) tick into
    /// a send-side `sync-progress` `transferring` event carrying byte figures
    /// (Task 13). Locates the pending slot whose minted announce carries this
    /// `package_id` (the same correlation [`on_ack`](Self::on_ack) uses); a tick
    /// for no live slot (already confirmed / terminal, or an unknown package) is
    /// dropped at debug. `bytes_sent` is the true collection-wide cumulative
    /// upload offset — summed across every blob by the iroh consumer's
    /// [`UploadAccumulator`](crate::sharing::iroh::UploadAccumulator), NOT
    /// a single blob's offset (a real bug caught in review: the provider's
    /// `Progress.end_offset` is per-blob, so a naive running-max freezes at the
    /// largest blob's size) — clamped to the announce's `byte_size` because the
    /// served collection carries manifest + hash-seq overhead beyond the raw
    /// frame bytes, so the true cumulative figure genuinely runs past it.
    fn on_serve_progress(&mut self, package_id: PackageId, bytes_sent: u64) {
        let slot = self.pending.iter().find_map(|(k, p)| match &p.announce {
            Some(a) if a.package_id == package_id => Some((*k, a.byte_size, a.frame_count)),
            _ => None,
        });
        let Some((id, byte_size, frame_count)) = slot else {
            tracing::debug!(package_id = %package_id.0, "serve-progress for no pending slot; dropped");
            return;
        };

        // The peer is actively pulling: push the ack-timeout ladder out and disarm
        // any retry armed mid-pull (item 3 fix) before the byte-figure emit below.
        self.note_serve_activity(id);

        // §D5: the first serve tick after a retry clears the stale `last_error` +
        // resets the attempt class — a transfer actually moving bytes must never
        // carry a stale reason. Exactly one clear-write per retry cycle. §D7: the
        // first tick of the session (peer began pulling) journals `serve_started`.
        let mut clear_error = false;
        let mut log_serve_started = false;
        if let Some(p) = self.pending.get_mut(&id) {
            if p.clear_error_on_next_serve {
                p.clear_error_on_next_serve = false;
                p.last_attempt_class = None;
                clear_error = true;
            }
            if !p.serve_started_logged {
                p.serve_started_logged = true;
                log_serve_started = true;
            }
        }
        // Bytes are flowing from this peer, so it is unambiguously reachable —
        // release anything its absence had parked (D1 §3.4). `note_serve_activity`
        // above already handled THIS package; this is about its queue-mates.
        self.peer_reachable("serve", ReachabilityProof::Contact);
        if clear_error {
            if let Err(e) = self.store.set_last_error(id, None) {
                tracing::warn!(package_id = id, error = %e, "clear last_error on serve tick failed");
            }
        }
        if log_serve_started {
            self.journal(id, "serve_started", None);
        }

        // The provider's counter is per-GET, so it restarts every time the peer
        // opens a new request — after a dropped connection it re-asks only for the
        // ranges it still lacks. Adding the base of what earlier sessions already
        // delivered is what keeps the overall bar from falling back to the start and
        // re-counting the remainder as the whole batch.
        //
        // A session boundary shows up as a tick BELOW the previous one — within one
        // request the provider's figure only grows. (The slot's opening base is
        // seeded at creation instead of here: re-reading it on the first tick would
        // double-count, because per-file ticks of the SAME session may already have
        // marked files uploaded whose bytes this session's counter also includes.)
        // The base comes from the durable per-file rows, not from summing ticks: it
        // is then correct across restarts, and a peer genuinely re-pulling a file
        // walks that row back to `sending` on its next partial tick, which removes
        // it from the base.
        let opens_session = self
            .pending
            .get(&id)
            .map(|p| bytes_sent < p.last_serve_tick)
            .unwrap_or(false);
        if opens_session {
            let base = self.delivered_bytes_base(id);
            if let Some(p) = self.pending.get_mut(&id) {
                p.served_base = base;
            }
        }
        let cumulative = if let Some(p) = self.pending.get_mut(&id) {
            p.last_serve_tick = bytes_sent;
            p.served_base.saturating_add(bytes_sent)
        } else {
            bytes_sent
        };

        self.emit_progress_bytes(
            id,
            "transferring",
            frame_count,
            cumulative.min(byte_size),
            byte_size,
        );
    }

    /// Bytes of this package the peer has ALREADY taken, read from the durable
    /// per-file rows: every row that reached [`Uploaded`](OutboundFileState::Uploaded)
    /// or `Done` counts its `bytes_done`.
    ///
    /// The base for [`on_serve_progress`](Self::on_serve_progress)'s per-session
    /// figure. Deliberately sourced from the rows rather than from a running sum of
    /// ticks — the rows survive a restart, and a row that walks back to `sending`
    /// (the peer really is re-pulling that file) drops out of the base on its own.
    /// A read failure warns and yields 0, which degrades to the old
    /// per-session-only figure rather than blocking the tick.
    fn delivered_bytes_base(&self, id: i64) -> u64 {
        match self.store.list_outbound_files(id) {
            Ok(rows) => rows
                .iter()
                .filter(|r| {
                    matches!(
                        r.state,
                        OutboundFileState::Uploaded | OutboundFileState::Done
                    )
                })
                .map(|r| r.bytes_done)
                .sum(),
            Err(e) => {
                tracing::warn!(package_id = id, error = %format!("{e:#}"), "read delivered-bytes base failed");
                0
            }
        }
    }

    /// Turn a transport [`ServeComplete`](TransportEvent::ServeComplete) into the
    /// honest "uploaded — awaiting confirmation" stage (Task 2.1): a send-side
    /// `sync-progress` `uploaded` tick reporting the FULL `byte_size` (the peer
    /// pulled everything it asked for), correlated to the pending slot the same
    /// way [`on_serve_progress`](Self::on_serve_progress) / [`on_ack`](Self::on_ack)
    /// are — by the announce carrying this `package_id`.
    ///
    /// Semantics: "finished serving what this peer asked for", NOT "delivered".
    /// The receiver ack stays the sole delivery truth — the batch stays
    /// non-terminal. Transfers Status Model v2 §D4/§D5 add persistence here: every
    /// still-in-flight per-file row (`pending`/`sending`) transitions to `uploaded`
    /// (a duplicate row already settled `done` is left as-is), and the batch row
    /// itself moves `Transferring → Delivered` (the reserved non-terminal state,
    /// "uploaded — awaiting confirmation") so the picture survives a restart. A
    /// later payload request (a resume) produces fresh `ServeProgress`
    /// `transferring` ticks; the ack path / retry / restart treat `Delivered`
    /// exactly like `Transferring`. A complete for no live slot (already confirmed /
    /// terminal, or an unknown package) is dropped at debug.
    fn on_serve_complete(&mut self, package_id: PackageId) {
        let slot = self.pending.iter().find_map(|(k, p)| match &p.announce {
            Some(a) if a.package_id == package_id => Some((*k, a.byte_size, a.frame_count)),
            _ => None,
        });
        let Some((id, byte_size, frame_count)) = slot else {
            tracing::debug!(package_id = %package_id.0, "serve-complete for no pending slot; dropped");
            return;
        };

        // The peer just finished pulling: the post-`ServeComplete` ack window starts
        // one `ack_timeout` from HERE (item 3 fix); also disarms a mid-pull retry.
        self.note_serve_activity(id);

        // §D4: mark every still-in-flight per-file row `uploaded` (the peer pulled
        // everything it asked for). Rows already settled `done` (a want-subset
        // duplicate) are skipped — the negotiate verdict wins.
        match self.store.list_outbound_files(id) {
            Ok(rows) => {
                for row in rows {
                    if matches!(
                        row.state,
                        OutboundFileState::Pending | OutboundFileState::Sending
                    ) {
                        if let Err(e) = self.store.set_outbound_file_state(
                            id,
                            &row.rel_path,
                            OutboundFileState::Uploaded,
                            row.byte_size,
                            None,
                            None,
                        ) {
                            tracing::warn!(package_id = id, rel_path = %row.rel_path, error = %format!("{e:#}"), "mark uploaded outbound file failed");
                        }
                        if let Some(p) = self.pending.get_mut(&id) {
                            p.file_states
                                .insert(row.rel_path, OutboundFileState::Uploaded);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(package_id = id, error = %format!("{e:#}"), "list outbound files at serve-complete failed")
            }
        }

        // §D4/§D5: persist the batch `uploaded` stage as `Delivered` (non-terminal,
        // survives restart; the ack remains delivery truth). The slot exists ⇒ the
        // row is non-terminal, so this never clobbers a confirmed/cancelled row.
        if let Err(e) = self.store.set_state(id, OutboundState::Delivered) {
            tracing::warn!(package_id = id, error = %format!("{e:#}"), "persist Delivered state failed");
        } else {
            tracing::info!(package_id = id, state = "delivered", "sync state");
        }
        self.journal(id, "uploaded", None);

        self.emit_progress_bytes(id, "uploaded", frame_count, byte_size, byte_size);
        tracing::info!(
            package_id = %package_id.0,
            "package upload complete; awaiting receiver confirmation"
        );
    }

    /// Turn a transport [`ServeFileProgress`](TransportEvent::ServeFileProgress)
    /// tick into a send-side `sync-file-progress` event (Task 2.2), so the expanded
    /// outbound row shows a live per-file bar while the peer pulls our collection.
    /// Correlated to the pending slot by the announce carrying this `package_id`
    /// (the same correlation [`on_serve_progress`](Self::on_serve_progress) uses),
    /// then emitted keyed by the outbound row id — matching the outbound Active row
    /// key and the send-side `sync-progress` convention. The transport `file` is a
    /// forward-slash `rel_path`, emitted VERBATIM (not reduced to a basename): the
    /// frontend live overlay keys the per-file bar by the FULL rel_path
    /// (`useTransferQueue` stores by `p.file`, `FileTree` looks it up by
    /// `entry.relPath`), and the receiver side already emits the full rel_path — so a
    /// nested outbound file (e.g. a WBPP object send) matches and animates. A tick
    /// for no live slot (already confirmed / terminal, or an unknown package) is
    /// dropped at debug.
    fn on_serve_file_progress(
        &mut self,
        package_id: PackageId,
        file: String,
        bytes_done: u64,
        bytes_total: u64,
    ) {
        let id = self.pending.iter().find_map(|(k, p)| match &p.announce {
            Some(a) if a.package_id == package_id => Some(*k),
            _ => None,
        });
        let Some(id) = id else {
            tracing::debug!(package_id = %package_id.0, "serve-file-progress for no pending slot; dropped");
            return;
        };

        // The peer is actively pulling this file: push the ack-timeout ladder out and
        // disarm any mid-pull retry (item 3 fix) before the per-file persist below.
        self.note_serve_activity(id);

        // §D4: persist the per-file lifecycle, keyed by `file` (the transport's
        // forward-slash `rel_path`, == the `sync_outbound_files.rel_path` — the
        // served/wanted collection order, not the full manifest order). A per-byte
        // tick only WRITES the DB when the file's state actually changes (the slot's
        // `file_states` tracks the last-persisted value), so the live bar stays
        // event-driven: `pending → sending` on the first partial tick,
        // `sending → uploaded` on completion. A served `manifest.ndjson` (or a
        // Perseus batch with no per-file rows) matches no row — the setter no-ops
        // at debug.
        let target = if bytes_done >= bytes_total {
            OutboundFileState::Uploaded
        } else {
            OutboundFileState::Sending
        };
        let changed = self
            .pending
            .get(&id)
            .map(|p| p.file_states.get(&file) != Some(&target))
            .unwrap_or(false);
        if changed {
            let persist_bytes = if target == OutboundFileState::Uploaded {
                bytes_total
            } else {
                bytes_done
            };
            if let Err(e) =
                self.store
                    .set_outbound_file_state(id, &file, target, persist_bytes, None, None)
            {
                tracing::warn!(package_id = id, rel_path = %file, error = %format!("{e:#}"), "set per-file serve state failed");
            }
            if let Some(p) = self.pending.get_mut(&id) {
                p.file_states.insert(file.clone(), target);
            }
        }

        self.emit_file_progress(id, file, bytes_done, bytes_total);
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
    fn on_ack(
        &mut self,
        from: NodeId,
        package_id: PackageId,
        receipts: Vec<FrameReceipt>,
    ) -> Result<()> {
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
            // Even a late ack is proof of life from the peer — release whatever its
            // absence had parked before returning (D1 §3.4).
            self.peer_reachable("ack", ReachabilityProof::Contact);
            return Ok(());
        };
        // An ack from the paired peer is the strongest possible reachability proof.
        self.peer_reachable("ack", ReachabilityProof::Contact);

        // §D4/§D7: settle each per-file row named in the receipts with the
        // receiver's verdict (`ingested`/`duplicate`/`cancelled`/`rejected:<reason>`,
        // state `done`; a rejection also stamps the per-file error), and journal the
        // ack with its outcome counts. Done for EVERY ack shape (full confirm,
        // partial/rejected, all-cancelled) so the per-file picture always reflects
        // the latest verdict — even while a partial/rejected ack keeps the batch in
        // flight for a redelivery.
        let records = {
            let cached = self
                .pending
                .get(&key)
                .and_then(|p| p.manifest_records.clone());
            match cached {
                Some(r) => r,
                None => {
                    let dir = self.pending.get(&key).map(|p| p.dir.clone());
                    dir.and_then(|d| crate::package::read_manifest(&d).ok())
                        .unwrap_or_default()
                }
            }
        };
        self.settle_files_from_receipts(key, &records, &receipts);
        {
            let (mut ing, mut dup, mut rej, mut can) = (0u32, 0u32, 0u32, 0u32);
            for r in &receipts {
                match &r.outcome {
                    ReceiptOutcome::Ingested => ing += 1,
                    ReceiptOutcome::Duplicate => dup += 1,
                    ReceiptOutcome::Rejected(_) => rej += 1,
                    ReceiptOutcome::Cancelled => can += 1,
                }
            }
            self.journal(
                key,
                "ack_received",
                Some(&format!(
                    "ingested={ing} duplicate={dup} rejected={rej} cancelled={can}"
                )),
            );
        }

        // Cancelled-by-receiver (Task 4): a non-empty ack whose receipts are ALL
        // `Cancelled` is a deliberate "no" for the whole package — terminal, NOT a
        // partial delivery. It must NOT run the confirm/completeness path below
        // (`Cancelled` is not `Rejected`, so it would otherwise fall through and
        // confirm). Mark the row `Cancelled`, stamp the exact "cancelled by
        // receiver" reason (the UI keys its "by receiver" display off this string),
        // record a per-frame `cancelled` history outcome through the shared
        // receipts→history path, run the same terminal epilogue `cancel_package`
        // uses (release blobs, cleanup_sink/in-line payload cleanup, drop the
        // slot), and emit the single `cancelled` finished event. MIXED receipts
        // (some cancelled, some ingested/rejected) fall through to the normal path
        // below, where cancelled frames still map to a `cancelled` history outcome.
        if !receipts.is_empty()
            && receipts
                .iter()
                .all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled))
        {
            let pending = self.pending.remove(&key).expect("key from live find");
            self.store
                .set_state(pending.id, OutboundState::Cancelled)
                .context("cancel outbound (all-cancelled ack)")?;
            // The exact reason string the Perseus/UI surface renders as "cancelled
            // by receiver" (single Rust source: [`CANCELLED_BY_RECEIVER_DETAIL`];
            // the api resend path keys the declined-resend divert on it too).
            // Best-effort diagnostic write — never fail the terminal transition on it.
            if let Err(se) = self
                .store
                .set_last_error(pending.id, Some(CANCELLED_BY_RECEIVER_DETAIL))
            {
                tracing::warn!(package_id = pending.id, error = %se, "record last_error (cancelled by receiver) failed");
            }
            // Terminal: no retry is pending — clear the persisted countdown (Task 2).
            self.clear_next_retry(pending.id);
            // Per-frame `cancelled` history via the shared receipts→history path
            // (every receipt is `Cancelled`, so each row's outcome is `cancelled`).
            self.append_confirmed_history(&pending, &receipts)?;
            // Terminal: free the served blobs (fire-and-forget). `package_id` is the
            // id the ack correlated against (== pending.announce.package_id).
            self.spawn_release(package_id);
            // Mirrors `cancel_package` exactly (spec §1/§4: a receiver-cancelled
            // package stays in the sender's history and is retryable, which requires
            // the payload to still be on disk): notify the fan-out coordinator when
            // shared, but — unlike the `Confirmed` path — never reclaim the payload
            // copies in line. Only a `Confirmed` package's copies are ever reclaimed
            // (see `cancelled_package_keeps_payloads`, the sibling terminal state this
            // one joins; the `Failed` terminal has no keep-payloads twin because it is
            // only ever the vanished-payload path — there are no copies left to keep).
            if let Some(sink) = &self.cleanup_sink {
                sink.on_terminal(&pending.dir);
            }
            tracing::info!(
                package_id = pending.id,
                state = "cancelled",
                reason = "cancelled_by_receiver",
                "sync state"
            );
            self.journal(pending.id, "cancelled", Some("by_receiver"));
            self.emit_finished(
                pending.id,
                "cancelled",
                0,
                Vec::new(),
                0,
                0,
                pending.project_id.clone(),
            );
            return Ok(());
        }

        let rejected: Vec<&str> = receipts
            .iter()
            .filter_map(|r| {
                matches!(r.outcome, ReceiptOutcome::Rejected(_)).then_some(r.frame_uuid.as_str())
            })
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
                    tracing::info!(
                        package_id = pending.id,
                        freed_bytes,
                        "package payloads cleaned"
                    );
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
        self.journal(pending.id, "confirmed", None);
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
                    let retry_info = if let Some(p) = self.pending.get_mut(&id) {
                        // An ack timeout carries NO class: the announce itself
                        // succeeded, so there is no failed dial to classify and
                        // nothing observed says the peer is absent. It therefore
                        // takes the escalating schedule — the right default for a
                        // peer whose ingest may simply be busy (D1 §3.3).
                        if !class_is_absent(p.last_attempt_class) {
                            p.rung = p.rung.saturating_add(1);
                        }
                        p.next_action = NextAction::Retry;
                        // §D5: a retry is pending — the next serve tick (if the peer
                        // is still pulling) must wipe this stale "no ack" reason.
                        p.clear_error_on_next_serve = true;
                        let delay =
                            retry_backoff(self.config.ack_timeout, p.rung, p.last_attempt_class);
                        p.deadline = Instant::now() + delay;
                        // Compute the wall-clock retry stamp ONCE and reuse it for
                        // both the diagnostic log and the persisted countdown
                        // (Task 2) — do not recompute (TEST-11: a one-line
                        // `query_logs` smoke reads the delay + next-retry off this
                        // event instead of watching the UI countdown).
                        let stamp = retry_deadline_stamp(delay);
                        tracing::info!(
                            package_id = id,
                            rung = p.rung,
                            delay_ms = delay.as_millis() as u64,
                            next_retry_at = %stamp,
                            "ack timeout, backing off"
                        );
                        Some((stamp, p.rung, delay.as_millis() as u64))
                    } else {
                        None
                    };
                    // Persist the wall-clock retry deadline (Task 2) + journal the
                    // ack-timeout and the scheduled retry (§D7) after the borrow drops.
                    if let Some((stamp, rung, delay_ms)) = retry_info {
                        self.persist_next_retry_at(id, &stamp);
                        self.journal(id, "ack_timeout", None);
                        self.journal(
                            id,
                            "retry_scheduled",
                            Some(&format!("rung={rung} delay_ms={delay_ms}")),
                        );
                    }
                }
                NextAction::Retry => {
                    if !refreshed {
                        refreshed = true;
                        // Task 3.6: whether this package's last dial failed in a way
                        // that implicates STALE peer addressing (dead direct path or
                        // dead relay). Read before the refresh so a fresh address can
                        // fully replace the stale lookup entry rather than merge onto
                        // a possibly-dead relay URL.
                        let addressing_dead = matches!(
                            self.pending.get(&id).and_then(|p| p.last_attempt_class),
                            Some(
                                ConnectClass::NoRoute
                                    | ConnectClass::Timeout
                                    | ConnectClass::RelayUnreachable
                            )
                        );
                        if let Some(refresher) = self.addr_refresher.clone() {
                            if let Some(addr) = refresher(self.peer).await {
                                // Task 3.6: a dead-addressing failure means the
                                // cached lookup entry (which never expires, and whose
                                // relay URL a merge can't drop) is suspect — remove it
                                // FIRST so the fresh hints below fully replace it. Only
                                // ever with a replacement already in hand (refresher
                                // returned `Some`); a `None` refresh keeps last-known.
                                if addressing_dead {
                                    self.transport.remove_peer_addr(self.peer);
                                    tracing::info!(
                                        peer = %super::node_id_hex(&self.peer),
                                        "retry: invalidated stale peer addressing before refresh"
                                    );
                                }
                                tracing::info!(
                                    peer = %super::node_id_hex(&self.peer),
                                    "retry: re-resolved peer address"
                                );
                                self.transport.add_peer_addr(addr);
                            } else {
                                // Task 3.4: the refresher is wired but yielded nothing
                                // (hub blip / peer gone). Keep-last-good is correct;
                                // log the fallback so a stuck retry is not silent.
                                tracing::warn!(
                                    peer = %super::node_id_hex(&self.peer),
                                    "retry: address refresh unavailable; dialing last-known address"
                                );
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
    /// of [`attempt`](Self::attempt)). A cancel is the sibling terminal but a
    /// distinct state: `cancel_package` transitions directly to `Cancelled`
    /// (not `Failed`, and not routed through here).
    fn fail_package(&mut self, id: i64) -> Result<()> {
        let removed = self.pending.remove(&id);
        let (dir, last_rejected, pkg_id, project_id, manifest_records, started) = match removed {
            Some(p) => (
                Some(p.dir),
                p.last_rejected,
                p.announce.map(|a| a.package_id),
                p.project_id,
                p.manifest_records,
                p.started,
            ),
            None => (None, Vec::new(), None, None, None, false),
        };
        self.store.set_state(id, OutboundState::Failed)?;
        // Terminal: no retry is pending — clear the persisted countdown (Task 2).
        self.clear_next_retry(id);
        // §D4: every not-yet-settled per-file row inherits the terminal outcome.
        self.settle_unsettled_files(id, "failed");
        // Terminal: release any served blobs (fire-and-forget). `pkg_id` is
        // `None` when the package never minted+served an announce (a pre-serve
        // failure) — nothing to release in that case. When an announce for THIS
        // attempt actually reached the peer (`started`), also best-effort `Revoke`
        // it so the receiver tears down any in-flight fetch (spec §D2 / B3) —
        // spawned, so the terminal transition never blocks on it.
        if let Some(pid) = pkg_id {
            if started {
                self.spawn_revoke(id, &pid, RevokeReason::Failed);
            }
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
        self.journal(id, "failed", Some(&outcome));
        self.emit_finished(id, &outcome, 0, last_rejected, 0, 0, project_id);
        Ok(())
    }

    /// Cancel a package → `Cancelled` with a `cancelled` outcome. Idempotent: a
    /// no-op if the package is already terminal / unknown.
    fn cancel_package(&mut self, id: i64) -> Result<()> {
        // Resolve the package dir: prefer the in-flight entry, else a live row.
        // Removing the slot also ensures no later timeout fires for this id. The
        // in-flight entry also carries the minted announce whose blobs need
        // releasing; a row resolved only from the store was never served this
        // session, so there is nothing to release (`pkg_id` stays `None`).
        let (dir, pkg_id, project_id, started) = if let Some(p) = self.pending.remove(&id) {
            (
                Some(p.dir),
                p.announce.map(|a| a.package_id),
                p.project_id,
                p.started,
            )
        } else {
            (
                self.store
                    .non_terminal()?
                    .into_iter()
                    .find(|r| r.id == id)
                    .map(|r| PathBuf::from(r.package_ref)),
                None,
                None,
                false,
            )
        };

        let Some(dir) = dir else {
            tracing::debug!(
                package_id = id,
                "cancel ignored (already terminal or unknown)"
            );
            return Ok(());
        };

        self.store.set_state(id, OutboundState::Cancelled)?;
        // Terminal: no retry is pending — clear the persisted countdown (Task 2).
        self.clear_next_retry(id);
        // §D4: every not-yet-settled per-file row inherits the `cancelled` outcome.
        self.settle_unsettled_files(id, "cancelled");
        tracing::info!(package_id = id, state = "cancelled", "sync state");
        // Journal the terminal `cancelled` BEFORE any `revoke_sent` so the batch's
        // Log reads cancelled → revoke_sent (§D3 order).
        self.journal(id, "cancelled", Some("by_user"));
        // Terminal: release any served blobs (fire-and-forget). When an announce for
        // THIS attempt actually reached the peer (`started`), also best-effort
        // `Revoke Cancelled` (spec §D2 / B3) — Revoke IS the stop mechanism (the
        // receiver drops its fetch → the provider upload tears down). Spawned, so a
        // dead/slow peer's ≤30s delivery-ack never stalls the cancel. The
        // store-fallback branch (row not in this session's `pending`) has no
        // current-attempt announce here → no revoke, only the local terminal stamp.
        if let Some(pid) = pkg_id {
            if started {
                self.spawn_revoke(id, &pid, RevokeReason::Cancelled);
            }
            self.spawn_release(pid);
        }
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
    /// The human batch name to stamp on this package's sent history rows
    /// (Transfers Status Model v2 §D1): the outbound row's `display_name` when
    /// set (non-blank), else `None` — so the transfer log shows the named batch,
    /// with the raw UUIDs/basename reserved for the Details tab. One store read
    /// per history-append call (before the per-frame loop, never per row); a read
    /// failure is warned and degrades to `None`, never aborting the history write.
    fn outbound_batch_name(&self, id: i64) -> Option<String> {
        match self.store.get_outbound(id) {
            Ok(Some(row)) => row.display_name.filter(|s| !s.trim().is_empty()),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(package_id = id, error = %e, "read display_name for history failed");
                None
            }
        }
    }

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
        let package_key = package_dir_key(dir);
        let batch_name = self.outbound_batch_name(id);
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
                package_id: package_key.clone(),
                batch_name: batch_name.clone(),
            })?;
        }
        tracing::debug!(
            package_id = id,
            count = records.len(),
            "sync history: transfer started"
        );
        Ok(())
    }

    /// Append one confirm history row per receipt, joining the peer's verdict to
    /// this package's manifest by `frame_uuid`.
    fn append_confirmed_history(&self, pending: &Pending, receipts: &[FrameReceipt]) -> Result<()> {
        let records = package::read_manifest(&pending.dir).with_context(|| {
            format!(
                "read manifest for confirm history {}",
                pending.dir.display()
            )
        })?;
        let by_uuid: HashMap<&str, &ManifestRecord> =
            records.iter().map(|r| (r.frame_uuid.as_str(), r)).collect();
        let peer_device = node_id_hex(&self.peer);
        let finished = now_iso();
        let package_key = package_dir_key(&pending.dir);
        let batch_name = self.outbound_batch_name(pending.id);

        for rec in receipts {
            let (filename, object, bytes, project) = match by_uuid.get(rec.frame_uuid.as_str()) {
                Some(m) => (
                    filename_of(&m.rel_path),
                    object_of(m),
                    m.byte_size,
                    project_of(m),
                ),
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
                package_id: package_key.clone(),
                batch_name: batch_name.clone(),
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
        let package_key = package_dir_key(dir);
        let batch_name = self.outbound_batch_name(id);
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
                package_id: package_key.clone(),
                batch_name: batch_name.clone(),
            })?;
        }
        Ok(())
    }
}

/// Basename of a forward-slash manifest `rel_path`.
fn filename_of(rel_path: &str) -> String {
    rel_path.rsplit('/').next().unwrap_or(rel_path).to_string()
}

/// The stable per-package history batch key (Task 14): the basename of the
/// package directory, which is exactly the last component of the durable
/// `sync_outbound.package_ref`. Stamped into `sync_history.package_id` by every
/// sender-side history writer so
/// [`list_transfer_files`](crate::api::sync::list_transfer_files) can recover the
/// same key from the outbound row and join a package's per-frame verdicts back to
/// its manifest. `None` only for the pathological path with no final component.
fn package_dir_key(dir: &Path) -> Option<String> {
    dir.file_name().and_then(|f| f.to_str()).map(str::to_string)
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

/// Stable snake_case tag for a [`RevokeReason`] — the `revoke_sent` journal detail
/// (§D7) and the failed-send log field. Frozen alongside the wire enum's variant
/// order (B1): the strings are a diagnostic contract, not just cosmetics.
fn revoke_reason_tag(reason: RevokeReason) -> &'static str {
    match reason {
        RevokeReason::Cancelled => "cancelled",
        RevokeReason::Superseded => "superseded",
        RevokeReason::Failed => "failed",
    }
}

/// Short outcome tag for a receipt verdict, stored in `sync_history.outcome`.
fn receipt_outcome_str(o: &ReceiptOutcome) -> String {
    match o {
        ReceiptOutcome::Ingested => "ingested",
        ReceiptOutcome::Duplicate => "duplicate",
        ReceiptOutcome::Cancelled => "cancelled",
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
