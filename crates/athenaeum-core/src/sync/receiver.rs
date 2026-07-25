//! The receive-side background service (Stage I, task A7).
//!
//! [`SyncReceiver`] is the primary-side counterpart of the sender-side
//! [`SyncEngine`](super::engine::SyncEngine): it listens on a
//! [`SharingTransport`]'s event stream and, for every announced package, fetches
//! it, ingests it into the app catalog ([`ingest`](super::ingest)), and acks the
//! per-frame receipts back to the peer. [`SyncRuntime`] is the thin app-lifecycle
//! holder the command layer talks to — it lazily builds the real iroh transport
//! behind the dev-pairing flag, spawns one receiver over it, and hands out the
//! pairing ticket.
//!
//! # Ack replay
//!
//! Receipts are durable ([`sync_receipts`](super::store::DDL_RECEIPTS)). A
//! re-received announce for a package that is already fully receipted (receipt
//! count == announced `frame_count`) is re-acked **straight from the log** — no
//! re-fetch, no re-ingest — so a lost ack that makes the sender resend never
//! double-ingests. Independently, [`ingest_package`](super::ingest::ingest_package)
//! is itself idempotent (per-frame uuid/content dedup), so even a bypassed replay
//! guard is safe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::events::{emit_event, ProgressEmitter};
use crate::sharing::iroh::node::{Role, SharedIrohNode};
use crate::sharing::types::{
    AnnounceFileEntry, FetchEvent, FrameReceipt, NodeId, PackageAnnounce, PackageId,
    ReceiptOutcome, RevokeReason, StartInfo, TransportEvent,
};
use crate::sharing::{noop_fetch_sink, FetchSink, SharingTransport};

use super::ingest::{self, IngestOutcome};
use super::models::Direction;
use super::models::{HistoryRow, InboundFileRow, InboundFileState, InboundState};
use super::now_iso;
use super::refusal::RefusalRefresher;
use super::store::{
    append_sync_event, count_satisfied_receipts, get_inbound, get_inbound_by_batch,
    get_inbound_by_row_id, inbound_active, insert_history_row, insert_receipt,
    landing_dir_claimed_by_active, list_inbound_files, load_receipts,
    mark_inbound_declined_with_anchor, receipt_outcome_to_db, replace_inbound_files,
    rotate_inbound_package_id, set_inbound_bytes_done, set_inbound_declined_at,
    set_inbound_display_name, set_inbound_file_state, set_inbound_landing_dir, set_inbound_state,
    settle_unsettled_inbound_files, upsert_inbound_attempt, CatalogSyncStore,
};

/// Resolves the landing root for the next received package, live. Called once per
/// package (immediately before ingest), so designating or clearing the
/// `sync_incoming` scan root takes effect on the very next package without
/// restarting the transport. The host builds one that re-reads the designated
/// root from the catalog (see [`crate::api::sync`]); tests inject their own.
pub type IncomingResolver = Arc<dyn Fn() -> PathBuf + Send + Sync>;

/// Decides whether the peer that sent an announce is authorized to deliver
/// packages to this receiver (finding H1). Evaluated **live, once per package**
/// (like [`IncomingResolver`]), so an allow-list / pairing change takes effect on
/// the next package without a transport restart. Returns `true` to accept the
/// announce, `false` to silently drop it (no fetch, no ingest, no ack).
///
/// The remote node id is already cryptographically authenticated by the
/// transport (iroh binds it to the peer's ed25519 key); this closure adds the
/// missing *authorization* — for the app primary, membership in the set of
/// capture-device pubkeys paired to this device (built by the host from the
/// account device list, cached for offline starts). Without it, any node that
/// can dial the endpoint could push files into the catalog and landing folder.
pub type PeerAuthorizer = Arc<dyn Fn(&NodeId) -> bool + Send + Sync>;

/// A [`PeerAuthorizer`] that accepts every peer. Used by tests and by the
/// dev-ticket escape hatch (which has no hub to build an allow-list from — it is
/// a developer-only flag). Production account-mode primaries always build a real
/// allow-list instead.
pub fn allow_all_peers() -> PeerAuthorizer {
    Arc::new(|_| true)
}

/// Decides whether an inbound PROJECT announce (collab exchange, slice 4) from
/// `node` for `project_id` may be accepted. Evaluated **live, per announce** so a
/// membership-snapshot refresh takes effect on the next event without a transport
/// restart. Wired by the host to
/// [`collab::authz::may_accept_announce`](crate::collab::authz::may_accept_announce)
/// — a verified current member of the project. Returns `true` to accept, `false`
/// to drop fail-closed. A missing gate is treated as "deny": a project announce
/// is only ever accepted when a real membership check passes.
pub type ProjectAnnounceGate = Arc<dyn Fn(&NodeId, &str) -> bool + Send + Sync>;

/// Refresh this device's cached announcements for a project (collab exchange,
/// slice 4, task 5). Invoked with the `project_id` when an inbound project
/// announce names a package whose `project_packages` row we do not yet know:
/// Task 8 wires it to a hub poll so the row appears, and the receiver re-checks
/// before deciding to drop fail-closed. Synchronous by contract (called on the
/// receiver loop); an implementation doing async hub I/O bridges it internally.
pub type ProjectAnnouncementsRefresher = Arc<dyn Fn(&str) + Send + Sync>;

/// Post-ingest callback (collab exchange, slice 4, task 5): invoked with
/// `(project_id, hub_package_id)` after a project package has been ingested and
/// acked. Task 8 wires it to report-have + notification data; absent = no-op.
pub type ProjectIngestedHook = Arc<dyn Fn(String, String) + Send + Sync>;

/// Holder-side handler for an inbound project pull request (slice 4, task 6):
/// invoked with `(from, project_id, hub_package_id)` when a member asks us to
/// serve a project package. The host wires it to
/// [`collab_exchange::handle_project_request`](crate::api::collab_exchange::handle_project_request)
/// — authorize (`may_serve_package`) → reconstruct the serve dir → enqueue an
/// explicit-target serve back to `from` through the collab sender map. Absent ⇒
/// the request is logged and dropped (the pre-task-6 behavior). Synchronous by
/// contract (called on the receiver loop); the host closure `tokio::spawn`s the
/// actual async serve so the receive loop never blocks.
pub type ProjectRequestHandler = Arc<dyn Fn(NodeId, String, String) + Send + Sync>;

/// The project-exchange receive hooks [`SyncReceiver::spawn`] threads into its
/// loop (slice 4): the per-announce membership gate plus the Task-8 refresher /
/// post-ingest callbacks. `Default` = "no project support installed" (every field
/// `None`), so a personal-sync-only caller passes `Default::default()`.
#[derive(Clone, Default)]
pub struct ProjectReceiveHooks {
    /// Per-announce project-membership gate. Absent or refusing ⇒ the announce is
    /// dropped fail-closed.
    pub gate: Option<ProjectAnnounceGate>,
    /// Refresh announcements when a project package's row is unknown (task 8).
    pub announcements_refresher: Option<ProjectAnnouncementsRefresher>,
    /// Fired after a project package is ingested + acked (task 8).
    pub on_project_ingested: Option<ProjectIngestedHook>,
    /// Holder-side serve handler for inbound project pull requests (task 6).
    /// Absent ⇒ an inbound request is logged and dropped.
    pub request_handler: Option<ProjectRequestHandler>,
}

/// Optional receive-side hooks the host threads into
/// [`SyncRuntime::ensure_started`] (collab exchange, slice 4). Every field is
/// optional and the whole struct is `Default` ("nothing installed"), so a caller
/// wires only what it needs and the transport keeps its pre-slice-4 behavior when
/// a hook is absent. Introduced with room for the Task-5/6/8 hooks.
#[derive(Clone, Default)]
pub struct ReceiverHooks {
    /// Connection-level authorization predicate installed on the iroh transport
    /// via [`set_connect_gate`](crate::sharing::iroh::IrohTransport::set_connect_gate):
    /// the composite account-allow-list ∪ project-member gate. Absent ⇒ the
    /// transport admits every connection (today's behavior).
    pub connect_gate: Option<crate::sharing::iroh::ConnectGate>,
    /// Per-announce project-membership gate for inbound `ProjectAnnounceReceived`
    /// events. Absent or refusing ⇒ the announce is dropped fail-closed.
    pub project_gate: Option<ProjectAnnounceGate>,
    /// Refresh cached announcements when a project package's hub row is unknown
    /// (task 5 flow; Task 8 wires it to the hub poll). Absent ⇒ an unknown-row
    /// announce is dropped fail-closed without a refresh attempt.
    pub announcements_refresher: Option<ProjectAnnouncementsRefresher>,
    /// Post-ingest callback (task 5 flow; Task 8 wires report-have + notification
    /// data). Absent = no-op.
    pub on_project_ingested: Option<ProjectIngestedHook>,
    /// Holder-side serve handler for inbound `ProjectRequestReceived` events
    /// (task 6): the host wires it to
    /// [`collab_exchange::handle_project_request`](crate::api::collab_exchange::handle_project_request).
    /// Absent ⇒ the request is logged and dropped (pre-task-6 behavior).
    pub project_request_handler: Option<ProjectRequestHandler>,
}

/// `sync-progress` payload: a per-package stage tick (never per-frame — discrete
/// stages only, per the plan's "notify on outcomes, don't spam progress" rule).
///
/// Shared by both transfer halves (task M3): `direction` discriminates a
/// receive-side tick (`received`/`fetching`/`ingesting`) from a send-side tick
/// (`queued`/`transferring`), so the Transfers UI can route one event stream to
/// the right pane without a second channel.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgressEvent {
    pub package_id: String,
    /// Which half emitted this tick (`received` = inbound, `sent` = outbound).
    pub direction: super::Direction,
    /// Coarse stage: receiver `received`/`fetching`/`ingesting`, or sender
    /// `queued`/`transferring`.
    pub stage: String,
    /// The other peer's node id (hex): the sending peer for a receive tick, the
    /// destination peer for a send tick.
    pub peer_device: String,
    pub frame_count: u32,
    /// Collab exchange (slice 4): the project id when this package is a project
    /// exchange, else `None` for personal sync. Additive — the Transfers UI reads
    /// it in Task 11 to route project transfers to the project view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Cumulative fetched bytes at this tick (Task 11). Present only on the
    /// `fetching`-stage ticks driven by the transport's batch progress; `None` on
    /// the coarse stage ticks (`received`/`ingesting`/sender stages) that carry no
    /// byte figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_done: Option<u64>,
    /// Total package bytes for the fetch (the announce's `byte_size`), paired with
    /// [`bytes_done`](Self::bytes_done) on `fetching` ticks; `None` elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
}

/// `sync-file-progress` payload: per-file fetch progress for one collection entry
/// (Task 11), emitted from the receiver's [`FetchSink`](crate::sharing::FetchSink)
/// as the transport streams a package's files. Distinct from the per-package
/// `sync-progress` stage tick so the Transfers UI can show a live per-file bar
/// without overloading the stage channel. Progress is UI data — never a log.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncFileProgressEvent {
    pub package_id: String,
    /// Sending peer's node id (hex).
    pub peer_device: String,
    /// The entry's forward-slash `rel_path` within the package.
    pub file: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// `sync-finished` payload: emitted once per package at the end of processing.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncFinishedEvent {
    pub package_id: String,
    /// Which half finished (`received` = inbound, `sent` = outbound), so the UI
    /// can raise the right notification ("frames arrived" vs "package delivered").
    pub direction: super::Direction,
    /// Receiver: `ingested` (all accepted), `partial` (some rejected), `failed`
    /// (all rejected), or `replayed` (re-acked from the receipt log, no ingest).
    /// Sender: `confirmed`, `failed[: …]`, or `cancelled`.
    pub outcome: String,
    /// The other peer's node id (hex).
    pub peer_device: String,
    pub ok_count: u32,
    /// Frame uuids the receiver rejected (integrity failure).
    pub failed: Vec<String>,
    /// Sender-only dedup outcome (Sync Phase 3): frames actually sent (`new`) vs.
    /// dropped as the peer's duplicates by the pre-announce handshake. Always `0`
    /// on the receiver-side emits — the receiver reports ingest/duplicate per
    /// frame in [`ok_count`](Self::ok_count) / its receipts, not this split.
    pub new_count: u32,
    pub duplicate_count: u32,
    /// Collab exchange (slice 4): the project id when this package is a project
    /// exchange, else `None` for personal sync. Additive — Task 11 wires it to
    /// the project-transfer UI/notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Shipped cap on simultaneous incoming transfers (W2). Two, not more: the
/// fetch+ingest phase is bound by DISK and connection contention (hash + copy out
/// of staging into the incoming tree), not by the network — a third concurrent
/// lane mostly buys head-of-line seek thrash on a spinning archive volume. Two is
/// enough to keep a small transfer from queueing behind a large one, which is the
/// whole point of the per-peer lanes.
pub const DEFAULT_MAX_CONCURRENT_RECEIVES: usize = 2;

/// Inclusive clamp for the receive-concurrency limit. Below 1 the receiver would
/// deadlock; above 8 the lanes fight over the same disk with nothing to gain.
const MIN_CONCURRENT_RECEIVES: usize = 1;
const MAX_CONCURRENT_RECEIVES: usize = 8;

/// The resizable half of [`ReceiveGate`], behind one lock so a `set_limit` racing
/// an `acquire` can never split the pair.
#[derive(Debug)]
struct GateState {
    /// The limit currently in force (already clamped).
    limit: usize,
    /// Permits a shrink still has to reclaim because they were IN USE at the time
    /// (see [`ReceiveGate`]'s doc — the debt-counter recipe).
    debt: usize,
}

/// Live-resizable concurrency gate for the fetch+ingest phase (W2 T2.2): at most
/// `limit` inbound transfers hold a permit at once, and the limit can be changed
/// from the settings UI WITHOUT restarting the receiver.
///
/// # Why a debt counter
///
/// Growing is trivial ([`Semaphore::add_permits`]). Shrinking is not:
/// [`Semaphore::forget_permits`] can only take permits that are AVAILABLE right
/// now, and it reports how many it actually got. Lowering the cap from 3 to 1
/// while all three lanes are busy therefore removes nothing at the moment of the
/// call — the permits are out in the world and tokio has no way to claw them back.
///
/// So the un-forgotten remainder is recorded as `debt`, and payment is deferred to
/// [`acquire`](Self::acquire): every successful acquire re-checks the debt and, if
/// any is outstanding, decrements it, [`forget`](tokio::sync::OwnedSemaphorePermit::forget)s
/// its own permit (so the permit is destroyed rather than returned) and loops to
/// wait again instead of proceeding. The shrink thus takes effect as the busy
/// lanes finish, one release at a time, and it never interrupts an in-flight
/// transfer — exactly the semantics a live setting change wants.
///
/// # Debt vs. grow
///
/// A grow that lands while debt is outstanding pays the debt down FIRST and only
/// adds real permits with what is left over (`grow -= min(grow, debt)`). Shrinking
/// 3→1 and then growing 1→3 is a full round trip: it cancels 2 debt units and adds
/// 0 permits, leaving the gate exactly where it started. Blindly calling
/// `add_permits(2)` instead would leave a 2-permit surplus AND the 2-unit debt that
/// eats it; total capacity converges to the same number either way (each debt unit
/// destroys exactly one permit), so it does not over-ADMIT — but it does inflate
/// `available_permits` with phantom capacity and forces the next acquirer through
/// two pointless acquire→forget→re-queue rounds, each landing it at the back of
/// the semaphore's FIFO queue behind later arrivals. Pay first; keep the books
/// honest.
///
/// # Invariant
///
/// At quiescence (no `acquire` mid-loop, no `set_limit` mid-flight):
/// `available + in_use - debt == limit`. Every mutation preserves it:
/// `add_permits(k)` raises `available` and `limit` by `k`; a shrink of `cut` lowers
/// `limit` by `cut` and lowers `available + in_use` and raises `debt` by exactly
/// `cut` between them; a debt payment lowers both `available + in_use` and `debt`
/// by 1.
pub struct ReceiveGate {
    /// Arc'd because [`acquire`](Self::acquire) hands out owned permits, which
    /// outlive the borrow of the gate.
    sem: Arc<tokio::sync::Semaphore>,
    state: std::sync::Mutex<GateState>,
}

impl ReceiveGate {
    /// A gate admitting `limit` concurrent receives, clamped to
    /// `MIN_CONCURRENT_RECEIVES..=MAX_CONCURRENT_RECEIVES`.
    pub fn new(limit: usize) -> Self {
        let limit = limit.clamp(MIN_CONCURRENT_RECEIVES, MAX_CONCURRENT_RECEIVES);
        Self {
            sem: Arc::new(tokio::sync::Semaphore::new(limit)),
            state: std::sync::Mutex::new(GateState { limit, debt: 0 }),
        }
    }

    /// Wait for a lane. The returned permit holds the slot until it is dropped, so
    /// the caller keeps it alive for the whole fetch+ingest phase.
    ///
    /// Cancel-safe: dropping the future while it waits leaves the gate untouched
    /// (tokio hands a permit only to a live waiter, and the debt bookkeeping runs
    /// entirely between await points).
    pub async fn acquire(&self) -> tokio::sync::OwnedSemaphorePermit {
        loop {
            let permit = Arc::clone(&self.sem)
                .acquire_owned()
                .await
                // `close()` is never called on this semaphore — it is private to
                // the gate, which lives as long as the `InboundControl` owning it,
                // and nothing here closes it. `AcquireError` is therefore
                // unreachable rather than a case to handle.
                .expect("receive gate semaphore is never closed");
            {
                let mut state = self
                    .state
                    .lock()
                    .expect("receive gate state mutex poisoned");
                if state.debt == 0 {
                    return permit;
                }
                // This permit belongs to a shrink that could not take effect when
                // it was requested. Pay one unit and go back to waiting.
                state.debt -= 1;
            }
            // Outside the lock: destroy the permit instead of releasing it, so the
            // semaphore's total shrinks by one for good.
            permit.forget();
        }
    }

    /// Change the cap live (clamped as in [`new`](Self::new)). Never blocks and
    /// never interrupts a transfer already holding a permit: a grow wakes parked
    /// waiters immediately, a shrink lands as debt that the in-flight lanes pay off
    /// as they finish.
    pub fn set_limit(&self, limit: usize) {
        let limit = limit.clamp(MIN_CONCURRENT_RECEIVES, MAX_CONCURRENT_RECEIVES);
        let mut state = self
            .state
            .lock()
            .expect("receive gate state mutex poisoned");
        if limit > state.limit {
            let mut grow = limit - state.limit;
            // Debt first — see "Debt vs. grow" on the struct.
            let paid = grow.min(state.debt);
            state.debt -= paid;
            grow -= paid;
            if grow > 0 {
                self.sem.add_permits(grow);
            }
        } else if limit < state.limit {
            let cut = state.limit - limit;
            // Takes only what is available right now; the rest becomes debt.
            let forgotten = self.sem.forget_permits(cut);
            state.debt += cut - forgotten;
        }
        state.limit = limit;
    }

    /// The cap currently in force (post-clamp) — for status reporting and tests.
    pub fn limit(&self) -> usize {
        self.state
            .lock()
            .expect("receive gate state mutex poisoned")
            .limit
    }
}

/// Receiver-side cancellation control (Task 12): the shared signal the command
/// layer uses to cancel an inbound package the receiver is about to fetch or is
/// already fetching. One instance per started receiver, threaded into
/// [`SyncReceiver::spawn`] and reachable from the command layer through
/// [`SyncRuntime::inbound_control`].
///
/// `cancels` holds the `package_id`s the user asked to cancel; [`is_cancelled`]
/// checks membership. [`request_cancel`] records one and wakes any in-flight
/// fetch's select loop via `notify`, so a cancel requested DURING a fetch aborts
/// the download promptly instead of waiting it out. The persisted
/// [`InboundState::Cancelled`] row is the restart-proof twin of this in-memory
/// set — a cancel survives a restart through the row even though this set does not.
///
/// `revoke_aborts` (Transfers Batch Model §D2, B4-fix) is a SEPARATE signal for the
/// same abort mechanism, requested by [`request_revoke_abort`] from the receiver's
/// event-ingress pump (see [`SyncReceiver::spawn`]) the instant a sender
/// [`Revoke`](crate::sharing::types::RevokeReason) arrives — cross-task, so it can
/// wake an in-flight fetch even while the serial receive loop is still busy
/// awaiting an earlier `handle_announce`. Kept distinct from `cancels`
/// deliberately: a revoke must NOT divert the aborted fetch into the local-decline
/// [`cancel_epilogue`] (which sends an ack) — the ALREADY-QUEUED `RevokeReceived`
/// event drives the reason-honest [`handle_revoke`] bookkeeping (no ack) once the
/// serial loop drains it next. Both signals share the one `notify` — the select
/// loop just re-checks both flags on every wake, so one `Notify` is sufficient.
///
/// [`is_cancelled`]: Self::is_cancelled
/// [`request_cancel`]: Self::request_cancel
/// [`request_revoke_abort`]: Self::request_revoke_abort
/// `receive_gate` (W2 T2.2) is a third, unrelated signal riding this struct: the
/// live-resizable cap on simultaneous fetch+ingest phases. It sits here because it
/// shares the control's lifetime and reach — one instance per started receiver,
/// already threaded to both the receive loop and the command layer, which is
/// exactly what "resize without restarting the receiver" needs.
pub struct InboundControl {
    cancels: std::sync::Mutex<std::collections::HashSet<String>>,
    revoke_aborts: std::sync::Mutex<std::collections::HashSet<String>>,
    notify: tokio::sync::Notify,
    /// Concurrency gate for the fetch+ingest phase — see [`ReceiveGate`]. Public
    /// because the receive lanes acquire it directly and the settings command
    /// layer resizes it; it carries no cross-field invariant with the cancel sets.
    pub receive_gate: ReceiveGate,
}

/// Hand-written rather than derived: `receive_gate` defaults to
/// [`DEFAULT_MAX_CONCURRENT_RECEIVES`], not to a `Default` impl on
/// [`ReceiveGate`] — the gate deliberately has no `Default`, so the shipped cap
/// has exactly one definition and cannot be silently conjured elsewhere.
impl Default for InboundControl {
    fn default() -> Self {
        Self {
            cancels: Default::default(),
            revoke_aborts: Default::default(),
            notify: Default::default(),
            receive_gate: ReceiveGate::new(DEFAULT_MAX_CONCURRENT_RECEIVES),
        }
    }
}

impl InboundControl {
    /// A fresh control with no cancellations requested and the receive gate at
    /// [`DEFAULT_MAX_CONCURRENT_RECEIVES`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation of `package_id`: record it and wake any in-flight
    /// fetch select loop so it re-checks and aborts promptly.
    pub fn request_cancel(&self, package_id: &str) {
        self.cancels
            .lock()
            .expect("inbound cancels mutex poisoned")
            .insert(package_id.to_string());
        self.notify.notify_waiters();
    }

    /// Whether `package_id` has been requested for cancellation.
    pub fn is_cancelled(&self, package_id: &str) -> bool {
        self.cancels
            .lock()
            .expect("inbound cancels mutex poisoned")
            .contains(package_id)
    }

    /// Request an in-flight-fetch abort for `package_id` because the SENDER
    /// revoked its announce (B4-fix) — cross-task equivalent of [`request_cancel`]
    /// but recorded on the separate `revoke_aborts` set so the fetch-abort call
    /// site can tell the two reasons apart (see the struct doc). Called from the
    /// receiver's event-ingress pump, never from `handle_revoke` itself (which
    /// only runs after the fetch has already been dealt with).
    pub fn request_revoke_abort(&self, package_id: &str) {
        self.revoke_aborts
            .lock()
            .expect("inbound revoke_aborts mutex poisoned")
            .insert(package_id.to_string());
        self.notify.notify_waiters();
    }

    /// Whether `package_id`'s in-flight fetch has been asked to abort for a sender
    /// revoke (as opposed to a local [`is_cancelled`](Self::is_cancelled) decline).
    pub fn is_revoke_abort_requested(&self, package_id: &str) -> bool {
        self.revoke_aborts
            .lock()
            .expect("inbound revoke_aborts mutex poisoned")
            .contains(package_id)
    }

    /// Consume a [`request_revoke_abort`](Self::request_revoke_abort) entry once
    /// its revoke has been drained by the serial loop (called at `handle_revoke`
    /// entry). By then any fetch the flag needed to abort has already returned
    /// (the loop is serial), and a LINGERING entry would wedge a straggler
    /// re-announce of the same wire id — under the Decline Finality Axis a
    /// revoke-cancelled row legitimately resets and re-fetches, and a stale abort
    /// flag would break that fetch on its first poll with no terminal written.
    pub fn clear_revoke_abort(&self, package_id: &str) {
        self.revoke_aborts
            .lock()
            .expect("inbound revoke_aborts mutex poisoned")
            .remove(package_id);
    }

    /// A future that resolves the next time [`request_cancel`](Self::request_cancel)
    /// or [`request_revoke_abort`](Self::request_revoke_abort) is called. The
    /// in-flight fetch loop selects on this to learn about either without polling.
    /// NB the tokio `Notify` registration caveat: the returned
    /// [`Notified`](tokio::sync::futures::Notified) only registers the waiter once
    /// polled/`enable`d, so the call site enables it before checking the flag.
    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
}

/// Handle to a running [`SyncReceiver`]. Dropping it (or calling
/// [`shutdown`](Self::shutdown)) stops the event loop.
///
/// It owns BOTH background tasks the receiver spawns: the serial fetch→ingest→ack
/// loop (`join`) AND the event-ingress `pump` (B4-fix) that forwards transport
/// events into the loop's channel. Both must be aborted on shutdown — the pump is
/// normally parked in `raw_events.recv().await`, so without an explicit abort it
/// would linger blocked on the transport stream after the loop stops (B5 §6a).
pub struct SyncReceiverHandle {
    join: JoinHandle<()>,
    pump: JoinHandle<()>,
}

impl SyncReceiverHandle {
    /// Abort both receiver tasks (serial loop + ingress pump) and await their exit.
    pub async fn shutdown(self) {
        self.join.abort();
        self.pump.abort();
        let _ = self.join.await;
        let _ = self.pump.await;
    }
}

/// The receive-side service. [`spawn`](Self::spawn) brings the transport online,
/// takes its single-consumer event stream, and runs the fetch → ingest → ack
/// loop on a background task.
pub struct SyncReceiver;

