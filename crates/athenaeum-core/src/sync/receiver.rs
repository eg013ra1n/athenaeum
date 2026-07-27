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
    AnnounceFileEntry, FetchEvent, FrameReceipt, NodeId, PackageAnnounce, PackageId, PackageLayout,
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
    /// Cap on simultaneous incoming transfers to start the receiver's
    /// [`ReceiveGate`] at (W2 T2.7) — the host's persisted
    /// `sync.max_concurrent_receives`. A number rather than a callback, but it
    /// rides this struct for the same reason the hooks do: it is per-start
    /// configuration the HOST resolves and the transport layer must not go
    /// looking for. `sync::` sits below `api::` and never touches a
    /// `ServiceContext`, so the settings read stays in
    /// `api::sync::receiver_hooks` where the ctx already is. Absent ⇒
    /// [`DEFAULT_MAX_CONCURRENT_RECEIVES`] (a host that says nothing gets the
    /// shipped cap, which is also what a settings read that FAILS degrades to).
    pub max_concurrent_receives: Option<usize>,
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

/// Apply a host-supplied receive-concurrency cap to a control that has NOT been
/// handed to a receive loop yet (W2 T2.7). `None` — no host value, or a settings
/// read that failed — leaves the gate at [`DEFAULT_MAX_CONCURRENT_RECEIVES`], so
/// a bad settings row degrades the cap, never the startup.
///
/// Called by [`SyncRuntime::ensure_started`] BEFORE
/// [`SyncReceiver::spawn`], which is what makes the persisted value effective for
/// the very first announce rather than from the second one on. Split out so the
/// application is pinned by a unit test — `ensure_started` itself needs a bound
/// [`SharedIrohNode`] and cannot run in one.
fn apply_receive_limit(control: &InboundControl, configured: Option<usize>) {
    let Some(limit) = configured else {
        return;
    };
    control.receive_gate.set_limit(limit);
    tracing::debug!(limit, "receive concurrency cap applied at receiver start");
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
/// wake an in-flight fetch even while that peer's receive lane is still busy
/// awaiting an earlier `handle_announce`. Kept distinct from `cancels`
/// deliberately: a revoke must NOT divert the aborted fetch into the local-decline
/// [`cancel_epilogue`] (which sends an ack) — the ALREADY-QUEUED `RevokeReceived`
/// event drives the reason-honest [`handle_revoke`] bookkeeping (no ack) once the
/// lane drains it next. Both signals share the one `notify` — the select
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
///
/// `queued_announces` (variant B) is a fourth: the announces that have been ROUTED
/// to a peer's lane but not yet processed — see [`note_queued_announce`].
///
/// `parked_for_slot` (variant C) is a fifth: the transfers whose lane HAS picked
/// the announce up and is now blocked on `receive_gate` — see
/// [`note_parked_for_slot`].
///
/// [`note_queued_announce`]: Self::note_queued_announce
/// [`note_parked_for_slot`]: Self::note_parked_for_slot
pub struct InboundControl {
    cancels: std::sync::Mutex<std::collections::HashSet<String>>,
    revoke_aborts: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Announces sitting in a peer's lane queue, keyed `(peer, batch_uuid)` — the
    /// ONLY evidence a second batch from a busy device exists at all, since no
    /// `sync_inbound` row is written until its lane picks the announce up. See
    /// [`note_queued_announce`](Self::note_queued_announce) for the whole contract.
    queued_announces: std::sync::Mutex<HashMap<(NodeId, String), QueuedAnnounce>>,
    /// Wire package ids whose lane is blocked on the [`ReceiveGate`], keyed the way
    /// the row is (`sync_inbound.package_id`). See
    /// [`note_parked_for_slot`](Self::note_parked_for_slot).
    parked_for_slot: std::sync::Mutex<std::collections::HashSet<String>>,
    notify: tokio::sync::Notify,
    /// Concurrency gate for the fetch+ingest phase — see [`ReceiveGate`]. Public
    /// because the receive lanes acquire it directly and the settings command
    /// layer resizes it; it carries no cross-field invariant with the cancel sets.
    pub receive_gate: ReceiveGate,
}

/// One announce parked in a peer's lane queue: everything a "queued behind the
/// current transfer" ghost row needs, captured off the wire announce at ROUTING
/// time (there is no `sync_inbound` row to read it from yet — that is the whole
/// point). Purely in-memory and per-process: a restart drops the lot, which is
/// correct, since a restart also drops the lane queue these describe.
#[derive(Debug, Clone)]
pub struct QueuedAnnounce {
    /// The human batch name from a v2/v3 announce, `None` for a v1/unnamed batch —
    /// the same normalization `handle_announce` applies to `display_name`.
    pub batch_name: Option<String>,
    pub frame_count: u32,
    pub byte_size: u64,
    /// When this batch FIRST queued, RFC3339 (`now_iso`). Not refreshed by a
    /// re-announce of the same key: the ghost dates from when the batch started
    /// waiting, not from the sender's latest retry rung.
    pub first_seen: String,
}

/// One entry of [`InboundControl::queued_announces_snapshot`]: the map key
/// (`peer` + `batch_uuid`) rejoined with its [`QueuedAnnounce`], so a caller can
/// resolve the peer's device name without reaching back into the map.
#[derive(Debug, Clone)]
pub struct QueuedAnnounceEntry {
    pub peer: NodeId,
    pub batch_uuid: String,
    pub queued: QueuedAnnounce,
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
            queued_announces: Default::default(),
            parked_for_slot: Default::default(),
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
    /// its revoke has been drained by the peer's lane (called at `handle_revoke`
    /// entry). By then any fetch the flag needed to abort has already returned —
    /// a revoke and the announce it revokes come from the SAME peer, so they share
    /// one serial lane. A LINGERING entry would wedge a straggler
    /// re-announce of the same wire id — under the Decline Finality Axis a
    /// revoke-cancelled row legitimately resets and re-fetches, and a stale abort
    /// flag would break that fetch on its first poll with no terminal written.
    pub fn clear_revoke_abort(&self, package_id: &str) {
        self.revoke_aborts
            .lock()
            .expect("inbound revoke_aborts mutex poisoned")
            .remove(package_id);
    }

    /// Record that `batch_uuid` from `peer` has been ROUTED to that peer's lane but
    /// not yet processed (variant B). Called from the receiver's lane ROUTER, the
    /// one place that sees an announce before the lane does.
    ///
    /// WHY this exists: the receiver processes one peer's events serially, so a
    /// second batch announced while the first is still fetching sits in the lane
    /// channel with `upsert_inbound_attempt` un-run — NO `sync_inbound` row, and so
    /// nothing at all in the receive-side UI. This map is the only place that batch
    /// is visible, and [`queued_announces_snapshot`](Self::queued_announces_snapshot)
    /// is what turns it into a "queued behind the current transfer" ghost row.
    ///
    /// INSERT-IF-ABSENT, keyed `(peer, batch_uuid)`: a sender re-announces on every
    /// backoff rung while it waits for an ack, so the same batch arrives repeatedly
    /// with a fresh wire `package_id` each time. The durable batch identity is the
    /// key precisely so those collapse into ONE entry — and the first insert's
    /// [`first_seen`](QueuedAnnounce::first_seen) survives, so the ghost dates from
    /// when the batch started waiting rather than from the latest retry.
    ///
    /// The ROUTER gates the insert on the peer authorizer (review fix — the
    /// original ungated design claimed the visible window was bounded by "that
    /// lane's next event", which was wrong: connection admission is wider than
    /// personal authorization, so a verified collab-project member — never
    /// personal-authorized at all — can deliver an announce while its lane is
    /// busy for minutes with a project ingest, rendering a phantom row with
    /// peer-chosen text the lane will silently drop). The lane arm still clears
    /// the entry at its very top, BEFORE its own authorization gate, so even a
    /// peer de-authorized between insert and processing is removed on the pass
    /// that drops it.
    pub fn note_queued_announce(
        &self,
        peer: NodeId,
        batch_uuid: &str,
        batch_name: Option<String>,
        frame_count: u32,
        byte_size: u64,
    ) {
        self.queued_announces
            .lock()
            .expect("inbound queued_announces mutex poisoned")
            .entry((peer, batch_uuid.to_string()))
            .or_insert_with(|| QueuedAnnounce {
                batch_name,
                frame_count,
                byte_size,
                first_seen: super::now_iso(),
            });
    }

    /// Drop `(peer, batch_uuid)`'s queued entry because its lane has STARTED
    /// processing that announce (called at the very top of the lane's
    /// `AnnounceReceived` arm, before any gate — so every way out of that arm
    /// removes it exactly once).
    ///
    /// Entries cannot leak in steady state: every announce the router inserts is
    /// also routed, lanes drain in order, and a lane whose task panicked is re-minted
    /// with the event RESENT (`route_to_lane`). The one honest edge: if the announce
    /// event is itself the one that PANICS its lane mid-handling, its entry lingers —
    /// until the sender's next re-announce of the same batch re-inserts under the
    /// same key and that one processes. Bounded and self-healing, never unbounded
    /// growth.
    pub fn clear_queued_announce(&self, peer: &NodeId, batch_uuid: &str) {
        self.queued_announces
            .lock()
            .expect("inbound queued_announces mutex poisoned")
            .remove(&(*peer, batch_uuid.to_string()));
    }

    /// Every announce currently parked in a lane queue (order unspecified — a
    /// `HashMap` iteration). Read by the status poll to build the receive-side
    /// ghost rows; cheap enough for a 10-second poll (one mutex, a handful of
    /// entries at most).
    pub fn queued_announces_snapshot(&self) -> Vec<QueuedAnnounceEntry> {
        self.queued_announces
            .lock()
            .expect("inbound queued_announces mutex poisoned")
            .iter()
            .map(|((peer, batch_uuid), queued)| QueuedAnnounceEntry {
                peer: *peer,
                batch_uuid: batch_uuid.clone(),
                queued: queued.clone(),
            })
            .collect()
    }

    /// Record that `package_id`'s lane is blocked on the [`ReceiveGate`] waiting for
    /// a free receive slot (variant C). Inserted immediately before the gate wait
    /// begins and removed on EVERY way out of it — always through
    /// [`ParkedForSlotGuard`], never by hand, so the two can't drift apart.
    ///
    /// WHY this exists: a parked transfer's row is already written and sits in
    /// [`InboundState::Announced`] — indistinguishable, in durable state, from one
    /// that just arrived. The receive-side UI therefore showed a bare "announced"
    /// for a transfer whose only remaining obstacle is the concurrency cap. This set
    /// is the live signal that separates the two; the status poll reads it through
    /// [`parked_for_slot_snapshot`](Self::parked_for_slot_snapshot) and relabels
    /// those rows `queued`.
    ///
    /// Keyed on the WIRE package id, unlike variant B's durable-batch keying: this
    /// entry is matched against a `sync_inbound` ROW, and `package_id` is the column
    /// that row is looked up by (it is re-stamped to the current attempt's wire id
    /// by `upsert_inbound_attempt`, which has already run by the time a lane parks).
    /// A re-announce under a fresh wire id is a fresh park of a fresh row; there is
    /// nothing to collapse.
    ///
    /// Personal sync only. `handle_project_announce` also waits on the same gate, but
    /// a project push writes no `sync_inbound` row at all, so an entry for one could
    /// never match anything the mapping reads — see the comment at its `acquire`.
    pub fn note_parked_for_slot(&self, package_id: &str) {
        self.parked_for_slot
            .lock()
            .expect("inbound parked_for_slot mutex poisoned")
            .insert(package_id.to_string());
    }

    /// Drop `package_id`'s parked entry because its lane has left the gate queue —
    /// by winning a permit, or by abandoning it for a decline/revoke/terminal row.
    /// Called from [`ParkedForSlotGuard`]'s `Drop`, which is why no exit path
    /// (including a panic) can leak an entry.
    pub fn clear_parked_for_slot(&self, package_id: &str) {
        self.parked_for_slot
            .lock()
            .expect("inbound parked_for_slot mutex poisoned")
            .remove(package_id);
    }

    /// Every transfer currently waiting for a receive slot. Read by the status poll
    /// to relabel those rows; cheap enough for a 10-second poll (one mutex, at most
    /// a handful of entries).
    pub fn parked_for_slot_snapshot(&self) -> std::collections::HashSet<String> {
        self.parked_for_slot
            .lock()
            .expect("inbound parked_for_slot mutex poisoned")
            .clone()
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

/// RAII marker for "this lane is waiting for a receive slot" (variant C): entering
/// records the package on [`InboundControl::note_parked_for_slot`], dropping clears
/// it.
///
/// A guard rather than paired calls because the wait has two shapes of exit — the
/// permit is won, or [`abandon_parked_receive`] elects to leave the queue (for a
/// revoke, for a row that went terminal underneath the lane, or for a local decline)
/// and the loop `return`s — and a lane that leaked its entry would leave a transfer
/// reading `queued` for the rest of the process's life, including long after it
/// finished. `Drop` covers both, plus any exit added later, by construction; it also
/// covers a panic, which no arrangement of manual clears can.
struct ParkedForSlotGuard<'a> {
    control: &'a InboundControl,
    package_id: &'a str,
}

impl<'a> ParkedForSlotGuard<'a> {
    /// Mark `package_id` parked; the returned guard un-marks it when it goes out of
    /// scope. Insert and clear are one lexical unit — there is no way to do one
    /// without the other.
    fn enter(control: &'a InboundControl, package_id: &'a str) -> Self {
        control.note_parked_for_slot(package_id);
        Self {
            control,
            package_id,
        }
    }
}

impl Drop for ParkedForSlotGuard<'_> {
    fn drop(&mut self) {
        self.control.clear_parked_for_slot(self.package_id);
    }
}

/// Handle to a running [`SyncReceiver`]. Dropping it (or calling
/// [`shutdown`](Self::shutdown)) stops the event loop.
///
/// It owns BOTH background tasks the receiver spawns: the per-peer lane ROUTER
/// (`join`) AND the event-ingress `pump` (B4-fix) that forwards transport events
/// into the router's channel. Both must be aborted on shutdown — the pump is
/// normally parked in `raw_events.recv().await`, so without an explicit abort it
/// would linger blocked on the transport stream after the router stops (B5 §6a).
///
/// The lanes themselves need no handle: they live in a `JoinSet` owned by the
/// router task, so aborting the router aborts every lane in flight with it —
/// unchanged "stop now" semantics from when the router WAS the single serial loop
/// (pinned by `shutdown_aborts_lanes_in_flight`).
pub struct SyncReceiverHandle {
    join: JoinHandle<()>,
    pump: JoinHandle<()>,
}

impl SyncReceiverHandle {
    /// Abort both receiver tasks (lane router + ingress pump) and await their exit.
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
        // transport event from PROCESSING it. Processing is serial WITHIN a peer
        // (its lane — no concurrent `handle_announce`/`handle_revoke` for one peer,
        // which is what the B2 single-writer invariant `upsert_inbound_attempt`
        // relies on), which means a `RevokeReceived` queued behind a
        // currently-running, blocking `handle_announce` fetch on that same lane
        // would otherwise sit unprocessed until the fetch finishes on its own — the
        // opposite of "revoke stops the download promptly". This pump task is never
        // blocked by that fetch: it forwards every event, in order, into a fresh
        // channel the lane router drains, and for an AUTHORIZED peer's
        // `RevokeReceived` it ALSO signals
        // [`InboundControl::request_revoke_abort`] synchronously, right here, the
        // instant the event arrives — which wakes the fetch-abort select loop
        // inside `handle_announce` cross-task (the same `Notify`
        // [`cancel_incoming_package`](crate::api::sync::cancel_incoming_package)
        // already wakes). Note the ORDER: the abort flag is set BEFORE routing, so
        // it reaches an in-flight fetch no matter which lane that fetch is on. The
        // event is still forwarded on so `handle_revoke` performs the reason-honest
        // bookkeeping (terminal state, settle files, staging, tags, history,
        // journal — no ack) once the peer's lane drains it, strictly after the
        // aborted `handle_announce` call has returned. The pump re-runs the SAME H1
        // authorizer the lane's arms use (never trust-then-verify) so an
        // unauthorized peer cannot poison the abort set — belt-and-suspenders in
        // dev-ticket mode, where there is no connect_gate. Unbounded so the pump's
        // `send` never itself blocks on a busy consumer.
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

        let deps = ReceiverLaneDeps {
            store,
            staging_root: staging_root.clone(),
            incoming,
            authorized,
            project_gate,
            announcements_refresher,
            on_project_ingested,
            request_handler,
            control,
            transport: Arc::clone(&transport),
            emitter,
        };

        // Per-peer serial lanes (W2 T2.3). This task is now a ROUTER: it owns no
        // handling of its own, it only fans each event out to the lane of the peer
        // it came from. One lane per peer, each a serial `run_peer_lane` task, so
        // a slow transfer from device A no longer holds device B's events hostage
        // while every peer's own events stay strictly FIFO.
        //
        // WHY per-peer is exactly the right boundary — every key the old global
        // serialization protected is PEER-OWNED, so two lanes can never contend
        // for one:
        //  - inbound rows key on `(peer, batch_uuid)` (`upsert_inbound_attempt`),
        //    so the B2 single-writer invariant holds per lane;
        //  - the revoke-abort / cancel flags name that peer's wire ids;
        //  - staging dirs are `<staging>/<wire_id>` on sender-minted uuids;
        //  - landing trees are `<incoming>/<sender_slug>/…` — with ONE caveat
        //    worth stating, since it is the only genuinely cross-lane key here:
        //    the slug prefers the peer's cached DEVICE NAME, so two devices that
        //    share a name share a parent dir. Still safe — `resolve_landing_dir`'s
        //    check-then-claim (active collision → `_2`/`_3`…) runs entirely under
        //    one `store.lock_conn()` guard, so two lanes serialize on the store
        //    mutex and the second one sees the first one's persisted claim.
        //  - `InboundControl`'s one shared `Notify` wakes every lane's fetch loop,
        //    but each re-checks its OWN package key on wake — already safe by
        //    construction (see `InboundControl`), a spurious wake costs one poll.
        //
        // Lanes are long-lived: one per peer that has ever sent us an event,
        // parked on `recv` when idle (a parked task costs ~nothing, and paired
        // devices are few). Not tearing them down between transfers is deliberate
        // — it is what keeps a peer's ordering across CONSECUTIVE transfers, not
        // just within one.
        //
        // ACCEPTED RISK, named rather than fixed: staging dirs are keyed by the
        // SENDER-minted wire id alone, so an authorized-but-malicious peer that
        // reuses another peer's wire id could collide across lanes. The exposure
        // is pre-existing and not created here (`handle_revoke` already correlates
        // by wire id globally); peer-scoped staging is a named follow-up, out of
        // scope for this task.
        //
        // The ingress pump above is deliberately NOT part of this: it sets the
        // revoke-abort flag BEFORE routing, so a revoke still aborts an in-flight
        // fetch cross-task regardless of which lane the fetch is on, and only the
        // bookkeeping is ordered behind that peer's own announce.
        //
        // A lane that PANICS costs its peer exactly the event it panicked on:
        // `route_to_lane` re-mints the lane and resends the next event rather than
        // dropping it (W2 review — a dropped `RevokeReceived` leaks the pump's abort
        // flag, whose only clear site is inside `handle_revoke`). The panicked event
        // itself is unrecoverable; if it was a revoke, that flag leak persists until
        // the next restart's reconcile — named, not fixed, in `route_to_lane`'s doc.
        let join = tokio::spawn(async move {
            tracing::info!(staging_root = %staging_root.display(), "sync receiver online");
            let mut lanes: HashMap<NodeId, mpsc::UnboundedSender<TransportEvent>> = HashMap::new();
            let mut lane_tasks = tokio::task::JoinSet::new();

            while let Some(ev) = events.recv().await {
                // No peer ⇒ not ours to process (`AckReceived` is the sender half;
                // the `Serve*` variants originate on our own endpoint). This is the
                // old `_ => {}` arm, now stated once in `event_peer`.
                let Some(from) = event_peer(&ev) else {
                    continue;
                };

                // Variant B: an announce is about to go into a lane that may
                // already be busy with an earlier transfer from the same peer, in
                // which case it will sit in that channel with NO `sync_inbound`
                // row — invisible to the receive-side UI until the lane drains.
                // The router is the only component that sees the announce before
                // the lane does, so it is the only place this can be recorded.
                // The lane clears the entry the moment it starts processing it.
                //
                // `AnnounceReceived` ONLY, deliberately: this feeds the PERSONAL
                // transfers list. `ProjectAnnounceReceived` is the collab-exchange
                // path with its own rows and its own surface — out of scope here.
                //
                // AUTHORIZED peers only (review fix). Connection admission is wider
                // than personal authorization — a verified collab-project member can
                // deliver an `Announce3` too, and its lane can be busy for MINUTES
                // with a project ingest — so an ungated insert would render a
                // phantom row with PEER-CHOSEN text (batch name) for a transfer the
                // lane will silently drop. The authorizer's only side effect on
                // refusal is the debounced hub refresh, which is the same kick the
                // lane's own check fires when it drops the announce.
                if let TransportEvent::AnnounceReceived {
                    announce,
                    batch_name,
                    batch_uuid,
                    ..
                } = &ev
                {
                    if (deps.authorized)(&from) {
                        deps.control.note_queued_announce(
                            from,
                            batch_uuid,
                            batch_name.clone().filter(|n| !n.trim().is_empty()),
                            announce.frame_count,
                            announce.byte_size,
                        );
                    }
                }

                route_to_lane(&mut lanes, from, ev, || {
                    let (tx, rx) = mpsc::unbounded_channel::<TransportEvent>();
                    lane_tasks.spawn(run_peer_lane(rx, deps.clone()));
                    tracing::debug!(
                        from = %super::node_id_hex(&from),
                        lanes = lane_tasks.len(),
                        "sync receiver opened a receive lane"
                    );
                    tx
                });
                // Reap eagerly so a lane panic is LOUD when it happens, not only at
                // shutdown (a lane never finishes on its own while we hold its
                // sender, so this only ever yields panicked tasks).
                while let Some(res) = lane_tasks.try_join_next() {
                    report_lane_exit(res);
                }
            }

            // Normal close (the transport stream ended, not an abort): dropping the
            // map closes every lane channel, so each lane drains the work already
            // queued on it and exits — then we wait for them. An ABORT of this task
            // instead drops the `JoinSet` itself, which aborts every lane in flight:
            // the same "stop now" semantics `SyncReceiverHandle::shutdown` has always
            // had.
            drop(lanes);
            while let Some(res) = lane_tasks.join_next().await {
                report_lane_exit(res);
            }
            tracing::info!("sync receiver event stream closed; loop stopping");
        });

        Ok((info, SyncReceiverHandle { join, pump }))
    }
}