impl SyncReceiver {
    /// Start the receiver over `transport`, staging fetched packages under
    /// `staging_root` and landing accepted files under the root the `incoming`
    /// resolver returns (resolved live, per package — so a `sync_incoming`
    /// designation change is honored on the next package without a restart).
    /// Ingests into `store`. Returns the transport's [`StartInfo`] (its node id +
    /// pairing ticket) plus a handle to the spawned loop.
    ///
    /// `transport.start()` is awaited here (so the ticket is available to the
    /// caller); the loop then takes the event stream exactly once.
    pub async fn spawn(
        store: Arc<CatalogSyncStore>,
        staging_root: PathBuf,
        incoming: IncomingResolver,
        authorized: PeerAuthorizer,
        project: ProjectReceiveHooks,
        control: Arc<InboundControl>,
        transport: Arc<dyn SharingTransport>,
        emitter: Arc<dyn ProgressEmitter>,
    ) -> Result<(StartInfo, SyncReceiverHandle)> {
        let info = transport
            .start()
            .await
            .context("start receiver transport")?;
        std::fs::create_dir_all(&staging_root)
            .with_context(|| format!("create staging root {}", staging_root.display()))?;

        let ProjectReceiveHooks {
            gate: project_gate,
            announcements_refresher,
            on_project_ingested,
            request_handler,
        } = project;

        let raw_events = transport.events().await;

        // Ingress pump (Transfers Batch Model §D2, B4-fix): decouples RECEIVING a
        // transport event from PROCESSING it. The serial loop below stays the sole
        // processor (no concurrent `handle_announce`/`handle_revoke` — the B2
        // single-writer invariant `upsert_inbound_attempt` relies on holds), which
        // means a `RevokeReceived` queued behind a currently-running, blocking
        // `handle_announce` fetch would otherwise sit unprocessed until that fetch
        // finishes on its own — the opposite of "revoke stops the download
        // promptly". This pump task is never blocked by that fetch: it forwards
        // every event, in order, into a fresh channel the serial loop drains, and
        // for an AUTHORIZED peer's `RevokeReceived` it ALSO signals
        // [`InboundControl::request_revoke_abort`] synchronously, right here, the
        // instant the event arrives — which wakes the fetch-abort select loop
        // inside `handle_announce` cross-task (the same `Notify`
        // [`cancel_incoming_package`](crate::api::sync::cancel_incoming_package)
        // already wakes). The event is still forwarded on to the serial loop so
        // `handle_revoke` performs the reason-honest bookkeeping (terminal state,
        // settle files, staging, tags, history, journal — no ack) once it drains
        // it, strictly after the aborted `handle_announce` call has returned. The
        // pump re-runs the SAME H1 authorizer the serial loop's arms use (never
        // trust-then-verify) so an unauthorized peer cannot poison the abort set —
        // belt-and-suspenders in dev-ticket mode, where there is no connect_gate.
        // Unbounded so the pump's `send` never itself blocks on a busy consumer.
        let (forward_tx, mut events) = mpsc::unbounded_channel::<TransportEvent>();
        let pump_control = Arc::clone(&control);
        let pump_authorized = Arc::clone(&authorized);
        let pump = tokio::spawn(async move {
            let mut raw_events = raw_events;
            while let Some(ev) = raw_events.recv().await {
                if let TransportEvent::RevokeReceived {
                    from, package_id, ..
                } = &ev
                {
                    if pump_authorized(from) {
                        pump_control.request_revoke_abort(&package_id.0);
                    }
                }
                if forward_tx.send(ev).is_err() {
                    break;
                }
            }
        });

        // Startup reconcile (zombie-inbound fix): any non-terminal `sync_inbound`
        // row left by a prior process cannot resume — a fetch/ingest never
        // survives a restart — so stamp every such row `Failed` BEFORE the event
        // loop consumes its first announce. A later re-announce resets the row via
        // `upsert_inbound_attempt` (declined rows stay final through `declined_at`,
        // which this reconcile never touches — Decline Finality Axis §D3), so this
        // is a clean lifecycle reset, not data loss. Runs on this task, fully
        // before the spawned loop exists.
        reconcile_stale_inbound(&store);

        let loop_transport = Arc::clone(&transport);
        let join = tokio::spawn(async move {
            tracing::info!(staging_root = %staging_root.display(), "sync receiver online");
            while let Some(ev) = events.recv().await {
                match ev {
                    // `batch_uuid` (spec §D1/§D2) is the durable per-transfer identity
                    // the receiver keys ONE inbound row on across every attempt (B4):
                    // v3 → the sender's package-dir basename; v1/v2 → the wire package
                    // id (B1 fallback), which reproduces today's per-attempt rows.
                    TransportEvent::AnnounceReceived {
                        from,
                        announce,
                        batch_name,
                        batch_uuid,
                        files,
                    } => {
                        // Authorization gate (finding H1): only ingest from a peer
                        // on this receiver's allow-list. An unauthorized (or
                        // revoked) node is silently dropped BEFORE any
                        // fetch/ingest/ack — it never touches the catalog or the
                        // landing folder, and gets no signal it was even heard.
                        if !authorized(&from) {
                            tracing::warn!(
                                from = %super::node_id_hex(&from),
                                package_id = %announce.package_id.0,
                                "sync receiver dropped announce from an unauthorized peer"
                            );
                            continue;
                        }
                        // Task 4.1 (candidate (a)): time each inline announce
                        // handling. `announce` is moved into the call, so capture
                        // the package id first; `from` (a `NodeId`) is `Copy`.
                        // Instrumentation only — no behavior change.
                        let announce_started = std::time::Instant::now();
                        let package_id_for_log = announce.package_id.0.clone();
                        if let Err(e) = handle_announce(
                            &store,
                            &staging_root,
                            &incoming,
                            loop_transport.as_ref(),
                            Arc::clone(&emitter),
                            &control,
                            from,
                            announce,
                            batch_name,
                            batch_uuid,
                            files,
                        )
                        .await
                        {
                            tracing::error!(error = %format!("{e:#}"), "sync receiver announce handling failed");
                        }
                        tracing::info!(
                            package_id = %package_id_for_log,
                            from = %super::node_id_hex(&from),
                            duration_ms = announce_started.elapsed().as_millis() as u64,
                            "sync receiver announce handled"
                        );
                    }
                    // Collab exchange (slice 4): an inbound PROJECT package
                    // advertisement. The ROW KEY is the event's hub `package_id`
                    // (audit B1) while fetch/ack use the wire `announce.package_id`.
                    TransportEvent::ProjectAnnounceReceived {
                        from,
                        project_id,
                        package_id,
                        announce,
                    } => {
                        // The hub package id is peer-controlled — reject anything
                        // that is not a single safe path segment BEFORE the gate
                        // (same C1 guard as personal sync).
                        if let Err(e) = crate::package::validate_package_id(&package_id) {
                            tracing::warn!(
                                from = %super::node_id_hex(&from),
                                project_id,
                                package_id,
                                error = %e,
                                "project announce rejected: unsafe package_id"
                            );
                            continue;
                        }
                        // Cross-account trust: only a verified current member of
                        // `project_id` may push-seed to us. Gate absent or
                        // refusing ⇒ drop (fail-closed — never accept-all).
                        let accepted = project_gate
                            .as_ref()
                            .map(|gate| gate(&from, &project_id))
                            .unwrap_or(false);
                        if !accepted {
                            tracing::warn!(
                                from = %super::node_id_hex(&from),
                                project_id,
                                package_id,
                                "project announce dropped: sender is not an authorized project member"
                            );
                            continue;
                        }
                        if let Err(e) = handle_project_announce(
                            &store,
                            &staging_root,
                            loop_transport.as_ref(),
                            emitter.as_ref(),
                            announcements_refresher.as_ref(),
                            on_project_ingested.as_ref(),
                            from,
                            project_id,
                            package_id,
                            announce,
                        )
                        .await
                        {
                            tracing::error!(error = %format!("{e:#}"), "sync receiver project announce handling failed");
                        }
                    }
                    // Collab exchange (slice 4, task 6): a member asked us (a
                    // holder) to serve a project package. Dispatch to the host's
                    // serve handler, which authorizes (`may_serve_package`),
                    // reconstructs the serve dir, and enqueues an explicit-target
                    // serve back to `from` through the collab sender map. The
                    // handler `tokio::spawn`s the async work, so this stays
                    // non-blocking. Absent handler ⇒ log + drop (pre-task-6).
                    TransportEvent::ProjectRequestReceived {
                        from,
                        project_id,
                        package_id,
                    } => match &request_handler {
                        Some(handler) => {
                            tracing::info!(
                                from = %super::node_id_hex(&from),
                                project_id,
                                package_id,
                                "project package requested — dispatching serve"
                            );
                            handler(from, project_id, package_id);
                        }
                        None => {
                            tracing::warn!(
                                from = %super::node_id_hex(&from),
                                project_id,
                                package_id,
                                "project package requested but no serve handler installed; dropping"
                            );
                        }
                    },
                    // A sender revoked an outstanding announce (spec §D2, B4): abort
                    // any in-flight fetch, drive the row to an honest terminal, settle
                    // file rows, release in-flight tags, and write history + journal.
                    // NO ack is sent for a revoke.
                    TransportEvent::RevokeReceived {
                        from,
                        package_id,
                        reason,
                    } => {
                        // H1 belt-and-suspenders (mirrors the `AnnounceReceived`
                        // arm): the connect_gate covers production, but dev-ticket
                        // mode installs no gate, so this per-event authorizer check
                        // must be uniform across every event kind that can touch
                        // catalog/receiver state. An unauthorized peer's revoke is
                        // silently dropped before it can terminalize any row.
                        if !authorized(&from) {
                            tracing::warn!(
                                from = %super::node_id_hex(&from),
                                package_id = %package_id.0,
                                "sync receiver dropped revoke from an unauthorized peer"
                            );
                            continue;
                        }
                        handle_revoke(
                            &store,
                            loop_transport.as_ref(),
                            emitter.as_ref(),
                            &control,
                            &staging_root,
                            from,
                            &package_id,
                            reason,
                        )
                        .await;
                    }
                    // `AckReceived` is the sender's half — the receiver loop does
                    // not consume it.
                    _ => {}
                }
            }
            tracing::info!("sync receiver event stream closed; loop stopping");
        });

        Ok((info, SyncReceiverHandle { join, pump }))
    }
}

/// Best-effort: stamp `package_id`'s inbound row [`Failed`](InboundState::Failed)
/// with `error`'s display text, warning (never propagating — the caller is
/// already on its own error path) if the write itself fails.
///
/// Shared by every early-return error site in [`handle_announce`] AFTER the row
/// exists (fetch / ingest / ack) — spec §8's terminal mapping requires "Failed +
/// last_error on fetch/ingest errors", and a fetch/ingest/ack error propagated
/// via `?` without this stamp leaves the row stuck non-terminal forever (visible
/// as a perpetual in-progress transfer in [`inbound_active`](super::store::inbound_active)
/// with no recorded reason) until a later re-announce happens to self-heal it
/// (reviewer finding, Task 11 follow-up).
/// Startup reconcile (zombie-inbound fix): park every non-terminal
/// `sync_inbound` row (`announced`/`fetching`/`ingesting`)
/// [`Waiting`](InboundState::Waiting) with `"interrupted by restart"`.
///
/// D2 §4 changed this from `Failed`: the ATTEMPT could not survive the restart,
/// but the TRANSFER is untouched — the sender is still obliged to deliver it and
/// the next announce revives this same row — so the honest state is outstanding,
/// not lost. Two consequences follow. The row's per-file rows are LEFT ALONE
/// (§3.3): they are the resume checkpoint and the file counter's evidence, and
/// settling them `failed` would throw away the record of what already arrived. And
/// a row that is ALREADY `Waiting` is skipped: being non-terminal it returns from
/// [`inbound_active`] on every launch, so re-stamping it would overwrite the
/// reason a vanished peer left behind with `"interrupted by restart"` at the first
/// restart.
///
/// A fetch/ingest is in-memory and cannot survive the process it ran in, so any
/// such row left on disk by a prior receiver is a zombie: the sender that owned
/// it has since re-announced under a fresh wire `package_id` (the sender's wire
/// id was not stable across its restart before this fix, and even with the fix a
/// resend is a new id), leaving this row stuck non-terminal forever and visible
/// in [`inbound_active`] as a perpetual in-progress transfer. [`inbound_active`]
/// already excludes the terminal states, so a `Cancelled` row is left untouched.
/// A later re-announce of the same `(peer, batch_uuid)` resets the row via
/// [`upsert_inbound_attempt`]'s non-declined reset rule — and a DECLINED row
/// this reconcile stamped `failed` stays final regardless, because finality is
/// the `declined_at` axis, which this fn never writes (Decline Finality Axis
/// §D3, pinned by `decline_survives_restart_reconcile_and_refuses_resend`).
///
/// Receipt-log repair (Transfers smoke №8, item 4) still runs FIRST and is
/// unchanged: if a row ALREADY holds a full receipt set under its wire id
/// (`count_satisfied_receipts == frame_count > 0`), it reached a real terminal
/// before the restart and its non-terminal `state` is only the residue of a
/// mid-transfer duplicate announce's `upsert` reset. Such a row is stamped the
/// HONEST terminal from its receipts (all-`Cancelled` → `Cancelled` + a
/// `declined_at` repair; else `Done`) — and, being genuinely terminal, it DOES
/// settle its per-file rows. Only the fallback below changed.
///
/// Best-effort: a failed enumeration or per-row write warns and never blocks
/// receiver startup.
fn reconcile_stale_inbound(store: &CatalogSyncStore) {
    let conn = store.lock_conn();
    let stale = match inbound_active(&conn) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "inbound startup reconcile enumeration failed");
            return;
        }
    };
    let mut count: u32 = 0;
    let mut repaired: u32 = 0;
    for row in stale {
        // D2 §4: a `Waiting` row is already parked with an honest reason, and being
        // non-terminal it returns from `inbound_active` on EVERY launch. Re-stamping
        // it would replace that reason with "interrupted by restart", so the first
        // restart after a peer vanishes would erase the very thing this state
        // records. Its per-file rows stay as the resume checkpoint too.
        if row.state == InboundState::Waiting {
            continue;
        }
        // Receipt-log repair (Transfers smoke №8, item 4): a stale non-terminal row
        // that ALREADY holds a full receipt set under its wire id reached a real
        // terminal before the restart — its `state` was only left non-terminal by a
        // mid-transfer duplicate announce's `upsert` reset (the same defect the
        // pure-replay guard now prevents at announce time). Stamp the HONEST terminal
        // from the durable receipts instead of mislabeling it `failed "interrupted by
        // restart"`: an all-`Cancelled` set is a receiver decline (Cancelled +
        // `declined_at` repair), else it delivered (Done). This heals a row already
        // stuck in the field on the next launch.
        if row.frame_count > 0 {
            let satisfied = match count_satisfied_receipts(&conn, &row.package_id) {
                Ok(n) => n,
                Err(e) => {
                    // A count read error must not silently mislabel: warn, then let the
                    // row take the honest zombie fallback (`failed`) below.
                    tracing::warn!(package_id = %row.package_id, error = %format!("{e:#}"), "inbound reconcile receipt count failed; falling back to failed");
                    0
                }
            };
            if satisfied == row.frame_count {
                let receipts = load_receipts(&conn, &row.package_id).unwrap_or_default();
                let all_cancelled = !receipts.is_empty()
                    && receipts
                        .iter()
                        .all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled));
                let (terminal, outcome, file_outcome) = if all_cancelled {
                    if let Err(e) = set_inbound_declined_at(&conn, row.id) {
                        tracing::warn!(package_id = %row.package_id, error = %format!("{e:#}"), "inbound declined_at (reconcile repair) write failed");
                    }
                    (InboundState::Cancelled, "cancelled", Some("cancelled"))
                } else {
                    (InboundState::Done, "done", None)
                };
                if let Err(e) = set_inbound_state(&conn, &row.package_id, terminal, None) {
                    tracing::warn!(package_id = %row.package_id, error = %format!("{e:#}"), "inbound reconcile repair state write failed");
                } else {
                    repaired += 1;
                    tracing::info!(package_id = %row.package_id, outcome, "stale inbound repaired from receipt log");
                }
                // Settle any per-file rows the reset left unsettled to match the
                // repaired terminal (a cancelled repair reuses the epilogue's
                // `done`+`cancelled`, a delivered one plain `done`). Best-effort.
                if let Err(e) = settle_unsettled_inbound_files(
                    &conn,
                    row.id,
                    InboundFileState::Done,
                    file_outcome,
                    None,
                ) {
                    tracing::warn!(inbound_id = row.id, error = %format!("{e:#}"), "inbound reconcile repair file-row settle failed");
                }
                continue;
            }
        }
        match set_inbound_state(
            &conn,
            &row.package_id,
            InboundState::Waiting,
            Some("interrupted by restart"),
        ) {
            Ok(()) => count += 1,
            Err(e) => tracing::warn!(
                package_id = %row.package_id,
                error = %format!("{e:#}"),
                "inbound startup reconcile write failed"
            ),
        }
        // D2 §3.3: NO per-file settle here. The rows record what actually arrived
        // before the restart, so they are this attempt's resume checkpoint and the
        // file counter's evidence; a later re-announce refreshes them back to
        // `announced` via `record_inbound_manifest` when the next attempt really
        // starts. Settling them `failed` for a transfer that is merely outstanding
        // would report zero received files for a batch we are still holding.
    }
    if count > 0 {
        tracing::info!(count, "stale inbound rows parked waiting after restart");
    }
    if repaired > 0 {
        tracing::info!(
            count = repaired,
            "stale inbound rows repaired to honest terminal from receipt log"
        );
    }
}

/// Build the receiver's live fetch sink (Task 11; per-file shape reshaped by
/// D2 §3.4).
///
/// Each batch tick persists live `bytes_done` and emits a `fetching` progress
/// carrying the byte figures; each per-file tick persists that file's state
/// transition and emits a `sync-file-progress`. DB writes are best-effort — a
/// failed write warns and never aborts the fetch. Ticks arrive throttled (≤ every
/// 300 ms per stream), so writing at that cadence is fine.
///
/// Extracted from `handle_announce` so the per-file transition rule is directly
/// testable: it is the one piece of receive-side progress logic with a real
/// state machine in it, and the bug it had (a first tick that already carried
/// full bytes could never reach the terminal rung) was invisible end-to-end
/// because ingest overwrites every file row moments later.
#[allow(clippy::too_many_arguments)]
fn build_fetch_sink(
    store: &Arc<CatalogSyncStore>,
    emitter: &Arc<dyn ProgressEmitter>,
    pkg: String,
    peer_device: String,
    frame_count: u32,
    inbound_id: i64,
    track_files: bool,
) -> FetchSink {
    let emitter = Arc::clone(emitter);
    let store = Arc::clone(store);
    // Per-file transition tracker (Transfers Status Model v2 §D4, reshaped by
    // D2 §3.4): remembers the last state WRITTEN for each file so the sink can
    // write on transitions only, never per byte-tick. Only populated when the
    // v2 manifest gave us per-file rows (`has_manifest`); a v1/nameless batch
    // has none, so the writes are skipped.
    let file_seen: Arc<std::sync::Mutex<HashMap<String, InboundFileState>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    Arc::new(move |ev| match ev {
        FetchEvent::Batch {
            bytes_done,
            bytes_total,
        } => {
            {
                let conn = store.lock_conn();
                if let Err(e) = set_inbound_bytes_done(&conn, &pkg, bytes_done) {
                    tracing::warn!(package_id = %pkg, error = %format!("{e:#}"), "inbound bytes_done update failed");
                }
            }
            emit_event(
                emitter.as_ref(),
                "sync-progress",
                &SyncProgressEvent {
                    package_id: pkg.clone(),
                    direction: super::Direction::Received,
                    stage: "fetching".to_string(),
                    peer_device: peer_device.clone(),
                    frame_count,
                    project_id: None,
                    bytes_done: Some(bytes_done),
                    bytes_total: Some(bytes_total),
                },
            );
        }
        FetchEvent::File {
            name,
            bytes_done,
            bytes_total,
            complete,
        } => {
            // Persist the per-file row transition (best-effort) BEFORE emitting
            // the live progress event — the live bar stays the event stream; the
            // DB row is the restart checkpoint and the file counter's evidence.
            //
            // D2 §3.4: the target state is computed from THIS tick and written
            // whenever it differs from what was last written — the sender's shape.
            // The pre-D2 scheme keyed the write on WHICH ARM ran, so a file whose
            // first tick already carried full bytes could never reach the terminal
            // rung.
            //
            // Completion comes from `complete`, NEVER from comparing the byte
            // figures. A blob that has not started downloading reports (0, 0), so
            // `bytes_done >= bytes_total` is true for every file of a batch before
            // a single byte arrives — it marked all of them fetched at once, then
            // walked them backwards as they actually started. Only the producer
            // knows; see the field doc on `FetchEvent::File::complete`.
            if track_files {
                let target = if complete {
                    InboundFileState::Fetched
                } else {
                    InboundFileState::Fetching
                };
                let mut map = file_seen.lock().expect("inbound file_seen mutex poisoned");
                if map.get(&name).copied() != Some(target) {
                    map.insert(name.clone(), target);
                    let conn = store.lock_conn();
                    if let Err(e) = set_inbound_file_state(
                        &conn, inbound_id, &name, target, bytes_done, None, None,
                    ) {
                        tracing::warn!(inbound_id, rel_path = %name, state = target.as_str(), error = %format!("{e:#}"), "inbound file state write failed");
                    }
                }
            }
            emit_event(
                emitter.as_ref(),
                "sync-file-progress",
                &SyncFileProgressEvent {
                    package_id: pkg.clone(),
                    peer_device: peer_device.clone(),
                    file: name,
                    bytes_done,
                    bytes_total,
                },
            );
        }
    })
}

/// Close `package_id`'s inbound row as [`Failed`](InboundState::Failed): stamp the
/// state with the reason, settle its un-settled per-file rows to `failed`
/// (Transfers Status Model v2 §D4 — a batch-level fetch/ingest/ack failure closes
/// every not-yet-`done` file row), AND announce the terminal.
///
/// **The announce is part of the operation, not the caller's job.** A terminal row
/// leaves [`inbound_active`], so the 10 s status poll drops it, and the durable
/// terminal list that must then carry it is only re-fetched on `sync-finished` —
/// a stamp without an emit makes the row vanish from a live Transfers screen
/// (owner smoke 2026-07-24). That is why the two halves live in one function:
/// this used to be a stamp plus a hand-copied eleven-line emit at each of three
/// call sites, and two of those three had no emit at all.
///
/// The full rule and its two deliberate exemptions are pinned by the
/// `every_terminal_writer_announces_or_is_a_named_exemption` guard test below.
///
/// Best-effort throughout: the caller is already on its own error path, so a failed
/// write only warns.
fn terminalize_inbound_failed(
    store: &CatalogSyncStore,
    emitter: &dyn ProgressEmitter,
    package_id: &str,
    peer_device: &str,
    error: &anyhow::Error,
) {
    let reason = format!("{error:#}");
    {
        let conn = store.lock_conn();
        if let Err(e) = set_inbound_state(&conn, package_id, InboundState::Failed, Some(&reason)) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound failed state write failed");
        }
        if let Some(row) = get_inbound(&conn, package_id).ok().flatten() {
            if let Err(e) = settle_unsettled_inbound_files(
                &conn,
                row.id,
                InboundFileState::Failed,
                None,
                Some(&reason),
            ) {
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound failed file-row settle failed");
            }
        }
    }
    // Emitted AFTER the writes so the refetch this triggers reads the settled row.
    emit_event(
        emitter,
        "sync-finished",
        &SyncFinishedEvent {
            package_id: package_id.to_string(),
            direction: super::Direction::Received,
            outcome: "failed".to_string(),
            peer_device: peer_device.to_string(),
            ok_count: 0,
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        },
    );
}

/// Stamp `package_id`'s inbound row [`Waiting`](InboundState::Waiting) with the
/// reason — and DELIBERATELY leave its per-file rows alone (D2 §3.3).
///
/// The twin of [`terminalize_inbound_failed`], minus the settle and the announce. This ATTEMPT ended, but
/// the TRANSFER did not: delivery-forever obliges the sender to redeliver, so the
/// per-file rows are the resume checkpoint. Settling them `failed` here would reset
/// the counter D2 §3.4 exists to make honest — the row would report zero received
/// files while holding most of them on disk — and
/// [`upsert_inbound_attempt`](super::store::upsert_inbound_attempt) refreshes them
/// anyway when the next attempt actually starts.
///
/// Best-effort like its twin: the caller is already on its own error path, so a
/// failed write only warns.
fn stamp_inbound_waiting(store: &CatalogSyncStore, package_id: &str, error: &anyhow::Error) {
    let reason = format!("{error:#}");
    let conn = store.lock_conn();
    if let Err(e) = set_inbound_state(&conn, package_id, InboundState::Waiting, Some(&reason)) {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound waiting state write failed");
    }
}

/// Best-effort per-batch event journal (Transfers Status Model v2 §D7, direction
/// `received`, `batch_key` = the inbound row id as text). Never fails the receive —
/// a failed append only warns. The receive-side twin of the sender engine's journal.
fn journal(store: &CatalogSyncStore, inbound_id: i64, kind: &str, detail: Option<&str>) {
    let conn = store.lock_conn();
    if let Err(e) = append_sync_event(
        &conn,
        Direction::Received,
        &inbound_id.to_string(),
        kind,
        detail,
    ) {
        tracing::warn!(inbound_id, kind, error = %format!("{e:#}"), "sync receiver journal append failed");
    }
}

/// Sanitize a human batch name into a single filesystem-safe path segment for the
/// landing tree (Transfers Status Model v2 §D2), reusing the same sanitizer family
/// as [`resolve_sender_slug`](ingest::resolve_sender_slug): whitespace/reserved
/// chars collapse, then leading/trailing `.` are trimmed so an all-dot name (`..`)
/// can never escape a directory level. `None` when the name is blank or sanitizes
/// to empty — the caller then lands without a batch level (v1-style).
fn sanitize_batch_slug(name: &str) -> Option<String> {
    let slug = crate::archive::path_layout::sanitize_for_filename(name);
    let slug = slug.trim_matches('.').to_string();
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Persist a v2 announce manifest onto the inbound row (Transfers Status Model v2
/// §D1/§D4): its `display_name` and the full set of `announced` per-file rows
/// (`bytes_done` 0), BEFORE any fetch — the receiver knows the whole tree the moment
/// the announce lands. `replace_*` swaps the whole set so a re-announce refreshes it.
/// Called only for a non-cancelled row (a cancelled row is final — the caller guards).
fn record_inbound_manifest(
    conn: &rusqlite::Connection,
    inbound_id: i64,
    name: Option<&str>,
    files: &[AnnounceFileEntry],
) -> Result<()> {
    set_inbound_display_name(conn, inbound_id, name)?;
    if !files.is_empty() {
        let now = now_iso();
        let rows: Vec<InboundFileRow> = files
            .iter()
            .map(|f| InboundFileRow {
                inbound_id,
                rel_path: f.rel_path.clone(),
                byte_size: f.byte_size,
                frame_uuid: f.frame_uuid.clone(),
                state: InboundFileState::Announced,
                bytes_done: 0,
                outcome: None,
                error: None,
                updated_at: now.clone(),
            })
            .collect();
        replace_inbound_files(conn, inbound_id, &rows)?;
    }
    Ok(())
}

/// Resolve the on-disk landing directory for a NAMED (v2) inbound package
/// (Transfers Status Model v2 §D2). Reuses the row's persisted `landing_dir` when
/// present (so a resume/restart lands into the same tree), else computes
/// `<incoming_root>/<sender_slug>/<batch_slug>` — suffixing `_2`/`_3`… while another
/// NON-terminal inbound row already claims that dir — persists it, and returns it.
/// A terminal prior batch with the same dir is reused (repeat sends of one object
/// merge). Best-effort persist: a failed write just means the next run re-resolves.
fn resolve_landing_dir(
    conn: &rusqlite::Connection,
    inbound_id: i64,
    incoming_root: &Path,
    peer_device: &str,
    batch_slug: &str,
) -> PathBuf {
    // Reuse the persisted dir first — the resume/restart guarantee.
    if let Ok(Some(row)) = super::store::get_inbound_by_row_id(conn, inbound_id) {
        if let Some(dir) = row.landing_dir.filter(|d| !d.is_empty()) {
            return PathBuf::from(dir);
        }
    }
    let sender_slug = ingest::resolve_sender_slug(conn, peer_device);
    let parent = incoming_root.join(sender_slug);
    let mut candidate = parent.join(batch_slug);
    let mut n: u32 = 2;
    while landing_dir_claimed_by_active(conn, &candidate.to_string_lossy(), inbound_id)
        .unwrap_or(false)
    {
        candidate = parent.join(format!("{batch_slug}_{n}"));
        n += 1;
    }
    if let Err(e) = set_inbound_landing_dir(conn, inbound_id, &candidate.to_string_lossy()) {
        tracing::warn!(inbound_id, error = %format!("{e:#}"), "persist landing_dir failed; will re-resolve next run");
    }
    candidate
}

/// Handle one announced package: persist an inbound row, ack-replay guard, else
/// fetch → ingest → ack, emitting stage progress (with live fetch bytes) and a
/// single finished event, walking the `sync_inbound` row through its lifecycle.
#[allow(clippy::too_many_arguments)]
async fn handle_announce(
    store: &Arc<CatalogSyncStore>,
    staging_root: &Path,
    incoming: &IncomingResolver,
    transport: &dyn SharingTransport,
    emitter: Arc<dyn ProgressEmitter>,
    control: &InboundControl,
    from: NodeId,
    announce: PackageAnnounce,
    batch_name: Option<String>,
    batch_uuid: String,
    files: Option<Vec<AnnounceFileEntry>>,
) -> Result<()> {
    let peer_device = super::node_id_hex(&from);
    let package_id = announce.package_id.0.clone();

    // B5b: received `sync_history` rows key on the durable `batch_uuid` — the same
    // identity the inbound row is keyed on — so every attempt's rows share ONE key
    // and an earlier attempt can never render as a phantom faded group. This is
    // exactly the `batch_uuid` `upsert_inbound_attempt` stores on the row below; an
    // empty value (never happens post-B4, where v1/v2 fall back to the wire id)
    // defends by reusing the wire id, matching the summary/detail NULL-edge fallback.
    let history_key = if batch_uuid.trim().is_empty() {
        package_id.clone()
    } else {
        batch_uuid.clone()
    };

    // v2 announce extras (Transfers Status Model v2): a blank/whitespace batch name
    // is treated as absent (v1-style, no batch landing level), an empty manifest as
    // "no per-file rows". The loopback mock always sends `Some("")`/`Some(vec![])`
    // for a nameless send, so normalise both here.
    let effective_name: Option<String> = batch_name.filter(|n| !n.trim().is_empty());
    let effective_files: Vec<AnnounceFileEntry> = files.unwrap_or_default();
    let has_manifest = !effective_files.is_empty();

    // The wire `package_id` is peer-controlled and is used below to build the
    // per-package staging directory. Reject anything that is not a single safe
    // path segment BEFORE it is ever joined onto a path — an absolute or
    // `..`-laden id would place the fetched package at an attacker-chosen
    // location (arbitrary file write / RCE, finding C1). Fail closed: refuse the
    // announce, emit a failed outcome, ingest nothing. No inbound row is written
    // for an unsafe id (we never persist an attacker-chosen key).
    if let Err(e) = crate::package::validate_package_id(&package_id) {
        tracing::warn!(
            from = %peer_device,
            package_id = %package_id,
            error = %e,
            "sync receiver rejected announce with unsafe package_id"
        );
        emit_event(
            emitter.as_ref(),
            "sync-finished",
            &SyncFinishedEvent {
                package_id,
                direction: super::Direction::Received,
                outcome: "failed".to_string(),
                peer_device,
                ok_count: 0,
                failed: Vec::new(),
                new_count: 0,
                duplicate_count: 0,
                project_id: None,
            },
        );
        return Ok(());
    }

    // Pure-replay guard (Transfers smoke №8, item 4) — placed BEFORE the upsert.
    // A SAME-WIRE re-announce of an already fully-receipted transfer must NOT reset
    // the row. Field failure mode: a serve-tick-armed ack-timeout re-announce raced
    // ahead of the receiver's own inline fetch+ingest; when finally processed, its
    // wire id still equalled the row's CURRENT attempt wire id AND that attempt had
    // already reached a terminal with a full receipt set. Letting it fall through to
    // `upsert_inbound_attempt` reset the Done/Cancelled row back to `announced`
    // (generation bump, bytes wipe, manifest refresh) BEFORE the post-upsert replay
    // guard re-acked it — and if that re-ack then FAILED (the sender already
    // confirmed, so its ack channel may reject), the earlier `?` aborted before the
    // terminal re-stamp, stranding the row at `announced` forever (owner's live stuck
    // row id=1). Instead: detect the same-wire fully-receipted case up front, skip the
    // upsert entirely (no reset / no generation bump / no manifest refresh / no
    // "received" progress emit), journal the announce for Log-feed continuity, and
    // replay the ack from the durable receipt log via the shared helper — which
    // stamps the terminal INDEPENDENT of ack success. A resend on a FRESH wire id
    // (`row.package_id != this announce`) deliberately falls through to the upsert →
    // seeding → post-upsert guard, so the declined-resend flow is byte-identical.
    {
        let existing = {
            let conn = store.lock_conn();
            get_inbound_by_batch(&conn, &peer_device, &batch_uuid).with_context(|| {
                format!("look up inbound row for replay ({package_id}, batch {batch_uuid})")
            })?
        };
        if let Some(row) = existing {
            if row.package_id == package_id && announce.frame_count > 0 {
                let satisfied_count = store.count_satisfied_receipts(&announce.package_id)?;
                if satisfied_count == announce.frame_count {
                    let inbound_id = row.id;
                    // A declined row's receipts are all `Cancelled`, so `all_cancelled`
                    // inside the helper always takes the Cancelled branch here; passing
                    // its flag preserves the `!declined_final` Done-stamp guard anyway.
                    let declined_final = row.declined_at.is_some();
                    journal(
                        store,
                        inbound_id,
                        "announce_received",
                        Some(&format!(
                            "name={} files={} frames={}",
                            effective_name.as_deref().unwrap_or("-"),
                            effective_files.len(),
                            announce.frame_count
                        )),
                    );
                    return replay_ack_from_log(
                        store,
                        transport,
                        emitter.as_ref(),
                        from,
                        &announce,
                        inbound_id,
                        &peer_device,
                        declined_final,
                        satisfied_count,
                    )
                    .await;
                }
            }
        }
    }

    // Persist (or refresh) the inbound row for this transfer, keyed on the durable
    // `(peer, batch_uuid)` (Transfers Batch Model §D1): every attempt of one
    // transfer resolves to ONE long-lived row. A re-announced attempt (resend or
    // retry) resets the SAME row back to `announced` with THIS attempt's wire id,
    // clearing its byte/finished markers while PRESERVING `display_name` +
    // `landing_dir` (so attempts land into the same tree). A DECLINED row
    // (`declined_at` non-NULL — Decline Finality Axis §D3) is left untouched and
    // reported via `declined_final` — the receiver's refusal is final, so we must
    // never re-fetch it (the seeding + replay-guard below re-ack the sender's new
    // attempt as all-cancelled without fetching). A row that is merely `cancelled`
    // by a SENDER revoke has `declined_at` NULL and resets like any other attempt
    // terminal — a sender's resend after its own cancel fetches normally.
    // v1/v2 announces arrive with `batch_uuid == wire package_id` (B1 fallback),
    // so each attempt's fresh wire id makes a fresh row — exactly today's
    // per-attempt behavior, now with a non-NULL key.
    let (inbound_id, declined_final) = {
        let conn = store.lock_conn();
        upsert_inbound_attempt(
            &conn,
            &peer_device,
            &batch_uuid,
            &package_id,
            announce.frame_count,
            announce.byte_size,
        )
        .with_context(|| format!("record inbound announce {package_id} (batch {batch_uuid})"))?
    };

    // Stamp the announcing peer's device capability onto the row (Perseus UI v2,
    // Task 9) so the Transfers UI can later show whether this transfer came from a
    // full Athenaeum peer or a send-only Perseus agent. Read from the cached
    // account-device capability map — persisting it onto the row means the label
    // survives a later device revocation that empties that cache. Best-effort and
    // strictly informational: a cache miss leaves the column NULL and a write
    // failure only warns; neither ever affects the transfer. Skipped on a DECLINED
    // (final) row — it was already stamped on its first attempt and must stay inert.
    if !declined_final {
        let conn = store.lock_conn();
        if let Some(cap) = super::ingest::cached_device_capability(&conn, &peer_device) {
            if let Err(error) = super::store::set_inbound_peer_capability(&conn, inbound_id, &cap) {
                tracing::warn!(%error, inbound_id, "failed to stamp peer capability");
            }
        }
    }

    // Record the v2 manifest onto the row BEFORE any fetch — the receiver knows the
    // whole tree the moment the announce lands. Refreshes name + REPLACES the
    // per-file set on a re-announce (naturally re-keyed to the same batch row);
    // NEVER touches a DECLINED row (it is final — `declined_final` and the guard
    // in `upsert_inbound_attempt` keep it inert). Best-effort — a failed write
    // only warns, never blocks the receive.
    if !declined_final && (effective_name.is_some() || has_manifest) {
        let conn = store.lock_conn();
        if let Err(e) = record_inbound_manifest(
            &conn,
            inbound_id,
            effective_name.as_deref(),
            &effective_files,
        ) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "record inbound manifest failed");
        }
    }
    journal(
        store,
        inbound_id,
        "announce_received",
        Some(&format!(
            "name={} files={} frames={}",
            effective_name.as_deref().unwrap_or("-"),
            effective_files.len(),
            announce.frame_count
        )),
    );

    emit_event(
        emitter.as_ref(),
        "sync-progress",
        &SyncProgressEvent {
            package_id: package_id.clone(),
            direction: super::Direction::Received,
            stage: "received".to_string(),
            peer_device: peer_device.clone(),
            frame_count: announce.frame_count,
            project_id: None,
            bytes_done: None,
            bytes_total: None,
        },
    );

    // Declined-transfer resend re-ack (Transfers Batch Model §D1/B4, Decline
    // Finality Axis §D5). A receiver that DECLINED a transfer keeps it declined:
    // the sender's resend arrives on a FRESH wire id, `upsert_inbound_attempt`
    // left the declined row untouched (`declined_final`), and that new wire id
    // carries no receipts — so the ack-replay guard below would NOT fire and the
    // row would fall through to the cancel epilogue, which would re-fetch the
    // manifest AND re-write per-frame history for every resend. Instead we seed
    // THIS attempt's wire id with the prior attempt's `Cancelled` receipts
    // (re-keyed, no manifest fetch), which makes the replay guard answer the
    // sender with an all-cancelled ack for the new wire id WITHOUT fetching and
    // WITHOUT duplicating history. The receipt-anchor invariant (§D5) keeps the
    // declined row's `package_id` pointing at the wire id that holds its newest
    // full `Cancelled` set (rotated here after a successful seed, and by the
    // epilogue when it writes a set under a fresh wire id), so this lookup always
    // finds it. When NO receipts exist yet (a declined row whose epilogue never
    // fired — decline-then-crash) this seeding is a no-op and the announce falls
    // through to the epilogue below (which writes the receipts + history for the
    // first time).
    if declined_final {
        let conn = store.lock_conn();
        match get_inbound_by_row_id(&conn, inbound_id) {
            Ok(Some(row)) => {
                let prev_wire = row.package_id;
                if prev_wire != package_id {
                    match load_receipts(&conn, &prev_wire) {
                        Ok(prior)
                            if !prior.is_empty()
                                && prior
                                    .iter()
                                    .all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled)) =>
                        {
                            let now = now_iso();
                            let mut seed_failures = 0usize;
                            for r in &prior {
                                if let Err(e) = insert_receipt(&conn, &package_id, r, &now) {
                                    seed_failures += 1;
                                    tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "seed cancelled receipt for resend failed");
                                }
                            }
                            // §D5: rotate the anchor ONLY when the new wire id holds
                            // the COMPLETE set — anchoring a partial set would orphan
                            // the full one under `prev_wire` and re-open the
                            // duplicate-history epilogue on every later resend.
                            if seed_failures == 0 {
                                if let Err(e) =
                                    rotate_inbound_package_id(&conn, inbound_id, &package_id)
                                {
                                    tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "rotate receipt anchor after seed failed");
                                }
                            }
                            tracing::info!(
                                package_id = %package_id,
                                prev_wire = %prev_wire,
                                count = prior.len(),
                                "receiver re-acking a declined transfer's resend from the prior attempt's receipts"
                            );
                        }
                        // No receipts (or a non-cancelled set) under the anchor: a
                        // declined row whose epilogue never completed — fall through
                        // to the epilogue below, which writes the set first-time.
                        Ok(_) => {
                            tracing::debug!(package_id = %package_id, prev_wire = %prev_wire, "no full cancelled receipt set under the anchor; epilogue will write it");
                        }
                        Err(e) => {
                            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "load prior cancelled receipts for resend re-ack failed")
                        }
                    }
                }
            }
            Ok(None) => {
                tracing::warn!(package_id = %package_id, inbound_id, "declined row vanished before resend seeding");
            }
            Err(e) => {
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "read declined row for resend seeding failed");
            }
        }
    }

    // Ack-replay guard: a fully-receipted package is re-acked from the log,
    // skipping the fetch and ingest entirely. Counts only non-Rejected
    // receipts as "satisfied" — a package with a pending Rejected receipt must
    // fall through to fetch+ingest below so that frame gets a real redelivery
    // attempt, not a replay of its stale rejection (fix-review finding #1). This
    // post-upsert site still fires for a declined-final resend (whose receipts the
    // seeding above re-keyed under THIS attempt's fresh wire id) and for a normal
    // re-delivery whose row the upsert just reset; the pure-replay guard above
    // handled the same-wire, no-reset case. Both share `replay_ack_from_log`.
    let satisfied_count = store.count_satisfied_receipts(&announce.package_id)?;
    if announce.frame_count > 0 && satisfied_count == announce.frame_count {
        return replay_ack_from_log(
            store,
            transport,
            emitter.as_ref(),
            from,
            &announce,
            inbound_id,
            &peer_device,
            declined_final,
            satisfied_count,
        )
        .await;
    }

    // Wire-in (a) — Task 12: a persisted DECLINED row (restart-proof via
    // `declined_at`) OR a control-requested cancel that reached us at/before this
    // announce runs the cancel epilogue instead of fetching — it fetches only the
    // manifest, writes a `Cancelled` receipt per frame, acks them, and stamps the
    // row Cancelled. The replay guard above already handled a package whose
    // epilogue previously wrote every frame's receipt (later re-announces replay
    // from the log — cheaper, no manifest fetch), so this only runs on the FIRST
    // cancel announce, before any receipts exist.
    if declined_final || control.is_cancelled(&package_id) {
        return cancel_epilogue(
            store,
            transport,
            emitter.as_ref(),
            from,
            &announce,
            staging_root,
            inbound_id,
            &history_key,
            effective_name.as_deref(),
        )
        .await;
    }

    // Fetch the package into a per-package staging dir under the staging root
    // (out of the user-visible landing tree, so a half-fetched package never
    // shows up in the designated sync_incoming folder).
    let staging = staging_root.join("staging").join(&package_id);
    {
        let conn = store.lock_conn();
        if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Fetching, None) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound fetching state write failed");
        }
    }
    journal(store, inbound_id, "fetch_started", None);
    emit_event(
        emitter.as_ref(),
        "sync-progress",
        &SyncProgressEvent {
            package_id: package_id.clone(),
            direction: super::Direction::Received,
            stage: "fetching".to_string(),
            peer_device: peer_device.clone(),
            frame_count: announce.frame_count,
            project_id: None,
            bytes_done: None,
            bytes_total: None,
        },
    );
    // I2 (T7): `from` dialed in to announce; the blob pull dials back out, so give
    // the downloader a relay dial hint for it before fetching (no-op on loopback /
    // when no relay set is resolved — never regresses the existing path reuse).
    transport.add_peer_dial_hint(from);
    // Real fetch sink (Task 11): each batch tick persists live `bytes_done` and
    // emits a `fetching` progress carrying the byte figures; each per-file tick
    // emits a `sync-file-progress`. DB writes are best-effort — a failed byte
    // update warns and never aborts the fetch. Ticks arrive throttled (≤ every
    // 300ms per stream), so a write at that cadence is fine.
    let sink: FetchSink = build_fetch_sink(
        store,
        &emitter,
        package_id.clone(),
        peer_device.clone(),
        announce.frame_count,
        inbound_id,
        has_manifest,
    );
    // Wire-in (b) — Task 12, extended B4-fix: the fetch is abortable on EITHER of
    // two distinct cross-task signals. Pin the fetch and race it against the
    // shared notify; a break drops the fetch future — Task 10's downloader aborts
    // the in-flight download on drop — BEFORE either post-break branch runs. The
    // two signals resolve differently on break:
    //  - `is_cancelled` (local decline via `cancel_incoming_package`) → diverts to
    //    the local `cancel_epilogue` (fetches the manifest, writes Cancelled
    //    receipts, sends an ack).
    //  - `is_revoke_abort_requested` (B4-fix: set by the receiver's event-ingress
    //    pump the instant a sender `RevokeReceived` arrives, cross-task, even
    //    while this call is still running) → does NOT run the epilogue and sends
    //    NO ack; it only needs the download stopped. The `RevokeReceived` event
    //    that set the flag is already queued behind this announce in the serial
    //    loop's channel (the pump forwards every event it observes), so
    //    `handle_revoke` runs the full reason-honest bookkeeping (terminal state,
    //    settle files, staging, tags, history, journal) the moment this call
    //    returns and the loop drains its next event.
    let fetch_outcome: Option<Result<()>> = {
        let fetch_fut = transport.fetch(from, &announce, &staging, sink);
        tokio::pin!(fetch_fut);
        loop {
            // Enable the notify waiter BEFORE checking the flags so a cancel/revoke
            // that races in right after the check still wakes us (a tokio
            // `Notified` only registers the waiter once polled / `enable`d).
            let notified = control.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();
            if control.is_cancelled(&package_id) || control.is_revoke_abort_requested(&package_id) {
                break None;
            }
            tokio::select! {
                biased;
                r = &mut fetch_fut => break Some(r),
                // Woken by a cancel or revoke abort (possibly for another
                // package); loop back and let the flag re-check at the top decide.
                _ = &mut notified => {}
            }
        }
    };
    let Some(fetch_result) = fetch_outcome else {
        if control.is_revoke_abort_requested(&package_id) {
            // Sender-revoked mid-fetch (B4-fix): the fetch is already dropped
            // above. Do NOT touch row/file state here and send NO ack — the
            // already-queued `RevokeReceived` event runs `handle_revoke`'s
            // reason-honest bookkeeping the moment the serial loop drains it next.
            tracing::info!(
                package_id = %package_id,
                peer_device = %peer_device,
                "sync receiver aborted in-flight fetch for a sender revoke"
            );
            return Ok(());
        }
        // Cancelled mid-fetch (local decline): the dropped fetch future aborted
        // the download.
        tracing::info!(package_id = %package_id, peer_device = %peer_device, "sync receiver cancelling in-flight fetch");
        return cancel_epilogue(
            store,
            transport,
            emitter.as_ref(),
            from,
            &announce,
            staging_root,
            inbound_id,
            &history_key,
            effective_name.as_deref(),
        )
        .await;
    };
    if let Err(e) = fetch_result {
        // D2 §3.2: WHY the fetch died decides whether this row is finished. The
        // classification is produced at the failure site (`LocalFault`, attached in
        // `sharing::iroh::blobs`), never sniffed from the error text — a vanished
        // peer and a full disk arrive through the same `Result`. Unmarked ⇒
        // peer-absent, which is the safer default: a local fault mislabeled as
        // waiting retries and stays visible, while a vanished peer mislabeled as
        // failed is the lie this design removes.
        if crate::sharing::types::is_local_fault(&e) {
            journal(store, inbound_id, "fetch_failed", Some(&format!("{e:#}")));
            terminalize_inbound_failed(store, emitter.as_ref(), &package_id, &peer_device, &e);
        } else {
            // The peer went away. Non-terminal, so the row stays in
            // `inbound_active` and the 10 s status poll keeps it visible on its
            // own — which is what the D1-era `sync-finished` on this path was
            // compensating for. No event here: there is no terminal to announce,
            // and the per-file rows stay as the resume checkpoint (§3.3).
            journal(store, inbound_id, "fetch_waiting", Some(&format!("{e:#}")));
            stamp_inbound_waiting(store, &package_id, &e);
        }
        return Err(e).with_context(|| format!("fetch package {package_id}"));
    }

    // Resolve the landing root LIVE, per package: a `sync_incoming` designation
    // (or clear) since the last package is honored here — not frozen at transport
    // start. Falls back to the caller's app-data default when none is designated.
    let incoming_root = incoming();

    // Resolve the per-package landing directory ONCE (Transfers Status Model v2 §D2).
    // A named (v2) batch lands under `<incoming_root>/<sender_slug>/<batch_slug>`
    // (collision-suffixed, persisted, resume-stable); an unnamed (v1) batch has no
    // override, so ingest lands under `<incoming_root>/<sender_slug>` — byte-identical
    // to the pre-v2 layout.
    let landing_override: Option<PathBuf> =
        match effective_name.as_deref().and_then(sanitize_batch_slug) {
            Some(batch_slug) => {
                let conn = store.lock_conn();
                Some(resolve_landing_dir(
                    &conn,
                    inbound_id,
                    &incoming_root,
                    &peer_device,
                    &batch_slug,
                ))
            }
            None => None,
        };

    // Ingest on a blocking thread (file I/O + SQLite); never block the runtime.
    {
        let conn = store.lock_conn();
        if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Ingesting, None) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound ingesting state write failed");
        }
    }
    journal(store, inbound_id, "ingest_started", None);
    emit_event(
        emitter.as_ref(),
        "sync-progress",
        &SyncProgressEvent {
            package_id: package_id.clone(),
            direction: super::Direction::Received,
            stage: "ingesting".to_string(),
            peer_device: peer_device.clone(),
            frame_count: announce.frame_count,
            project_id: None,
            bytes_done: None,
            bytes_total: None,
        },
    );
    let ingest_result: Result<IngestOutcome> = {
        let store = Arc::clone(store);
        let staging_for_ingest = staging.clone();
        let announce = announce.clone();
        let peer_device = peer_device.clone();
        let history_key = history_key.clone();
        let batch_name = effective_name.clone();
        let landing_override = landing_override.clone();
        match tokio::task::spawn_blocking(move || -> Result<IngestOutcome> {
            // Per-frame connection locking (W2 T2.1): ingest acquires the store
            // guard for the prologue and then once per frame, never across the
            // whole package — a concurrent lane waits one frame, not minutes.
            ingest::ingest_package(
                ingest::IngestConn::Shared(store.as_ref()),
                &incoming_root,
                &staging_for_ingest,
                &announce,
                &peer_device,
                &history_key,
                batch_name.as_deref(),
                landing_override.as_deref(),
            )
        })
        .await
        {
            Ok(inner) => inner,
            Err(join_err) => Err(anyhow::Error::new(join_err).context("ingest join")),
        }
    };
    let outcome = match ingest_result {
        Ok(o) => o,
        Err(e) => {
            // An ingest failure (manifest unreadable, DB error, or the blocking
            // task itself panicking) is terminal for this row (Failed + reason);
            // propagate so the receiver loop logs it too.
            journal(store, inbound_id, "failed", Some(&format!("{e:#}")));
            terminalize_inbound_failed(store, emitter.as_ref(), &package_id, &peer_device, &e);
            return Err(e).with_context(|| format!("ingest package {package_id}"));
        }
    };

    // Settle the per-file rows from the per-frame receipts (Transfers Status Model v2
    // §D4) — OUTSIDE the ingest transaction (it committed inside `ingest_package`),
    // keyed rel_path ← frame_uuid via the announced file rows. `done` + the receiver's
    // verdict text (same encoding as `sync_receipts`); a rejected frame also stamps the
    // per-file `error`. Skipped when there are no per-file rows (v1). Best-effort.
    if has_manifest {
        let conn = store.lock_conn();
        settle_inbound_files_from_receipts(&conn, inbound_id, &outcome.receipts);
    }

    // Ack the per-frame receipts, then emit the single finished event.
    if let Err(e) = transport
        .ack(from, &announce.package_id, outcome.receipts.clone())
        .await
    {
        // An ack failure is terminal for this row too — the frames landed but the
        // sender never learns their verdict this round; a redelivery re-acks from
        // the receipt log (ack-replay guard above) once the peer is reachable
        // again, but this row must not sit stuck non-terminal until then.
        //
        // D2 §3.2 rules this Failed rather than Waiting even though the connection
        // is what died: the receive SUCCEEDED — every frame is landed and
        // catalogued — so there is nothing outstanding to wait for on our side.
        // Only the verdict is undelivered, and the ack-replay guard hands it back
        // whole on the sender's next announce.
        journal(store, inbound_id, "failed", Some(&format!("{e:#}")));
        terminalize_inbound_failed(store, emitter.as_ref(), &package_id, &peer_device, &e);
        return Err(e).with_context(|| format!("ack package {package_id}"));
    }

    // Terminal for the receiver: the package is acked, so drop the fetched
    // blobs. Never fails the (successful) receive — log-and-continue on error.
    if let Err(e) = transport.release(&announce.package_id).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "receiver blob release failed");
    }

    // Best-effort staging cleanup — a leftover staging dir is harmless but tidy.
    if let Err(e) = std::fs::remove_dir_all(&staging) {
        tracing::debug!(error = %e, path = %staging.display(), "sync receiver staging cleanup skipped");
    }

    let failed: Vec<String> = outcome
        .receipts
        .iter()
        .filter(|r| matches!(r.outcome, ReceiptOutcome::Rejected(_)))
        .map(|r| r.frame_uuid.clone())
        .collect();
    let finished_outcome = if outcome.failed() == 0 {
        "ingested"
    } else if outcome.ok_count() == 0 {
        "failed"
    } else {
        "partial"
    };
    // Stamp the terminal inbound state: Done on ingested/partial, Failed (with a
    // reason) when every frame was rejected.
    {
        let conn = store.lock_conn();
        let res = if finished_outcome == "failed" {
            set_inbound_state(
                &conn,
                &package_id,
                InboundState::Failed,
                Some(&format!("{} frame(s) rejected", failed.len())),
            )
        } else {
            set_inbound_state(&conn, &package_id, InboundState::Done, None)
        };
        if let Err(e) = res {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound terminal state write failed");
        }
    }
    // Journal the terminal ingest outcome (§D7): `ingested` (any accepted) carries
    // the ok/duplicate/rejected split; a whole-package rejection journals `failed`.
    if finished_outcome == "failed" {
        journal(
            store,
            inbound_id,
            "failed",
            Some(&format!("{} frame(s) rejected", failed.len())),
        );
    } else {
        journal(
            store,
            inbound_id,
            "ingested",
            Some(&format!(
                "ingested={} duplicate={} rejected={}",
                outcome.ingested,
                outcome.duplicate + outcome.skipped_older,
                outcome.rejected
            )),
        );
    }
    emit_event(
        emitter.as_ref(),
        "sync-finished",
        &SyncFinishedEvent {
            package_id,
            direction: super::Direction::Received,
            outcome: finished_outcome.to_string(),
            peer_device,
            ok_count: outcome.ok_count(),
            failed,
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        },
    );
    Ok(())
}

/// Re-ack a fully-receipted transfer straight from the durable receipt log — the
/// shared replay path behind BOTH the pure-replay guard (a same-wire re-announce
/// of an already-terminal row, no upsert reset) AND the post-upsert replay guard
/// (a declined-final resend whose receipts the seeding re-keyed under this fresh
/// wire id, or a normal re-delivery whose row the upsert just reset). Factoring
/// the two into one fn keeps their behaviour from drifting.
///
/// Ordering contract (Transfers smoke №8, item 4): the terminal stamp is written
/// BEFORE — and INDEPENDENT of — the ack. The receipts are the durable answer, so
/// the ack is best-effort (warn + continue on error; the sender's next retry
/// re-triggers this replay). The pre-fix code propagated a failed re-ack with `?`,
/// which aborted before the Done re-stamp and stranded a just-reset row at
/// `announced` forever. The stamp is CONDITIONAL on the state actually differing so
/// a genuine no-op re-announce (already Done/Cancelled) writes nothing.
///
/// `all_cancelled` (a non-empty, all-`Cancelled` receipt set) is a receiver-decline
/// replay: the finished outcome is "cancelled", `ok_count` is 0 (item 2 — never
/// toast an arrival for a decline), and the row is stamped `Cancelled` + the
/// transfer-level `declined_at` repair (§D2 repair 2 — a full all-cancelled set only
/// ever originates from a decline, so this also heals a crash between a prior
/// epilogue's receipts and its row stamp). Otherwise it is a normal `Done` replay
/// with `ok_count = satisfied_count`, unless `declined_final` (the row stays
/// untouched — a declined-final row must never be stamped Done).
#[allow(clippy::too_many_arguments)]
async fn replay_ack_from_log(
    store: &Arc<CatalogSyncStore>,
    transport: &dyn SharingTransport,
    emitter: &dyn ProgressEmitter,
    from: NodeId,
    announce: &PackageAnnounce,
    inbound_id: i64,
    peer_device: &str,
    declined_final: bool,
    satisfied_count: u32,
) -> Result<()> {
    let package_id = announce.package_id.0.clone();
    let receipts = store.load_receipts(&announce.package_id)?;
    // MANDATORY carry-over item 1 (Task 4 review): a package whose replayed receipts
    // are ALL `Cancelled` is a receiver-cancel replay — its finished outcome must be
    // "cancelled", NEVER "ingested" (and the row stays Cancelled, not stamped Done).
    let all_cancelled = !receipts.is_empty()
        && receipts
            .iter()
            .all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled));

    // Terminal stamp FIRST, ordered before / independent of the ack, and only when
    // the state actually differs (a no-op re-announce writes nothing — no generation
    // churn, no finished_at rewrite).
    {
        let conn = store.lock_conn();
        let current_state = get_inbound_by_row_id(&conn, inbound_id)
            .ok()
            .flatten()
            .map(|r| r.state);
        if all_cancelled {
            if let Err(e) = set_inbound_declined_at(&conn, inbound_id) {
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound declined_at (replay) write failed");
            }
            if current_state != Some(InboundState::Cancelled) {
                if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Cancelled, None)
                {
                    tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound cancelled (replay) write failed");
                }
            }
        } else if !declined_final && current_state != Some(InboundState::Done) {
            if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Done, None) {
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound done (replay) write failed");
            }
        }
    }

    // Re-ack from the receipt log — NON-FATAL. On error, warn and continue: the
    // receipts are durable and the sender's next retry re-triggers this replay.
    if let Err(e) = transport.ack(from, &announce.package_id, receipts).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "replay ack failed; sender retry will re-trigger");
    }
    tracing::info!(package_id = %package_id, count = satisfied_count, all_cancelled, "sync receiver replayed ack from receipt log");
    journal(
        store,
        inbound_id,
        if all_cancelled {
            "cancelled"
        } else {
            "replayed"
        },
        Some(&format!("count={satisfied_count}")),
    );
    // Terminal for the receiver: drop the fetched blobs. A lost-ack resend may have
    // re-downloaded them; release is idempotent. Never fails the receive.
    if let Err(e) = transport.release(&announce.package_id).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "receiver blob release failed");
    }
    emit_event(
        emitter,
        "sync-finished",
        &SyncFinishedEvent {
            package_id,
            direction: super::Direction::Received,
            outcome: if all_cancelled {
                "cancelled"
            } else {
                "replayed"
            }
            .to_string(),
            peer_device: peer_device.to_string(),
            // Item 2: a decline replay accepted no frames — never report an arrival count.
            ok_count: if all_cancelled { 0 } else { satisfied_count },
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        },
    );
    Ok(())
}