/// Everything one receive lane needs to process an event: exactly the values the
/// pre-lane single serial `match` captured, bundled so each lane task can own a
/// clone. Every field is an `Arc` (or the cheap `staging_root` path), so a clone
/// shares the one store / control / transport rather than duplicating state —
/// which is what lets lanes stay independent tasks without splitting ownership.
#[derive(Clone)]
struct ReceiverLaneDeps {
    store: Arc<CatalogSyncStore>,
    staging_root: PathBuf,
    incoming: IncomingResolver,
    authorized: PeerAuthorizer,
    project_gate: Option<ProjectAnnounceGate>,
    announcements_refresher: Option<ProjectAnnouncementsRefresher>,
    on_project_ingested: Option<ProjectIngestedHook>,
    request_handler: Option<ProjectRequestHandler>,
    control: Arc<InboundControl>,
    transport: Arc<dyn SharingTransport>,
    emitter: Arc<dyn ProgressEmitter>,
}

/// Which peer an event belongs to — the lane key. `None` = the receiver does not
/// process this variant at all.
///
/// Deliberately EXHAUSTIVE with no `_` arm: a new [`TransportEvent`] variant must
/// fail to compile here, forcing whoever adds it to decide whether it is
/// peer-scoped work (route it) or not (an explicit `None`). The old loop's silent
/// `_ => {}` is the failure mode this replaces.
fn event_peer(ev: &TransportEvent) -> Option<NodeId> {
    match ev {
        TransportEvent::AnnounceReceived { from, .. }
        | TransportEvent::ProjectAnnounceReceived { from, .. }
        | TransportEvent::ProjectRequestReceived { from, .. }
        | TransportEvent::RevokeReceived { from, .. } => Some(*from),
        // The sender half of the protocol — consumed by `SyncEngine`, never here.
        TransportEvent::AckReceived { .. } => None,
        // Locally-originated serve-side progress (our own endpoint, no peer):
        // routed to the sender engine, never processed by the receiver.
        TransportEvent::ServeProgress { .. } => None,
        TransportEvent::ServeComplete { .. } => None,
        TransportEvent::ServeFileProgress { .. } => None,
    }
}