/// Settle an inbound batch's per-file rows from the per-frame ingest receipts
/// (Transfers Status Model v2 §D4). Maps each receipt's `frame_uuid` to its
/// `rel_path` via the announced per-file rows, then stamps that row `done` with the
/// receiver's verdict text (same encoding as `sync_receipts`); a `Rejected` receipt
/// also records the reason as the per-file `error`. MUST run OUTSIDE the ingest
/// transaction (the per-file CRUD open their own `unchecked_transaction`). Every
/// write is best-effort — a failure only warns.
fn settle_inbound_files_from_receipts(
    conn: &rusqlite::Connection,
    inbound_id: i64,
    receipts: &[FrameReceipt],
) {
    let rows = match list_inbound_files(conn, inbound_id) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(inbound_id, error = %format!("{e:#}"), "list inbound files for settle failed");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    // frame_uuid → (rel_path, byte_size) from the announced rows.
    let by_uuid: HashMap<&str, (&str, u64)> = rows
        .iter()
        .map(|r| (r.frame_uuid.as_str(), (r.rel_path.as_str(), r.byte_size)))
        .collect();
    for receipt in receipts {
        let Some(&(rel_path, byte_size)) = by_uuid.get(receipt.frame_uuid.as_str()) else {
            continue;
        };
        let outcome_txt = receipt_outcome_to_db(&receipt.outcome);
        let error = match &receipt.outcome {
            ReceiptOutcome::Rejected(msg) => Some(msg.as_str()),
            _ => None,
        };
        if let Err(e) = set_inbound_file_state(
            conn,
            inbound_id,
            rel_path,
            InboundFileState::Done,
            byte_size,
            Some(&outcome_txt),
            error,
        ) {
            tracing::warn!(inbound_id, rel_path, error = %format!("{e:#}"), "inbound file settle write failed");
        }
    }
}

/// Cancel epilogue (Task 12): the receiver's terminal path for a package the user
/// declined. Fetches ONLY the manifest (no payload frames), writes a
/// [`Cancelled`](ReceiptOutcome::Cancelled) receipt for every manifest frame into
/// the durable receipt log, acks them to the sender (best-effort — the replay path
/// re-acks a lost ack on the next re-announce), stamps the inbound row
/// [`Cancelled`](InboundState::Cancelled), and emits a single `sync-finished`
/// "cancelled" event (direction `received`).
///
/// Idempotent (double-cancel / retried epilogue safe): `insert_receipt` upserts by
/// `(package_id, frame_uuid)`, the `Cancelled` row is final and re-stamping it is a
/// no-op-shaped write, and `release`/staging-cleanup are best-effort. A `Cancelled`
/// receipt counts as satisfied, so the ONE full set of receipts written here makes
/// every later re-announce replay the cancel from the log (the replay guard fires
/// before wire-in (a)) — no repeat manifest fetch.
#[allow(clippy::too_many_arguments)]
async fn cancel_epilogue(
    store: &Arc<CatalogSyncStore>,
    transport: &dyn SharingTransport,
    emitter: &dyn ProgressEmitter,
    from: NodeId,
    announce: &PackageAnnounce,
    staging_root: &Path,
    inbound_id: i64,
    history_key: &str,
    batch_name: Option<&str>,
) -> Result<()> {
    let peer_device = super::node_id_hex(&from);
    let package_id = announce.package_id.0.clone();

    // 0. Decline Finality Axis: stamp the transfer-level decline marker
    //    (first-write-wins — §D2 repair 1; every path into this fn is
    //    decline-originated: the local `cancels` flag, which `handle_revoke`
    //    deliberately never touches, or a declined-final resend) and rotate the
    //    receipt anchor to THIS attempt's wire id (§D5) — the receipts written
    //    below land under `package_id`, and rotating FIRST also makes every
    //    `package_id`-keyed read/write in this fn hit the row even when the row
    //    still carried an older attempt's wire id (the declined-final resend
    //    path, where `upsert_inbound_attempt` left the row untouched). One merged
    //    UPDATE = one commit on the serial loop; rotating to the already-current
    //    id is a no-op-shaped write.
    {
        let conn = store.lock_conn();
        if let Err(e) = mark_inbound_declined_with_anchor(&conn, inbound_id, &package_id) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound declined_at + anchor write failed");
        }
    }

    // 1. Fetch just the manifest into staging (tiny — no payload frames). We need
    //    the per-frame identities (uuid + xxh3) to build the Cancelled receipts.
    let staging = staging_root.join("staging").join(&package_id);
    transport
        .fetch_manifest(from, announce, &staging)
        .await
        .with_context(|| format!("fetch manifest for cancel {package_id}"))?;
    // Reuse the same manifest reader `ingest_package` uses.
    let records = crate::package::read_manifest(&staging)
        .with_context(|| format!("read manifest for cancel {package_id}"))?;
    let frame_count = records.len();

    // A cancel row may carry a batch name resolved at announce time even when the
    // caller passed none (re-announce refreshed it, restart, …). Prefer the caller's
    // value; else fall back to the persisted `display_name`.
    let batch_name: Option<String> = batch_name.map(|s| s.to_string()).or_else(|| {
        let conn = store.lock_conn();
        get_inbound(&conn, &package_id)
            .ok()
            .flatten()
            .and_then(|r| r.display_name)
    });

    // 2. A `Cancelled` receipt per manifest frame → sync_receipts (the replay log),
    //    plus a receiver-side `sync_history` row per frame (Transfers Status Model v2
    //    §D6 — a declined package is a first-class outcome on BOTH sides). One tx so
    //    the receipt + history never drift.
    let receipts: Vec<FrameReceipt> = records
        .iter()
        .map(|r| FrameReceipt {
            frame_uuid: r.frame_uuid.clone(),
            xxh3: r.xxh3.clone(),
            outcome: ReceiptOutcome::Cancelled,
        })
        .collect();
    let history =
        ingest::cancelled_history_rows(&records, &peer_device, history_key, batch_name.as_deref());
    let now = super::now_iso();
    {
        let conn = store.lock_conn();
        for r in &receipts {
            insert_receipt(&conn, &package_id, r, &now)
                .with_context(|| format!("record cancel receipt for {}", r.frame_uuid))?;
        }
        for h in &history {
            if let Err(e) = insert_history_row(&conn, h) {
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "cancel history row write failed");
            }
        }
    }

    // 3. Ack the Cancelled receipts (best-effort — a lost ack is re-sent by the
    //    replay guard on the sender's next re-announce). The sender's all-cancelled
    //    handler (Task 4) then drives its outbound row to Cancelled.
    if let Err(e) = transport.ack(from, &announce.package_id, receipts).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "cancel ack failed; will replay");
    }

    // 4. Terminal row (final) + settle the per-file rows + drop any fetched blobs +
    //    tidy staging. Un-landed file rows (announced/fetching) settle to `done` with
    //    a `cancelled` outcome; any file that DID land keeps its own verdict.
    let landed = {
        let conn = store.lock_conn();
        // Count files that genuinely made it before this settle for the journal
        // (§D6 — settled done+cancelled rows from an earlier revoke don't count).
        let landed = list_inbound_files(&conn, inbound_id)
            .map(|rows| count_landed(&rows))
            .unwrap_or(0);
        if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Cancelled, None) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound cancelled state write failed");
        }
        if let Err(e) = settle_unsettled_inbound_files(
            &conn,
            inbound_id,
            InboundFileState::Done,
            Some("cancelled"),
            None,
        ) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound cancel file-row settle failed");
        }
        landed
    };
    if let Err(e) = transport.release(&announce.package_id).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "receiver blob release failed");
    }
    if let Err(e) = std::fs::remove_dir_all(&staging) {
        tracing::debug!(error = %e, path = %staging.display(), "cancel epilogue staging cleanup skipped");
    }
    journal(
        store,
        inbound_id,
        "cancelled",
        Some(&format!("frames={frame_count} landed={landed}")),
    );

    tracing::info!(package_id = %package_id, peer_device = %peer_device, frames = frame_count, "sync receiver cancelled inbound package");
    emit_event(
        emitter,
        "sync-finished",
        &SyncFinishedEvent {
            package_id,
            direction: super::Direction::Received,
            outcome: "cancelled".to_string(),
            peer_device,
            ok_count: 0,
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        },
    );
    Ok(())
}

/// Basename of a forward-slash manifest `rel_path` (mirrors [`ingest::filename_of`]).
fn filename_of(rel_path: &str) -> String {
    rel_path.rsplit('/').next().unwrap_or(rel_path).to_string()
}

/// Count of per-file rows that genuinely LANDED on disk (Decline Finality Axis
/// §D6). A settle (`settle_unsettled_inbound_files`) reuses `state = done` with a
/// `cancelled`/`superseded` outcome for files that never arrived — those are NOT
/// landings, only ingest-verdict `done` rows are. Keeps the revoke/cancel journals
/// honest (`landed=42` on a transfer where nothing arrived was a mislabel).
fn count_landed(rows: &[InboundFileRow]) -> usize {
    rows.iter()
        .filter(|r| {
            r.state == InboundFileState::Done
                && !matches!(r.outcome.as_deref(), Some("cancelled") | Some("superseded"))
        })
        .count()
}

/// Apply a sender [`Revoke`](TransportEvent::RevokeReceived) to the matching
/// inbound transfer (Transfers Batch Model §D2, B4). The sender sends a revoke on
/// ANY terminal transition with an outstanding un-acked announce — a user cancel,
/// an all-duplicate confirm that raced its own announce, or a local failure — and
/// **Revoke IS the stop mechanism**: an iroh-blobs provider serves purely by hash
/// and cannot unilaterally abort an in-flight upload, so the teardown is
/// receiver-driven (drop the fetch → connection closes → provider write error).
///
/// Look up the row by the revoke's `package_id` — the CURRENT attempt's wire id
/// (== the row's `package_id` column). A revoke for a SUPERSEDED older wire id no
/// longer matches any row's current id and is a `debug!` no-op; a revoke for an
/// unknown id, or for an already-terminal row, is likewise a `debug!` no-op. For a
/// non-terminal row:
///
/// - [`Cancelled`](RevokeReason::Cancelled) → row `cancelled` ("by sender"). An
///   ATTEMPT terminal only — `declined_at` is never written here (Decline
///   Finality Axis §D4), so the sender's own resend of this transfer resets the
///   row and fetches normally instead of hitting `declined_final`.
/// - [`Superseded`](RevokeReason::Superseded) → row `done` (a supersede is issued
///   only when the peer already holds every frame, so the receiver lacks nothing —
///   the honest terminal is success, NOT a decline). Detail "nothing to fetch
///   (superseded by sender)" when nothing landed, else "superseded (N of M
///   landed)". Deliberately NOT `cancelled`: marking it cancelled would misrender a
///   benign supersede as a user decline.
/// - [`Failed`](RevokeReason::Failed) → row `failed` ("sender failed").
///
/// In every non-terminal case: abort any in-flight fetch through the SAME
/// [`InboundControl`] the receiver-cancel command uses (reuse, don't fork — see the
/// serial-loop note below), settle the un-settled per-file rows, remove staging,
/// release the in-flight blob tags, write one receiver `sync_history` row per known
/// file (from the announced file rows — a revoke NEVER fetches a manifest), journal
/// `revoked`, and emit a single `sync-finished` event so a live receiver's Transfers
/// widget auto-dismisses the row immediately instead of lagging until the next status
/// poll (B5 §4). The finished `outcome` is the mapped terminal — `cancelled` / `done`
/// (superseded) / `failed` — matching the `emit_finished` siblings' payload shape.
/// **NO ack is sent for a revoke.**
///
/// Serial-loop note: `RevokeReceived` and `AnnounceReceived` share the single
/// consumer of the receiver event stream, so a revoke is processed only BETWEEN
/// announces — it can never run concurrently with a fetch on the same loop. This
/// fn touches NEITHER `InboundControl` signal: the in-flight abort was already
/// requested cross-task by the ingress pump (`request_revoke_abort`), and the
/// local-decline `cancels` set must never carry a revoked wire id — its entries
/// are permanent and a straggler re-announce of the same wire id would otherwise
/// divert into the cancel epilogue and mint a `declined_at` the user never chose.
/// A sender's RESEND after its own cancel is a legitimate follow-up (Decline
/// Finality Axis §D4): the resend resets this row and fetches normally.
#[allow(clippy::too_many_arguments)]
async fn handle_revoke(
    store: &Arc<CatalogSyncStore>,
    transport: &dyn SharingTransport,
    emitter: &dyn ProgressEmitter,
    control: &InboundControl,
    staging_root: &Path,
    from: NodeId,
    package_id: &PackageId,
    reason: RevokeReason,
) {
    let peer_device = super::node_id_hex(&from);
    let wire_id = package_id.0.clone();

    // Consume the ingress pump's abort flag FIRST, in every branch: the serial
    // loop has drained everything queued before this revoke, so any fetch the
    // flag needed to abort has already returned — and a lingering entry would
    // break the fetch of a legitimate same-wire straggler re-announce (which the
    // Decline Finality Axis now resets and re-fetches) with no terminal written.
    control.clear_revoke_abort(&wire_id);

    // Correlate the revoke to the CURRENT attempt's row by its wire id. A revoke
    // for a superseded older wire id (the row has since rotated to a newer id) —
    // or for an id we never saw — matches nothing: no-op.
    let row = {
        let conn = store.lock_conn();
        match get_inbound(&conn, &wire_id) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(package_id = %wire_id, error = %format!("{e:#}"), "revoke row lookup failed");
                return;
            }
        }
    };
    let Some(row) = row else {
        tracing::debug!(from = %peer_device, package_id = %wire_id, ?reason, "revoke for an unknown/stale wire id; ignoring");
        return;
    };
    if row.state.is_terminal() {
        tracing::debug!(
            from = %peer_device,
            package_id = %wire_id,
            ?reason,
            state = row.state.as_str(),
            "revoke for an already-terminal row; ignoring"
        );
        return;
    }
    let inbound_id = row.id;
    let batch_name = row.display_name.clone();
    // B5b: the revoke's received `sync_history` rows key on the row's durable
    // `batch_uuid` (fallback to the wire id for a legacy NULL-batch_uuid row),
    // matching the ingest/cancel writers and the summary/detail NULL-edge fallback.
    let history_key = row
        .batch_uuid
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| wire_id.clone());

    // Deliberately NO `control.request_cancel` here (review fix, Decline Finality
    // Axis §D2): the `cancels` set is the LOCAL-DECLINE signal and its entries are
    // never removed, so poisoning it with a revoked wire id would let a straggler
    // re-announce of that SAME wire id (retry tick racing the cancel; announce and
    // revoke have no cross-stream ordering guarantee) divert into the cancel
    // epilogue and mint a permanent `declined_at` the user never chose — turning a
    // benign sender cancel/supersede/failure into a receiver decline. The in-flight
    // fetch abort is already handled by the ingress pump's SEPARATE
    // `request_revoke_abort` signal, and by the time this fn runs on the serial
    // loop the fetch has been dealt with; every entry in `cancels` must stay
    // decline-originated so the epilogue's `declined_at` stamp is sound.

    // Honest terminal mapping (§D2): (row state, row detail, file state, file
    // outcome, file error, history outcome, reason tag).
    let (row_state, row_detail, file_state, file_outcome, file_error, history_outcome, reason_tag): (
        InboundState,
        String,
        InboundFileState,
        &str,
        Option<&str>,
        &str,
        &str,
    ) = match reason {
        RevokeReason::Cancelled => (
            InboundState::Cancelled,
            super::models::REVOKED_BY_SENDER_DETAIL.to_string(),
            InboundFileState::Done,
            "cancelled",
            None,
            "cancelled",
            "cancelled",
        ),
        RevokeReason::Failed => (
            InboundState::Failed,
            "sender failed".to_string(),
            InboundFileState::Failed,
            "failed",
            Some("sender failed"),
            "failed",
            "failed",
        ),
        RevokeReason::Superseded => {
            // A supersede is issued only when the peer already holds every frame, so
            // the receiver lacks nothing → the honest terminal is `done`, whether or
            // not this attempt landed anything.
            let done_count = {
                let conn = store.lock_conn();
                list_inbound_files(&conn, inbound_id)
                    .map(|rows| count_landed(&rows))
                    .unwrap_or(0)
            };
            let detail = if done_count == 0 {
                "nothing to fetch (superseded by sender)".to_string()
            } else {
                format!("superseded ({done_count} of {} landed)", row.frame_count)
            };
            (
                InboundState::Done,
                detail,
                InboundFileState::Done,
                "superseded",
                None,
                "superseded",
                "superseded",
            )
        }
    };

    // Write the terminal row state, settle the un-settled per-file rows, and record
    // one receiver `sync_history` row per known file — built from the ANNOUNCED file
    // rows (a revoke never fetches a manifest), mirroring the cancel epilogue's
    // per-frame audit. A file that already landed keeps its own verdict (settle
    // leaves `done` rows intact); its history verdict was already written on ingest.
    let landed = {
        let conn = store.lock_conn();
        let file_rows = list_inbound_files(&conn, inbound_id).unwrap_or_default();
        let landed = count_landed(&file_rows);
        if let Err(e) = set_inbound_state(&conn, &wire_id, row_state, Some(&row_detail)) {
            tracing::warn!(package_id = %wire_id, error = %format!("{e:#}"), "revoke terminal state write failed");
        }
        if let Err(e) = settle_unsettled_inbound_files(
            &conn,
            inbound_id,
            file_state,
            Some(file_outcome),
            file_error,
        ) {
            tracing::warn!(package_id = %wire_id, error = %format!("{e:#}"), "revoke file-row settle failed");
        }
        let now = super::now_iso();
        for f in &file_rows {
            let h = HistoryRow {
                frame_uuid: f.frame_uuid.clone(),
                filename: filename_of(&f.rel_path),
                object: None,
                peer_device: peer_device.clone(),
                direction: Direction::Received,
                bytes: f.byte_size,
                started_at: now.clone(),
                finished_at: Some(now.clone()),
                outcome: history_outcome.to_string(),
                project: None,
                package_id: Some(history_key.clone()),
                batch_name: batch_name.clone(),
            };
            if let Err(e) = insert_history_row(&conn, &h) {
                tracing::warn!(package_id = %wire_id, error = %format!("{e:#}"), "revoke history row write failed");
            }
        }
        landed
    };

    // Best-effort staging cleanup + in-flight tag release (the receiver-driven
    // teardown D2 describes — the released blobs stay pullable by root hash until
    // the next GC pass, which is benign).
    let staging = staging_root.join("staging").join(&wire_id);
    if let Err(e) = std::fs::remove_dir_all(&staging) {
        tracing::debug!(error = %e, path = %staging.display(), "revoke staging cleanup skipped");
    }
    if let Err(e) = transport.release(package_id).await {
        tracing::warn!(package_id = %wire_id, error = %format!("{e:#}"), "receiver blob release failed on revoke");
    }

    journal(
        store,
        inbound_id,
        "revoked",
        Some(&format!("reason={reason_tag} {row_detail}")),
    );
    tracing::info!(
        package_id = %wire_id,
        from = %peer_device,
        ?reason,
        state = row_state.as_str(),
        landed,
        "sync receiver applied sender revoke"
    );

    // Live signal (B5 §4): emit a single terminal `sync-finished` so the receiver's
    // Transfers widget dismisses this row now, not on the next 10s poll. The
    // `outcome` is the mapped terminal — `row_state.as_str()` is exactly
    // `cancelled` / `failed` / `done` — and the payload mirrors the other terminal
    // emits (cancel epilogue / ingest). `ok_count` carries the files that landed.
    emit_event(
        emitter,
        "sync-finished",
        &SyncFinishedEvent {
            package_id: wire_id,
            direction: super::Direction::Received,
            outcome: row_state.as_str().to_string(),
            peer_device,
            ok_count: landed as u32,
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        },
    );
}

/// Handle one authorized PROJECT announce (collab exchange, slice 4): resolve the
/// hub package row (refreshing announcements once if unknown), fetch into staging,
/// ingest the contributions, ack the receipts, and emit `sync-progress` /
/// `sync-finished` carrying the `project_id`.
///
/// `hub_package_id` is the event's hub uuid (the `project_packages` row key);
/// `announce.package_id` is the engine-minted wire id used for fetch/ack. The
/// gate + hub-id `validate_package_id` already ran in the loop.
#[allow(clippy::too_many_arguments)]
async fn handle_project_announce(
    store: &Arc<CatalogSyncStore>,
    staging_root: &Path,
    transport: &dyn SharingTransport,
    emitter: &dyn ProgressEmitter,
    announcements_refresher: Option<&super::ProjectAnnouncementsRefresher>,
    on_project_ingested: Option<&super::ProjectIngestedHook>,
    from: NodeId,
    project_id: String,
    hub_package_id: String,
    announce: PackageAnnounce,
) -> Result<()> {
    let peer_device = super::node_id_hex(&from);
    let wire_package_id = announce.package_id.0.clone();

    // Row-key check on the HUB package id: unknown ⇒ ask the refresher to poll
    // the hub once, then re-check. Still unknown ⇒ drop fail-closed (we never
    // fetch a package we can't anchor).
    let known = |store: &Arc<CatalogSyncStore>| -> Result<bool> {
        let conn = store.lock_conn();
        Ok(crate::db::collab_exchange::get_package(&conn, &hub_package_id)?.is_some())
    };
    if !known(store)? {
        if let Some(refresh) = announcements_refresher {
            refresh(&project_id);
        }
        if !known(store)? {
            tracing::warn!(
                from = %peer_device,
                project_id,
                package_id = %hub_package_id,
                "project announce dropped: package row unknown after refresh"
            );
            return Ok(());
        }
    }

    // The WIRE package id builds the staging path — guard it (C1) before the join.
    if let Err(e) = crate::package::validate_package_id(&wire_package_id) {
        tracing::warn!(
            from = %peer_device,
            project_id,
            package_id = %wire_package_id,
            error = %e,
            "project announce rejected: unsafe wire package_id"
        );
        return Ok(());
    }

    emit_event(
        emitter,
        "sync-progress",
        &SyncProgressEvent {
            package_id: hub_package_id.clone(),
            direction: super::Direction::Received,
            stage: "received".to_string(),
            peer_device: peer_device.clone(),
            frame_count: announce.frame_count,
            project_id: Some(project_id.clone()),
            bytes_done: None,
            bytes_total: None,
        },
    );

    // Fetch into a per-package staging dir keyed by the WIRE id (mirrors personal
    // sync — out of the user-visible landing tree).
    let staging = staging_root.join("staging").join(&wire_package_id);
    emit_event(
        emitter,
        "sync-progress",
        &SyncProgressEvent {
            package_id: hub_package_id.clone(),
            direction: super::Direction::Received,
            stage: "fetching".to_string(),
            peer_device: peer_device.clone(),
            frame_count: announce.frame_count,
            project_id: Some(project_id.clone()),
            bytes_done: None,
            bytes_total: None,
        },
    );
    // I2 (T7): relay dial hint for the holder we're about to pull from (relay-only
    // — cross-account safe; the node's hint never carries direct addrs).
    transport.add_peer_dial_hint(from);
    transport
        .fetch(from, &announce, &staging, noop_fetch_sink())
        .await
        .with_context(|| format!("fetch project package {wire_package_id}"))?;

    // Ingest on a blocking thread (file I/O + SQLite).
    emit_event(
        emitter,
        "sync-progress",
        &SyncProgressEvent {
            package_id: hub_package_id.clone(),
            direction: super::Direction::Received,
            stage: "ingesting".to_string(),
            peer_device: peer_device.clone(),
            frame_count: announce.frame_count,
            project_id: Some(project_id.clone()),
            bytes_done: None,
            bytes_total: None,
        },
    );
    let outcome = {
        let store = Arc::clone(store);
        let staging_for_ingest = staging.clone();
        let project_id = project_id.clone();
        let hub_package_id = hub_package_id.clone();
        let peer_device = peer_device.clone();
        tokio::task::spawn_blocking(move || -> Result<super::ProjectIngestOutcome> {
            // Per-frame connection locking (W2 T2.1), same as personal ingest.
            super::project_ingest::ingest_project_package(
                super::ingest::IngestConn::Shared(store.as_ref()),
                &staging_for_ingest,
                &project_id,
                &hub_package_id,
                &peer_device,
            )
        })
        .await
        .context("project ingest join")??
    };

    // Ack the per-frame receipts to the serving peer, keyed by the WIRE id.
    transport
        .ack(from, &announce.package_id, outcome.receipts.clone())
        .await
        .with_context(|| format!("ack project package {wire_package_id}"))?;

    // Terminal: drop the fetched blobs (best-effort, idempotent).
    if let Err(e) = transport.release(&announce.package_id).await {
        tracing::warn!(package_id = %wire_package_id, error = %format!("{e:#}"), "receiver blob release failed");
    }
    // Best-effort staging cleanup.
    if let Err(e) = std::fs::remove_dir_all(&staging) {
        tracing::debug!(error = %e, path = %staging.display(), "project ingest staging cleanup skipped");
    }

    let finished_outcome = if outcome.failed.is_empty() {
        "ingested"
    } else if outcome.ok_count == 0 {
        "failed"
    } else {
        "partial"
    };
    emit_event(
        emitter,
        "sync-finished",
        &SyncFinishedEvent {
            package_id: hub_package_id.clone(),
            direction: super::Direction::Received,
            outcome: finished_outcome.to_string(),
            peer_device,
            ok_count: outcome.ok_count as u32,
            failed: outcome.failed,
            new_count: 0,
            duplicate_count: 0,
            project_id: Some(project_id.clone()),
        },
    );

    // Post-ingest hook (Task 8 wires report-have + notification data; None = no-op).
    if let Some(hook) = on_project_ingested {
        hook(project_id, hub_package_id);
    }
    Ok(())
}

// ── App-lifecycle runtime holder ────────────────────────────────────────────

/// One started-transport bundle held by [`SyncRuntime`]. `transport` is the
/// live receive-side transport — read back by [`SyncRuntime::transport`] so the
/// collab download loop can issue an outbound `request_project` over the SAME
/// endpoint the receiver listens on. `inbound_control` is the cancel signal the
/// running receiver loop watches (Task 12), handed back by
/// [`SyncRuntime::inbound_control`] to the command layer. `_receiver` is a
/// lifetime anchor — kept so the endpoint's event loop lives for the runtime's
/// lifetime, not read directly.
struct Started {
    transport: Arc<dyn SharingTransport>,
    ticket: String,
    inbound_control: Arc<InboundControl>,
    _receiver: SyncReceiverHandle,
}

/// App-lifecycle holder for the receive side. Lives in the host `AppState`
/// (desktop + web) and is reached by the `sync` commands. Cheap to construct;
/// the transport is built lazily on the first
/// [`get_sync_pairing_ticket`](crate::api::sync::get_pairing_ticket) call behind
/// the dev flag.
pub struct SyncRuntime {
    inner: tokio::sync::Mutex<Option<Started>>,
    /// Once-per-process guard + handle for the hourly authorized-peers refresh
    /// timer (task 7). `Some` after the first
    /// [`ensure_peers_refresh_task`](crate::api::sync::ensure_peers_refresh_task);
    /// every later call sees it populated and no-ops.
    pub(crate) peers_refresh_task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    /// Process-wide debounce shared by BOTH receiver gates (per-announce
    /// authorizer + connection connect-gate): refusing an unknown peer kicks a
    /// rate-limited hub refresh of the authorized set (task 7). One instance so a
    /// refusal burst across either gate triggers at most one hub round-trip per
    /// gap.
    pub(crate) refusal: Arc<RefusalRefresher>,
}

impl SyncRuntime {
    /// A fresh, unstarted runtime.
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(None),
            peers_refresh_task: tokio::sync::Mutex::new(None),
            // 5-minute refusal debounce (spec): a machine just added to the
            // account is admitted within one gap of its first refused retry.
            refusal: Arc::new(RefusalRefresher::new(std::time::Duration::from_secs(300))),
        }
    }

    /// Whether the transport has been started (a ticket exists).
    pub async fn is_started(&self) -> bool {
        self.inner.lock().await.is_some()
    }

    /// The current pairing ticket, if started.
    pub async fn ticket(&self) -> Option<String> {
        self.inner.lock().await.as_ref().map(|s| s.ticket.clone())
    }

    /// The live receive-side transport, if started — the collab download loop
    /// issues `request_project` over it (the receiver listens on the same
    /// endpoint, and `request_project` is an outbound send that never touches the
    /// receiver's single-consumer event stream).
    pub async fn transport(&self) -> Option<Arc<dyn SharingTransport>> {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(|s| Arc::clone(&s.transport))
    }

    /// The running receiver's [`InboundControl`], if started (Task 12). The
    /// command layer ([`cancel_incoming_package`](crate::api::sync::cancel_incoming_package))
    /// uses it to request cancellation of an inbound package. `None` before the
    /// transport is started (nothing is being received, so there is nothing to
    /// cancel through the in-memory signal — a persisted `Cancelled` row still
    /// covers the restart case).
    pub async fn inbound_control(&self) -> Option<Arc<InboundControl>> {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(|s| Arc::clone(&s.inbound_control))
    }

    /// Test-only: seed a started runtime around an already-online `transport`
    /// (typically a loopback endpoint whose receiver was spawned separately), so
    /// [`transport`](Self::transport) hands it back to the collab download loop.
    /// Loopback routing needs no dial hint (no shared node is bound in tests).
    #[cfg(test)]
    pub(crate) async fn set_started_for_test(
        &self,
        transport: Arc<dyn SharingTransport>,
        receiver: SyncReceiverHandle,
        ticket: String,
    ) {
        *self.inner.lock().await = Some(Started {
            transport,
            ticket,
            inbound_control: Arc::new(InboundControl::new()),
            _receiver: receiver,
        });
    }

    /// Lazily build the iroh transport under `sync_dir`, spawn one receiver that
    /// ingests into the catalog at `db_path`, and return the pairing ticket.
    /// Idempotent — a second call returns the existing ticket without starting a
    /// second transport.
    ///
    /// The `node` is the process-wide [`SharedIrohNode`] (bound by
    /// [`crate::api::sync::ensure_iroh_node`], which resolves the relay mode once):
    /// the receiver rides it as its `Recv` role handle, sharing the single
    /// endpoint + `<sync>/blobs` store with the personal + collab senders (C1
    /// fix). This method installs the dedup responder + connect gate on that
    /// shared node and spawns one receiver over its `Recv` handle.
    pub async fn ensure_started(
        &self,
        node: Arc<SharedIrohNode>,
        sync_dir: PathBuf,
        db_path: PathBuf,
        incoming: IncomingResolver,
        authorized: PeerAuthorizer,
        hooks: ReceiverHooks,
        emitter: Arc<dyn ProgressEmitter>,
    ) -> Result<String> {
        let mut guard = self.inner.lock().await;
        if let Some(started) = guard.as_ref() {
            return Ok(started.ticket.clone());
        }

        std::fs::create_dir_all(&sync_dir)
            .with_context(|| format!("create sync dir {}", sync_dir.display()))?;
        let store = Arc::new(
            CatalogSyncStore::open(&db_path)
                .with_context(|| format!("open catalog sync store {}", db_path.display()))?,
        );
        // A running receiver answers a peer's pre-Announce dedup handshake from
        // its own catalog: install the responder on the shared node so the
        // control channel routes inbound Offer/FullHashes to it (spec §7, task 4).
        // Until this call the node answers offers want-all — nothing withheld.
        let responder: Arc<dyn crate::sync::DedupResponder> =
            Arc::new(crate::sync::CatalogDedupResponder::new(Arc::clone(&store)));
        node.set_dedup_responder(responder);

        // Install the connection-level authorization gate on the shared node
        // (slice 4): it governs BOTH ALPNs at the node level. Absent ⇒ admit all.
        if let Some(gate) = hooks.connect_gate {
            node.set_connect_gate(gate);
        }

        // The receiver is the node's `Recv` role handle — one endpoint + store
        // shared with the personal/collab senders (C1 fix); role-prefixed blob
        // tags (Д3) keep the roles isolated on the shared store. The collab
        // download loop attaches its dial hints via the node's `add_peer` (it
        // reads the bound node off `ServiceContext`), not through this handle.
        let transport: Arc<dyn SharingTransport> = node.handle(Role::Recv);

        // The receiver-side cancel signal (Task 12): created here, watched by the
        // spawned loop, and stashed on `Started` so `inbound_control()` hands it to
        // the command layer.
        let control = Arc::new(InboundControl::new());

        // Staging lives under the sync dir; the landing root is resolved live per
        // package by the caller-supplied resolver (task 5).
        let (info, receiver) = SyncReceiver::spawn(
            store,
            sync_dir.clone(),
            incoming,
            authorized,
            ProjectReceiveHooks {
                gate: hooks.project_gate,
                announcements_refresher: hooks.announcements_refresher,
                on_project_ingested: hooks.on_project_ingested,
                request_handler: hooks.project_request_handler,
            },
            Arc::clone(&control),
            Arc::clone(&transport),
            emitter,
        )
        .await?;

        tracing::info!(
            ticket_len = info.pairing_ticket.len(),
            "sync runtime started (dev pairing)"
        );
        *guard = Some(Started {
            transport,
            ticket: info.pairing_ticket.clone(),
            inbound_control: control,
            _receiver: receiver,
        });
        Ok(info.pairing_ticket)
    }
}