/// Hand `ev` to `from`'s lane, minting one via `open_lane` when the peer has none
/// yet — and RE-minting, then resending that same event, when the peer's lane
/// turns out to be dead.
///
/// A lane channel can only be closed while the router still holds its sender if
/// the lane task is gone, i.e. it panicked. Re-mint-and-resend (rather than
/// dropping the event) is not politeness — it is what keeps the self-heal from
/// leaking state:
///
/// - the ingress pump sets `request_revoke_abort(wire_id)` BEFORE routing, and the
///   ONLY production site that clears it is inside `handle_revoke`. A dropped
///   `RevokeReceived` therefore leaks that flag until the next restart's reconcile:
///   the row never terminalizes, and a straggler re-announce of the same wire id
///   breaks on its first fetch poll with no terminal written and no ack — the very
///   wedge [`InboundControl::clear_revoke_abort`]'s doc warns about.
/// - an announce would survive being dropped (the sender re-announces on ack
///   timeout), so the failure is silent and ASYMMETRIC across event kinds. Resending
///   removes the asymmetry instead of reasoning about it per variant.
///
/// The resend cannot fail: the fresh lane's receiver is alive (its task has been
/// spawned but not yet polled), and the channel is unbounded. It is still handled
/// rather than `unwrap`ped — the router must never panic, or one bad event takes
/// down every peer's routing. Should the fresh lane panic on this same event, the
/// peer's NEXT event repeats the cycle: bounded, and loud (`report_lane_exit`).
///
/// What this does NOT close, stated plainly: the event that PANICKED the lane is
/// lost with the task. If that event was itself a revoke (i.e. `handle_revoke`
/// panicked mid-handling), the abort flag still leaks and still heals only at the
/// next restart's reconcile. That case is rarer — it needs a panic inside the
/// revoke handler, not merely a dead lane — and is deliberately out of scope here.
///
/// `open_lane` is a closure so the routing decision is testable without spawning
/// real lane tasks (`a_dead_lane_is_reminted_and_the_event_resent`); production
/// passes one that spawns [`run_peer_lane`] into the router's `JoinSet`.
fn route_to_lane<F>(
    lanes: &mut HashMap<NodeId, mpsc::UnboundedSender<TransportEvent>>,
    from: NodeId,
    ev: TransportEvent,
    mut open_lane: F,
) where
    F: FnMut() -> mpsc::UnboundedSender<TransportEvent>,
{
    let lane = lanes.entry(from).or_insert_with(&mut open_lane);
    let ev = match lane.send(ev) {
        Ok(()) => return,
        // `SendError` hands the event back — that is what makes the resend possible.
        Err(mpsc::error::SendError(ev)) => ev,
    };

    tracing::warn!(
        from = %super::node_id_hex(&from),
        "sync receiver lane is gone (its task panicked); re-minting it and resending the event"
    );
    let fresh = open_lane();
    let send_result = fresh.send(ev);
    lanes.insert(from, fresh);
    if send_result.is_err() {
        tracing::error!(
            from = %super::node_id_hex(&from),
            "sync receiver could not deliver into a freshly minted lane; event dropped"
        );
    }
}

/// One peer's serial lane: drains its channel in order, awaiting each event's
/// handling before the next. FIFO within a peer is a hard guarantee (a revoke
/// must do its bookkeeping strictly after the announce it revokes has returned);
/// FIFO ACROSS peers is deliberately given up — that is the point of lanes.
async fn run_peer_lane(mut rx: mpsc::UnboundedReceiver<TransportEvent>, deps: ReceiverLaneDeps) {
    while let Some(ev) = rx.recv().await {
        process_receiver_event(ev, &deps).await;
    }
}

/// Log a finished lane task. A lane exits cleanly only when its channel closes
/// (receiver shutting down); anything else is a panic that silently kills that
/// peer's processing until the next event reopens a lane, so it is logged at
/// `error` rather than swallowed by the `JoinSet`.
fn report_lane_exit(res: Result<(), tokio::task::JoinError>) {
    if let Err(e) = res {
        if e.is_panic() {
            tracing::error!(
                error = %e,
                "sync receiver lane PANICKED — that peer's events stopped being processed"
            );
        }
    }
}