impl Default for SyncRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `upsert_inbound_announced` is retired from production (`handle_announce` now
    // keys rows by `(peer, batch_uuid)` via `upsert_inbound_attempt`); tests still
    // use it as a convenient inbound-row seeder.
    use super::super::store::upsert_inbound_announced;
    use crate::sharing::loopback::{FaultPlan, LoopbackNetwork};
    use crate::sharing::types::{PackageAnnounce, PackageId};
    use crate::sync::node_id_hex;
    use std::sync::Mutex;

    /// Captures the events the receiver emits so a test can assert the rejection
    /// path fired.
    #[derive(Default)]
    struct RecordingEmitter {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }
    impl ProgressEmitter for RecordingEmitter {
        fn emit_json(&self, name: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((name.to_string(), payload));
        }
    }

    /// C1 regression: an announce carrying a path-shaped `package_id` must be
    /// refused before any fetch, and must never create the attacker-chosen path.
    /// Unfixed, `staging_root.join("staging").join(&package_id)` resolves to the
    /// absolute `evil` path (Path::join replaces the base on an absolute
    /// component) and the fetched package lands there.
    #[tokio::test]
    async fn handle_announce_rejects_unsafe_package_id_without_escaping_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_root = tmp.path().join("stage");
        std::fs::create_dir_all(&staging_root).unwrap();
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let transport = LoopbackNetwork::new().endpoint();
        let incoming_root = tmp.path().join("incoming");
        let incoming: IncomingResolver = Arc::new(move || incoming_root.clone());
        let emitter = Arc::new(RecordingEmitter::default());

        let evil = tmp.path().join("evil_escape");
        let announce = PackageAnnounce {
            package_id: PackageId(evil.to_string_lossy().into_owned()),
            root_hash: "0".repeat(64),
            byte_size: 0,
            frame_count: 1,
        };

        let control = InboundControl::new();
        // The unsafe package_id is rejected before the row upsert, so `batch_uuid`
        // is never consumed here — any value is fine.
        handle_announce(
            &store,
            &staging_root,
            &incoming,
            &transport,
            Arc::clone(&emitter) as Arc<dyn ProgressEmitter>,
            &control,
            [7u8; 32],
            announce,
            None,
            "unsafe-batch".to_string(),
            None,
        )
        .await
        .expect("an unsafe announce is rejected as a clean Ok, not an error");

        assert!(
            !evil.exists(),
            "receiver must not create the attacker-chosen path"
        );
        let events = emitter.events.lock().unwrap();
        let finished = events
            .iter()
            .find(|(n, _)| n == "sync-finished")
            .expect("a finished event is emitted for the rejected package");
        assert_eq!(finished.1["outcome"], "failed");
        assert!(
            !events.iter().any(|(n, _)| n == "sync-progress"),
            "rejection happens before any fetch/ingest progress tick"
        );
    }

    /// Zombie-inbound fix (Part 2), as revised by D2 §4: on startup the receiver
    /// reconciles every non-terminal `sync_inbound` row (announced/fetching/
    /// ingesting) to `waiting` with `"interrupted by restart"` — a fetch cannot
    /// survive a restart, so a row left mid-fetch by a prior process is a stale
    /// attempt that would otherwise show as a perpetual in-progress transfer. It is
    /// `Waiting` rather than `Failed` because the transfer itself is untouched: the
    /// sender is still obliged to deliver it, and the next announce revives this
    /// same row. A `cancelled` row is terminal and left untouched.
    #[tokio::test]
    async fn startup_reconciles_stale_inbound_rows_to_waiting() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_root = tmp.path().join("stage");
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());

        let peer = "aa".repeat(32);
        {
            let conn = store.lock_conn();
            // A stuck `fetching` row (a pre-restart in-flight fetch) …
            upsert_inbound_announced(&conn, &peer, "pkg-fetching", 3, 3000).unwrap();
            set_inbound_state(&conn, "pkg-fetching", InboundState::Fetching, None).unwrap();
            set_inbound_bytes_done(&conn, "pkg-fetching", 1500).unwrap();
            // … and a terminal `cancelled` row that must stay untouched.
            upsert_inbound_announced(&conn, &peer, "pkg-cancelled", 2, 2000).unwrap();
            set_inbound_state(
                &conn,
                "pkg-cancelled",
                InboundState::Cancelled,
                Some("declined"),
            )
            .unwrap();
        }

        // Spawn the receiver: the reconcile runs on this task, before the loop
        // consumes its first event, so it has completed once `spawn` returns.
        let incoming_root = tmp.path().join("incoming");
        let incoming: IncomingResolver = Arc::new(move || incoming_root.clone());
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            staging_root,
            incoming,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            transport,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let conn = store.lock_conn();
        // The stuck fetching row is now parked waiting with the restart reason —
        // non-terminal (no finished_at), and STILL active, because the transfer is
        // still outstanding and the sender's next announce revives this same row.
        let fetching = get_inbound(&conn, "pkg-fetching").unwrap().unwrap();
        assert_eq!(
            fetching.state,
            InboundState::Waiting,
            "a stale fetching row is parked, not failed — the sender still owes it"
        );
        assert_eq!(
            fetching.last_error.as_deref(),
            Some("interrupted by restart")
        );
        assert!(
            fetching.finished_at.is_none(),
            "a waiting row is non-terminal and stamps no finished_at"
        );
        assert!(
            inbound_active(&conn)
                .unwrap()
                .iter()
                .any(|r| r.package_id == "pkg-fetching"),
            "the parked row stays in the active set, visible without an event"
        );

        // The cancelled row is terminal and completely untouched.
        let cancelled = get_inbound(&conn, "pkg-cancelled").unwrap().unwrap();
        assert_eq!(
            cancelled.state,
            InboundState::Cancelled,
            "a cancelled row stays cancelled"
        );
        assert_eq!(
            cancelled.last_error.as_deref(),
            Some("declined"),
            "the cancel reason is preserved"
        );
    }

    /// Transfers smoke №8 (item 4) reconcile repair: a stale non-terminal row that
    /// ALREADY holds a full receipt set under its wire id reached a real terminal
    /// before the restart (its non-terminal `state` is only the residue of a
    /// mid-transfer duplicate announce's upsert reset). The startup reconcile must
    /// stamp the HONEST terminal from the receipts — an all-Ingested set → `Done`,
    /// an all-Cancelled set → `Cancelled` + `declined_at` — instead of a misleading
    /// `"interrupted by restart"`. A row with NO receipts never reached a terminal,
    /// so it takes the fallback (D2 §4: `waiting`, not `failed` — the transfer is
    /// outstanding, not lost). This heals the owner's live stuck row.
    #[tokio::test]
    async fn startup_repairs_fully_receipted_stale_inbound_from_receipt_log() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_root = tmp.path().join("stage");
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());

        let peer = "bb".repeat(32);
        let now = now_iso();
        {
            let conn = store.lock_conn();
            // (1) A stale `fetching` row with a FULL Ingested receipt set → Done.
            let done_id = upsert_inbound_announced(&conn, &peer, "pkg-done", 2, 2000).unwrap();
            set_inbound_state(&conn, "pkg-done", InboundState::Fetching, None).unwrap();
            for uuid in ["d0", "d1"] {
                insert_receipt(
                    &conn,
                    "pkg-done",
                    &FrameReceipt {
                        frame_uuid: uuid.into(),
                        xxh3: "h".into(),
                        outcome: ReceiptOutcome::Ingested,
                    },
                    &now,
                )
                .unwrap();
            }
            // (2) A stale `fetching` row with a FULL Cancelled receipt set → Cancelled
            //     + declined_at (a receiver decline whose epilogue crashed mid-way).
            let _declined_id =
                upsert_inbound_announced(&conn, &peer, "pkg-declined", 2, 2000).unwrap();
            set_inbound_state(&conn, "pkg-declined", InboundState::Fetching, None).unwrap();
            for uuid in ["c0", "c1"] {
                insert_receipt(
                    &conn,
                    "pkg-declined",
                    &FrameReceipt {
                        frame_uuid: uuid.into(),
                        xxh3: "h".into(),
                        outcome: ReceiptOutcome::Cancelled,
                    },
                    &now,
                )
                .unwrap();
            }
            // (3) A stale `fetching` row with NO receipts → genuine zombie → failed.
            upsert_inbound_announced(&conn, &peer, "pkg-zombie", 3, 3000).unwrap();
            set_inbound_state(&conn, "pkg-zombie", InboundState::Fetching, None).unwrap();

            assert!(done_id > 0);
        }

        // Spawn the receiver: the reconcile runs before the loop consumes an event.
        let incoming_root = tmp.path().join("incoming");
        let incoming: IncomingResolver = Arc::new(move || incoming_root.clone());
        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            staging_root,
            incoming,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            transport,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let conn = store.lock_conn();
        // (1) repaired to Done, terminal, NOT a decline.
        let done = get_inbound(&conn, "pkg-done").unwrap().unwrap();
        assert_eq!(
            done.state,
            InboundState::Done,
            "a fully-Ingested stale row repairs to Done, not failed"
        );
        assert!(
            done.finished_at.is_some(),
            "the repaired terminal stamps finished_at"
        );
        assert!(
            done.declined_at.is_none(),
            "a delivered repair never invents a decline"
        );

        // (2) repaired to Cancelled + declined_at.
        let declined = get_inbound(&conn, "pkg-declined").unwrap().unwrap();
        assert_eq!(
            declined.state,
            InboundState::Cancelled,
            "a fully-Cancelled stale row repairs to Cancelled"
        );
        assert!(
            declined.declined_at.is_some(),
            "an all-cancelled repair stamps the decline axis"
        );

        // (3) no receipts → the honest fallback: this attempt never reached a
        // terminal, so the transfer is outstanding (D2 §4), not lost.
        let zombie = get_inbound(&conn, "pkg-zombie").unwrap().unwrap();
        assert_eq!(
            zombie.state,
            InboundState::Waiting,
            "a receiptless stale row is parked, not failed"
        );
        assert_eq!(zombie.last_error.as_deref(), Some("interrupted by restart"));
    }

    /// Poll until the recorded gate-call log reaches `n` entries (or time out).
    async fn wait_for_calls(seen: &Arc<Mutex<Vec<(NodeId, String)>>>, n: usize) {
        for _ in 0..200 {
            if seen.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {n} project-gate call(s)");
    }

    /// Slice-4 receiver gate: an inbound project announce is validated (an unsafe
    /// hub `package_id` is refused BEFORE the gate) and then routed through the
    /// project gate. Task 4 only logs+drops, so the gate call itself — with the
    /// transport-authenticated `from` and the project id — is the observable that
    /// the announce reached the gate; the flippable verdict distinguishes the
    /// dropped-unauthorized path from the accepted (info!) path.
    #[tokio::test]
    async fn project_announce_is_validated_then_routed_through_the_project_gate() {
        use crate::sharing::SharingTransport;
        use std::sync::atomic::{AtomicBool, Ordering};

        let tmp = tempfile::tempdir().unwrap();
        let net = LoopbackNetwork::new();
        let sender = net.endpoint();
        let receiver_ep = net.endpoint();
        let sender_node = sender.node_id();
        let receiver_node = receiver_ep.node_id();

        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let staging = tmp.path().join("stage");
        let incoming_root = tmp.path().join("incoming");
        let incoming: IncomingResolver = Arc::new(move || incoming_root.clone());

        // Recording gate: logs every (from, project_id) it is asked about and
        // answers from a flippable verdict.
        let seen: Arc<Mutex<Vec<(NodeId, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let verdict = Arc::new(AtomicBool::new(false));
        let project_gate: ProjectAnnounceGate = {
            let seen = Arc::clone(&seen);
            let verdict = Arc::clone(&verdict);
            Arc::new(move |from: &NodeId, project_id: &str| {
                seen.lock().unwrap().push((*from, project_id.to_string()));
                verdict.load(Ordering::SeqCst)
            })
        };

        let (_info, _handle) = SyncReceiver::spawn(
            store,
            staging,
            incoming,
            allow_all_peers(),
            ProjectReceiveHooks {
                gate: Some(project_gate),
                ..Default::default()
            },
            Arc::new(InboundControl::new()),
            Arc::new(receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .expect("spawn receiver");

        let announce = PackageAnnounce {
            package_id: PackageId("wire-pkg".into()),
            root_hash: "0".repeat(64),
            byte_size: 0,
            frame_count: 1,
        };

        // (1) Unsafe hub package_id: refused before the gate is ever consulted.
        sender
            .announce_project(receiver_node, "proj-1", "../evil", &announce)
            .await
            .expect("deliver unsafe project announce");
        // (2) Safe hub package_id, verdict=false: reaches the gate, then dropped
        //     as unauthorized — but the gate WAS consulted with the real sender.
        sender
            .announce_project(receiver_node, "proj-1", "hub-pkg-1", &announce)
            .await
            .expect("deliver unauthorized project announce");

        wait_for_calls(&seen, 1).await;
        {
            let s = seen.lock().unwrap();
            assert_eq!(
                s.len(),
                1,
                "the unsafe package_id never reached the gate; the safe one did"
            );
            assert_eq!(s[0], (sender_node, "proj-1".to_string()));
        }

        // (3) Authorize: a safe announce now passes the gate (reaches the info!
        //     path). Same authenticated sender, a different project id.
        verdict.store(true, Ordering::SeqCst);
        sender
            .announce_project(receiver_node, "proj-2", "hub-pkg-2", &announce)
            .await
            .expect("deliver authorized project announce");
        wait_for_calls(&seen, 2).await;
        {
            let s = seen.lock().unwrap();
            assert_eq!(s.len(), 2);
            assert_eq!(s[1], (sender_node, "proj-2".to_string()));
        }
    }

    /// Build a one-frame fixture package (real 4x4 FITS payload + a manifest with
    /// a full `Frame` snapshot) under `root`; returns `(pkg_dir, announce)`. A
    /// self-contained copy of the ingest-test fixtures so the receiver test can
    /// drive a real fetch → ingest end to end.
    fn build_inbound_fixture(root: &std::path::Path) -> (std::path::PathBuf, PackageAnnounce) {
        use crate::models::{Frame, ImageType};
        use crate::package::{ManifestRecord, PayloadKind, MANIFEST_VERSION};

        let src_dir = root.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("L_0001.fits");
        crate::fits_writer::write_fits_f32(&src, 4, 4, 1, &[0.25f32; 16], &[]).unwrap();

        let byte_size = std::fs::metadata(&src).unwrap().len();
        let xxh3 = crate::package::xxh3_full_file(&src).unwrap();
        let frame = Frame {
            object: Some("M31".to_string()),
            date_obs: Some("2026-01-15T22:30:00Z".parse().unwrap()),
            instrume: Some("ASI2600MM".to_string()),
            imagetyp: Some(ImageType::Light),
            naxis1: Some(4),
            naxis2: Some(4),
            uuid: Some("frame-inbound-track".to_string()),
            updated_at: Some("2026-01-16T10:00:00.000Z".to_string()),
            ..Default::default()
        };
        let record = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: "frame-inbound-track".to_string(),
            origin_catalog_uuid: "catalog-uuid".to_string(),
            origin_device: "aa".repeat(32),
            payload_kind: PayloadKind::RawFrame,
            rel_path: "L_0001.fits".to_string(),
            byte_size,
            xxh3,
            frame_meta: serde_json::to_value(&frame).unwrap(),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        };
        let pkg_dir = root.join("pkg-inbound-track");
        let announce = crate::package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
        (pkg_dir, announce)
    }

    /// Task 11 (Step 4): a full loopback receive walks the `sync_inbound` row
    /// `Announced → Fetching → Ingesting → Done`. Asserts the final persisted row
    /// is `Done` with a stamped `finished_at`, `bytes_done == byte_size`, and no
    /// longer active; and that the recorded stage events include a `fetching`
    /// `sync-progress` carrying bytes plus at least one `sync-file-progress`.
    #[tokio::test]
    async fn inbound_row_tracks_announce_to_done() {
        use crate::sharing::SharingTransport;
        use crate::sync::store::inbound_active;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        // Full catalog schema (files/frames/fits_header + sync tables) so ingest
        // lands rows; the held connection keeps the DB file around.
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            Arc::new(move || incoming.clone()) as IncomingResolver,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce) = build_inbound_fixture(tmp.path());
        assert!(announce.byte_size > 0, "fixture package has non-zero bytes");
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce, "", "", &[])
            .await
            .unwrap();

        // The terminal Done write lands just after the receiver acks — poll for it.
        let mut final_row = None;
        for _ in 0..400 {
            let row = {
                let conn = store.lock_conn();
                get_inbound(&conn, &announce.package_id.0).unwrap()
            };
            if let Some(r) = row {
                if r.state == InboundState::Done {
                    final_row = Some(r);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let row = final_row.expect("inbound row reached Done");
        assert!(row.finished_at.is_some(), "a Done row stamps finished_at");
        assert_eq!(row.frame_count, announce.frame_count);
        assert_eq!(row.byte_size, announce.byte_size);
        assert_eq!(
            row.bytes_done, announce.byte_size,
            "the final batch tick persisted the full byte_size"
        );
        let active_empty = {
            let conn = store.lock_conn();
            inbound_active(&conn).unwrap().is_empty()
        };
        assert!(active_empty, "a Done row drops out of the active set");

        // Recorded events: a fetching tick carrying bytes + ≥1 per-file tick.
        let events = recorder.events.lock().unwrap();
        let fetch_with_bytes = events.iter().any(|(n, v)| {
            n == "sync-progress"
                && v["stage"] == "fetching"
                && v.get("bytesDone").map(|b| !b.is_null()).unwrap_or(false)
        });
        assert!(
            fetch_with_bytes,
            "a fetching progress tick carried bytes: {events:?}"
        );
        let file_ticks = events
            .iter()
            .filter(|(n, _)| n == "sync-file-progress")
            .count();
        assert!(
            file_ticks >= 1,
            "at least one sync-file-progress event: {events:?}"
        );
    }

    /// Perseus UI v2 (Task 9): when the announcing peer is in the cached
    /// capability map, the receiver stamps that capability onto the inbound row at
    /// announce time. Seeds `SYNC_PEER_CAPABILITIES` with `sender_hex → "perseus"`,
    /// drives a full announce, and asserts the persisted row carries
    /// `peer_capability == Some("perseus")`.
    #[tokio::test]
    async fn announce_stamps_peer_capability_from_cache() {
        use crate::sharing::SharingTransport;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        // Seed the capability cache for the SENDING node (the `from` the receiver
        // stamps). Written through the store's own connection so the receiver reads
        // exactly what we wrote.
        let sender_hex = node_id_hex(&sender.node_id());
        {
            let conn = store.lock_conn();
            let map: std::collections::HashMap<String, String> =
                [(sender_hex.clone(), "perseus".to_string())]
                    .into_iter()
                    .collect();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::SYNC_PEER_CAPABILITIES,
                &serde_json::to_string(&map).unwrap(),
            )
            .unwrap();
        }

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            Arc::new(move || incoming.clone()) as IncomingResolver,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce) = build_inbound_fixture(tmp.path());
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce, "", "", &[])
            .await
            .unwrap();

        let mut final_row = None;
        for _ in 0..400 {
            let row = {
                let conn = store.lock_conn();
                get_inbound(&conn, &announce.package_id.0).unwrap()
            };
            if let Some(r) = row {
                if r.state == InboundState::Done {
                    final_row = Some(r);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let row = final_row.expect("inbound row reached Done");
        assert_eq!(
            row.peer_capability.as_deref(),
            Some("perseus"),
            "the announcing peer's cached capability is stamped onto the row",
        );
    }

    /// Perseus UI v2 (Task 9): with NO capability cache seeded, the stamp is a
    /// silent no-op (`peer_capability` stays NULL) and the transfer still lands
    /// end-to-end — proving the stamp is strictly best-effort/informational.
    #[tokio::test]
    async fn announce_without_capability_cache_leaves_null_and_still_lands() {
        use crate::sharing::SharingTransport;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            Arc::new(move || incoming.clone()) as IncomingResolver,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce) = build_inbound_fixture(tmp.path());
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce, "", "", &[])
            .await
            .unwrap();

        let mut final_row = None;
        for _ in 0..400 {
            let row = {
                let conn = store.lock_conn();
                get_inbound(&conn, &announce.package_id.0).unwrap()
            };
            if let Some(r) = row {
                if r.state == InboundState::Done {
                    final_row = Some(r);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let row = final_row.expect("inbound row reached Done even with no capability cache");
        assert_eq!(row.peer_capability, None, "no cache ⇒ no capability stamp");
    }

    /// Reviewer finding (Task 11 follow-up): an ingest failure AFTER the row is
    /// stamped `Ingesting` must not leave it stuck non-terminal. Forced by
    /// deleting `manifest.ndjson` from the served package dir before announcing
    /// — the loopback `fetch` still succeeds (it just copies whatever files are
    /// present), but `ingest_package`'s `read_manifest` then fails, which is
    /// exactly the `.context("ingest join")??` early-return site at issue.
    /// Asserts the persisted row ends `Failed` with `last_error` set and is
    /// absent from `inbound_active`.
    #[tokio::test]
    async fn inbound_row_stamps_failed_on_ingest_error() {
        use crate::sharing::SharingTransport;
        use crate::sync::store::inbound_active;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            Arc::new(move || incoming.clone()) as IncomingResolver,
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce) = build_inbound_fixture(tmp.path());
        // Remove the manifest AFTER `write_package` built it: the fetch (a plain
        // filesystem copy in the loopback mock) still succeeds and lands the
        // payload file into staging, but `ingest_package`'s `read_manifest` call
        // on that staging dir then fails with a real "file not found" error —
        // triggering the ingest-error early return, not the (already-covered)
        // per-frame-rejected "all frames rejected" path.
        std::fs::remove_file(pkg_dir.join(crate::package::MANIFEST_FILENAME)).unwrap();
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce, "", "", &[])
            .await
            .unwrap();

        // The Failed write lands once the ingest join error propagates — poll for it.
        let mut final_row = None;
        for _ in 0..400 {
            let row = {
                let conn = store.lock_conn();
                get_inbound(&conn, &announce.package_id.0).unwrap()
            };
            if let Some(r) = row {
                if r.state == InboundState::Failed {
                    final_row = Some(r);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let row = final_row.expect("inbound row reached Failed after the ingest error");
        assert!(row.finished_at.is_some(), "a Failed row stamps finished_at");
        assert!(
            row.last_error.is_some(),
            "a Failed row from an ingest error carries a reason"
        );
        let active_empty = {
            let conn = store.lock_conn();
            inbound_active(&conn).unwrap().is_empty()
        };
        assert!(
            active_empty,
            "a Failed row drops out of the active set — never stuck non-terminal"
        );
        // D2 §3.2: a terminal must announce itself. This path emitted nothing
        // before, so the row left the Active list with nothing to carry it into the
        // terminal list — the same vanishing row the fetch path had.
        wait_for_finished(&recorder, 1).await;
        let ev = finished_events(&recorder)
            .pop()
            .expect("a terminal finished event");
        assert_eq!(
            ev["outcome"], "failed",
            "the ingest error is announced: {ev}"
        );
        assert_eq!(ev["direction"], "received", "receive-side event: {ev}");
    }

    /// D2 §3.2: an ack failure stays terminal even though a dead connection is what
    /// caused it — the ONE place where that is true, so it is pinned explicitly.
    /// Every frame is landed and catalogued by this point, so nothing is
    /// outstanding on our side; only the verdict is undelivered, and the ack-replay
    /// guard hands it back whole on the sender's next announce. Treating this as
    /// `Waiting` would park a transfer that has, from our side, already happened.
    #[tokio::test]
    async fn an_ack_failure_stays_failed_and_emits_its_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce, files) = build_v2_fixture(tmp.path());
        let wire = announce.package_id.0.clone();
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        // Fetch and ingest both succeed; only the receipt hand-back fails.
        receiver_ep.set_fault(FaultPlan {
            fail_ack_once: true,
            ..Default::default()
        });
        sender
            .announce(
                receiver_node,
                &announce,
                "M31 Lights",
                "batch-ack-failure",
                &files,
            )
            .await
            .unwrap();

        let row = poll_inbound(&store, &wire, InboundState::Failed).await;
        assert_eq!(
            row.state,
            InboundState::Failed,
            "the receive happened; only the verdict did not"
        );
        assert!(row.finished_at.is_some(), "a terminal stamps finished_at");

        wait_for_finished(&recorder, 1).await;
        let ev = finished_events(&recorder)
            .pop()
            .expect("a terminal finished event");
        assert_eq!(
            ev["outcome"], "failed",
            "the ack failure is announced: {ev}"
        );
        assert_eq!(
            ev["packageId"], wire,
            "keyed on the attempt's wire id: {ev}"
        );
    }

    // ── Task 12: receiver-side cancel ───────────────────────────────────────

    /// Drain a peer endpoint's event stream until the next `AckReceived`,
    /// returning its `(package_id, receipts)`. Times out with a panic.
    async fn recv_ack(
        events: &mut tokio::sync::mpsc::Receiver<TransportEvent>,
    ) -> (PackageId, Vec<FrameReceipt>) {
        for _ in 0..400 {
            match tokio::time::timeout(std::time::Duration::from_millis(20), events.recv()).await {
                Ok(Some(TransportEvent::AckReceived {
                    package_id,
                    receipts,
                    ..
                })) => return (package_id, receipts),
                Ok(Some(_)) => continue,
                Ok(None) => panic!("sender event stream closed before an ack arrived"),
                Err(_) => continue, // timeout tick; keep polling
            }
        }
        panic!("timed out waiting for an ack");
    }

    /// Poll the inbound row for `package_id` until it reaches `want` (or panic).
    async fn poll_inbound(
        store: &Arc<CatalogSyncStore>,
        package_id: &str,
        want: InboundState,
    ) -> crate::sync::models::InboundRow {
        for _ in 0..400 {
            let row = {
                let conn = store.lock_conn();
                get_inbound(&conn, package_id).unwrap()
            };
            if let Some(r) = row {
                if r.state == want {
                    return r;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("inbound row for {package_id} never reached {want:?}");
    }

    /// Poll until `n` `cancelled` receipts exist under `package_id` (the re-ack /
    /// seed proof), panicking on timeout with the count actually seen.
    async fn poll_cancelled_receipts(store: &Arc<CatalogSyncStore>, package_id: &str, n: i64) {
        let sql =
            format!("SELECT COUNT(*) FROM sync_receipts WHERE package_id='{package_id}' AND outcome='cancelled'");
        for _ in 0..400 {
            if count_scalar(store, &sql) == n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!(
            "never saw {n} cancelled receipts under {package_id} (got {})",
            count_scalar(store, &sql)
        );
    }

    /// True when no regular files exist under `incoming` (a cancelled package
    /// lands nothing — only the manifest is fetched, into the sync staging dir).
    fn no_files_landed(incoming: &std::path::Path) -> bool {
        if !incoming.exists() {
            return true;
        }
        !walkdir::WalkDir::new(incoming)
            .into_iter()
            .filter_map(|e| e.ok())
            .any(|e| e.file_type().is_file())
    }

    /// Every recorded `sync-finished` payload, in order.
    fn finished_events(recorder: &RecordingEmitter) -> Vec<serde_json::Value> {
        recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == "sync-finished")
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// Poll until at least `n` `sync-finished` events have been recorded.
    async fn wait_for_finished(recorder: &RecordingEmitter, n: usize) {
        for _ in 0..400 {
            if finished_events(recorder).len() >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {n} sync-finished event(s)");
    }

    /// Step 1 (TDD): a package the receiver was told to cancel BEFORE its announce
    /// (`InboundControl::request_cancel`) must, on that announce, ack every frame
    /// `Cancelled` WITHOUT fetching the payload, land no files, and leave the
    /// inbound row `Cancelled`. A SECOND announce replays the cancel from the
    /// receipt log without re-fetching — after the honest-event fix (Transfers
    /// smoke №8, item 2) the replay's `okCount` is 0 (a decline accepted no
    /// frames), so replay-vs-epilogue is discriminated instead by the received
    /// cancelled `sync_history` count staying EXACTLY `frame_count` (a second
    /// epilogue would have duplicated the history rows). The outcome is never
    /// "ingested" (mandatory carry-over item 1).
    #[tokio::test]
    async fn cancel_before_fetch_acks_cancelled_and_replays() {
        use crate::sharing::SharingTransport;
        use crate::sync::store::inbound_active;

        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();
        // Hold the sender's inbound event stream to capture the receiver's acks.
        let mut sender_events = sender.events().await;

        let control = Arc::new(InboundControl::new());
        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::clone(&control),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce) = build_inbound_fixture(tmp.path());
        sender.serve(&announce, &pkg_dir, None).await.unwrap();

        // Cancel BEFORE the announce, then announce.
        control.request_cancel(&announce.package_id.0);
        sender
            .announce(receiver_node, &announce, "", "", &[])
            .await
            .unwrap();

        // (b) The sender observes an all-Cancelled ack, one receipt per frame.
        let (ack_pkg, ack_receipts) = recv_ack(&mut sender_events).await;
        assert_eq!(ack_pkg.0, announce.package_id.0);
        assert_eq!(
            ack_receipts.len(),
            announce.frame_count as usize,
            "a cancel ack carries a receipt per manifest frame"
        );
        assert!(
            ack_receipts
                .iter()
                .all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled)),
            "every receipt in a cancel ack is Cancelled"
        );

        // (c) The inbound row reaches the terminal Cancelled state.
        let row = poll_inbound(&store, &announce.package_id.0, InboundState::Cancelled).await;
        assert!(
            row.finished_at.is_some(),
            "a Cancelled row stamps finished_at"
        );
        let active_empty = {
            let conn = store.lock_conn();
            inbound_active(&conn).unwrap().is_empty()
        };
        assert!(active_empty, "a Cancelled row drops out of the active set");

        // (a) No payload files landed under the incoming root.
        assert!(
            no_files_landed(&incoming),
            "cancel lands no payload files under {incoming:?}"
        );

        // The first (epilogue) finished event: outcome "cancelled", ok_count 0.
        wait_for_finished(&recorder, 1).await;
        let first = finished_events(&recorder);
        assert_eq!(
            first[0]["outcome"], "cancelled",
            "the epilogue emits a cancelled outcome"
        );
        assert_eq!(
            first[0]["okCount"].as_u64().unwrap(),
            0,
            "the epilogue accepts no frames"
        );
        // The epilogue wrote one cancelled received-history row per frame; a second
        // epilogue on the re-announce would DOUBLE this — the replay must not.
        let cancelled_history_after_first = count_scalar(
            &store,
            "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled'",
        );
        assert_eq!(
            cancelled_history_after_first, announce.frame_count as i64,
            "the epilogue wrote one cancelled history row per frame"
        );

        // (d) A second announce replays the cancel from the receipt log WITHOUT
        //     re-fetching. Post honest-event fix the replay emits okCount == 0 (a
        //     decline accepted no frames); the replay-vs-epilogue discriminator is
        //     now that the cancelled history count is UNCHANGED (a second epilogue
        //     would have duplicated the rows).
        sender
            .announce(receiver_node, &announce, "", "", &[])
            .await
            .unwrap();
        let (_pkg2, ack2) = recv_ack(&mut sender_events).await;
        assert!(
            ack2.iter()
                .all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled)),
            "the replayed ack is still all-Cancelled"
        );
        wait_for_finished(&recorder, 2).await;
        let all_finished = finished_events(&recorder);
        let second = all_finished.last().unwrap();
        assert_eq!(
            second["outcome"], "cancelled",
            "a replayed all-cancelled package is never labelled ingested (carry-over item 1)"
        );
        assert_eq!(
            second["okCount"].as_u64().unwrap(),
            0,
            "an all-cancelled replay reports zero arrivals (item 2 — no false arrival toast)"
        );
        assert_eq!(
            count_scalar(
                &store,
                "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled'",
            ),
            cancelled_history_after_first,
            "the replay guard (not a second epilogue) handled the re-announce — history not duplicated"
        );

        // Still no files, still Cancelled.
        assert!(
            no_files_landed(&incoming),
            "the replay lands no files either"
        );
        let row2 = {
            let conn = store.lock_conn();
            get_inbound(&conn, &announce.package_id.0).unwrap().unwrap()
        };
        assert_eq!(
            row2.state,
            InboundState::Cancelled,
            "the row stays Cancelled across the replay"
        );
    }

    // ── Transfers Status Model v2 (Task 5): manifest-at-announce, batch landing,
    //    per-file settle, cancel history, restart reconcile, journal ────────────

    /// One `AnnounceFileEntry` for a manifest test.
    fn afe(rel: &str, uuid: &str, size: u64) -> AnnounceFileEntry {
        AnnounceFileEntry {
            rel_path: rel.to_string(),
            byte_size: size,
            frame_uuid: uuid.to_string(),
        }
    }

    /// A store with one announced inbound row carrying `files`, plus the live fetch
    /// sink that row's fetch would use. Lets the D2 §3.4 per-file transition rule be
    /// driven tick by tick — end-to-end it is unobservable, because ingest
    /// overwrites every file row moments after the fetch returns.
    fn fetch_sink_fixture(
        tmp: &tempfile::TempDir,
        files: &[AnnounceFileEntry],
    ) -> (Arc<CatalogSyncStore>, i64, FetchSink) {
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let id = {
            let conn = store.lock_conn();
            let id = upsert_inbound_announced(
                &conn,
                &"aa".repeat(32),
                "wire-1",
                files.len() as u32,
                100,
            )
            .unwrap();
            record_inbound_manifest(&conn, id, Some("Sink Batch"), files).unwrap();
            id
        };
        let emitter: Arc<dyn ProgressEmitter> = Arc::new(RecordingEmitter::default());
        let sink = build_fetch_sink(
            &store,
            &emitter,
            "wire-1".to_string(),
            "peer".to_string(),
            files.len() as u32,
            id,
            true,
        );
        (store, id, sink)
    }

    fn file_state(store: &Arc<CatalogSyncStore>, id: i64, rel: &str) -> InboundFileRow {
        let conn = store.lock_conn();
        list_inbound_files(&conn, id)
            .unwrap()
            .into_iter()
            .find(|r| r.rel_path == rel)
            .unwrap_or_else(|| panic!("no file row for {rel}"))
    }

    /// REGRESSION (owner smoke 2026-07-25): the sink must take completion from the
    /// producer's `complete` flag and NEVER re-derive it from the byte figures.
    ///
    /// The real transport observes a blob's bitfield, and a blob whose download has
    /// not begun has an EMPTY one — `size() == 0`, `total_bytes() == 0`. So the
    /// first tick of EVERY file in a batch reads (0, 0), and a
    /// `bytes_done >= bytes_total` test calls all of them finished before a single
    /// byte arrives: the counter jumped to the full figure at once and then walked
    /// backwards as files actually started. The loopback mock hid this by helpfully
    /// supplying the true size on its start tick; it no longer does.
    #[test]
    fn an_unstarted_file_reporting_zero_of_zero_is_not_fetched() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![afe("a.fits", "u1", 500), afe("b.fits", "u2", 500)];
        let (store, id, sink) = fetch_sink_fixture(&tmp, &files);

        // What iroh emits first for a blob it has not begun: size unknown.
        for name in ["a.fits", "b.fits"] {
            sink(FetchEvent::File {
                name: name.to_string(),
                bytes_done: 0,
                bytes_total: 0,
                complete: false,
            });
        }

        for name in ["a.fits", "b.fits"] {
            assert_ne!(
                file_state(&store, id, name).state,
                InboundFileState::Fetched,
                "{name}: an unstarted file must not count as received"
            );
        }

        // …and only the one that genuinely finishes is counted.
        sink(FetchEvent::File {
            name: "a.fits".to_string(),
            bytes_done: 500,
            bytes_total: 500,
            complete: true,
        });
        assert_eq!(
            file_state(&store, id, "a.fits").state,
            InboundFileState::Fetched
        );
        assert_ne!(
            file_state(&store, id, "b.fits").state,
            InboundFileState::Fetched,
            "b never completed"
        );
    }

    /// D2 §3.4: a file whose FIRST tick is already its terminal one — resumed,
    /// already-present, or empty — must still reach `fetched`. The pre-D2 sink
    /// keyed the write on which arm ran, so its completion arm could never fire for
    /// these; they sat `fetching` forever and were never counted.
    #[test]
    fn a_file_that_completes_in_one_tick_still_reaches_fetched() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![
            afe("sub/frame1.fits", "u1", 500),
            afe("sub/empty.txt", "u2", 0),
        ];
        let (store, id, sink) = fetch_sink_fixture(&tmp, &files);

        // One and only one tick each, both already complete. The empty file is the
        // case the byte figures CANNOT distinguish from an unstarted one — only
        // `complete` separates them.
        sink(FetchEvent::File {
            name: "sub/frame1.fits".to_string(),
            bytes_done: 500,
            bytes_total: 500,
            complete: true,
        });
        sink(FetchEvent::File {
            name: "sub/empty.txt".to_string(),
            bytes_done: 0,
            bytes_total: 0,
            complete: true,
        });

        assert_eq!(
            file_state(&store, id, "sub/frame1.fits").state,
            InboundFileState::Fetched,
            "a file complete on its first tick is fetched, not stuck fetching"
        );
        assert_eq!(
            file_state(&store, id, "sub/empty.txt").state,
            InboundFileState::Fetched,
            "an empty file reports (0, 0) exactly like an unstarted one — `complete` is what tells them apart"
        );
    }

    /// And the ordinary path still walks both rungs, writing once per transition
    /// rather than once per tick.
    #[test]
    fn a_file_walks_fetching_then_fetched() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![afe("a.fits", "u1", 500)];
        let (store, id, sink) = fetch_sink_fixture(&tmp, &files);

        sink(FetchEvent::File {
            name: "a.fits".to_string(),
            bytes_done: 100,
            bytes_total: 500,
            complete: false,
        });
        assert_eq!(
            file_state(&store, id, "a.fits").state,
            InboundFileState::Fetching
        );

        // A second partial tick changes nothing — same state, no rewrite.
        sink(FetchEvent::File {
            name: "a.fits".to_string(),
            bytes_done: 300,
            bytes_total: 500,
            complete: false,
        });
        let mid = file_state(&store, id, "a.fits");
        assert_eq!(mid.state, InboundFileState::Fetching);
        assert_eq!(
            mid.bytes_done, 100,
            "a same-state tick is not written — checkpoints ride transitions, not bytes"
        );

        sink(FetchEvent::File {
            name: "a.fits".to_string(),
            bytes_done: 500,
            bytes_total: 500,
            complete: true,
        });
        let done = file_state(&store, id, "a.fits");
        assert_eq!(done.state, InboundFileState::Fetched);
        assert_eq!(
            done.bytes_done, 500,
            "the terminal rung checkpoints full bytes"
        );
    }

    /// Build a TWO-file v2 fixture package (one flat, one nested `rel_path`) with
    /// real distinct FITS payloads + a full manifest; returns
    /// `(pkg_dir, announce, files)` where `files` is the announce manifest the
    /// loopback delivers as the v2 extras.
    fn build_v2_fixture(
        root: &std::path::Path,
    ) -> (std::path::PathBuf, PackageAnnounce, Vec<AnnounceFileEntry>) {
        use crate::models::{Frame, ImageType};
        use crate::package::{ManifestRecord, PayloadKind, MANIFEST_VERSION};

        let src_dir = root.join("v2src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // (rel_path in the package, frame uuid, pixel value → distinct hash)
        let specs = [
            ("L_0001.fits", "frame-v2-a", 0.25f32),
            ("camera_ASI/lights/L_0002.fits", "frame-v2-b", 0.5f32),
        ];
        let mut entries = Vec::new();
        let mut items = Vec::new();
        for (rel, uuid, val) in specs {
            let src = src_dir.join(uuid); // unique flat source name
            crate::fits_writer::write_fits_f32(&src, 4, 4, 1, &[val; 16], &[]).unwrap();
            let byte_size = std::fs::metadata(&src).unwrap().len();
            let xxh3 = crate::package::xxh3_full_file(&src).unwrap();
            let frame = Frame {
                object: Some("M31".to_string()),
                imagetyp: Some(ImageType::Light),
                naxis1: Some(4),
                naxis2: Some(4),
                uuid: Some(uuid.to_string()),
                updated_at: Some("2026-01-16T10:00:00.000Z".to_string()),
                ..Default::default()
            };
            let record = ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: uuid.to_string(),
                origin_catalog_uuid: "catalog-uuid".to_string(),
                origin_device: "aa".repeat(32),
                payload_kind: PayloadKind::RawFrame,
                rel_path: rel.to_string(),
                byte_size,
                xxh3,
                frame_meta: serde_json::to_value(&frame).unwrap(),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            };
            entries.push(afe(rel, uuid, byte_size));
            items.push((src, record));
        }
        let pkg_dir = root.join("pkg-v2");
        let announce = crate::package::write_package(&pkg_dir, items).unwrap();
        (pkg_dir, announce, entries)
    }

    /// Item 1: a v2 manifest is recorded onto the inbound row at announce time —
    /// the `display_name` plus one `announced`/`bytes_done 0` per-file row per
    /// entry, ordered by rel_path, BEFORE any fetch.
    #[test]
    fn record_inbound_manifest_writes_name_and_announced_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap();
        let conn = store.lock_conn();
        let peer = "aa".repeat(32);
        let id = upsert_inbound_announced(&conn, &peer, "pkg-1", 2, 100).unwrap();

        let files = vec![afe("b/L2.fits", "u2", 40), afe("L1.fits", "u1", 60)];
        record_inbound_manifest(&conn, id, Some("M31 Lights"), &files).unwrap();

        assert_eq!(
            get_inbound(&conn, "pkg-1")
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("M31 Lights")
        );
        let rows = list_inbound_files(&conn, id).unwrap();
        assert_eq!(rows.len(), 2, "one row per manifest entry");
        assert_eq!(rows[0].rel_path, "L1.fits", "rows ordered by rel_path");
        assert!(
            rows.iter().all(|r| r.state == InboundFileState::Announced
                && r.bytes_done == 0
                && r.outcome.is_none()
                && r.error.is_none()),
            "every row is announced with no progress/verdict before any fetch"
        );
    }

    /// Item 1: a re-announce replaces the whole per-file set and updates the name.
    #[test]
    fn record_inbound_manifest_refresh_replaces_rows_and_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap();
        let conn = store.lock_conn();
        let peer = "aa".repeat(32);
        let id = upsert_inbound_announced(&conn, &peer, "pkg-1", 1, 10).unwrap();

        record_inbound_manifest(&conn, id, Some("A"), &[afe("x.fits", "ux", 10)]).unwrap();
        record_inbound_manifest(
            &conn,
            id,
            Some("B"),
            &[afe("y.fits", "uy", 20), afe("z.fits", "uz", 30)],
        )
        .unwrap();

        assert_eq!(
            get_inbound(&conn, "pkg-1")
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("B")
        );
        let rels: Vec<String> = list_inbound_files(&conn, id)
            .unwrap()
            .into_iter()
            .map(|r| r.rel_path)
            .collect();
        assert_eq!(
            rels,
            vec!["y.fits".to_string(), "z.fits".to_string()],
            "the old set is fully replaced"
        );
    }

    /// Item 2: the landing-dir collision rule — an active same-name batch forces a
    /// `_2` suffix; a terminal prior batch's dir is reused; a persisted dir is
    /// reused verbatim on resume.
    #[test]
    fn resolve_landing_dir_suffixes_active_collision_reuses_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap();
        let conn = store.lock_conn();
        let peer = "bb".repeat(32);
        let incoming = tmp.path().join("incoming");

        let id1 = upsert_inbound_announced(&conn, &peer, "p1", 1, 10).unwrap();
        let d1 = resolve_landing_dir(&conn, id1, &incoming, &peer, "M31");
        assert!(d1.to_string_lossy().ends_with("M31"));

        // id1 is still `announced` (active) → id2 with the same name is suffixed.
        let id2 = upsert_inbound_announced(&conn, &peer, "p2", 1, 10).unwrap();
        let d2 = resolve_landing_dir(&conn, id2, &incoming, &peer, "M31");
        assert_ne!(d1, d2);
        assert!(
            d2.to_string_lossy().ends_with("M31_2"),
            "an active collision suffixes _2: {d2:?}"
        );

        // Retire id1 → its dir is now free; id3 reuses it (merge repeat sends).
        set_inbound_state(&conn, "p1", InboundState::Done, None).unwrap();
        let id3 = upsert_inbound_announced(&conn, &peer, "p3", 1, 10).unwrap();
        let d3 = resolve_landing_dir(&conn, id3, &incoming, &peer, "M31");
        assert_eq!(d3, d1, "a terminal prior batch's dir is reused");

        // Resume: re-resolving id2 returns its persisted dir unchanged.
        assert_eq!(
            resolve_landing_dir(&conn, id2, &incoming, &peer, "M31"),
            d2,
            "persisted landing_dir reused on resume"
        );
    }

    /// Item 2: the batch-slug sanitizer trims dot-only names to absent (v1-style).
    #[test]
    fn sanitize_batch_slug_handles_spaces_dots_and_blanks() {
        assert_eq!(
            sanitize_batch_slug("M31 Lights").as_deref(),
            Some("M31_Lights")
        );
        assert_eq!(sanitize_batch_slug("   "), None);
        assert_eq!(sanitize_batch_slug(".."), None);
        assert_eq!(
            sanitize_batch_slug("My.Object").as_deref(),
            Some("My.Object")
        );
    }

    /// Items 2/3/7: a full v2 loopback receive lands files under
    /// `<sender>/<batch>/rel…` (nested preserved), settles every per-file row
    /// `done`/`ingested`, persists the landing dir + name, and stamps `batch_name`
    /// on the received history rows.
    #[tokio::test]
    async fn v2_receive_lands_under_batch_and_settles_files_with_history() {
        use crate::sync::store::list_sync_events;

        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce, files) = build_v2_fixture(tmp.path());
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce, "M31 Lights", "", &files)
            .await
            .unwrap();

        let row = poll_inbound(&store, &announce.package_id.0, InboundState::Done).await;
        assert_eq!(
            row.display_name.as_deref(),
            Some("M31 Lights"),
            "the batch name is persisted"
        );
        let landing = row
            .landing_dir
            .clone()
            .expect("a v2 batch persists its landing dir");
        assert!(
            landing.ends_with("M31_Lights"),
            "landing dir carries the sanitized batch name: {landing}"
        );

        // Files landed under <incoming>/<sender_slug>/M31_Lights/… — nested preserved.
        let landed: Vec<std::path::PathBuf> = walkdir::WalkDir::new(&incoming)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect();
        assert_eq!(landed.len(), 2, "both files landed: {landed:?}");
        assert!(
            landed.iter().all(|p| p.starts_with(&landing)),
            "every file lands under the batch dir"
        );
        assert!(
            landed.iter().any(|p| p
                .to_string_lossy()
                .contains("camera_ASI/lights/L_0002.fits")),
            "the nested rel_path is preserved: {landed:?}"
        );

        // Per-file rows all settled done/ingested.
        let file_rows = {
            let conn = store.lock_conn();
            list_inbound_files(&conn, row.id).unwrap()
        };
        assert_eq!(file_rows.len(), 2);
        assert!(
            file_rows
                .iter()
                .all(|r| r.state == InboundFileState::Done
                    && r.outcome.as_deref() == Some("ingested")),
            "each per-file row settled done/ingested: {file_rows:?}"
        );

        // Received history rows carry the batch name.
        let named: i64 = {
            let conn = store.lock_conn();
            conn.query_row(
                "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND batch_name='M31 Lights'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(named, 2, "both received history rows carry the batch name");

        // The event journal saw the lifecycle.
        let kinds: Vec<String> = {
            let conn = store.lock_conn();
            list_sync_events(&conn, Direction::Received, &row.id.to_string())
                .unwrap()
                .into_iter()
                .map(|e| e.kind)
                .collect()
        };
        for want in [
            "announce_received",
            "fetch_started",
            "ingest_started",
            "ingested",
        ] {
            assert!(
                kinds.iter().any(|k| k == want),
                "journal has {want}: {kinds:?}"
            );
        }
    }

    /// Item 1: a v1 announce (blank name, empty files) creates NO per-file rows and
    /// NO display name, and never resolves a landing dir (the pre-v2 layout).
    #[tokio::test]
    async fn v1_announce_creates_no_file_rows_or_name() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let (pkg_dir, announce) = build_inbound_fixture(tmp.path());
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        // v1: blank name, empty manifest (the loopback delivers Some("")/Some(vec![])).
        sender
            .announce(receiver_node, &announce, "", "", &[])
            .await
            .unwrap();

        let row = poll_inbound(&store, &announce.package_id.0, InboundState::Done).await;
        assert!(row.display_name.is_none(), "a v1 announce records no name");
        assert!(
            row.landing_dir.is_none(),
            "a v1 announce resolves no batch landing dir"
        );
        let file_rows = {
            let conn = store.lock_conn();
            list_inbound_files(&conn, row.id).unwrap()
        };
        assert!(
            file_rows.is_empty(),
            "a v1 announce creates no per-file rows"
        );
        // A file still landed (byte-identical v1 layout under the sender slug).
        let landed = walkdir::WalkDir::new(&incoming)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .count();
        assert_eq!(landed, 1, "the frame still lands under the v1 layout");
    }

    /// Item 4: a receiver cancel of a v2 package writes one `cancelled` received
    /// history row per frame (with the batch name), settles every per-file row
    /// `done`/`cancelled`, and journals `cancelled` — no files land.
    #[tokio::test]
    async fn cancel_v2_writes_receiver_history_settles_files_and_journals() {
        use crate::sync::store::list_sync_events;

        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let control = Arc::new(InboundControl::new());
        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::clone(&control),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce, files) = build_v2_fixture(tmp.path());
        sender.serve(&announce, &pkg_dir, None).await.unwrap();

        // Cancel BEFORE the announce → the epilogue runs (manifest still recorded).
        control.request_cancel(&announce.package_id.0);
        sender
            .announce(receiver_node, &announce, "M31 Lights", "", &files)
            .await
            .unwrap();

        let row = poll_inbound(&store, &announce.package_id.0, InboundState::Cancelled).await;

        // Receiver history: one cancelled row per frame, carrying the batch name.
        let cancelled_named: i64 = {
            let conn = store.lock_conn();
            conn.query_row(
                "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled' AND batch_name='M31 Lights'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            cancelled_named, announce.frame_count as i64,
            "one cancelled history row per frame, batch-named"
        );

        // Per-file rows: created at announce, then settled done/cancelled.
        let file_rows = {
            let conn = store.lock_conn();
            list_inbound_files(&conn, row.id).unwrap()
        };
        assert_eq!(file_rows.len(), 2);
        assert!(
            file_rows
                .iter()
                .all(|r| r.state == InboundFileState::Done
                    && r.outcome.as_deref() == Some("cancelled")),
            "un-landed file rows settle done/cancelled: {file_rows:?}"
        );

        // Journal has a cancelled entry; no files landed.
        let kinds: Vec<String> = {
            let conn = store.lock_conn();
            list_sync_events(&conn, Direction::Received, &row.id.to_string())
                .unwrap()
                .into_iter()
                .map(|e| e.kind)
                .collect()
        };
        assert!(
            kinds.iter().any(|k| k == "cancelled"),
            "journal records the cancel: {kinds:?}"
        );
        assert!(
            no_files_landed(&incoming),
            "a declined package lands no files"
        );

        // The durable replay source: a cancelled receipt per frame (existing behavior).
        let cancelled_receipts: i64 = {
            let conn = store.lock_conn();
            conn.query_row(
                "SELECT COUNT(*) FROM sync_receipts WHERE package_id=?1 AND outcome='cancelled'",
                [&announce.package_id.0],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            cancelled_receipts, announce.frame_count as i64,
            "a cancelled receipt per frame is written"
        );
    }

    /// Item 5, as revised by D2 §3.3: the restart reconcile parks a stale row
    /// `waiting` and LEAVES its per-file rows exactly as the interrupted attempt
    /// left them — they are the resume checkpoint and the file counter's evidence,
    /// and the transfer is still outstanding, so settling them `failed` would throw
    /// away the record of what already arrived. A later re-announce refreshes them
    /// to `announced` when the next attempt genuinely starts.
    #[test]
    fn reconcile_parks_the_row_and_keeps_file_rows_then_reannounce_restores() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap();
        let peer = "cc".repeat(32);
        let files = vec![afe("L1.fits", "u1", 10), afe("L2.fits", "u2", 20)];

        let id = {
            let conn = store.lock_conn();
            let id = upsert_inbound_announced(&conn, &peer, "pkg", 2, 30).unwrap();
            record_inbound_manifest(&conn, id, Some("N"), &files).unwrap();
            // Simulate a mid-fetch process: row fetching, one file row fetching.
            set_inbound_state(&conn, "pkg", InboundState::Fetching, None).unwrap();
            set_inbound_file_state(
                &conn,
                id,
                "L1.fits",
                InboundFileState::Fetching,
                5,
                None,
                None,
            )
            .unwrap();
            id
        };

        reconcile_stale_inbound(&store);

        {
            let conn = store.lock_conn();
            assert_eq!(
                get_inbound(&conn, "pkg").unwrap().unwrap().state,
                InboundState::Waiting
            );
            let rows = list_inbound_files(&conn, id).unwrap();
            assert!(
                rows.iter().all(|r| r.state != InboundFileState::Failed),
                "no file row is settled by the reconcile — they are the checkpoint: {rows:?}"
            );
            let l1 = rows.iter().find(|r| r.rel_path == "L1.fits").unwrap();
            assert_eq!(
                l1.state,
                InboundFileState::Fetching,
                "the mid-fetch file keeps its state"
            );
            assert_eq!(l1.bytes_done, 5, "and the bytes it had already received");
            assert!(l1.error.is_none(), "a benign wait writes no per-file error");
        }

        // A re-announce refreshes the row + rows back to announced.
        {
            let conn = store.lock_conn();
            let id2 = upsert_inbound_announced(&conn, &peer, "pkg", 2, 30).unwrap();
            assert_eq!(id2, id, "the same durable row is reused");
            record_inbound_manifest(&conn, id2, Some("N"), &files).unwrap();
            let rows = list_inbound_files(&conn, id2).unwrap();
            assert!(
                rows.iter().all(|r| r.state == InboundFileState::Announced),
                "a re-announce restores the file rows to announced: {rows:?}"
            );
        }
    }

    /// F5 (delivery-model audit): "every terminal path emits" was a CONVENTION,
    /// and a convention is exactly what let one of six paths forget. The row left
    /// [`inbound_active`], the durable terminal list is re-fetched only on
    /// `sync-finished`, and it appeared in neither — it simply vanished from a live
    /// Transfers screen (owner smoke 2026-07-24). Two MORE silent paths turned up
    /// when D2 re-audited the set, which is how a convention fails: quietly, one
    /// path at a time.
    ///
    /// The rule this guards:
    ///
    /// > Every write that puts a `sync_inbound` row into a TERMINAL state on the
    /// > receiver's LIVE path also announces it with `sync-finished`.
    ///
    /// The live-path writers, all of which announce:
    ///
    /// | Writer | Terminal | Where the announce lives |
    /// | ---- | ---- | ---- |
    /// | `terminalize_inbound_failed` | `Failed` | inside the helper — the fetch local-fault, ingest-error and ack-error paths all route through it, so none of them can forget |
    /// | `handle_announce` ingest terminal | `Done` / `Failed` | the single emit at the end of the ingest arm |
    /// | `replay_ack_from_log` | `Done` / `Cancelled` | its own emit |
    /// | `cancel_epilogue` | `Cancelled` | its own emit |
    /// | `handle_revoke` | `Failed` / `Cancelled` | its own emit |
    ///
    /// TWO deliberate exemptions, both OUTSIDE the live path. Neither is an
    /// oversight: `sync-finished` is what raises a user notification
    /// (`notifyFinished`, `src/hooks/useSyncStatus.ts`), so emitting from either
    /// would produce a FALSE one.
    ///
    /// - the startup receipt-repair in [`reconcile_stale_inbound`] settles rows
    ///   that reached their terminal in a PREVIOUS session; announcing would toast
    ///   "N frames arrived" at every launch. The frontend's mount fetch already
    ///   carries those rows.
    /// - `api::sync::cancel_incoming_package` — the user just performed the action,
    ///   so a notification about it is noise; its caller re-fetches the terminal
    ///   list instead (`useTransferQueue`'s `cancelInbound`).
    ///
    /// A drift guard, not a proof. It fails when the number of `sync_inbound` state
    /// writers changes — which is precisely the moment to re-read the table above
    /// and decide which column the new one belongs in.
    #[test]
    fn every_terminal_writer_announces_or_is_a_named_exemption() {
        /// Count state-write call sites in a file's PRODUCTION half, ignoring
        /// comment lines (so prose mentioning the function never moves the number).
        fn write_sites(src: &str) -> usize {
            src.split("\n#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(src)
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    !t.starts_with("//") && t.contains("set_inbound_state(")
                })
                .count()
        }

        assert_eq!(
            write_sites(include_str!("receiver.rs")),
            12,
            "the set of `sync_inbound` state writers in the receiver changed. \
             If the new one puts a row in a TERMINAL state on the live path, it \
             MUST announce it — route it through `terminalize_inbound_failed`, or \
             emit `sync-finished` yourself after the write. A terminal row leaves \
             `inbound_active`, so without the announce it disappears from the \
             Transfers screen entirely. See this test's doc comment for the table \
             and the two exemptions."
        );
        assert_eq!(
            write_sites(include_str!("../api/sync.rs")),
            1,
            "the only `sync_inbound` state write outside the receiver is \
             `cancel_incoming_package`'s Cancelled stamp, which is a NAMED \
             exemption from the announce rule (the user took the action; its \
             caller re-fetches). A second one here needs the same justification \
             written down, or it belongs on the receiver's live path."
        );
    }

    /// D2 §4: a `Waiting` row is non-terminal, so it comes back from
    /// `inbound_active` on EVERY launch. Without an explicit skip the fallback
    /// would overwrite the preserved reason with "interrupted by restart" — the
    /// first restart after a peer vanishes would destroy exactly what the state
    /// exists to record, and every later one would keep re-stamping it.
    #[test]
    fn the_reconcile_leaves_an_existing_waiting_row_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap();
        let peer = "dd".repeat(32);
        let files = vec![afe("L1.fits", "u1", 10)];

        let id = {
            let conn = store.lock_conn();
            let id = upsert_inbound_announced(&conn, &peer, "pkg", 1, 10).unwrap();
            record_inbound_manifest(&conn, id, Some("N"), &files).unwrap();
            set_inbound_file_state(
                &conn,
                id,
                "L1.fits",
                InboundFileState::Fetching,
                7,
                None,
                None,
            )
            .unwrap();
            set_inbound_state(
                &conn,
                "pkg",
                InboundState::Waiting,
                Some("peer gone: connection lost"),
            )
            .unwrap();
            id
        };

        reconcile_stale_inbound(&store);
        reconcile_stale_inbound(&store); // and again — idempotent across restarts

        let conn = store.lock_conn();
        let row = get_inbound(&conn, "pkg").unwrap().unwrap();
        assert_eq!(row.state, InboundState::Waiting);
        assert_eq!(
            row.last_error.as_deref(),
            Some("peer gone: connection lost"),
            "the original reason survives every restart"
        );
        let rows = list_inbound_files(&conn, id).unwrap();
        assert_eq!(
            rows[0].state,
            InboundFileState::Fetching,
            "and the checkpoint is untouched"
        );
        assert_eq!(rows[0].bytes_done, 7);
    }

    // ── Transfers Batch Model (§D1/§D2, B4) ─────────────────────────────────────

    /// Count `sync_events` kinds for a received batch (the per-transfer journal).
    fn journal_kinds(store: &Arc<CatalogSyncStore>, inbound_id: i64) -> Vec<String> {
        use crate::sync::store::list_sync_events;
        let conn = store.lock_conn();
        list_sync_events(&conn, Direction::Received, &inbound_id.to_string())
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    fn count_scalar(store: &Arc<CatalogSyncStore>, sql: &str) -> i64 {
        let conn = store.lock_conn();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// D2 §3.1: a fetch that loses its PEER is not a failure. The row parks
    /// non-terminal, so it stays in `inbound_active` and the 10 s status poll keeps
    /// it on screen by itself — no event needed, and none emitted, because there is
    /// no terminal to announce.
    ///
    /// This replaces the D1-era `failed_fetch_emits_a_terminal_finished_event`.
    /// That fix was compensating for the row leaving `inbound_active` the moment it
    /// was stamped terminal (owner smoke 2026-07-24, "closed the sender
    /// mid-transfer" — the row vanished from the Transfers screen because the
    /// terminal list that should then carry it is only re-fetched on
    /// `sync-finished`). Non-terminality removes the cause rather than announcing
    /// the symptom; the emission moves to the paths that really do end the transfer
    /// (`a_local_fault_fetch_is_failed_and_emits_its_terminal` below).
    #[tokio::test]
    async fn a_peer_absent_fetch_leaves_the_row_waiting_and_emits_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce, files) = build_v2_fixture(tmp.path());
        let wire = announce.package_id.0.clone();
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        // Kill the payload transfer mid-flight — the loopback stand-in for "the
        // sender's app was closed while we were downloading".
        receiver_ep.set_fault(FaultPlan {
            abort_after_bytes: Some(1),
            ..Default::default()
        });
        sender
            .announce(
                receiver_node,
                &announce,
                "M31 Lights",
                "batch-finished-on-failure",
                &files,
            )
            .await
            .unwrap();

        let row = poll_inbound(&store, &wire, InboundState::Waiting).await;
        assert!(
            no_files_landed(&incoming),
            "the aborted fetch lands no files"
        );

        assert_eq!(
            row.state,
            InboundState::Waiting,
            "non-terminal — the sender still owes us this transfer"
        );
        assert!(
            row.finished_at.is_none(),
            "a waiting row never stamps finished_at"
        );
        assert!(
            row.last_error
                .as_deref()
                .unwrap_or_default()
                .contains("injected fault"),
            "the reason is preserved: {:?}",
            row.last_error
        );
        assert!(
            finished_events(&recorder).is_empty(),
            "no terminal event for a non-terminal end: {:?}",
            finished_events(&recorder)
        );

        // The row is exactly where a live Transfers screen looks for it.
        {
            let conn = store.lock_conn();
            let active = crate::sync::store::inbound_active(&conn).unwrap();
            assert!(
                active.iter().any(|r| r.package_id == wire),
                "the parked row stays in the Active list with no event to carry it"
            );
        }

        // The per-file rows are the resume checkpoint, not casualties (D2 §3.3).
        {
            let conn = store.lock_conn();
            let files = list_inbound_files(&conn, row.id).unwrap();
            assert!(
                files.iter().all(|f| f.state != InboundFileState::Failed),
                "no file row is settled failed by a benign wait: {:?}",
                files.iter().map(|f| f.state).collect::<Vec<_>>()
            );
        }
    }

    /// D2 §3.2: the other half of the classification. A fetch that transferred
    /// fine and then failed writing to OUR disk is terminal — "we cannot accept
    /// this" — and still announces itself so the row moves to the terminal list.
    #[tokio::test]
    async fn a_local_fault_fetch_is_failed_and_emits_its_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        let (pkg_dir, announce, files) = build_v2_fixture(tmp.path());
        let wire = announce.package_id.0.clone();
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        // Everything transfers; writing it out fails. The real transport's
        // materialize phase (dest-dir creation / blob export) is where this lives.
        receiver_ep.set_fault(FaultPlan {
            fetch_local_fault_once: true,
            ..Default::default()
        });
        sender
            .announce(
                receiver_node,
                &announce,
                "M31 Lights",
                "batch-local-fault",
                &files,
            )
            .await
            .unwrap();

        let row = poll_inbound(&store, &wire, InboundState::Failed).await;
        assert_eq!(
            row.state,
            InboundState::Failed,
            "we cannot accept it — terminal"
        );
        assert!(
            row.last_error
                .as_deref()
                .unwrap_or_default()
                .contains("No space left on device"),
            "the honest local reason is preserved: {:?}",
            row.last_error
        );

        wait_for_finished(&recorder, 1).await;
        let ev = finished_events(&recorder)
            .pop()
            .expect("a terminal finished event");
        assert_eq!(
            ev["outcome"], "failed",
            "the event carries the terminal outcome: {ev}"
        );
        assert_eq!(ev["direction"], "received", "receive-side event: {ev}");
        assert_eq!(
            ev["packageId"], wire,
            "keyed on the attempt's wire id: {ev}"
        );
    }

    /// The primary batch-model contract (the TDD driver): a transfer whose FIRST
    /// attempt fails at the payload fetch and whose SECOND attempt (a fresh wire id
    /// for the SAME `batch_uuid`) succeeds keeps exactly ONE long-lived inbound row
    /// (id constant), lands its files only on the successful attempt, and its
    /// journal carries both attempts' cycles. The `batch_uuid` column is the
    /// batch-model proof (pre-B4 the row keyed on the wire id with a NULL
    /// `batch_uuid`).
    #[tokio::test]
    async fn resend_after_failed_fetch_reuses_one_batch_row_and_delivers() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        const BATCH: &str = "batch-m31-lights";
        let (pkg_dir, announce1, files) = build_v2_fixture(tmp.path());
        let w1 = announce1.package_id.0.clone();

        // Attempt 1: serve + announce with the fetch armed to abort (one-shot). No
        // payload lands; the row is stamped Failed.
        sender.serve(&announce1, &pkg_dir, None).await.unwrap();
        receiver_ep.set_fault(FaultPlan {
            abort_after_bytes: Some(1),
            ..Default::default()
        });
        sender
            .announce(receiver_node, &announce1, "M31 Lights", BATCH, &files)
            .await
            .unwrap();

        // D2 §3.1: the aborted transfer parks Waiting (peer-absent), not Failed —
        // the resend below is exactly the redelivery that state is waiting for.
        let row1 = poll_inbound(&store, &w1, InboundState::Waiting).await;
        let inbound_id = row1.id;
        assert_eq!(
            row1.batch_uuid.as_deref(),
            Some(BATCH),
            "the row is keyed on the durable batch_uuid"
        );
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_inbound"),
            1,
            "exactly one inbound row"
        );
        assert!(
            no_files_landed(&incoming),
            "the aborted first attempt lands no files"
        );

        // Attempt 2 (resend): a FRESH wire id for the SAME batch_uuid; the fault has
        // auto-disarmed, so the fetch now succeeds and the frames land.
        let announce2 = PackageAnnounce {
            package_id: PackageId("resend-wire-2".to_string()),
            root_hash: announce1.root_hash.clone(),
            byte_size: announce1.byte_size,
            frame_count: announce1.frame_count,
        };
        let w2 = announce2.package_id.0.clone();
        sender.serve(&announce2, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce2, "M31 Lights", BATCH, &files)
            .await
            .unwrap();

        let row2 = poll_inbound(&store, &w2, InboundState::Done).await;
        assert_eq!(
            row2.id, inbound_id,
            "the SAME durable inbound row is reused across attempts"
        );
        assert_eq!(
            row2.batch_uuid.as_deref(),
            Some(BATCH),
            "batch_uuid is stable across attempts"
        );
        assert_eq!(
            row2.package_id, w2,
            "the row's current wire id rotated to the successful attempt"
        );
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_inbound"),
            1,
            "still exactly one inbound row"
        );

        // Files land only on the successful attempt, under ONE landing dir.
        let landed: Vec<std::path::PathBuf> = walkdir::WalkDir::new(&incoming)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect();
        assert_eq!(landed.len(), 2, "both files land on the resend: {landed:?}");
        let landing = row2
            .landing_dir
            .clone()
            .expect("the delivered batch persists its landing dir");
        assert!(
            landed.iter().all(|p| p.starts_with(&landing)),
            "every file lands under the one batch dir"
        );

        // The journal (per-transfer feed) carries BOTH attempts.
        let kinds = journal_kinds(&store, inbound_id);
        assert_eq!(
            kinds.iter().filter(|k| *k == "announce_received").count(),
            2,
            "two announces in the feed: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| *k == "fetch_started").count(),
            2,
            "two fetch attempts in the feed: {kinds:?}"
        );
        // D2 §3.1: attempt 1 ended because the peer went away, so the journal kind
        // is `fetch_waiting` — the attempt parked, it did not fail. `fetch_failed`
        // now means only a local fault we cannot accept.
        assert!(
            kinds.iter().any(|k| k == "fetch_waiting"),
            "attempt 1's park is journaled: {kinds:?}"
        );
        assert!(
            kinds.iter().any(|k| k == "ingested"),
            "attempt 2's ingest is journaled: {kinds:?}"
        );
    }

    /// B5b: received `sync_history` rows key on the durable `batch_uuid`, never the
    /// per-attempt wire id — so an earlier attempt's history can never render as a
    /// phantom faded group the current batch-keyed row fails to dedupe. Runs B4's
    /// two-attempt harness (attempt 1 fails at the payload fetch; attempt 2, a FRESH
    /// wire id for the SAME `batch_uuid`, delivers) and asserts every received
    /// history row's `package_id` equals the row's `batch_uuid` and NONE keys on
    /// either wire id (both `!= batch_uuid`). Pre-B5b the ingest path stamped the
    /// wire id, so attempt 2's rows keyed on `resend-wire-2` — red by construction.
    #[tokio::test]
    async fn received_history_keys_on_batch_uuid_not_wire_id() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        const BATCH: &str = "batch-m31-lights";
        let (pkg_dir, announce1, files) = build_v2_fixture(tmp.path());
        let w1 = announce1.package_id.0.clone();
        assert_ne!(
            w1, BATCH,
            "the fixture wire id must differ from the batch_uuid for this test to discriminate"
        );

        // Attempt 1: fetch armed to abort → nothing lands, no history written.
        sender.serve(&announce1, &pkg_dir, None).await.unwrap();
        receiver_ep.set_fault(FaultPlan {
            abort_after_bytes: Some(1),
            ..Default::default()
        });
        sender
            .announce(receiver_node, &announce1, "M31 Lights", BATCH, &files)
            .await
            .unwrap();
        let _ = poll_inbound(&store, &w1, InboundState::Waiting).await;

        // Attempt 2 (resend): a FRESH wire id for the SAME batch_uuid; delivers.
        let announce2 = PackageAnnounce {
            package_id: PackageId("resend-wire-2".to_string()),
            root_hash: announce1.root_hash.clone(),
            byte_size: announce1.byte_size,
            frame_count: announce1.frame_count,
        };
        let w2 = announce2.package_id.0.clone();
        assert_ne!(
            w2, BATCH,
            "the resend wire id must differ from the batch_uuid"
        );
        sender.serve(&announce2, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce2, "M31 Lights", BATCH, &files)
            .await
            .unwrap();
        let row = poll_inbound(&store, &w2, InboundState::Done).await;
        let batch = row
            .batch_uuid
            .clone()
            .expect("the delivered row carries its batch_uuid");
        assert_eq!(batch, BATCH);

        // Both frames' ingest history exists (proves the writer ran).
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='ingested'"),
            2,
            "both frames' ingest history recorded"
        );
        // Every received history row keys on the batch_uuid …
        assert_eq!(
            count_scalar(&store, &format!("SELECT COUNT(*) FROM sync_history WHERE direction='received' AND package_id <> '{batch}'")),
            0,
            "no received history row keys on anything but the batch_uuid"
        );
        // … and the phantom-group condition (a wire-id-keyed row) is impossible.
        assert_eq!(
            count_scalar(&store, &format!("SELECT COUNT(*) FROM sync_history WHERE direction='received' AND package_id IN ('{w1}','{w2}')")),
            0,
            "no received history row keys on a rotated wire id"
        );
    }

    /// The cancelled-transfer-vs-resend contract (§B4): once the receiver DECLINES a
    /// transfer, a sender resend on a FRESH wire id keeps it declined — the receiver
    /// answers the new attempt with an all-cancelled ack WITHOUT fetching (no files
    /// land even though the payload is served), reusing the ONE cancelled row and
    /// NOT duplicating the receiver history. The chosen re-ack mechanism: the new
    /// wire id is seeded with the prior attempt's `Cancelled` receipts, so the
    /// ack-replay guard produces the all-cancelled ack.
    #[tokio::test]
    async fn cancelled_transfer_resend_is_reacked_cancelled_without_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let control = Arc::new(InboundControl::new());
        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::clone(&control),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
        )
        .await
        .unwrap();

        const BATCH: &str = "batch-declined";
        let (pkg_dir, announce1, files) = build_v2_fixture(tmp.path());
        let w1 = announce1.package_id.0.clone();
        let n = announce1.frame_count as i64;

        // Attempt 1: cancel BEFORE the announce → the cancel epilogue runs (row
        // Cancelled + a Cancelled receipt per frame under w1).
        sender.serve(&announce1, &pkg_dir, None).await.unwrap();
        control.request_cancel(&w1);
        sender
            .announce(receiver_node, &announce1, "Declined", BATCH, &files)
            .await
            .unwrap();

        let row1 = poll_inbound(&store, &w1, InboundState::Cancelled).await;
        let inbound_id = row1.id;
        assert_eq!(
            count_scalar(&store, &format!("SELECT COUNT(*) FROM sync_receipts WHERE package_id='{w1}' AND outcome='cancelled'")),
            n,
            "attempt 1 wrote a cancelled receipt per frame"
        );
        let cancelled_history_after_1 = count_scalar(
            &store,
            "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled'",
        );
        assert_eq!(
            cancelled_history_after_1, n,
            "one cancelled history row per frame after the decline"
        );
        assert!(
            no_files_landed(&incoming),
            "the declined transfer lands no files"
        );

        // Attempt 2 (resend): a FRESH wire id, FULLY SERVED (so a re-fetch WOULD
        // deliver) — the receiver must still decline it.
        let announce2 = PackageAnnounce {
            package_id: PackageId("declined-resend-2".to_string()),
            root_hash: announce1.root_hash.clone(),
            byte_size: announce1.byte_size,
            frame_count: announce1.frame_count,
        };
        let w2 = announce2.package_id.0.clone();
        sender.serve(&announce2, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce2, "Declined", BATCH, &files)
            .await
            .unwrap();

        // The resend is re-acked cancelled: a Cancelled receipt set now exists under
        // the NEW wire id (the chosen mechanism), and a `cancelled` finished event
        // fired for w2.
        poll_cancelled_receipts(&store, &w2, n).await;

        // Still ONE row, still declined, receipt anchor rotated to the NEWEST
        // fully-receipted attempt (§D5 — so the next resend seeds from w2), no
        // files landed, and the history was NOT duplicated.
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_inbound"),
            1,
            "still exactly one inbound row"
        );
        let row2 = {
            let conn = store.lock_conn();
            get_inbound_by_row_id(&conn, inbound_id).unwrap().unwrap()
        };
        assert_eq!(row2.id, inbound_id, "the same declined row is reused");
        assert_eq!(
            row2.state,
            InboundState::Cancelled,
            "the transfer stays declined"
        );
        assert!(
            row2.declined_at.is_some(),
            "the decline is recorded on the finality axis"
        );
        assert_eq!(
            row2.package_id, w2,
            "the receipt anchor rotated to the resend's wire id"
        );
        assert!(
            no_files_landed(&incoming),
            "the resend of a declined transfer fetches nothing"
        );
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled'"),
            cancelled_history_after_1,
            "the resend re-ack does NOT duplicate the cancelled history"
        );
        let finished = finished_events(&recorder);
        assert!(
            finished
                .iter()
                .any(|e| e["packageId"] == w2 && e["outcome"] == "cancelled"),
            "the resend emitted a cancelled finished event for the new wire id: {finished:?}"
        );
    }

    /// Owner smoke №7 regression (Decline Finality Axis §D3/§D4): a transfer whose
    /// first attempt the SENDER revoked (cancel) must accept the sender's resend —
    /// the revoke-cancelled row (`declined_at` NULL) resets like any attempt
    /// terminal, the payload is fetched and ingested, and no all-cancelled re-ack
    /// fires. Pre-fix, the row hit declined-final and both sides blamed each other
    /// forever.
    #[tokio::test]
    async fn sender_cancel_then_resend_fetches_and_ingests() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        const BATCH: &str = "batch-revoked-then-resent";
        let (pkg_dir, announce2, files) = build_v2_fixture(tmp.path());
        let w2 = announce2.package_id.0.clone();
        let peer_hex = super::super::node_id_hex(&sender.node_id());

        // Attempt 1: an announced row the sender then CANCELS (revoke over the
        // wire). Seeded directly (deterministic — no fetch to race) exactly as the
        // announce would have written it, then revoked through the real transport
        // path so `handle_revoke` does the bookkeeping.
        let inbound_id = {
            let conn = store.lock_conn();
            let (id, declined_final) = upsert_inbound_attempt(
                &conn,
                &peer_hex,
                BATCH,
                "wire-revoked-1",
                announce2.frame_count,
                announce2.byte_size,
            )
            .unwrap();
            assert!(!declined_final);
            id
        };
        sender
            .revoke(
                receiver_node,
                &PackageId("wire-revoked-1".to_string()),
                RevokeReason::Cancelled,
            )
            .await
            .unwrap();
        let row1 = poll_inbound(&store, "wire-revoked-1", InboundState::Cancelled).await;
        assert_eq!(row1.id, inbound_id);
        assert_eq!(
            row1.last_error.as_deref(),
            Some("by sender"),
            "the revoke's attempt terminal"
        );
        assert!(
            row1.declined_at.is_none(),
            "a sender revoke NEVER records a receiver decline"
        );

        // Attempt 2 (the sender's resend): fresh wire id, SAME batch_uuid, fully
        // served. The receiver must reset the row and deliver — not re-ack cancelled.
        sender.serve(&announce2, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce2, "M31 Lights", BATCH, &files)
            .await
            .unwrap();

        let row2 = poll_inbound(&store, &w2, InboundState::Done).await;
        assert_eq!(row2.id, inbound_id, "one long-lived row per transfer");
        assert_eq!(row2.package_id, w2, "the resend's wire id was stamped");
        assert_eq!(row2.generation, 2, "the resend is attempt 2");
        assert!(
            row2.declined_at.is_none(),
            "delivery never invents a decline"
        );
        assert!(
            !no_files_landed(&incoming),
            "the resend fetched and landed the payload"
        );
        assert_eq!(
            count_scalar(&store, &format!("SELECT COUNT(*) FROM sync_receipts WHERE package_id='{w2}' AND outcome='cancelled'")),
            0,
            "no cancelled receipts on the resend — the pre-fix failure mode"
        );
        let kinds = journal_kinds(&store, inbound_id);
        assert!(
            kinds.iter().any(|k| k == "revoked"),
            "attempt 1's revoke is journaled: {kinds:?}"
        );
        assert!(
            kinds.iter().any(|k| k == "ingested"),
            "attempt 2's ingest is journaled: {kinds:?}"
        );
    }

    /// Review-fix regression (Decline Finality Axis §D2): a straggler re-announce
    /// of the SAME wire id arriving AFTER the sender's revoke (announce/revoke have
    /// no cross-stream ordering guarantee; a retry tick can race the cancel) must
    /// deliver like any reset attempt — `handle_revoke` no longer poisons the
    /// local-decline `cancels` set, so the straggler cannot divert into the cancel
    /// epilogue and mint a `declined_at` the user never chose (which would brick
    /// every future resend with an all-cancelled re-ack).
    #[tokio::test]
    async fn revoke_then_same_wire_straggler_announce_delivers() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        const BATCH: &str = "batch-straggler";
        let (pkg_dir, announce1, files) = build_v2_fixture(tmp.path());
        let w1 = announce1.package_id.0.clone();
        let peer_hex = super::super::node_id_hex(&sender.node_id());

        // Seed the announced attempt, then revoke it over the transport — the row
        // is now cancelled "by sender" (attempt-terminal, declined_at NULL).
        let inbound_id = {
            let conn = store.lock_conn();
            let (id, _) = upsert_inbound_attempt(
                &conn,
                &peer_hex,
                BATCH,
                &w1,
                announce1.frame_count,
                announce1.byte_size,
            )
            .unwrap();
            id
        };
        sender
            .revoke(
                receiver_node,
                &announce1.package_id,
                RevokeReason::Cancelled,
            )
            .await
            .unwrap();
        let row1 = poll_inbound(&store, &w1, InboundState::Cancelled).await;
        assert!(row1.declined_at.is_none(), "a sender revoke never declines");

        // The straggler: the SAME wire id re-announced after the revoke (served, so
        // a wrongly-diverted epilogue would be distinguishable from a delivery).
        sender.serve(&announce1, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce1, "Straggler", BATCH, &files)
            .await
            .unwrap();

        let row2 = poll_inbound(&store, &w1, InboundState::Done).await;
        assert_eq!(row2.id, inbound_id, "same batch-keyed row");
        assert!(
            row2.declined_at.is_none(),
            "the straggler must NOT mint a decline"
        );
        assert!(
            !no_files_landed(&incoming),
            "the straggler announce delivered the payload"
        );
        assert_eq!(
            count_scalar(&store, &format!("SELECT COUNT(*) FROM sync_receipts WHERE package_id='{w1}' AND outcome='cancelled'")),
            0,
            "no cancelled receipts — the epilogue never ran"
        );
    }

    /// Decline durability across a crash (Decline Finality Axis §D2): a decline
    /// stamped during a live fetch (`declined_at` written immediately by the
    /// command; the epilogue never ran — crash) survives the startup reconcile's
    /// `failed "interrupted by restart"` state overwrite, so the sender's resend is
    /// re-acked all-cancelled by the epilogue WITHOUT landing files — and a THIRD
    /// attempt replays from the seeded receipt log without duplicating history
    /// (§D5 receipt-anchor rotation).
    #[tokio::test]
    async fn decline_survives_restart_reconcile_and_refuses_resend() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());

        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        const BATCH: &str = "batch-declined-then-crash";
        let (pkg_dir, announce2, files) = build_v2_fixture(tmp.path());
        let w2 = announce2.package_id.0.clone();
        let n = announce2.frame_count as i64;
        let peer_hex = super::super::node_id_hex(&sender.node_id());

        // Pre-restart state: a mid-fetch attempt the user declined — the §D2
        // primary write stamped `declined_at` immediately; the crash meant the
        // epilogue never ran (no receipts, state still `fetching`).
        let inbound_id = {
            let conn = store.lock_conn();
            let (id, _) = upsert_inbound_attempt(
                &conn,
                &peer_hex,
                BATCH,
                "wire-declined-1",
                announce2.frame_count,
                announce2.byte_size,
            )
            .unwrap();
            set_inbound_state(&conn, "wire-declined-1", InboundState::Fetching, None).unwrap();
            set_inbound_declined_at(&conn, id).unwrap();
            id
        };

        // "Restart": spawning the receiver runs the startup reconcile, which
        // overwrites the zombie fetching STATE — but not the decline axis.
        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();
        {
            let conn = store.lock_conn();
            let row = get_inbound_by_row_id(&conn, inbound_id).unwrap().unwrap();
            assert_eq!(
                row.state,
                InboundState::Waiting,
                "the reconcile parked the zombie attempt"
            );
            assert_eq!(row.last_error.as_deref(), Some("interrupted by restart"));
            assert!(
                row.declined_at.is_some(),
                "the decline survives the restart reconcile"
            );
        }

        // The sender's resend (fresh wire id, same batch, fully served): the
        // declined transfer must be re-acked cancelled by the epilogue — receipts
        // under w2, anchor rotated, NO files landed.
        sender.serve(&announce2, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce2, "M31 Lights", BATCH, &files)
            .await
            .unwrap();
        let row2 = poll_inbound(&store, &w2, InboundState::Cancelled).await;
        assert_eq!(row2.id, inbound_id, "one long-lived row per transfer");
        assert!(row2.declined_at.is_some(), "still declined");
        assert_eq!(
            row2.package_id, w2,
            "the receipt anchor rotated to the epilogue's wire id"
        );
        assert_eq!(
            count_scalar(&store, &format!("SELECT COUNT(*) FROM sync_receipts WHERE package_id='{w2}' AND outcome='cancelled'")),
            n,
            "the epilogue wrote a cancelled receipt per frame under the resend's wire id"
        );
        assert!(
            no_files_landed(&incoming),
            "a declined transfer never lands files"
        );
        let history_after_2 = count_scalar(
            &store,
            "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled'",
        );
        assert_eq!(
            history_after_2, n,
            "one cancelled history row per frame, written once"
        );

        // A THIRD attempt (unserved — a replay needs no fetch at all): answered
        // from the seeded receipt log; history NOT duplicated; anchor rotated on.
        let announce3 = PackageAnnounce {
            package_id: PackageId("wire-declined-3".to_string()),
            root_hash: announce2.root_hash.clone(),
            byte_size: announce2.byte_size,
            frame_count: announce2.frame_count,
        };
        let w3 = announce3.package_id.0.clone();
        sender
            .announce(receiver_node, &announce3, "M31 Lights", BATCH, &files)
            .await
            .unwrap();
        poll_cancelled_receipts(&store, &w3, n).await;
        let row3 = {
            let conn = store.lock_conn();
            get_inbound_by_row_id(&conn, inbound_id).unwrap().unwrap()
        };
        assert_eq!(
            row3.package_id, w3,
            "the anchor rotated to the newest fully-receipted attempt"
        );
        assert_eq!(
            row3.state,
            InboundState::Cancelled,
            "still declined-terminal"
        );
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled'"),
            history_after_2,
            "the replayed attempt duplicated NO history"
        );
        assert!(no_files_landed(&incoming), "still nothing landed");
    }

    /// Seed an `announced` inbound row (peer, wire id) with `frames` two-file
    /// manifest rows for the direct `handle_revoke` matrix below.
    fn seed_announced_row(store: &Arc<CatalogSyncStore>, peer: &str, wire: &str) -> i64 {
        let conn = store.lock_conn();
        let id = upsert_inbound_announced(&conn, peer, wire, 2, 100).unwrap();
        set_inbound_display_name(&conn, id, Some("Revoke Batch")).unwrap();
        replace_inbound_files(
            &conn,
            id,
            &[
                InboundFileRow {
                    inbound_id: id,
                    rel_path: "L_0001.fits".to_string(),
                    byte_size: 50,
                    frame_uuid: "rev-a".to_string(),
                    state: InboundFileState::Announced,
                    bytes_done: 0,
                    outcome: None,
                    error: None,
                    updated_at: now_iso(),
                },
                InboundFileRow {
                    inbound_id: id,
                    rel_path: "L_0002.fits".to_string(),
                    byte_size: 50,
                    frame_uuid: "rev-b".to_string(),
                    state: InboundFileState::Announced,
                    bytes_done: 0,
                    outcome: None,
                    error: None,
                    updated_at: now_iso(),
                },
            ],
        )
        .unwrap();
        id
    }

    /// Revoke(Cancelled) on a non-terminal row → row `cancelled` ("by sender"),
    /// file rows settled done/cancelled, one `cancelled` received history row per
    /// known file, and a `revoked` journal entry. NO ack is sent.
    #[tokio::test]
    async fn revoke_cancelled_terminalizes_row_settles_files_and_journals() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let staging_root = tmp.path().join("stage");
        let transport = net_endpoint_only();
        let control = InboundControl::new();
        let emitter = RecordingEmitter::default();
        let peer = "cc".repeat(32);
        let id = seed_announced_row(&store, &peer, "rev-w1");

        handle_revoke(
            &store,
            &transport,
            &emitter,
            &control,
            &staging_root,
            [9u8; 32],
            &PackageId("rev-w1".to_string()),
            RevokeReason::Cancelled,
        )
        .await;

        let row = {
            let conn = store.lock_conn();
            get_inbound(&conn, "rev-w1").unwrap().unwrap()
        };
        assert_eq!(
            row.state,
            InboundState::Cancelled,
            "cancelled revoke → cancelled row"
        );
        assert_eq!(row.last_error.as_deref(), Some("by sender"));
        assert!(
            row.finished_at.is_some(),
            "a terminal row stamps finished_at"
        );

        // B5 §4: the revoke emits a terminal `sync-finished` (cancelled) so the
        // receive-side widget dismisses the row immediately.
        let finished = finished_events(&emitter);
        assert!(
            finished.iter().any(|e| e["packageId"] == "rev-w1"
                && e["outcome"] == "cancelled"
                && e["direction"] == "received"),
            "a cancelled revoke emits a received/cancelled sync-finished: {finished:?}"
        );
        let file_rows = {
            let conn = store.lock_conn();
            list_inbound_files(&conn, id).unwrap()
        };
        assert!(
            file_rows
                .iter()
                .all(|r| r.state == InboundFileState::Done
                    && r.outcome.as_deref() == Some("cancelled")),
            "un-settled file rows settle done/cancelled: {file_rows:?}"
        );
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='cancelled' AND batch_name='Revoke Batch'"),
            2,
            "one cancelled history row per known file, batch-named"
        );
        let kinds = journal_kinds(&store, id);
        assert!(
            kinds.iter().any(|k| k == "revoked"),
            "the revoke is journaled: {kinds:?}"
        );
    }

    /// Revoke(Superseded) on an un-fetched row → row `done` ("nothing to fetch") —
    /// a supersede means the peer already holds every frame, so the honest terminal
    /// is success, never a decline.
    #[tokio::test]
    async fn revoke_superseded_maps_to_done_nothing_to_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let staging_root = tmp.path().join("stage");
        let transport = net_endpoint_only();
        let control = InboundControl::new();
        let emitter = RecordingEmitter::default();
        let peer = "dd".repeat(32);
        let id = seed_announced_row(&store, &peer, "rev-sup");

        handle_revoke(
            &store,
            &transport,
            &emitter,
            &control,
            &staging_root,
            [9u8; 32],
            &PackageId("rev-sup".to_string()),
            RevokeReason::Superseded,
        )
        .await;

        let row = {
            let conn = store.lock_conn();
            get_inbound(&conn, "rev-sup").unwrap().unwrap()
        };
        assert_eq!(
            row.state,
            InboundState::Done,
            "superseded revoke → done (not a decline)"
        );
        assert_eq!(
            row.last_error.as_deref(),
            Some("nothing to fetch (superseded by sender)")
        );
        assert!(
            finished_events(&emitter)
                .iter()
                .any(|e| e["packageId"] == "rev-sup" && e["outcome"] == "done"),
            "a superseded revoke emits a `done` sync-finished"
        );
        let file_rows = {
            let conn = store.lock_conn();
            list_inbound_files(&conn, id).unwrap()
        };
        assert!(
            file_rows
                .iter()
                .all(|r| r.state == InboundFileState::Done
                    && r.outcome.as_deref() == Some("superseded")),
            "file rows settle done/superseded: {file_rows:?}"
        );
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_history WHERE direction='received' AND outcome='superseded'"),
            2,
            "one superseded history row per known file"
        );
    }

    /// Revoke(Failed) on a non-terminal row → row `failed` ("sender failed").
    #[tokio::test]
    async fn revoke_failed_maps_to_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let staging_root = tmp.path().join("stage");
        let transport = net_endpoint_only();
        let control = InboundControl::new();
        let emitter = RecordingEmitter::default();
        let peer = "ee".repeat(32);
        let id = seed_announced_row(&store, &peer, "rev-fail");

        handle_revoke(
            &store,
            &transport,
            &emitter,
            &control,
            &staging_root,
            [9u8; 32],
            &PackageId("rev-fail".to_string()),
            RevokeReason::Failed,
        )
        .await;

        let row = {
            let conn = store.lock_conn();
            get_inbound(&conn, "rev-fail").unwrap().unwrap()
        };
        assert_eq!(
            row.state,
            InboundState::Failed,
            "failed revoke → failed row"
        );
        assert_eq!(row.last_error.as_deref(), Some("sender failed"));
        assert!(
            finished_events(&emitter)
                .iter()
                .any(|e| e["packageId"] == "rev-fail" && e["outcome"] == "failed"),
            "a failed revoke emits a `failed` sync-finished"
        );
        let file_rows = {
            let conn = store.lock_conn();
            list_inbound_files(&conn, id).unwrap()
        };
        assert!(
            file_rows.iter().all(|r| r.state == InboundFileState::Failed
                && r.error.as_deref() == Some("sender failed")),
            "file rows settle failed with the reason: {file_rows:?}"
        );
    }

    /// D2 §4: `handle_revoke` returns early on a terminal row. A fetch that lost
    /// its peer used to be `Failed` — terminal — so a revoke arriving afterwards
    /// was a debug no-op; now the row is `Waiting`, so the revoke runs the full
    /// bookkeeping. It must terminalize correctly and write each known file's
    /// history exactly once (`insert_history_row` is a plain INSERT, not an
    /// upsert, so a second bookkeeping pass over the same row would duplicate).
    #[tokio::test]
    async fn a_revoke_for_a_waiting_row_terminalizes_it_once() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let staging_root = tmp.path().join("stage");
        let transport = net_endpoint_only();
        let control = InboundControl::new();
        let emitter = RecordingEmitter::default();
        let peer = "ee".repeat(32);
        let id = seed_announced_row(&store, &peer, "rev-wait");
        {
            let conn = store.lock_conn();
            set_inbound_state(
                &conn,
                "rev-wait",
                InboundState::Waiting,
                Some("peer gone: connection lost"),
            )
            .unwrap();
        }

        handle_revoke(
            &store,
            &transport,
            &emitter,
            &control,
            &staging_root,
            [9u8; 32],
            &PackageId("rev-wait".to_string()),
            RevokeReason::Cancelled,
        )
        .await;

        let row = {
            let conn = store.lock_conn();
            get_inbound(&conn, "rev-wait").unwrap().unwrap()
        };
        assert_eq!(
            row.state,
            InboundState::Cancelled,
            "the revoke closes the parked row instead of being ignored"
        );
        assert!(row.finished_at.is_some(), "and it is genuinely terminal");

        let (file_count, history_count) = {
            let conn = store.lock_conn();
            let files = list_inbound_files(&conn, id).unwrap().len() as i64;
            let history: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sync_history WHERE direction='received'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (files, history)
        };
        assert_eq!(
            history_count, file_count,
            "one history row per known file, no duplicates"
        );
    }

    /// Revoke for an unknown/stale wire id, and revoke for an already-terminal row,
    /// are both no-ops (the row is untouched; no history/journal written).
    #[tokio::test]
    async fn revoke_for_unknown_or_terminal_row_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("catalog.db")).unwrap());
        let staging_root = tmp.path().join("stage");
        let transport = net_endpoint_only();
        let control = InboundControl::new();
        let emitter = RecordingEmitter::default();

        // Unknown wire id → no row is created, no history written.
        handle_revoke(
            &store,
            &transport,
            &emitter,
            &control,
            &staging_root,
            [9u8; 32],
            &PackageId("nope".to_string()),
            RevokeReason::Cancelled,
        )
        .await;
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_inbound"),
            0,
            "an unknown revoke creates no row"
        );
        assert_eq!(
            count_scalar(&store, "SELECT COUNT(*) FROM sync_history"),
            0,
            "an unknown revoke writes no history"
        );

        // Terminal row → left untouched.
        let peer = "ff".repeat(32);
        let id = seed_announced_row(&store, &peer, "rev-done");
        {
            let conn = store.lock_conn();
            set_inbound_state(&conn, "rev-done", InboundState::Done, None).unwrap();
        }
        handle_revoke(
            &store,
            &transport,
            &emitter,
            &control,
            &staging_root,
            [9u8; 32],
            &PackageId("rev-done".to_string()),
            RevokeReason::Cancelled,
        )
        .await;
        let row = {
            let conn = store.lock_conn();
            get_inbound(&conn, "rev-done").unwrap().unwrap()
        };
        assert_eq!(
            row.state,
            InboundState::Done,
            "a terminal row is untouched by a revoke"
        );
        assert!(
            journal_kinds(&store, id).iter().all(|k| k != "revoked"),
            "no revoke journal on a terminal row"
        );
        // A no-op revoke emits nothing (no widget signal for an unknown/terminal row).
        assert!(
            finished_events(&emitter).is_empty(),
            "a no-op revoke emits no sync-finished"
        );
    }

    /// Wiring proof: a `RevokeReceived` delivered over the transport's real event
    /// stream reaches the receiver loop and terminalizes the row.
    #[tokio::test]
    async fn revoke_over_transport_terminalizes_seeded_row() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        let sender_node = sender.node_id();
        sender.start().await.unwrap();

        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = sync_dir.join("incoming");
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        // Seed a non-terminal row for the wire id the revoke targets.
        {
            let conn = store.lock_conn();
            upsert_inbound_announced(&conn, &node_id_hex(&sender_node), "wire-live", 1, 10)
                .unwrap();
        }
        let _ = receiver_node; // the sender addresses the revoke by peer inbox
        sender
            .revoke(
                receiver_node,
                &PackageId("wire-live".to_string()),
                RevokeReason::Cancelled,
            )
            .await
            .unwrap();

        let row = poll_inbound(&store, "wire-live", InboundState::Cancelled).await;
        assert_eq!(
            row.last_error.as_deref(),
            Some("by sender"),
            "the transport revoke drove the row cancelled"
        );
    }

    /// H1 (deny), the revoke twin of `receiver_drops_announce_from_unauthorized_peer`
    /// (B5 §6b): a `RevokeReceived` from a peer NOT on the allow-list must be dropped
    /// on BOTH the ingress pump (no `request_revoke_abort`) AND the serial loop (no
    /// `handle_revoke`), leaving the seeded non-terminal row and its journal untouched.
    /// Without the guard the unauthorized revoke would terminalize a stranger's row.
    #[tokio::test]
    async fn unauthorized_revoke_is_dropped_leaves_row_and_journal_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        let sender_node = sender.node_id();
        sender.start().await.unwrap();

        // Allow-list contains a DIFFERENT node, never the actual sender.
        let allowed_other: NodeId = [9u8; 32];
        let authorizer: PeerAuthorizer = Arc::new(move |id| *id == allowed_other);
        // Keep our own handle to the control so we can prove the pump never poisoned
        // the abort set for this wire id.
        let control = Arc::new(InboundControl::new());

        let (_info, handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = sync_dir.join("incoming");
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            authorizer,
            Default::default(),
            Arc::clone(&control),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        // Seed a non-terminal row (after spawn, so the startup reconcile can't touch
        // it) for the wire id the unauthorized revoke targets.
        let id = {
            let conn = store.lock_conn();
            upsert_inbound_announced(&conn, &node_id_hex(&sender_node), "wire-deny", 1, 10).unwrap()
        };
        sender
            .revoke(
                receiver_node,
                &PackageId("wire-deny".to_string()),
                RevokeReason::Cancelled,
            )
            .await
            .unwrap();

        // Give the loop ample time to (wrongly) act, then assert it did nothing.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let row = {
            let conn = store.lock_conn();
            get_inbound(&conn, "wire-deny").unwrap().unwrap()
        };
        assert_eq!(
            row.state,
            InboundState::Announced,
            "an unauthorized revoke must not terminalize the row"
        );
        assert!(
            journal_kinds(&store, id).iter().all(|k| k != "revoked"),
            "no revoke journal for a dropped revoke"
        );
        assert!(
            !control.is_revoke_abort_requested("wire-deny"),
            "the pump must not request a revoke-abort for an unauthorized peer"
        );

        handle.shutdown().await;
    }

    /// The REAL concurrency story the ingress-pump fix exists for: a sender
    /// `Revoke` delivered while the receiver's fetch is GENUINELY blocking (not a
    /// near-instant loopback copy) aborts it PROMPTLY — well before the fetch
    /// would have finished on its own. Without the fix, `RevokeReceived` queues
    /// behind the single-consumer serial loop's in-progress, blocking
    /// `handle_announce` call and only gets processed (as a no-op, on the
    /// already-`Done` row) once the fetch completes naturally — exactly the bug
    /// the review flagged.
    ///
    /// Proof, all three required by the review:
    /// 1. **Bounded deadline, not fetch-completion**: revoke→`Cancelled` lands
    ///    within 2s of firing, against a fetch that (uninterrupted) blocks for
    ///    ~2× 1.5s = ~3s.
    /// 2. **Partial staging cleaned**: the staging dir — proven non-empty
    ///    mid-fetch (the fetch was genuinely writing into it) — is fully removed
    ///    after.
    /// 3. **Reason-honest, ack-free path**: zero `sync_receipts` rows for the wire
    ///    id and no `cancelled` journal entry prove the FAST `handle_revoke` path
    ///    ran, never the local-decline `cancel_epilogue` (which would write one
    ///    `Cancelled` receipt per frame and send an ack — forbidden for a revoke).
    #[tokio::test]
    async fn revoke_mid_fetch_aborts_promptly_not_at_fetch_completion() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        sender.start().await.unwrap();

        // Hold every fetch read chunk open for 1.5s: the v2 fixture's two small
        // files (each under the 8KB copy granularity) complete in ONE read apiece,
        // so each contributes exactly one 1.5s pause — a genuinely blocking ~3s
        // fetch if left uninterrupted.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(1500);
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });

        let (_info, _handle) = SyncReceiver::spawn(
            Arc::clone(&store),
            sync_dir.clone(),
            {
                let incoming = incoming.clone();
                Arc::new(move || incoming.clone()) as IncomingResolver
            },
            allow_all_peers(),
            Default::default(),
            Arc::new(InboundControl::new()),
            Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let (pkg_dir, announce, files) = build_v2_fixture(tmp.path());
        let wire = announce.package_id.0.clone();
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(receiver_node, &announce, "Live Batch", "batch-live", &files)
            .await
            .unwrap();

        // Wait for the row to actually enter Fetching (the fetch is genuinely under
        // way), then a short grace so the transport has created the staging dir and
        // started copying — this is "mid-fetch", not "before the fetch began".
        poll_inbound(&store, &wire, InboundState::Fetching).await;
        let inbound_id = {
            let conn = store.lock_conn();
            get_inbound(&conn, &wire).unwrap().unwrap().id
        };
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let staging = sync_dir.join("staging").join(&wire);
        assert!(
            staging.exists(),
            "the fetch has started writing into staging before the revoke: {staging:?}"
        );

        // Fire the revoke mid-fetch and time how long it takes to land.
        let fired_at = std::time::Instant::now();
        sender
            .revoke(receiver_node, &announce.package_id, RevokeReason::Cancelled)
            .await
            .unwrap();

        let bound = std::time::Duration::from_secs(2);
        let mut cancelled_row = None;
        while fired_at.elapsed() < bound {
            let row = {
                let conn = store.lock_conn();
                get_inbound(&conn, &wire).unwrap()
            };
            if matches!(&row, Some(r) if r.state == InboundState::Cancelled) {
                cancelled_row = row;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let elapsed = fired_at.elapsed();
        let row = cancelled_row.unwrap_or_else(|| {
            panic!(
                "row did not reach Cancelled within {bound:?} of the revoke — the fetch was NOT \
                 aborted promptly (an uninterrupted fetch needs ~{:?})",
                DELAY * 2
            )
        });
        assert!(
            elapsed < bound,
            "revoke→cancelled took {elapsed:?}, expected well under the ~{:?} uninterrupted fetch duration",
            DELAY * 2
        );
        assert_eq!(row.last_error.as_deref(), Some("by sender"));

        // Partial staging is fully cleaned (handle_revoke's cleanup), not left
        // behind mid-copy.
        assert!(
            !staging.exists(),
            "the partially-fetched staging dir is removed after the revoke"
        );

        // Zero receipts: proves the FAST revoke-abort path ran (`handle_revoke`),
        // never the local `cancel_epilogue` (which writes one `Cancelled` receipt
        // per manifest frame and sends an ack).
        let receipt_count: i64 = {
            let conn = store.lock_conn();
            conn.query_row(
                "SELECT COUNT(*) FROM sync_receipts WHERE package_id = ?1",
                [&wire],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            receipt_count, 0,
            "the revoke path sends no ack and writes no receipts"
        );

        // The journal recorded the revoke, never a local cancel-epilogue entry.
        let kinds = journal_kinds(&store, inbound_id);
        assert!(
            kinds.iter().any(|k| k == "revoked"),
            "journal has the revoke: {kinds:?}"
        );
        assert!(
            kinds.iter().all(|k| k != "cancelled"),
            "the local cancel_epilogue never ran: {kinds:?}"
        );
    }

    /// A bare loopback endpoint used purely as a `&dyn SharingTransport` for the
    /// direct `handle_revoke` matrix (release is a best-effort no-op on it).
    fn net_endpoint_only() -> crate::sharing::loopback::LoopbackTransport {
        LoopbackNetwork::new().endpoint()
    }

    // ─── ReceiveGate (W2 T2.2) ──────────────────────────────────────────────
    //
    // Behavioural pins for the live-resizable receive-concurrency gate. Sequencing
    // rides `tokio::time::timeout` around acquires and permit drops instead of raw
    // sleeps: "must block" is a timeout that ELAPSES, "must be admitted" is one
    // that resolves far inside its bound. A blocked acquire is also a DROPPED
    // acquire future, so every such assertion doubles as a cancel-safety check —
    // an abandoned wait must not strand a permit or a debt unit.

    /// Bound for "this acquire must NOT be admitted". Small: it is paid in real
    /// wall time on every negative assertion.
    const GATE_BLOCKED: std::time::Duration = std::time::Duration::from_millis(50);
    /// Bound for "this acquire must be admitted". Generous: blowing it means the
    /// gate deadlocked, not that the machine was busy.
    const GATE_ADMITTED: std::time::Duration = std::time::Duration::from_secs(5);

    /// Admit one receive through `gate`, failing loudly (not hanging) if the gate
    /// wrongly blocks it.
    async fn gate_admit(gate: &ReceiveGate, what: &str) -> tokio::sync::OwnedSemaphorePermit {
        match tokio::time::timeout(GATE_ADMITTED, gate.acquire()).await {
            Ok(permit) => permit,
            Err(_) => panic!("{what}: acquire must be admitted, but the gate blocked it"),
        }
    }

    /// Assert `gate` refuses another receive right now.
    async fn gate_blocks(gate: &ReceiveGate, what: &str) {
        assert!(
            tokio::time::timeout(GATE_BLOCKED, gate.acquire())
                .await
                .is_err(),
            "{what}: acquire must block"
        );
    }

    #[tokio::test]
    async fn receive_gate_admits_up_to_limit_and_blocks_next() {
        let gate = ReceiveGate::new(2);
        assert_eq!(gate.limit(), 2);

        let p1 = gate_admit(&gate, "first of two").await;
        let _p2 = gate_admit(&gate, "second of two").await;

        gate_blocks(&gate, "third while both permits are held").await;

        drop(p1);
        let _p3 = gate_admit(&gate, "third after a release").await;
    }

    #[tokio::test]
    async fn receive_gate_grow_wakes_waiter() {
        let gate = Arc::new(ReceiveGate::new(1));
        let _held = gate_admit(&gate, "the single permit at limit 1").await;

        let waiting_gate = Arc::clone(&gate);
        let mut waiter = tokio::spawn(async move {
            let _permit = waiting_gate.acquire().await;
        });
        assert!(
            tokio::time::timeout(GATE_BLOCKED, &mut waiter)
                .await
                .is_err(),
            "the second receive parks while the only permit is held"
        );

        gate.set_limit(2);
        assert_eq!(gate.limit(), 2);
        tokio::time::timeout(GATE_ADMITTED, &mut waiter)
            .await
            .expect("growing the limit must wake the parked waiter — no restart, no re-poll nudge")
            .expect("waiter task panicked");
    }

    /// The debt mechanics, pinned step by step: `forget_permits` can only take
    /// AVAILABLE permits, so a shrink under load lands as debt that the next
    /// releases pay off before anyone is admitted again.
    #[tokio::test]
    async fn receive_gate_shrink_takes_effect_as_permits_release() {
        let gate = ReceiveGate::new(3);
        let p1 = gate_admit(&gate, "first of three").await;
        let p2 = gate_admit(&gate, "second of three").await;
        let p3 = gate_admit(&gate, "third of three").await;

        // Nothing is available, so this shrink is 100% debt (2 units).
        gate.set_limit(1);
        assert_eq!(gate.limit(), 1);

        drop(p1);
        gate_blocks(&gate, "first release pays a debt unit, admits nobody").await;

        drop(p2);
        gate_blocks(&gate, "second release pays the last debt unit").await;

        drop(p3);
        let _p4 = gate_admit(&gate, "debt cleared: the third release is a real permit").await;
        gate_blocks(&gate, "only one concurrent receive at limit 1").await;
    }

    /// A shrink/grow round trip must leave no surplus: after 3→1→3 the gate still
    /// admits exactly 3.
    ///
    /// NB what this does and does not pin, established by mutation rather than by
    /// argument. Deleting the debt bookkeeping from the shrink (`state.debt +=
    /// cut - forgotten`) FAILS this test at the "fourth while all three are still
    /// held" assertion — real over-provision. Deleting only the pay-first step from
    /// the grow (minting `add_permits(2)` on top of a 2-unit debt) still PASSES:
    /// each debt unit destroys exactly one permit whenever it is paid, so total
    /// capacity converges to the same 3 either way. Pay-first is therefore chosen
    /// for the interim state — an honest `available_permits` and no needless
    /// acquire→forget→re-queue trips to the back of the FIFO — and no test here
    /// can tell the two apart. Do not read a green run as a pay-first guard.
    #[tokio::test]
    async fn receive_gate_grow_pays_debt_first() {
        let gate = ReceiveGate::new(3);
        let p1 = gate_admit(&gate, "first of three").await;
        let p2 = gate_admit(&gate, "second of three").await;
        let p3 = gate_admit(&gate, "third of three").await;

        gate.set_limit(1); // debt 2 — nothing available to forget
        gate.set_limit(3); // pays the debt down instead of adding permits
        assert_eq!(gate.limit(), 3);

        // Still three in flight against a limit of three: at capacity.
        gate_blocks(&gate, "fourth while all three are still held").await;

        drop(p1);
        drop(p2);
        drop(p3);

        let _a = gate_admit(&gate, "first of three after the round trip").await;
        let _b = gate_admit(&gate, "second of three after the round trip").await;
        let _c = gate_admit(&gate, "third of three after the round trip").await;
        gate_blocks(&gate, "fourth — the round trip minted no surplus").await;
    }

    #[test]
    fn receive_gate_clamps_limit_to_one_through_eight() {
        assert_eq!(ReceiveGate::new(0).limit(), 1, "0 clamps up to 1");
        assert_eq!(ReceiveGate::new(99).limit(), 8, "99 clamps down to 8");

        let gate = ReceiveGate::new(4);
        gate.set_limit(0);
        assert_eq!(gate.limit(), 1, "set_limit clamps up to 1");
        gate.set_limit(usize::MAX);
        assert_eq!(gate.limit(), 8, "set_limit clamps down to 8");
    }

    #[test]
    fn inbound_control_default_gate_limit_is_two() {
        assert_eq!(DEFAULT_MAX_CONCURRENT_RECEIVES, 2);
        assert_eq!(
            InboundControl::new().receive_gate.limit(),
            DEFAULT_MAX_CONCURRENT_RECEIVES,
            "a fresh control gates receives at the shipped default"
        );
        assert_eq!(
            InboundControl::default().receive_gate.limit(),
            DEFAULT_MAX_CONCURRENT_RECEIVES,
            "`new()` and `default()` agree — `new()` still just forwards"
        );
    }
}