/// Handle one routed transport event. Lifted verbatim out of the pre-lane serial
/// loop's `match`; the only change is that its inputs arrive through
/// [`ReceiverLaneDeps`] instead of being captured by the loop's closure.
async fn process_receiver_event(ev: TransportEvent, deps: &ReceiverLaneDeps) {
    let ReceiverLaneDeps {
        store,
        staging_root,
        incoming,
        authorized,
        project_gate,
        announcements_refresher,
        on_project_ingested,
        request_handler,
        control,
        transport,
        emitter,
    } = deps;

    match ev {
        // `batch_uuid` (spec §D1/§D2) is the durable per-transfer identity
        // the receiver keys ONE inbound row on across every attempt (B4):
        // v3 → the sender's package-dir basename; v1/v2 → the wire package
        // id (B1 fallback), which reproduces today's per-attempt rows.
        // `layout` (mirror-hierarchy) selects WHERE this transfer lands: a
        // `Mirror` announce skips the batch level entirely (see the landing block
        // in `handle_announce`). v1/v2/v3 announces decode as `Batch`, so the
        // pre-mirror behavior is byte-identical.
        TransportEvent::AnnounceReceived {
            from,
            announce,
            batch_name,
            batch_uuid,
            files,
            layout,
        } => {
            // Variant B: this announce has LEFT the lane queue — it is
            // being processed right now, so it must stop rendering as a
            // ghost row (the durable `sync_inbound` row the rest of this
            // arm writes takes over). Cleared at the VERY TOP, before the
            // authorization gate and before every other early return
            // (unsafe package id, pure-replay), so exactly one point
            // covers every way out of this arm — a per-branch clear would
            // be one `return` away from a permanent ghost.
            control.clear_queued_announce(&from, &batch_uuid);
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
                return;
            }
            // Task 4.1 (candidate (a)): time each inline announce
            // handling. `announce` is moved into the call, so capture
            // the package id first; `from` (a `NodeId`) is `Copy`.
            // Instrumentation only — no behavior change.
            let announce_started = std::time::Instant::now();
            let package_id_for_log = announce.package_id.0.clone();
            if let Err(e) = handle_announce(
                store,
                staging_root,
                incoming,
                transport.as_ref(),
                Arc::clone(emitter),
                control,
                from,
                announce,
                batch_name,
                batch_uuid,
                files,
                layout,
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
                return;
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
                return;
            }
            if let Err(e) = handle_project_announce(
                store,
                staging_root,
                transport.as_ref(),
                emitter.as_ref(),
                // Only the gate, not the whole control: the project path has no
                // cancel/revoke surface of its own, so it borrows the one signal it
                // actually uses.
                &control.receive_gate,
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
        } => match request_handler {
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
                return;
            }
            handle_revoke(
                store,
                transport.as_ref(),
                emitter.as_ref(),
                control,
                staging_root,
                from,
                &package_id,
                reason,
            )
            .await;
        }
        // Never routed to a lane (`event_peer` returns `None` for these), so this
        // arm is unreachable in practice — kept total rather than `unreachable!`
        // so a routing mistake degrades to a no-op instead of killing the lane.
        TransportEvent::AckReceived { .. }
        | TransportEvent::ServeProgress { .. }
        | TransportEvent::ServeComplete { .. }
        | TransportEvent::ServeFileProgress { .. } => {}
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
    layout: PackageLayout,
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

    // ── Receive gate (W2 T2.4) ──────────────────────────────────────────────
    //
    // THIS line is where "this transfer will actually move bytes" becomes true, and
    // that is why the permit is taken HERE and nowhere earlier. Everything above
    // returns without touching the network payload or the disk: the pure-replay
    // guard, the declined-final resend seeding, the post-upsert ack replay, and the
    // cancel/decline epilogue diversion. Those paths must NEVER wait on a permit —
    // a receiver sitting at its concurrency cap still has to re-ack a replayed
    // package instantly (otherwise a benign lost-ack retry turns into minutes of
    // silence and another ack-timeout re-announce on the sender) and bounce a
    // declined one instantly (a decline is final; making the sender wait for a
    // fetch slot to be told "no" is the exact opposite of what the cap is for).
    //
    // The permit is held to the end of the function — fetch, ingest AND ack all run
    // under it. Ingest is the disk-heavy half, so releasing after the fetch would
    // cap the wrong stage.
    //
    // The WAIT IS INTERRUPTIBLE (W2 review). A bare `acquire().await` was wrong in
    // two ways that only show up at the seam, both fixed by leaving the queue rather
    // than merely declining to fetch once the permit finally arrives:
    //
    //  1. A parked transfer sits `announced`, and `cancel_incoming_package` reads
    //     that state as "no live fetch to interrupt", so it stamps the row terminal
    //     ITSELF. The parked lane then woke into the unconditional
    //     `set_inbound_state(Fetching)` below and RESURRECTED a terminal row —
    //     `fetching` carrying both `declined_at` and the `finished_at` of the
    //     terminal it overwrote. Worse, it only re-closed if `cancel_epilogue`
    //     succeeded, and that epilogue's `fetch_manifest` propagates with `?`: a
    //     sender that left after the decline stranded the row at `fetching` forever,
    //     where `delete_transfer_history` refuses it (non-terminal) and
    //     `cancel_incoming_package` will not re-stamp it. Unclearable short of a
    //     restart.
    //  2. This lane owns its peer's FIFO channel while it waits, so a
    //     `RevokeReceived` for the PARKED transfer queued behind it and the
    //     sender-cancel terminal waited for a permit — the head-of-line block T2.3
    //     removed, reintroduced for every peer beyond the cap.
    //
    // So: re-check the abort signals on every wake and abandon the queue outright.
    // `request_cancel` and `request_revoke_abort` both `notify_waiters()`, which
    // wakes only waiters registered AT THAT MOMENT (no stored permit), so the
    // `Notified` is enabled BEFORE the flags are read — the same ordering the fetch
    // select loop uses further down. The acquire future is pinned ACROSS wakes so a
    // spurious wake (a cancel for some other package) does not cost this lane its
    // place in the semaphore's FIFO queue.
    let _receive_permit = {
        // Variant C: while this block runs, the row sits `announced` with nothing in
        // durable state to say it is merely waiting its turn. The marker is the live
        // signal the status poll relabels `queued` on, and its scope is EXACTLY this
        // block — entered before the acquire is first polled, dropped both when the
        // permit is won and when the re-check below abandons the queue. Winning the
        // permit therefore un-marks the transfer BEFORE the post-acquire guard and
        // the `Fetching` stamp, which is right: it is no longer waiting for a slot,
        // it has one.
        let _parked = ParkedForSlotGuard::enter(control, &package_id);
        let acquire_fut = control.receive_gate.acquire();
        tokio::pin!(acquire_fut);
        loop {
            let notified = control.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();
            if abandon_parked_receive(store, control, &package_id, peer_device.as_str(), "parked") {
                return Ok(());
            }
            tokio::select! {
                biased;
                permit = &mut acquire_fut => break permit,
                // Woken by a cancel or revoke abort (possibly for another package);
                // loop back and let the re-check at the top decide.
                _ = &mut notified => {}
            }
        }
    };
    // Post-acquire guard: the same decision once more, because a flag can land in
    // the gap between the last wake and `acquire` resolving, and this is the last
    // moment before the `Fetching` stamp below makes the row non-terminal again.
    // Returning here drops the permit we just won — correct: a transfer nobody wants
    // any more must hand its slot straight back.
    if abandon_parked_receive(
        store,
        control,
        &package_id,
        peer_device.as_str(),
        "admitted",
    ) {
        return Ok(());
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
            // reason-honest bookkeeping the moment this peer's lane — the same one
            // running this call, so strictly after it returns — drains it next.
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
    //
    // Mirror layout (spec 2026-07-27): NO batch landing level — land under
    // `<incoming_root>/<sender_slug>` via the pre-v2 (v1) path, which IS the
    // stable capture-mirror tree. resolve_landing_dir is deliberately not
    // called: concurrent mirror transfers from one sender must share the tree,
    // and per-file collisions are handled by ingest's unique_path.
    let landing_override: Option<PathBuf> = match layout {
        PackageLayout::Mirror => None,
        PackageLayout::Batch => match effective_name.as_deref().and_then(sanitize_batch_slug) {
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
        },
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

/// Should a transfer waiting for (or just admitted through) the receive gate
/// ABANDON the queue instead of fetching? `true` ⇒ the caller returns `Ok(())`
/// immediately, holding no permit and touching no row it does not own.
///
/// Called from two places in [`handle_announce`] with the same semantics: on every
/// wake while parked (`stage = "parked"`, so the lane leaves the queue instead of
/// waiting out a permit it no longer needs) and once more right after the permit is
/// won (`stage = "admitted"`, closing the window where a flag lands between the last
/// wake and `acquire` resolving). Both call sites are BEFORE the `Fetching` stamp,
/// which is what keeps a terminal row terminal.
///
/// Three exits, in priority order:
///
/// 1. **Sender revoke** (`revoke_aborts`, set cross-task by the ingress pump). Leave
///    the row ALONE and return: the `RevokeReceived` that set the flag is already
///    queued on this same peer's lane and does the full reason-honest bookkeeping
///    (terminal state, files, staging, tags, history, journal — no ack) the moment
///    this call returns. Returning WITHOUT a permit is the point: the bookkeeping no
///    longer waits for a receive slot.
/// 2. **Row already terminal.** Somebody else (the decline command) closed it while
///    this lane was parked. Never overwrite it — that write is the resurrection bug.
/// 3. **Local decline** whose stamp has not landed yet. `cancel_incoming_package`
///    signals before it writes, so the flag is briefly visible while the row is
///    still `announced`. Return anyway and write NOTHING: that command owns this
///    terminal (`stamp_now`: `Some(Announced) => true`, and it only ever signals
///    when a control exists, so the stamp always follows), and it is a NAMED
///    EXEMPTION from the "every terminal writer announces" invariant because the
///    user just performed the action — a `sync-finished` from here would be the
///    duplicate/false notification that exemption exists to prevent. Do not "fix"
///    this by adding a state write.
///
/// Reaching case 2 or 3 at all means the flag landed DURING the wait: the same
/// `is_cancelled` check runs before the gate (the cancel-epilogue diversion), so a
/// transfer that was already declined never gets here.
///
/// **Deliberately NOT running [`cancel_epilogue`]** — that is what removes the
/// wedge, and it is a decision, not an omission. The epilogue fetches the manifest
/// to build its Cancelled receipts, so running it here would (a) require the permit
/// this fn exists to give up, re-serializing declines behind the cap, and (b) put a
/// `?`-propagating network call on the path of a row someone else already
/// terminalized. The sender still learns: it never got an ack, so under
/// delivery-forever it re-announces, and that announce hits the declined-final
/// bounce ABOVE the gate — where the epilogue runs against an already-terminal row,
/// so a failed manifest fetch cannot strand it. The only cost is that a sender which
/// never re-announces leaves this row without per-frame Cancelled receipts/history;
/// a terminal, deletable, honestly-labelled row is strictly better than the
/// unclearable `fetching` one that alternative produced.
fn abandon_parked_receive(
    store: &Arc<CatalogSyncStore>,
    control: &InboundControl,
    package_id: &str,
    peer_device: &str,
    stage: &str,
) -> bool {
    if control.is_revoke_abort_requested(package_id) {
        tracing::info!(
            package_id = %package_id,
            peer_device = %peer_device,
            stage,
            "sync receiver left the receive queue for a sender revoke; its queued revoke does the bookkeeping"
        );
        return true;
    }
    let state = {
        let conn = store.lock_conn();
        get_inbound(&conn, package_id)
            .unwrap_or_else(|e| {
                // Never swallow: a read failure here would otherwise silently look
                // like "row not terminal" and let the fetch proceed over a decline.
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "read inbound row while gated failed");
                None
            })
            .map(|r| r.state)
    };
    if state.map(|s| s.is_terminal()).unwrap_or(false) {
        tracing::info!(
            package_id = %package_id,
            peer_device = %peer_device,
            stage,
            state = ?state,
            "sync receiver left the receive queue: the transfer was closed while it waited"
        );
        return true;
    }
    if control.is_cancelled(package_id) {
        // No state write here on purpose — see the doc comment: the decline command
        // owns this terminal and its stamp always follows its signal.
        tracing::info!(
            package_id = %package_id,
            peer_device = %peer_device,
            stage,
            "sync receiver left the receive queue for a local decline"
        );
        return true;
    }
    false
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
    //    UPDATE = one commit on the peer's lane; rotating to the already-current
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
/// Lane note (W2 T2.3): `RevokeReceived` and `AnnounceReceived` from ONE peer share
/// that peer's serial lane, so a revoke is processed only between that peer's own
/// announces — it can never run concurrently with the fetch it revokes. It CAN run
/// while a DIFFERENT peer is mid-fetch, which is safe because everything this fn
/// touches is owned by the revoking peer: its own `sync_inbound` row (keyed
/// `(peer, batch_uuid)`), that row's file rows, its staging dir and its in-flight
/// tags. This fn touches NEITHER `InboundControl` signal: the in-flight abort was already
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
    // `request_revoke_abort` signal, and by the time this fn runs on the revoking
    // peer's own lane that fetch has been dealt with (it either aborted or, if it
    // was still parked on the receive gate, abandoned the queue outright); every
    // entry in `cancels` must stay decline-originated so the epilogue's
    // `declined_at` stamp is sound.

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
    receive_gate: &ReceiveGate,
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

    // Receive gate (W2 T2.4) — same placement rule as personal sync: taken only
    // once this announce is committed to moving bytes, i.e. AFTER both cheap
    // fail-closed gates above (the unknown-hub-row drop, which may poll the hub, and
    // the wire-id validation) and before the fetch. A project push shares the one
    // disk with personal transfers, so it shares the one cap; held through ingest and
    // ack, released on return.
    //
    // The wait is bare here, unlike personal sync's interruptible one: a project
    // push has no cancel/revoke surface of its own (no `sync_inbound` row, no
    // `InboundControl` signal keyed on it), so there is nothing a parked lane could
    // re-check. NAMED FOLLOW-UP, not fixed here (W2 review, Minor): the permit is
    // held across `transport.fetch` with no abort path at all, so a stalled project
    // push occupies a receive slot until the transport itself gives up.
    //
    // For the same reason it carries no variant-C parked marker: that marker exists
    // to relabel a `sync_inbound` row's display state, and a project push has no such
    // row for an entry to ever match. It would also need `InboundControl` threaded in
    // here, which this handler deliberately does not take — only the gate. A project
    // push waiting for a slot is invisible in the Transfers UI because a project push
    // is invisible in the Transfers UI, which is the collab surface's own question.
    let _receive_permit = receive_gate.acquire().await;

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
        // W2 T2.7: the persisted receive-concurrency cap the host resolved, applied
        // BEFORE the loop spawns — so the first announce is already gated at the
        // operator's number, not at the default until someone re-saves the setting.
        apply_receive_limit(&control, hooks.max_concurrent_receives);

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
    use crate::sharing::types::{PackageAnnounce, PackageId, PackageLayout};
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
            PackageLayout::Batch,
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
            .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
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
            .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
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
            .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
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
            .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
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
                PackageLayout::Batch,
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
            .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
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
            .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
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
            .announce(
                receiver_node,
                &announce,
                "M31 Lights",
                "",
                &files,
                PackageLayout::Batch,
            )
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

    /// Build a mirror-layout fixture package in `root/<dir_name>` from `specs`
    /// (`rel_path`, `frame_uuid`, pixel value — a distinct value gives distinct
    /// bytes, hence a distinct xxh3, so ingest's content dedup lets it travel).
    /// Returns `(pkg_dir, announce, files)` like [`build_v2_fixture`], but
    /// parameterized so one test can drive SEVERAL sequential sends.
    fn build_mirror_fixture(
        root: &std::path::Path,
        dir_name: &str,
        specs: &[(&str, &str, f32)],
    ) -> (std::path::PathBuf, PackageAnnounce, Vec<AnnounceFileEntry>) {
        use crate::models::{Frame, ImageType};
        use crate::package::{ManifestRecord, PayloadKind, MANIFEST_VERSION};

        let src_dir = root.join(format!("{dir_name}-src"));
        std::fs::create_dir_all(&src_dir).unwrap();
        let mut entries = Vec::new();
        let mut items = Vec::new();
        for (rel, uuid, val) in specs {
            let src = src_dir.join(uuid); // unique flat source name
            crate::fits_writer::write_fits_f32(&src, 4, 4, 1, &[*val; 16], &[]).unwrap();
            let byte_size = std::fs::metadata(&src).unwrap().len();
            let xxh3 = crate::package::xxh3_full_file(&src).unwrap();
            let frame = Frame {
                object: Some("M31".to_string()),
                imagetyp: Some(ImageType::Light),
                naxis1: Some(4),
                naxis2: Some(4),
                uuid: Some((*uuid).to_string()),
                updated_at: Some("2026-01-16T10:00:00.000Z".to_string()),
                ..Default::default()
            };
            let record = ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: (*uuid).to_string(),
                origin_catalog_uuid: "catalog-uuid".to_string(),
                origin_device: "aa".repeat(32),
                payload_kind: PayloadKind::RawFrame,
                rel_path: (*rel).to_string(),
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
        let pkg_dir = root.join(dir_name);
        let announce = crate::package::write_package(&pkg_dir, items).unwrap();
        (pkg_dir, announce, entries)
    }

    /// Mirror layout (mirror-hierarchy T4): two SEQUENTIAL mirror sends from one
    /// sender land in ONE stable tree (no batch level, adjacent files) even though
    /// each announce carries its own batch name + uuid, the inbound rows persist NO
    /// landing_dir (v1-style), and a changed-content re-send of an existing
    /// rel_path lands collision-suffixed `_2` instead of overwriting.
    #[tokio::test]
    async fn mirror_layout_lands_adjacent_across_batches_and_suffixes_collisions() {
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

        // Every landed regular file under the incoming root, as paths RELATIVE to
        // it — the shape the landing contract is stated in.
        let landed_rel = || -> Vec<std::path::PathBuf> {
            let mut v: Vec<std::path::PathBuf> = walkdir::WalkDir::new(&incoming)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .map(|e| e.path().strip_prefix(&incoming).unwrap().to_path_buf())
                .collect();
            v.sort();
            v
        };

        // 1. Batch A: two files under one capture-tree dir, announced Mirror with a
        //    batch NAME and UUID of its own (a mirror send still carries both — the
        //    layout, not their absence, is what must suppress the batch level).
        let (dir_a, ann_a, files_a) = build_mirror_fixture(
            tmp.path(),
            "pkg-mirror-a",
            &[
                ("M31/L_0001.fits", "frame-mirror-a1", 0.25),
                ("M31/L_0002.fits", "frame-mirror-a2", 0.5),
            ],
        );
        sender.serve(&ann_a, &dir_a, None).await.unwrap();
        sender
            .announce(
                receiver_node,
                &ann_a,
                "Night A",
                "batch-mirror-a",
                &files_a,
                PackageLayout::Mirror,
            )
            .await
            .unwrap();
        let row_a = poll_inbound(&store, &ann_a.package_id.0, InboundState::Done).await;

        // 2. Batch B: a THIRD file in the same capture-tree dir, a separate transfer.
        let (dir_b, ann_b, files_b) = build_mirror_fixture(
            tmp.path(),
            "pkg-mirror-b",
            &[("M31/L_0003.fits", "frame-mirror-b1", 0.75)],
        );
        sender.serve(&ann_b, &dir_b, None).await.unwrap();
        sender
            .announce(
                receiver_node,
                &ann_b,
                "Night B",
                "batch-mirror-b",
                &files_b,
                PackageLayout::Mirror,
            )
            .await
            .unwrap();
        let row_b = poll_inbound(&store, &ann_b.package_id.0, InboundState::Done).await;

        // 3. All three files sit ADJACENT in one dir — `<sender_slug>/M31/…`, three
        //    path components, no batch level from either transfer.
        let rels = landed_rel();
        assert_eq!(rels.len(), 3, "all three files landed: {rels:?}");
        for rel in &rels {
            let comps: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
            assert_eq!(
                comps.len(),
                3,
                "mirror lands <sender_slug>/<rel_path> — no batch level: {rel:?}"
            );
            assert_eq!(
                comps[1], "M31",
                "the sender's own tree is preserved: {rel:?}"
            );
            for c in &comps {
                for forbidden in [
                    "Night A",
                    "Night B",
                    "Night_A",
                    "Night_B",
                    "batch-mirror-a",
                    "batch-mirror-b",
                ] {
                    assert_ne!(
                        c, forbidden,
                        "no path component may carry a batch name/uuid: {rel:?}"
                    );
                }
            }
        }
        let names: Vec<String> = rels
            .iter()
            .map(|r| r.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "L_0001.fits".to_string(),
                "L_0002.fits".to_string(),
                "L_0003.fits".to_string()
            ],
            "both batches' files are adjacent in one dir"
        );

        // 4. Neither row persisted a landing dir — a mirror transfer never claims one.
        assert!(
            row_a.landing_dir.is_none(),
            "a mirror announce resolves no batch landing dir: {:?}",
            row_a.landing_dir
        );
        assert!(
            row_b.landing_dir.is_none(),
            "a mirror announce resolves no batch landing dir: {:?}",
            row_b.landing_dir
        );

        // 5. Batch C re-sends an EXISTING rel_path with different bytes (fresh uuid
        //    + a different pixel value, so neither dedup rung catches it): it lands
        //    `_2`-suffixed and the original file is byte-unchanged.
        let m31_dir = incoming.join(rels[0].parent().unwrap());
        let original = std::fs::read(m31_dir.join("L_0001.fits")).unwrap();
        let (dir_c, ann_c, files_c) = build_mirror_fixture(
            tmp.path(),
            "pkg-mirror-c",
            &[("M31/L_0001.fits", "frame-mirror-c1", 0.125)],
        );
        sender.serve(&ann_c, &dir_c, None).await.unwrap();
        sender
            .announce(
                receiver_node,
                &ann_c,
                "Night C",
                "batch-mirror-c",
                &files_c,
                PackageLayout::Mirror,
            )
            .await
            .unwrap();
        poll_inbound(&store, &ann_c.package_id.0, InboundState::Done).await;

        let collided = m31_dir.join("L_0001_2.fits");
        assert!(
            collided.exists(),
            "a same-rel_path re-send lands collision-suffixed: {:?}",
            landed_rel()
        );
        assert_eq!(
            std::fs::read(m31_dir.join("L_0001.fits")).unwrap(),
            original,
            "the original file is never overwritten"
        );
        assert_ne!(
            std::fs::read(&collided).unwrap(),
            original,
            "the suffixed file carries the NEW bytes"
        );
        assert_eq!(landed_rel().len(), 4, "nothing else moved or vanished");
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
            .announce(receiver_node, &announce, "", "", &[], PackageLayout::Batch)
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
            .announce(
                receiver_node,
                &announce,
                "M31 Lights",
                "",
                &files,
                PackageLayout::Batch,
            )
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
                PackageLayout::Batch,
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
                PackageLayout::Batch,
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
            .announce(
                receiver_node,
                &announce1,
                "M31 Lights",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce2,
                "M31 Lights",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce1,
                "M31 Lights",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce2,
                "M31 Lights",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce1,
                "Declined",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce2,
                "Declined",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce2,
                "M31 Lights",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce1,
                "Straggler",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce2,
                "M31 Lights",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
            .announce(
                receiver_node,
                &announce3,
                "M31 Lights",
                BATCH,
                &files,
                PackageLayout::Batch,
            )
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
    /// on BOTH the ingress pump (no `request_revoke_abort`) AND the peer's lane (no
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
    /// behind the in-progress, blocking `handle_announce` call of the SAME peer
    /// (one serial lane, by design — a revoke and the announce it revokes always
    /// share one) and only gets processed (as a no-op, on the already-`Done` row)
    /// once the fetch completes naturally — exactly the bug the review flagged.
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
            .announce(
                receiver_node,
                &announce,
                "Live Batch",
                "batch-live",
                &files,
                PackageLayout::Batch,
            )
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

    /// The startup half of `sync.max_concurrent_receives` (W2 T2.7). This is the
    /// REAL function `ensure_started` calls on the control it is about to hand
    /// the receive loop — pinned here rather than through `ensure_started`
    /// itself, which needs a bound `SharedIrohNode` (a real endpoint + relay
    /// resolution) and so cannot run in a unit test. What that leaves untested
    /// is one call line inside `ensure_started`; both halves of the behavior
    /// (the settings read → `Option<usize>` carrier, and the carrier → gate
    /// application below) are pinned by real functions.
    #[test]
    fn apply_receive_limit_absent_keeps_the_shipped_default() {
        let control = InboundControl::new();
        apply_receive_limit(&control, None);
        assert_eq!(
            control.receive_gate.limit(),
            DEFAULT_MAX_CONCURRENT_RECEIVES,
            "no host-supplied value ⇒ the shipped default stands"
        );
    }

    #[test]
    fn apply_receive_limit_applies_and_clamps_a_configured_value() {
        let control = InboundControl::new();
        apply_receive_limit(&control, Some(5));
        assert_eq!(control.receive_gate.limit(), 5, "the persisted value wins");

        // Defense in depth: the settings getter already clamps, and so does the
        // gate — a value that somehow arrives out of range still lands in range
        // rather than deadlocking the receiver (0) or unbounding it.
        let control = InboundControl::new();
        apply_receive_limit(&control, Some(0));
        assert_eq!(control.receive_gate.limit(), 1);
        let control = InboundControl::new();
        apply_receive_limit(&control, Some(99));
        assert_eq!(control.receive_gate.limit(), 8);
    }

    // ─── Per-peer receive lanes (W2 T2.3) ───────────────────────────────────

    /// The four peer-scoped variants route to their sender's lane, and a
    /// locally-originated one does not route at all. The REAL guard against a
    /// future variant slipping through unrouted is [`event_peer`]'s wildcard-free
    /// match (a new variant fails to compile there); this pins the mapping those
    /// arms produce today, so a lane key can never be quietly re-pointed at
    /// something other than the sending peer.
    #[test]
    fn event_peer_covers_every_processed_variant() {
        let from: NodeId = [9u8; 32];
        let announce = PackageAnnounce {
            package_id: PackageId("pkg".into()),
            root_hash: "0".repeat(64),
            byte_size: 0,
            frame_count: 1,
        };

        assert_eq!(
            event_peer(&TransportEvent::AnnounceReceived {
                from,
                announce: announce.clone(),
                batch_name: None,
                batch_uuid: "batch".into(),
                files: None,
                layout: PackageLayout::Batch,
            }),
            Some(from)
        );
        assert_eq!(
            event_peer(&TransportEvent::ProjectAnnounceReceived {
                from,
                project_id: "proj".into(),
                package_id: "hub-pkg".into(),
                announce,
            }),
            Some(from)
        );
        assert_eq!(
            event_peer(&TransportEvent::ProjectRequestReceived {
                from,
                project_id: "proj".into(),
                package_id: "hub-pkg".into(),
            }),
            Some(from)
        );
        assert_eq!(
            event_peer(&TransportEvent::RevokeReceived {
                from,
                package_id: PackageId("pkg".into()),
                reason: RevokeReason::Cancelled,
            }),
            Some(from)
        );

        // The sender half of the protocol: carries a `from`, but the receiver has
        // no work for it — it must NOT open a lane.
        assert_eq!(
            event_peer(&TransportEvent::AckReceived {
                from,
                package_id: PackageId("pkg".into()),
                receipts: vec![],
            }),
            None
        );
        // Locally-originated serve progress: no peer at all.
        assert_eq!(
            event_peer(&TransportEvent::ServeComplete {
                package_id: PackageId("pkg".into()),
            }),
            None
        );
    }

    /// A lane whose task has PANICKED must not cost the event that discovers it.
    /// The router re-mints the lane and resends that very event into the fresh one.
    ///
    /// Why this is not cosmetic (W2 review): the ingress pump sets
    /// `request_revoke_abort(wire_id)` BEFORE routing, and the only production site
    /// that ever clears it is inside `handle_revoke`. Dropping a `RevokeReceived`
    /// therefore leaks the abort flag until the next restart's reconcile — the row
    /// never terminalizes, and a straggler re-announce of that wire id breaks on its
    /// first fetch poll with no terminal and no ack (exactly the wedge
    /// `clear_revoke_abort`'s own doc warns about). An announce would have been
    /// re-sent by the peer on ack timeout; a revoke has no such retry.
    ///
    /// `open_lane` is injected so this pins the ROUTING decision without spawning
    /// real lane tasks: each "lane" here is a bare channel whose receiving half the
    /// test keeps, so "which lane got the event" is directly observable, and
    /// dropping a receiver is a faithful stand-in for a panicked lane task (that is
    /// precisely what a panic does to the channel).
    #[test]
    fn a_dead_lane_is_reminted_and_the_event_resent() {
        let from: NodeId = [3u8; 32];
        let mut lanes: HashMap<NodeId, mpsc::UnboundedSender<TransportEvent>> = HashMap::new();
        // Every lane ever minted, in order; `None` = its task is gone (panicked).
        let mut minted: Vec<Option<mpsc::UnboundedReceiver<TransportEvent>>> = Vec::new();

        fn revoke(wire: &str) -> TransportEvent {
            TransportEvent::RevokeReceived {
                from: [3u8; 32],
                package_id: PackageId(wire.into()),
                reason: RevokeReason::Cancelled,
            }
        }
        fn wire_of(ev: &TransportEvent) -> String {
            match ev {
                TransportEvent::RevokeReceived { package_id, .. } => package_id.0.clone(),
                other => panic!("unexpected event: {other:?}"),
            }
        }

        // 1. First event from this peer: mint lane #0 and deliver.
        route_to_lane(&mut lanes, from, revoke("wire-1"), || {
            let (tx, rx) = mpsc::unbounded_channel();
            minted.push(Some(rx));
            tx
        });
        assert_eq!(
            minted.len(),
            1,
            "the peer's first event opens exactly one lane"
        );
        let got = minted[0]
            .as_mut()
            .unwrap()
            .try_recv()
            .expect("lane #0 got it");
        assert_eq!(wire_of(&got), "wire-1");

        // 2. The lane task panics — its receiving half goes away while the router
        //    still holds the sender. The next event must NOT be lost.
        minted[0] = None;
        route_to_lane(&mut lanes, from, revoke("wire-2"), || {
            let (tx, rx) = mpsc::unbounded_channel();
            minted.push(Some(rx));
            tx
        });
        assert_eq!(minted.len(), 2, "the dead lane is re-minted on the spot");
        let got = minted[1].as_mut().unwrap().try_recv().expect(
            "the revoke that found the lane dead is RESENT into the fresh lane, not dropped",
        );
        assert_eq!(
            wire_of(&got),
            "wire-2",
            "the resent event is the same one, not a replacement"
        );
        assert_eq!(lanes.len(), 1, "the fresh sender replaced the dead entry");

        // 3. The peer keeps its new lane — a healthy lane is never re-minted.
        route_to_lane(&mut lanes, from, revoke("wire-3"), || {
            let (tx, rx) = mpsc::unbounded_channel();
            minted.push(Some(rx));
            tx
        });
        assert_eq!(minted.len(), 2, "no needless mint while the lane is alive");
        let got = minted[1].as_mut().unwrap().try_recv().unwrap();
        assert_eq!(wire_of(&got), "wire-3");
    }

    /// Build a package fixture of `count` distinct FITS payloads under
    /// `root/<tag>`, with every frame uuid prefixed by `tag` AND its pixel values
    /// offset by `tag`. Two fixtures built with different tags therefore collide
    /// neither in frame identity nor in CONTENT (ingest dedups on both), and share
    /// no directory — so one receiver can take both at once, which is the whole
    /// point of the lane tests. Returns `(pkg_dir, announce, files)` like
    /// [`build_v2_fixture`].
    fn build_lane_fixture(
        root: &std::path::Path,
        tag: &str,
        count: usize,
    ) -> (std::path::PathBuf, PackageAnnounce, Vec<AnnounceFileEntry>) {
        use crate::models::{Frame, ImageType};
        use crate::package::{ManifestRecord, PayloadKind, MANIFEST_VERSION};

        let base = root.join(format!("lane-{tag}"));
        let src_dir = base.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        // Per-tag pixel offset: two fixtures must never produce byte-identical
        // payloads, or the second one's frames ingest as content duplicates.
        let tag_offset = tag.bytes().map(u32::from).sum::<u32>() as f32;

        let mut entries = Vec::new();
        let mut items = Vec::new();
        for i in 0..count {
            let uuid = format!("frame-{tag}-{i}");
            let rel = format!("L_{i:04}.fits");
            let src = src_dir.join(&uuid); // unique flat source name
            let val = tag_offset + 0.1f32 * (i as f32 + 1.0);
            crate::fits_writer::write_fits_f32(&src, 4, 4, 1, &[val; 16], &[]).unwrap();
            let byte_size = std::fs::metadata(&src).unwrap().len();
            let xxh3 = crate::package::xxh3_full_file(&src).unwrap();
            let frame = Frame {
                object: Some("M31".to_string()),
                imagetyp: Some(ImageType::Light),
                naxis1: Some(4),
                naxis2: Some(4),
                uuid: Some(uuid.clone()),
                updated_at: Some("2026-01-16T10:00:00.000Z".to_string()),
                ..Default::default()
            };
            let record = ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: uuid.clone(),
                origin_catalog_uuid: format!("catalog-{tag}"),
                origin_device: "aa".repeat(32),
                payload_kind: PayloadKind::RawFrame,
                rel_path: rel.clone(),
                byte_size,
                xxh3,
                frame_meta: serde_json::to_value(&frame).unwrap(),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            };
            entries.push(afe(&rel, &uuid, byte_size));
            items.push((src, record));
        }
        let pkg_dir = base.join("pkg");
        let announce = crate::package::write_package(&pkg_dir, items).unwrap();
        (pkg_dir, announce, entries)
    }

    /// The headline of W2 T2.3: transfers from DIFFERENT devices no longer queue
    /// behind each other. One receiver, two sender endpoints. Peer A's fetch is
    /// paced so it genuinely blocks for seconds; peer B announces a small package
    /// while A is mid-fetch and must run to a terminal **while A is still
    /// fetching**.
    ///
    /// Against the pre-lane single serial consumer this cannot pass at all: B's
    /// `AnnounceReceived` sits unread in the consumer channel until A's inline
    /// `handle_announce` returns, so B's row does not even exist inside the
    /// deadline — the poll below panics with "peer B never reached Done".
    ///
    /// Pacing mechanics: the loopback `fetch` latches `delay_per_read` ONCE at
    /// entry, so arming the fault before A's announce and clearing it after A is
    /// observably `Fetching` paces A only — B's fetch runs at full speed while A's
    /// already-running one keeps its pauses.
    #[tokio::test]
    async fn two_peers_get_independent_lanes() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let slow_sender = Arc::new(net.endpoint());
        let fast_sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        slow_sender.start().await.unwrap();
        fast_sender.start().await.unwrap();

        // 3 payloads + the manifest ⇒ 4 paced reads ⇒ a ~2.8s fetch for peer A.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(700);
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });

        let (_info, handle) = SyncReceiver::spawn(
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

        let (dir_a, ann_a, files_a) = build_lane_fixture(tmp.path(), "slow", 3);
        let wire_a = ann_a.package_id.0.clone();
        slow_sender.serve(&ann_a, &dir_a, None).await.unwrap();
        slow_sender
            .announce(
                receiver_node,
                &ann_a,
                "Slow Batch",
                "batch-slow",
                &files_a,
                PackageLayout::Batch,
            )
            .await
            .unwrap();
        poll_inbound(&store, &wire_a, InboundState::Fetching).await;

        // A is under way with its pacing latched; B gets a full-speed fetch.
        receiver_ep.set_fault(FaultPlan::default());

        let (dir_b, ann_b, files_b) = build_lane_fixture(tmp.path(), "fast", 1);
        let wire_b = ann_b.package_id.0.clone();
        fast_sender.serve(&ann_b, &dir_b, None).await.unwrap();
        fast_sender
            .announce(
                receiver_node,
                &ann_b,
                "Fast Batch",
                "batch-fast",
                &files_b,
                PackageLayout::Batch,
            )
            .await
            .unwrap();

        // Comfortably inside A's ~2.8s paced fetch: if B only lands after A, this
        // window expires with no B row (or a non-terminal one) and the test fails.
        let deadline = std::time::Duration::from_millis(2000);
        let started = std::time::Instant::now();
        let mut landed = None;
        while started.elapsed() < deadline {
            let (b_row, a_row) = {
                let conn = store.lock_conn();
                (
                    get_inbound(&conn, &wire_b).unwrap(),
                    get_inbound(&conn, &wire_a).unwrap(),
                )
            };
            if matches!(&b_row, Some(r) if r.state == InboundState::Done) {
                landed = Some((b_row.unwrap(), a_row));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let (b_row, a_row) = landed.unwrap_or_else(|| {
            panic!(
                "peer B never reached Done within {deadline:?} while peer A was fetching — \
                 B is queued behind A instead of riding its own lane"
            )
        });
        assert_eq!(b_row.state, InboundState::Done);

        // The other half of the proof: A really was still mid-fetch when B landed,
        // so B overtook a genuinely blocking transfer rather than following a fast
        // one.
        assert_eq!(
            a_row.map(|r| r.state),
            Some(InboundState::Fetching),
            "peer A must still be mid-fetch when B lands — otherwise this proves nothing"
        );

        // And A finishes normally on its own lane afterwards.
        poll_inbound(&store, &wire_a, InboundState::Done).await;

        handle.shutdown().await;
    }

    /// Variant B, control-level: the queued-announce map collapses a sender's
    /// re-announces into ONE entry per `(peer, batch_uuid)` and forgets it when the
    /// lane starts processing it.
    ///
    /// The dedupe is not cosmetic: a sender re-announces the same batch on every
    /// backoff rung while it waits for an ack, each time under a FRESH wire
    /// `package_id`. Keying on the wire id would grow one ghost row per retry for a
    /// single waiting batch. Insert-if-absent on the durable batch identity also
    /// keeps `first_seen` at the moment the batch started waiting rather than
    /// letting the latest retry reset it.
    #[test]
    fn queued_announce_map_dedupes_reannounces_and_clears_on_process() {
        let control = InboundControl::new();
        let peer: NodeId = [9u8; 32];
        let other: NodeId = [8u8; 32];

        control.note_queued_announce(peer, "batch-1", Some("First Batch".into()), 3, 300);
        let first_seen = control.queued_announces_snapshot()[0]
            .queued
            .first_seen
            .clone();

        // The SAME batch announced again (next backoff rung, new wire id, and — to
        // make the assertion unambiguous — different figures).
        control.note_queued_announce(peer, "batch-1", Some("First Batch (retry)".into()), 9, 900);
        let snap = control.queued_announces_snapshot();
        assert_eq!(
            snap.len(),
            1,
            "a re-announce of a queued batch is the SAME queued batch, not a second one"
        );
        assert_eq!(
            snap[0].queued.batch_name.as_deref(),
            Some("First Batch"),
            "insert-if-absent: the first announce's payload is kept, not overwritten"
        );
        assert_eq!(snap[0].queued.frame_count, 3);
        assert_eq!(snap[0].queued.byte_size, 300);
        assert_eq!(
            snap[0].queued.first_seen, first_seen,
            "the ghost dates from when the batch started waiting, not from the latest retry"
        );

        // The key is the PAIR: another batch from the same peer, and the same batch
        // uuid from a different peer, are both entries of their own.
        control.note_queued_announce(peer, "batch-2", None, 1, 10);
        control.note_queued_announce(other, "batch-1", None, 1, 10);
        assert_eq!(control.queued_announces_snapshot().len(), 3);

        // Its lane picked it up ⇒ the durable row takes over and the ghost goes.
        control.clear_queued_announce(&peer, "batch-1");
        let after: Vec<(NodeId, String)> = control
            .queued_announces_snapshot()
            .into_iter()
            .map(|e| (e.peer, e.batch_uuid))
            .collect();
        assert_eq!(after.len(), 2, "clear removes exactly the one key");
        assert!(!after.contains(&(peer, "batch-1".to_string())));
        assert!(after.contains(&(peer, "batch-2".to_string())));
        assert!(
            after.contains(&(other, "batch-1".to_string())),
            "another peer's identically-named batch is untouched"
        );

        // Clearing a key that is not there is a no-op, never a panic — the lane arm
        // clears unconditionally, including for announces that were never queued
        // (a lane that was idle when the event arrived).
        control.clear_queued_announce(&peer, "batch-1");
        assert_eq!(control.queued_announces_snapshot().len(), 2);
    }

    /// The headline of variant B: while ONE peer's lane is busy fetching batch 1,
    /// that peer's batch 2 — announced, routed, and sitting in the lane channel with
    /// no `sync_inbound` row of its own — is visible in the control's queued map.
    ///
    /// This is the gap the feature exists to close: `upsert_inbound_attempt` has not
    /// run for batch 2, so the durable state says nothing at all about it and the
    /// receive-side UI shows nothing. The ROUTER is the only component that sees the
    /// announce before the lane does, which is why the insert lives there.
    ///
    /// Same pacing mechanics as `two_peers_get_independent_lanes`: the loopback
    /// `fetch` latches `delay_per_read` ONCE at entry, so arming the fault before
    /// batch 1's announce and clearing it after batch 1 is observably `Fetching`
    /// paces batch 1 only — batch 2 still fetches at full speed once its turn comes.
    #[tokio::test]
    async fn router_tracks_a_queued_announce_while_the_lane_is_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        // ONE sender: both batches must land on the SAME lane, or nothing queues.
        let sender = Arc::new(net.endpoint());
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();
        let sender_node: NodeId = sender.node_id();
        sender.start().await.unwrap();

        // 3 payloads + the manifest ⇒ 4 paced reads ⇒ a ~2.8s fetch for batch 1.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(700);
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });

        let control = Arc::new(InboundControl::new());
        let (_info, handle) = SyncReceiver::spawn(
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
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let (dir_1, ann_1, files_1) = build_lane_fixture(tmp.path(), "busy", 3);
        let wire_1 = ann_1.package_id.0.clone();
        sender.serve(&ann_1, &dir_1, None).await.unwrap();
        sender
            .announce(
                receiver_node,
                &ann_1,
                "Busy Batch",
                "batch-busy",
                &files_1,
                PackageLayout::Batch,
            )
            .await
            .unwrap();
        poll_inbound(&store, &wire_1, InboundState::Fetching).await;

        // Batch 1 is under way with its pacing latched; batch 2 gets a full-speed
        // fetch whenever the lane eventually reaches it.
        receiver_ep.set_fault(FaultPlan::default());

        let (dir_2, ann_2, files_2) = build_lane_fixture(tmp.path(), "queued", 1);
        let wire_2 = ann_2.package_id.0.clone();
        sender.serve(&ann_2, &dir_2, None).await.unwrap();
        sender
            .announce(
                receiver_node,
                &ann_2,
                "Queued Batch",
                "batch-queued",
                &files_2,
                PackageLayout::Batch,
            )
            .await
            .unwrap();

        // Comfortably inside batch 1's ~2.8s paced fetch.
        let deadline = std::time::Duration::from_millis(2000);
        let started = std::time::Instant::now();
        let mut seen = None;
        while started.elapsed() < deadline {
            if let Some(entry) = control
                .queued_announces_snapshot()
                .into_iter()
                .find(|e| e.batch_uuid == "batch-queued")
            {
                seen = Some(entry);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let entry = seen.unwrap_or_else(|| {
            panic!(
                "the second batch never showed up as QUEUED within {deadline:?} — \
                 it is invisible while the lane is busy, which is the whole gap"
            )
        });
        assert_eq!(entry.peer, sender_node, "the ghost names its sending peer");
        assert_eq!(entry.queued.batch_name.as_deref(), Some("Queued Batch"));
        assert_eq!(entry.queued.frame_count, ann_2.frame_count);
        assert_eq!(entry.queued.byte_size, ann_2.byte_size);

        // The other half of the proof, both directions at once: batch 1 really was
        // mid-fetch (so the queueing is genuine) AND batch 2 has no durable row at
        // all (so the map is the only thing that knows it exists).
        {
            let conn = store.lock_conn();
            assert_eq!(
                get_inbound(&conn, &wire_1).unwrap().map(|r| r.state),
                Some(InboundState::Fetching),
                "batch 1 must still be fetching — otherwise nothing was queued behind it"
            );
            assert!(
                get_inbound(&conn, &wire_2).unwrap().is_none(),
                "a queued announce has NO sync_inbound row yet — that is the gap being closed"
            );
        }
        assert_eq!(
            control.queued_announces_snapshot().len(),
            1,
            "only the queued batch is tracked; batch 1 cleared when its lane took it"
        );

        // Once the lane drains both, nothing is queued any more.
        poll_inbound(&store, &wire_1, InboundState::Done).await;
        poll_inbound(&store, &wire_2, InboundState::Done).await;
        assert!(
            control.queued_announces_snapshot().is_empty(),
            "processing an announce removes its ghost — the durable row has taken over"
        );

        handle.shutdown().await;
    }

    /// Shutdown semantics are UNCHANGED by the lane split: `shutdown` aborts the
    /// router task, whose `JoinSet` aborts every lane still in flight on drop —
    /// exactly the "stop now, mid-fetch" behavior the single serial loop had when
    /// it was the thing being aborted.
    ///
    /// Pinned by outcome: a paced fetch that is aborted mid-flight never reaches a
    /// terminal, checked a full uninterrupted-fetch duration later. A lane that
    /// survived its router (or a `shutdown` that drained instead of aborting) would
    /// land the row `Done` inside that window.
    #[tokio::test]
    async fn shutdown_aborts_lanes_in_flight() {
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

        const DELAY: std::time::Duration = std::time::Duration::from_millis(700);
        const READS: u32 = 4; // 3 payloads + manifest
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });

        let (_info, handle) = SyncReceiver::spawn(
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

        let (pkg_dir, announce, files) = build_lane_fixture(tmp.path(), "abort", 3);
        let wire = announce.package_id.0.clone();
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(
                receiver_node,
                &announce,
                "Abort Batch",
                "batch-abort",
                &files,
                PackageLayout::Batch,
            )
            .await
            .unwrap();
        poll_inbound(&store, &wire, InboundState::Fetching).await;

        handle.shutdown().await;

        // Well past the point the fetch would have finished had its lane lived on.
        tokio::time::sleep(DELAY * READS + std::time::Duration::from_millis(500)).await;
        let row = {
            let conn = store.lock_conn();
            get_inbound(&conn, &wire).unwrap().unwrap()
        };
        assert_ne!(
            row.state,
            InboundState::Done,
            "shutdown must abort the in-flight lane, not let it run to completion"
        );
        assert!(
            row.finished_at.is_none(),
            "the aborted attempt writes no terminal; the startup reconcile parks it on the next launch"
        );
    }

    /// The invariant pin for the same change: within ONE peer, events stay strictly
    /// FIFO — a lane is serial. Green before AND after the lane split; it exists so
    /// a later "make it faster" edit cannot quietly buy cross-peer parallelism with
    /// same-peer reordering.
    ///
    /// Sequence: announce (paced, genuinely blocking fetch) → revoke. What must
    /// hold, and why each assertion is an ORDERING statement rather than a repeat of
    /// [`revoke_mid_fetch_aborts_promptly_not_at_fetch_completion`] (which pins the
    /// promptness and the ack-free reason-honesty of the same pair):
    ///
    /// 1. The fetch aborts promptly (the pump's cross-task flag) — kept here only
    ///    so the rest of the test is meaningful.
    /// 2. **`handle_revoke` ran strictly AFTER `handle_announce` returned**: its
    ///    staging cleanup is still in effect a full uninterrupted-fetch duration
    ///    later. Had the two overlapped, the still-running fetch would have
    ///    re-created files under the staging dir it had just deleted.
    /// 3. The terminal is the revoke's mapping (`Cancelled` / `by sender`) and
    ///    survives that same settle window — nothing from the announce side wrote
    ///    after it.
    /// 4. No cancel-epilogue ack: zero receipts, no `cancelled` journal entry.
    #[tokio::test]
    async fn same_peer_events_stay_fifo_under_lanes() {
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

        // 3 payloads + manifest ⇒ 4 paced reads ⇒ ~2.8s uninterrupted.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(700);
        const READS: u32 = 4;
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });

        let (_info, handle) = SyncReceiver::spawn(
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

        let (pkg_dir, announce, files) = build_lane_fixture(tmp.path(), "fifo", 3);
        let wire = announce.package_id.0.clone();
        sender.serve(&announce, &pkg_dir, None).await.unwrap();
        sender
            .announce(
                receiver_node,
                &announce,
                "Fifo Batch",
                "batch-fifo",
                &files,
                PackageLayout::Batch,
            )
            .await
            .unwrap();

        poll_inbound(&store, &wire, InboundState::Fetching).await;
        let inbound_id = {
            let conn = store.lock_conn();
            get_inbound(&conn, &wire).unwrap().unwrap().id
        };
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let staging = sync_dir.join("staging").join(&wire);
        assert!(
            staging.exists(),
            "the fetch is writing into staging before the revoke: {staging:?}"
        );

        sender
            .revoke(receiver_node, &announce.package_id, RevokeReason::Cancelled)
            .await
            .unwrap();
        let row = poll_inbound(&store, &wire, InboundState::Cancelled).await;
        assert_eq!(row.last_error.as_deref(), Some("by sender"));

        // Settle past the point the uninterrupted fetch would have finished: a
        // concurrent (non-FIFO) announce handler would still be copying here.
        tokio::time::sleep(DELAY * READS + std::time::Duration::from_millis(500)).await;

        assert!(
            !staging.exists(),
            "staging re-appeared after the revoke's cleanup — the announce handler \
             was still running, so the revoke did NOT follow it in order: {staging:?}"
        );
        let settled = {
            let conn = store.lock_conn();
            get_inbound(&conn, &wire).unwrap().unwrap()
        };
        assert_eq!(
            settled.state,
            InboundState::Cancelled,
            "the revoke's terminal is final — nothing from the announce side wrote after it"
        );
        assert_eq!(settled.last_error.as_deref(), Some("by sender"));
        assert!(settled.finished_at.is_some(), "a revoked row is terminal");

        assert_eq!(
            count_scalar(
                &store,
                &format!("SELECT COUNT(*) FROM sync_receipts WHERE package_id='{wire}'")
            ),
            0,
            "the revoke path sends no ack and writes no receipts"
        );
        let kinds = journal_kinds(&store, inbound_id);
        assert!(
            kinds.iter().any(|k| k == "revoked"),
            "journal has the revoke: {kinds:?}"
        );
        assert!(
            kinds.iter().all(|k| k != "cancelled"),
            "the local cancel_epilogue never ran: {kinds:?}"
        );

        handle.shutdown().await;
    }

    // ─── ReceiveGate wiring (W2 T2.4) ───────────────────────────────────────

    /// The headline of W2 T2.4: per-peer lanes (T2.3) removed the head-of-line
    /// block but left inbound concurrency bounded only by PEER COUNT — on a busy
    /// night that is "every device at once, all seeking the same disk". The
    /// [`ReceiveGate`] bounds the expensive phase (fetch + ingest) instead.
    ///
    /// Three peers, gate at its shipped default of 2, every fetch paced so it
    /// genuinely blocks for seconds. The store is sampled throughout the run and
    /// three things must hold:
    ///
    /// 1. At NO sample are more than 2 rows in `fetching|ingesting` — the cap holds
    ///    across both the fetch and the ingest stage, not just the download.
    /// 2. The third row leaves `announced` only AFTER one of the first two reached
    ///    a terminal. That is the difference between "parked on the gate" and
    ///    "merely slow to start", and it is what makes assertion 1 meaningful.
    /// 3. All three still finish. A cap that dropped or wedged the third transfer
    ///    would be worse than no cap at all.
    ///
    /// RED before the wiring: nothing acquired the gate, so all three fetched at
    /// once and both (1) and (2) failed.
    #[tokio::test]
    async fn third_transfer_waits_for_receive_permit() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();

        // 2 payloads + the manifest ⇒ 3 paced reads ⇒ a ~2.1s fetch each, so the
        // run is two gate rounds (~4.2s) — far longer than the 50ms sampler's
        // resolution, which is what makes the phase sampling robust.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(700);
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });

        let control = Arc::new(InboundControl::new());
        assert_eq!(
            control.receive_gate.limit(),
            2,
            "this test pins the SHIPPED default, not a test-only limit"
        );

        let (_info, handle) = SyncReceiver::spawn(
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
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        // Announce in a deterministic order: each of the first two is waited onto
        // `Fetching` before the next goes out, so "the third" is a fact about the
        // test rather than a race the assertions have to tolerate. The endpoints
        // are kept alive in a vec — dropping a sender would take its serve with it.
        let mut senders = Vec::new();
        let mut wires: Vec<String> = Vec::new();
        for tag in ["one", "two", "three"] {
            let sender = Arc::new(net.endpoint());
            sender.start().await.unwrap();
            let (dir, ann, files) = build_lane_fixture(tmp.path(), tag, 2);
            let wire = ann.package_id.0.clone();
            sender.serve(&ann, &dir, None).await.unwrap();
            sender
                .announce(
                    receiver_node,
                    &ann,
                    &format!("Batch {tag}"),
                    &format!("batch-{tag}"),
                    &files,
                    PackageLayout::Batch,
                )
                .await
                .unwrap();
            // Only the first two are waited onto their lane; the third is the one
            // under test and must NOT be expected to start.
            if wires.len() < 2 {
                poll_inbound(&store, &wire, InboundState::Fetching).await;
            }
            wires.push(wire);
            senders.push(sender);
        }

        let active = |s: &Option<InboundState>| {
            matches!(
                s,
                Some(InboundState::Fetching) | Some(InboundState::Ingesting)
            )
        };
        let terminal = |s: &Option<InboundState>| {
            matches!(
                s,
                Some(InboundState::Done)
                    | Some(InboundState::Failed)
                    | Some(InboundState::Cancelled)
            )
        };

        let mut max_active = 0usize;
        let mut first_terminal_at: Option<std::time::Instant> = None;
        let mut started_at: std::collections::HashMap<String, std::time::Instant> =
            std::collections::HashMap::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let now = std::time::Instant::now();
            let states: Vec<Option<InboundState>> = {
                let conn = store.lock_conn();
                wires
                    .iter()
                    .map(|w| get_inbound(&conn, w).unwrap().map(|r| r.state))
                    .collect()
            };
            max_active = max_active.max(states.iter().filter(|s| active(s)).count());
            for (wire, state) in wires.iter().zip(&states) {
                // A terminal row necessarily started, so record both from the same
                // snapshot — otherwise a transfer that ran to completion entirely
                // between two samples would look like it never started at all.
                if active(state) || terminal(state) {
                    started_at.entry(wire.clone()).or_insert(now);
                }
                if terminal(state) {
                    first_terminal_at.get_or_insert(now);
                }
            }
            if states.iter().all(|s| matches!(s, Some(InboundState::Done))) {
                break;
            }
            assert!(
                now < deadline,
                "not every transfer finished under the gate: {states:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        assert!(
            max_active <= 2,
            "{max_active} transfers were in fetch/ingest at once — the receive gate \
             (limit {}) is not bounding the expensive phase",
            control.receive_gate.limit()
        );

        let first_terminal = first_terminal_at.expect("some transfer reached a terminal");
        let third_start = *started_at
            .get(&wires[2])
            .expect("the third transfer eventually started");
        assert!(
            third_start >= first_terminal,
            "the third transfer started fetching before any earlier one finished — \
             it was never parked on the gate"
        );

        handle.shutdown().await;
    }

    /// The other half of the placement contract: a CHEAP path must never queue
    /// behind the gate. A receiver already at its concurrency cap still owes a
    /// replayed sender its ack instantly — the answer comes from the durable
    /// receipt log, costs no bytes and no disk, and making it wait would turn a
    /// benign lost-ack retry into a minutes-long silence (and, on the sender, into
    /// another ack-timeout re-announce).
    ///
    /// Peer A completes a transfer, then peers B and C saturate the gate with paced
    /// fetches, then A re-announces the SAME wire id. The pure-replay guard must
    /// re-ack it while B and C are still mid-fetch.
    ///
    /// Green both before and after the wiring by construction — its job is to fail
    /// the day someone hoists the `acquire` to the top of `handle_announce`.
    #[tokio::test]
    async fn replay_ack_bypasses_the_receive_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();

        let control = Arc::new(InboundControl::new());
        let recorder = Arc::new(RecordingEmitter::default());
        let (_info, handle) = SyncReceiver::spawn(
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

        // Peer A, un-paced, runs to a full receipt set — the precondition the
        // pure-replay guard keys on.
        let sender_a = Arc::new(net.endpoint());
        sender_a.start().await.unwrap();
        let (dir_a, ann_a, files_a) = build_lane_fixture(tmp.path(), "replay", 1);
        let wire_a = ann_a.package_id.0.clone();
        sender_a.serve(&ann_a, &dir_a, None).await.unwrap();
        sender_a
            .announce(
                receiver_node,
                &ann_a,
                "Replay Batch",
                "batch-replay",
                &files_a,
                PackageLayout::Batch,
            )
            .await
            .unwrap();
        poll_inbound(&store, &wire_a, InboundState::Done).await;

        // Now saturate the gate: two paced transfers from two other peers.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(700);
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });
        let mut hogs = Vec::new();
        let mut hog_wires = Vec::new();
        for tag in ["hog-a", "hog-b"] {
            let sender = Arc::new(net.endpoint());
            sender.start().await.unwrap();
            let (dir, ann, files) = build_lane_fixture(tmp.path(), tag, 3);
            let wire = ann.package_id.0.clone();
            sender.serve(&ann, &dir, None).await.unwrap();
            sender
                .announce(
                    receiver_node,
                    &ann,
                    &format!("Batch {tag}"),
                    &format!("batch-{tag}"),
                    &files,
                    PackageLayout::Batch,
                )
                .await
                .unwrap();
            poll_inbound(&store, &wire, InboundState::Fetching).await;
            hog_wires.push(wire);
            hogs.push(sender);
        }

        // The replayed announce: same wire id, same batch, nothing to fetch.
        sender_a
            .announce(
                receiver_node,
                &ann_a,
                "Replay Batch",
                "batch-replay",
                &files_a,
                PackageLayout::Batch,
            )
            .await
            .unwrap();

        let budget = std::time::Duration::from_millis(1200);
        let started = std::time::Instant::now();
        let mut replayed = false;
        while started.elapsed() < budget {
            if finished_events(&recorder)
                .iter()
                .any(|e| e["packageId"] == wire_a && e["outcome"] == "replayed")
            {
                replayed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            replayed,
            "the replayed ack did not land within {budget:?} while the gate was \
             saturated — a cheap re-ack is waiting on a receive permit"
        );

        // The proof it BYPASSED the gate rather than being handed a freed permit:
        // both saturating transfers are still mid-fetch, so no permit came free.
        let hog_states: Vec<Option<InboundState>> = {
            let conn = store.lock_conn();
            hog_wires
                .iter()
                .map(|w| get_inbound(&conn, w).unwrap().map(|r| r.state))
                .collect()
        };
        assert!(
            hog_states
                .iter()
                .all(|s| matches!(s, Some(InboundState::Fetching))),
            "both gate-holding transfers must still be fetching when the replay \
             lands, or this proves nothing: {hog_states:?}"
        );

        handle.shutdown().await;
    }

    /// Saturate the receive gate: start `n` peers whose fetches are paced so they
    /// genuinely block for seconds, and return once every one of them holds a
    /// permit (`Fetching`). The senders are returned so the caller keeps them —
    /// and their serves — alive for the rest of the test.
    ///
    /// The fault is armed HERE rather than by the caller because the loopback
    /// latches `delay_per_read` once per `fetch` call: arming it before these
    /// announces is what makes exactly these transfers the slow ones.
    async fn saturate_receive_gate(
        net: &LoopbackNetwork,
        receiver_ep: &Arc<crate::sharing::loopback::LoopbackTransport>,
        receiver_node: NodeId,
        store: &Arc<CatalogSyncStore>,
        root: &std::path::Path,
        n: usize,
    ) -> Vec<Arc<crate::sharing::loopback::LoopbackTransport>> {
        const DELAY: std::time::Duration = std::time::Duration::from_millis(700);
        receiver_ep.set_fault(FaultPlan {
            delay_per_read: Some(DELAY),
            ..Default::default()
        });
        let mut senders = Vec::new();
        for i in 0..n {
            let tag = format!("hog{i}");
            let sender = Arc::new(net.endpoint());
            sender.start().await.unwrap();
            let (dir, ann, files) = build_lane_fixture(root, &tag, 3);
            let wire = ann.package_id.0.clone();
            sender.serve(&ann, &dir, None).await.unwrap();
            sender
                .announce(
                    receiver_node,
                    &ann,
                    &format!("Batch {tag}"),
                    &format!("batch-{tag}"),
                    &files,
                    PackageLayout::Batch,
                )
                .await
                .unwrap();
            poll_inbound(store, &wire, InboundState::Fetching).await;
            senders.push(sender);
        }
        senders
    }

    /// Poll until `package_id` has a row in `want`, returning it; `None` on timeout.
    async fn try_poll_inbound(
        store: &Arc<CatalogSyncStore>,
        package_id: &str,
        want: InboundState,
        budget: std::time::Duration,
    ) -> Option<crate::sync::models::InboundRow> {
        let started = std::time::Instant::now();
        while started.elapsed() < budget {
            let row = {
                let conn = store.lock_conn();
                get_inbound(&conn, package_id).unwrap()
            };
            if let Some(r) = row {
                if r.state == want {
                    return Some(r);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        None
    }

    /// W2 review, Important 1: a transfer PARKED on the receive gate that the user
    /// declines must stay declined. The gate introduced a minutes-long window in
    /// which a transfer sits `announced` with a live `handle_announce` behind it,
    /// and `cancel_incoming_package` treats exactly that state as "no live fetch to
    /// interrupt" — it stamps the row terminal itself
    /// (`stamp_now`: `Some(Announced) => true`, api/sync.rs). The parked lane then
    /// woke up and drove straight into an UNCONDITIONAL `set_inbound_state(Fetching)`,
    /// resurrecting a terminal row: `state=fetching` carrying both `declined_at` and
    /// the `finished_at` of the terminal it overwrote (the non-terminal branch of
    /// `set_inbound_state` leaves `finished_at` alone).
    ///
    /// The tail is what makes it more than cosmetic. The resurrected row only
    /// re-closes if `cancel_epilogue` succeeds, and the epilogue's `fetch_manifest`
    /// propagates with `?` — so a sender that left after the decline strands the row
    /// at `fetching` forever: `delete_transfer_history` refuses a non-terminal row
    /// and `cancel_incoming_package` will not re-stamp it (`Fetching => control.is_none()`
    /// is false while a receiver is running). Unclearable short of a restart.
    ///
    /// The un-served third package models that departed sender exactly as the
    /// receiver experiences it — `fetch_manifest` answers "package not served by
    /// peer" either way — so this test pins the whole defect, not just its cosmetic
    /// half.
    ///
    /// Assertions: the row is NEVER observed non-terminal after the decline, no
    /// `fetch_started` journal entry is ever written for it, and it is still
    /// `cancelled` + `declined_at` once the permit-holders have finished and the
    /// parked lane has had its chance to run.
    #[tokio::test]
    async fn decline_while_parked_stays_terminal_and_never_wedges() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();

        let control = Arc::new(InboundControl::new());
        let (_info, handle) = SyncReceiver::spawn(
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
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let _hogs =
            saturate_receive_gate(&net, &receiver_ep, receiver_node, &store, tmp.path(), 2).await;

        // The third peer announces but never serves — the departed sender (above).
        let victim = Arc::new(net.endpoint());
        victim.start().await.unwrap();
        let (_dir, ann, files) = build_lane_fixture(tmp.path(), "parked", 2);
        let wire = ann.package_id.0.clone();
        victim
            .announce(
                receiver_node,
                &ann,
                "Parked Batch",
                "batch-parked",
                &files,
                PackageLayout::Batch,
            )
            .await
            .unwrap();

        // It parks: the row exists (the upsert runs before the gate) and stays
        // `announced` because the permit never comes.
        let parked = try_poll_inbound(
            &store,
            &wire,
            InboundState::Announced,
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("the third announce should reach `announced` and park on the gate");
        let inbound_id = parked.id;

        // Decline it exactly as `cancel_incoming_package` does, in ITS order:
        // `declined_at` first (guarded UPDATE), then the control flag, then the
        // terminal stamp that `stamp_now` elects for an `Announced` row.
        {
            let conn = store.lock_conn();
            conn.execute(
                "UPDATE sync_inbound
                 SET declined_at = COALESCE(declined_at, ?1)
                 WHERE id = ?2 AND state NOT IN ('ingesting', 'done')",
                rusqlite::params![crate::sync::now_iso(), inbound_id],
            )
            .unwrap();
        }
        control.request_cancel(&wire);
        {
            let conn = store.lock_conn();
            set_inbound_state(&conn, &wire, InboundState::Cancelled, None).unwrap();
        }

        // Watch across the whole window in which a permit frees and the parked lane
        // gets its turn. Sampling catches the resurrection live; the journal catches
        // it durably even if every sample misses.
        let watch_until = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut seen_non_terminal: Vec<InboundState> = Vec::new();
        while std::time::Instant::now() < watch_until {
            let state = {
                let conn = store.lock_conn();
                get_inbound(&conn, &wire).unwrap().map(|r| r.state)
            };
            if let Some(s) = state {
                if !s.is_terminal() {
                    seen_non_terminal.push(s);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert!(
            seen_non_terminal.is_empty(),
            "the declined row was resurrected to {seen_non_terminal:?} — the parked \
             lane overwrote a terminal row after winning its permit"
        );
        let kinds = journal_kinds(&store, inbound_id);
        assert!(
            kinds.iter().all(|k| k != "fetch_started"),
            "the parked lane started a fetch for a declined transfer: {kinds:?}"
        );
        let final_row = {
            let conn = store.lock_conn();
            get_inbound(&conn, &wire).unwrap().unwrap()
        };
        assert_eq!(
            final_row.state,
            InboundState::Cancelled,
            "the decline is final and the row must be clearable (a non-terminal row \
             is refused by delete_transfer_history)"
        );
        assert!(
            final_row.declined_at.is_some(),
            "the decline stays on the finality axis"
        );

        handle.shutdown().await;
    }

    /// Variant C: the live evidence that a transfer is waiting for a receive slot.
    ///
    /// A parked row sits in `Announced` — durably indistinguishable from one that
    /// just arrived — so the receive-side UI could only say "announced" for a
    /// transfer whose sole remaining obstacle is the concurrency cap.
    /// [`InboundControl::parked_for_slot_snapshot`] is the signal that separates
    /// them, and its whole value depends on being exact in BOTH directions: an entry
    /// that never appears says nothing, and one that never leaves would leave a
    /// finished transfer reading `queued` for the rest of the process's life.
    ///
    /// Gate crushed to 1 and one hog holding the only permit, so two victims park at
    /// once. Then every exit from the wait is driven:
    ///
    /// 1. Both parked wire ids are in the snapshot while they wait.
    /// 2. The DECLINED one's entry clears — that lane leaves the queue via
    ///    `abandon_parked_receive` and never gets a permit at all.
    /// 3. The other one's entry clears when it WINS its permit and runs to `Done`.
    /// 4. Nothing at all is left behind once the run is over — including the hog,
    ///    which parked for an instant of its own before being admitted.
    ///
    /// RED before the wiring: (1) failed — nothing ever recorded a park.
    #[tokio::test]
    async fn parked_rows_are_tracked_and_cleared_on_every_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();

        let control = Arc::new(InboundControl::new());
        // One slot, so a single hog is enough to park everything behind it.
        control.receive_gate.set_limit(1);

        let (_info, handle) = SyncReceiver::spawn(
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
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let _hogs =
            saturate_receive_gate(&net, &receiver_ep, receiver_node, &store, tmp.path(), 1).await;

        // Poll `cond` until it holds or the budget runs out.
        async fn poll_until(budget: std::time::Duration, mut cond: impl FnMut() -> bool) -> bool {
            let deadline = std::time::Instant::now() + budget;
            while std::time::Instant::now() < deadline {
                if cond() {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            cond()
        }

        // Two more peers (own lanes, so both reach the gate) that both park behind it.
        // The survivor is SERVED so it can actually complete once admitted; the victim
        // is declined before it ever gets that far. The senders are kept alive for the
        // rest of the test — dropping one takes its serve with it, and the survivor
        // still has to be fetchable.
        let mut wires: Vec<String> = Vec::new();
        let mut senders = Vec::new();
        for tag in ["survivor", "victim"] {
            let sender = Arc::new(net.endpoint());
            sender.start().await.unwrap();
            let (dir, ann, files) = build_lane_fixture(tmp.path(), tag, 2);
            let wire = ann.package_id.0.clone();
            sender.serve(&ann, &dir, None).await.unwrap();
            sender
                .announce(
                    receiver_node,
                    &ann,
                    &format!("Batch {tag}"),
                    &format!("batch-{tag}"),
                    &files,
                    PackageLayout::Batch,
                )
                .await
                .unwrap();
            try_poll_inbound(
                &store,
                &wire,
                InboundState::Announced,
                std::time::Duration::from_secs(5),
            )
            .await
            .unwrap_or_else(|| panic!("{tag} should reach `announced` and park on the gate"));
            wires.push(wire);
            senders.push(sender);
        }
        let (survivor, victim) = (wires[0].clone(), wires[1].clone());

        // 1. Both are on record as waiting for a slot.
        let parked = control.parked_for_slot_snapshot();
        assert!(
            parked.contains(&survivor) && parked.contains(&victim),
            "both parked transfers must be visible as waiting for a slot, got: {parked:?}"
        );

        // 2. Decline the victim exactly as `cancel_incoming_package` does. Its lane
        //    leaves the gate queue without ever taking a permit, so ONLY the guard's
        //    `Drop` can clear it.
        let victim_id = {
            let conn = store.lock_conn();
            get_inbound(&conn, &victim).unwrap().unwrap().id
        };
        {
            let conn = store.lock_conn();
            conn.execute(
                "UPDATE sync_inbound
                 SET declined_at = COALESCE(declined_at, ?1)
                 WHERE id = ?2 AND state NOT IN ('ingesting', 'done')",
                rusqlite::params![crate::sync::now_iso(), victim_id],
            )
            .unwrap();
        }
        control.request_cancel(&victim);
        {
            let conn = store.lock_conn();
            set_inbound_state(&conn, &victim, InboundState::Cancelled, None).unwrap();
        }

        let cleared = poll_until(std::time::Duration::from_secs(10), || {
            !control.parked_for_slot_snapshot().contains(&victim)
        })
        .await;
        assert!(
            cleared,
            "the declined transfer left the gate queue but stayed marked as waiting \
             for a slot — an abandon path leaks its entry: {:?}",
            control.parked_for_slot_snapshot()
        );

        // 3. The survivor takes the freed permit and finishes; winning a permit is
        //    the other way out of the wait, and it must clear the entry too.
        //
        //    Explicit budget rather than `poll_inbound`'s 4s: this transfer waits out
        //    the hog's paced fetch AND then pays the same pacing itself, which is more
        //    than that helper allows even on an idle machine — and this suite runs its
        //    tests in parallel.
        try_poll_inbound(
            &store,
            &survivor,
            InboundState::Done,
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("the survivor should take the freed permit and finish");
        assert!(
            !control.parked_for_slot_snapshot().contains(&survivor),
            "a transfer that WON its permit is still marked as waiting for one — a \
             finished row would read `queued` forever"
        );

        // 4. Nothing left behind at all (the hog parked for an instant of its own
        //    before it was admitted).
        let empty = poll_until(std::time::Duration::from_secs(10), || {
            control.parked_for_slot_snapshot().is_empty()
        })
        .await;
        assert!(
            empty,
            "entries outlived the run: {:?}",
            control.parked_for_slot_snapshot()
        );

        handle.shutdown().await;
    }

    /// W2 review, Important 2: the gate must not re-serialize BOOKKEEPING. A lane
    /// parked on an uninterruptible `acquire()` still owns its peer's FIFO channel,
    /// so a `RevokeReceived` for the parked transfer queued behind it and the
    /// sender-cancel terminal waited for a receive permit — the exact head-of-line
    /// block W2 T2.3 removed, reintroduced for every peer beyond the cap. The abort
    /// flag correctly stopped bytes from moving; what stalled was the honest
    /// terminal the user sees.
    ///
    /// The proof is a timestamp: the revoked row reaches its terminal WHILE both
    /// permit-holders are still mid-fetch, so no permit can have been what let it
    /// through.
    #[tokio::test]
    async fn revoke_while_parked_terminalizes_without_a_permit() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("catalog.db");
        let _assert_db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let sync_dir = tmp.path().join("sync");
        let incoming = sync_dir.join("incoming");

        let net = LoopbackNetwork::new();
        let receiver_ep = Arc::new(net.endpoint());
        let receiver_node: NodeId = receiver_ep.node_id();

        let control = Arc::new(InboundControl::new());
        let (_info, handle) = SyncReceiver::spawn(
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
            Arc::new(RecordingEmitter::default()),
        )
        .await
        .unwrap();

        let hogs =
            saturate_receive_gate(&net, &receiver_ep, receiver_node, &store, tmp.path(), 2).await;
        let hog_wires: Vec<String> = {
            let conn = store.lock_conn();
            crate::sync::store::inbound_active(&conn)
                .unwrap()
                .into_iter()
                .filter(|r| r.state == InboundState::Fetching)
                .map(|r| r.package_id)
                .collect()
        };
        assert_eq!(hog_wires.len(), 2, "both permits are held: {hog_wires:?}");

        let victim = Arc::new(net.endpoint());
        victim.start().await.unwrap();
        let (dir, ann, files) = build_lane_fixture(tmp.path(), "revoked", 2);
        let wire = ann.package_id.0.clone();
        victim.serve(&ann, &dir, None).await.unwrap();
        victim
            .announce(
                receiver_node,
                &ann,
                "Revoked Batch",
                "batch-revoked",
                &files,
                PackageLayout::Batch,
            )
            .await
            .unwrap();
        try_poll_inbound(
            &store,
            &wire,
            InboundState::Announced,
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("the third announce should park on the gate");

        // The sender revokes what it just announced.
        victim
            .revoke(receiver_node, &ann.package_id, RevokeReason::Cancelled)
            .await
            .unwrap();

        // Well inside the permit-holders' ~2.8s paced fetches.
        let budget = std::time::Duration::from_millis(1200);
        let row = try_poll_inbound(&store, &wire, InboundState::Cancelled, budget)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "the revoked transfer did not terminalize within {budget:?} — its \
                     bookkeeping is queued behind a lane parked on the receive gate"
                )
            });
        assert_eq!(
            row.last_error.as_deref(),
            Some(crate::sync::models::REVOKED_BY_SENDER_DETAIL),
            "the terminal is the revoke's, reason-honest"
        );

        // The proof it did not simply inherit a freed permit.
        let hog_states: Vec<Option<InboundState>> = {
            let conn = store.lock_conn();
            hog_wires
                .iter()
                .map(|w| get_inbound(&conn, w).unwrap().map(|r| r.state))
                .collect()
        };
        assert!(
            hog_states
                .iter()
                .all(|s| matches!(s, Some(InboundState::Fetching))),
            "both permit-holders must still be fetching when the revoke terminalizes, \
             or the revoke merely waited its turn: {hog_states:?}"
        );
        let _ = hogs;

        handle.shutdown().await;
    }
}
