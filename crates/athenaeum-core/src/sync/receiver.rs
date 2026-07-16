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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::events::{emit_event, ProgressEmitter};
use crate::sharing::iroh::node::{Role, SharedIrohNode};
use crate::sharing::types::{
    FetchEvent, FrameReceipt, NodeId, PackageAnnounce, ReceiptOutcome, StartInfo, TransportEvent,
};
use crate::sharing::{noop_fetch_sink, FetchSink, SharingTransport};

use super::ingest::{self, IngestOutcome};
use super::models::InboundState;
use super::refusal::RefusalRefresher;
use super::store::{
    get_inbound, insert_receipt, set_inbound_bytes_done, set_inbound_state,
    upsert_inbound_announced, CatalogSyncStore,
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
/// [`is_cancelled`]: Self::is_cancelled
/// [`request_cancel`]: Self::request_cancel
#[derive(Default)]
pub struct InboundControl {
    cancels: std::sync::Mutex<std::collections::HashSet<String>>,
    notify: tokio::sync::Notify,
}

impl InboundControl {
    /// A fresh control with no cancellations requested.
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

    /// A future that resolves the next time [`request_cancel`](Self::request_cancel)
    /// is called. The in-flight fetch loop selects on this to learn about a cancel
    /// without polling. NB the tokio `Notify` registration caveat: the returned
    /// [`Notified`](tokio::sync::futures::Notified) only registers the waiter once
    /// polled/`enable`d, so the call site enables it before checking the flag.
    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
}

/// Handle to a running [`SyncReceiver`]. Dropping it (or calling
/// [`shutdown`](Self::shutdown)) stops the event loop.
pub struct SyncReceiverHandle {
    join: JoinHandle<()>,
}

impl SyncReceiverHandle {
    /// Abort the receiver loop and await its exit.
    pub async fn shutdown(self) {
        self.join.abort();
        let _ = self.join.await;
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
        let info = transport.start().await.context("start receiver transport")?;
        std::fs::create_dir_all(&staging_root)
            .with_context(|| format!("create staging root {}", staging_root.display()))?;

        let ProjectReceiveHooks {
            gate: project_gate,
            announcements_refresher,
            on_project_ingested,
            request_handler,
        } = project;

        let mut events = transport.events().await;
        let loop_transport = Arc::clone(&transport);
        let join = tokio::spawn(async move {
            tracing::info!(staging_root = %staging_root.display(), "sync receiver online");
            while let Some(ev) = events.recv().await {
                match ev {
                    TransportEvent::AnnounceReceived { from, announce } => {
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
                        if let Err(e) = handle_announce(
                            &store,
                            &staging_root,
                            &incoming,
                            loop_transport.as_ref(),
                            Arc::clone(&emitter),
                            &control,
                            from,
                            announce,
                        )
                        .await
                        {
                            tracing::error!(error = %format!("{e:#}"), "sync receiver announce handling failed");
                        }
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
                    // `AckReceived` is the sender's half — the receiver loop does
                    // not consume it.
                    _ => {}
                }
            }
            tracing::info!("sync receiver event stream closed; loop stopping");
        });

        Ok((info, SyncReceiverHandle { join }))
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
fn stamp_inbound_failed(store: &CatalogSyncStore, package_id: &str, error: &anyhow::Error) {
    let conn = store.lock_conn();
    if let Err(e) = set_inbound_state(&conn, package_id, InboundState::Failed, Some(&format!("{error:#}"))) {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound failed state write failed");
    }
}

/// Handle one announced package: persist an inbound row, ack-replay guard, else
/// fetch → ingest → ack, emitting stage progress (with live fetch bytes) and a
/// single finished event, walking the `sync_inbound` row through its lifecycle.
async fn handle_announce(
    store: &Arc<CatalogSyncStore>,
    staging_root: &Path,
    incoming: &IncomingResolver,
    transport: &dyn SharingTransport,
    emitter: Arc<dyn ProgressEmitter>,
    control: &InboundControl,
    from: NodeId,
    announce: PackageAnnounce,
) -> Result<()> {
    let peer_device = super::node_id_hex(&from);
    let package_id = announce.package_id.0.clone();

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
        emit_event(emitter.as_ref(), "sync-finished", &SyncFinishedEvent {
            package_id,
            direction: super::Direction::Received,
            outcome: "failed".to_string(),
            peer_device,
            ok_count: 0,
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        });
        return Ok(());
    }

    // Persist (or refresh) the inbound row to `announced`. A re-announced
    // redelivery resets it back to `announced` and clears its byte/finished
    // markers; a `cancelled` row (Task 12) is left untouched — it is final, so
    // we must never re-fetch it.
    {
        let conn = store.lock_conn();
        upsert_inbound_announced(&conn, &peer_device, &package_id, announce.frame_count, announce.byte_size)
            .with_context(|| format!("record inbound announce {package_id}"))?;
    }
    let is_cancelled = {
        let conn = store.lock_conn();
        matches!(get_inbound(&conn, &package_id)?, Some(r) if r.state == InboundState::Cancelled)
    };

    emit_event(emitter.as_ref(), "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "received".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: None,
        bytes_done: None,
        bytes_total: None,
    });

    // Ack-replay guard: a fully-receipted package is re-acked from the log,
    // skipping the fetch and ingest entirely. Counts only non-Rejected
    // receipts as "satisfied" — a package with a pending Rejected receipt must
    // fall through to fetch+ingest below so that frame gets a real redelivery
    // attempt, not a replay of its stale rejection (fix-review finding #1).
    let satisfied_count = store.count_satisfied_receipts(&announce.package_id)?;
    if announce.frame_count > 0 && satisfied_count == announce.frame_count {
        let receipts = store.load_receipts(&announce.package_id)?;
        // MANDATORY carry-over item 1 (Task 4 review): a package whose replayed
        // receipts are ALL `Cancelled` is a receiver-cancel replay — its finished
        // outcome must be "cancelled", NEVER "ingested" (and the row must stay
        // Cancelled, not be stamped Done). A `Cancelled` receipt bumps no ingest
        // counter in `ingest.rs`, so without this the label would drift.
        let all_cancelled =
            !receipts.is_empty() && receipts.iter().all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled));
        transport
            .ack(from, &announce.package_id, receipts)
            .await
            .context("ack (replayed)")?;
        tracing::info!(package_id = %package_id, count = satisfied_count, all_cancelled, "sync receiver replayed ack from receipt log");
        // Terminal for the receiver: drop the fetched blobs. A lost-ack resend
        // may have re-downloaded them; release is idempotent. Never fails the
        // (successful) receive — log-and-continue on error.
        if let Err(e) = transport.release(&announce.package_id).await {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "receiver blob release failed");
        }
        // Terminal state: an all-cancelled replay is `Cancelled` (final — also
        // repairs a crash between a prior epilogue's receipt writes and its row
        // stamp); a normal replay is `Done`, unless the row is already cancelled
        // (which stays final).
        if all_cancelled {
            let conn = store.lock_conn();
            if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Cancelled, None) {
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound cancelled (replay) write failed");
            }
        } else if !is_cancelled {
            let conn = store.lock_conn();
            if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Done, None) {
                tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound done (replay) write failed");
            }
        }
        emit_event(emitter.as_ref(), "sync-finished", &SyncFinishedEvent {
            package_id,
            direction: super::Direction::Received,
            outcome: if all_cancelled { "cancelled" } else { "replayed" }.to_string(),
            peer_device: peer_device.clone(),
            ok_count: satisfied_count,
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        });
        return Ok(());
    }

    // Wire-in (a) — Task 12: a persisted-`Cancelled` inbound row (restart-proof)
    // OR a control-requested cancel that reached us at/before this announce runs
    // the cancel epilogue instead of fetching — it fetches only the manifest,
    // writes a `Cancelled` receipt per frame, acks them, and stamps the row
    // Cancelled. The replay guard above already handled a package whose epilogue
    // previously wrote every frame's receipt (later re-announces replay from the
    // log — cheaper, no manifest fetch), so this only runs on the FIRST cancel
    // announce, before any receipts exist.
    if is_cancelled || control.is_cancelled(&package_id) {
        return cancel_epilogue(store, transport, emitter.as_ref(), from, &announce, staging_root).await;
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
    emit_event(emitter.as_ref(), "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "fetching".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: None,
        bytes_done: None,
        bytes_total: None,
    });
    // I2 (T7): `from` dialed in to announce; the blob pull dials back out, so give
    // the downloader a relay dial hint for it before fetching (no-op on loopback /
    // when no relay set is resolved — never regresses the existing path reuse).
    transport.add_peer_dial_hint(from);
    // Real fetch sink (Task 11): each batch tick persists live `bytes_done` and
    // emits a `fetching` progress carrying the byte figures; each per-file tick
    // emits a `sync-file-progress`. DB writes are best-effort — a failed byte
    // update warns and never aborts the fetch. Ticks arrive throttled (≤ every
    // 300ms per stream), so a write at that cadence is fine.
    let sink: FetchSink = {
        let emitter = Arc::clone(&emitter);
        let store = Arc::clone(store);
        let pkg = package_id.clone();
        let peer_device = peer_device.clone();
        let frame_count = announce.frame_count;
        Arc::new(move |ev| match ev {
            FetchEvent::Batch { bytes_done, bytes_total } => {
                {
                    let conn = store.lock_conn();
                    if let Err(e) = set_inbound_bytes_done(&conn, &pkg, bytes_done) {
                        tracing::warn!(package_id = %pkg, error = %format!("{e:#}"), "inbound bytes_done update failed");
                    }
                }
                emit_event(emitter.as_ref(), "sync-progress", &SyncProgressEvent {
                    package_id: pkg.clone(),
                    direction: super::Direction::Received,
                    stage: "fetching".to_string(),
                    peer_device: peer_device.clone(),
                    frame_count,
                    project_id: None,
                    bytes_done: Some(bytes_done),
                    bytes_total: Some(bytes_total),
                });
            }
            FetchEvent::File { name, bytes_done, bytes_total } => {
                emit_event(emitter.as_ref(), "sync-file-progress", &SyncFileProgressEvent {
                    package_id: pkg.clone(),
                    peer_device: peer_device.clone(),
                    file: name,
                    bytes_done,
                    bytes_total,
                });
            }
        })
    };
    // Wire-in (b) — Task 12: the fetch is abortable. Pin it and race it against a
    // cancel signal; a cancel drops the fetch future — Task 10's downloader aborts
    // the in-flight download on drop — and diverts to the cancel epilogue. The
    // fetch future is scoped so it drops (aborting the download) BEFORE the
    // epilogue's manifest fetch runs.
    let fetch_outcome: Option<Result<()>> = {
        let fetch_fut = transport.fetch(from, &announce, &staging, sink);
        tokio::pin!(fetch_fut);
        loop {
            // Enable the notify waiter BEFORE checking the flag so a cancel that
            // races in right after the check still wakes us (a tokio `Notified`
            // only registers the waiter once polled / `enable`d).
            let notified = control.notified();
            tokio::pin!(notified);
            // Register the waiter now (a `Notified` only enrolls once polled/enabled)
            // so a cancel that races in right after the flag check still wakes us.
            let _ = notified.as_mut().enable();
            if control.is_cancelled(&package_id) {
                break None;
            }
            tokio::select! {
                biased;
                r = &mut fetch_fut => break Some(r),
                // Woken by a cancel (possibly for another package); loop back and
                // let the flag re-check at the top decide.
                _ = &mut notified => {}
            }
        }
    };
    let Some(fetch_result) = fetch_outcome else {
        // Cancelled mid-fetch: the dropped fetch future aborted the download.
        tracing::info!(package_id = %package_id, peer_device = %peer_device, "sync receiver cancelling in-flight fetch");
        return cancel_epilogue(store, transport, emitter.as_ref(), from, &announce, staging_root).await;
    };
    if let Err(e) = fetch_result {
        // A failed fetch is terminal for this row (Failed + reason); propagate so
        // the receiver loop logs it too.
        stamp_inbound_failed(store, &package_id, &e);
        return Err(e).with_context(|| format!("fetch package {package_id}"));
    }

    // Resolve the landing root LIVE, per package: a `sync_incoming` designation
    // (or clear) since the last package is honored here — not frozen at transport
    // start. Falls back to the caller's app-data default when none is designated.
    let incoming_root = incoming();

    // Ingest on a blocking thread (file I/O + SQLite); never block the runtime.
    {
        let conn = store.lock_conn();
        if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Ingesting, None) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound ingesting state write failed");
        }
    }
    emit_event(emitter.as_ref(), "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "ingesting".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: None,
        bytes_done: None,
        bytes_total: None,
    });
    let ingest_result: Result<IngestOutcome> = {
        let store = Arc::clone(store);
        let staging_for_ingest = staging.clone();
        let announce = announce.clone();
        let peer_device = peer_device.clone();
        match tokio::task::spawn_blocking(move || -> Result<IngestOutcome> {
            let conn = store.lock_conn();
            ingest::ingest_package(&conn, &incoming_root, &staging_for_ingest, &announce, &peer_device)
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
            stamp_inbound_failed(store, &package_id, &e);
            return Err(e).with_context(|| format!("ingest package {package_id}"));
        }
    };

    // Ack the per-frame receipts, then emit the single finished event.
    if let Err(e) = transport.ack(from, &announce.package_id, outcome.receipts.clone()).await {
        // An ack failure is terminal for this row too — the frames landed but the
        // sender never learns their verdict this round; a redelivery re-acks from
        // the receipt log (ack-replay guard above) once the peer is reachable
        // again, but this row must not sit stuck non-terminal until then.
        stamp_inbound_failed(store, &package_id, &e);
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
    emit_event(emitter.as_ref(), "sync-finished", &SyncFinishedEvent {
        package_id,
        direction: super::Direction::Received,
        outcome: finished_outcome.to_string(),
        peer_device,
        ok_count: outcome.ok_count(),
        failed,
        new_count: 0,
        duplicate_count: 0,
        project_id: None,
    });
    Ok(())
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
async fn cancel_epilogue(
    store: &Arc<CatalogSyncStore>,
    transport: &dyn SharingTransport,
    emitter: &dyn ProgressEmitter,
    from: NodeId,
    announce: &PackageAnnounce,
    staging_root: &Path,
) -> Result<()> {
    let peer_device = super::node_id_hex(&from);
    let package_id = announce.package_id.0.clone();

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

    // 2. A `Cancelled` receipt per manifest frame → sync_receipts (the replay log).
    let receipts: Vec<FrameReceipt> = records
        .iter()
        .map(|r| FrameReceipt {
            frame_uuid: r.frame_uuid.clone(),
            xxh3: r.xxh3.clone(),
            outcome: ReceiptOutcome::Cancelled,
        })
        .collect();
    let now = super::now_iso();
    {
        let conn = store.lock_conn();
        for r in &receipts {
            insert_receipt(&conn, &package_id, r, &now)
                .with_context(|| format!("record cancel receipt for {}", r.frame_uuid))?;
        }
    }

    // 3. Ack the Cancelled receipts (best-effort — a lost ack is re-sent by the
    //    replay guard on the sender's next re-announce). The sender's all-cancelled
    //    handler (Task 4) then drives its outbound row to Cancelled.
    if let Err(e) = transport.ack(from, &announce.package_id, receipts).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "cancel ack failed; will replay");
    }

    // 4. Terminal row (final) + drop any fetched blobs + tidy staging.
    {
        let conn = store.lock_conn();
        if let Err(e) = set_inbound_state(&conn, &package_id, InboundState::Cancelled, None) {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "inbound cancelled state write failed");
        }
    }
    if let Err(e) = transport.release(&announce.package_id).await {
        tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "receiver blob release failed");
    }
    if let Err(e) = std::fs::remove_dir_all(&staging) {
        tracing::debug!(error = %e, path = %staging.display(), "cancel epilogue staging cleanup skipped");
    }

    tracing::info!(package_id = %package_id, peer_device = %peer_device, frames = frame_count, "sync receiver cancelled inbound package");
    emit_event(emitter, "sync-finished", &SyncFinishedEvent {
        package_id,
        direction: super::Direction::Received,
        outcome: "cancelled".to_string(),
        peer_device,
        ok_count: 0,
        failed: Vec::new(),
        new_count: 0,
        duplicate_count: 0,
        project_id: None,
    });
    Ok(())
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

    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: hub_package_id.clone(),
        direction: super::Direction::Received,
        stage: "received".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: Some(project_id.clone()),
        bytes_done: None,
        bytes_total: None,
    });

    // Fetch into a per-package staging dir keyed by the WIRE id (mirrors personal
    // sync — out of the user-visible landing tree).
    let staging = staging_root.join("staging").join(&wire_package_id);
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: hub_package_id.clone(),
        direction: super::Direction::Received,
        stage: "fetching".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: Some(project_id.clone()),
        bytes_done: None,
        bytes_total: None,
    });
    // I2 (T7): relay dial hint for the holder we're about to pull from (relay-only
    // — cross-account safe; the node's hint never carries direct addrs).
    transport.add_peer_dial_hint(from);
    transport
        .fetch(from, &announce, &staging, noop_fetch_sink())
        .await
        .with_context(|| format!("fetch project package {wire_package_id}"))?;

    // Ingest on a blocking thread (file I/O + SQLite).
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: hub_package_id.clone(),
        direction: super::Direction::Received,
        stage: "ingesting".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: Some(project_id.clone()),
        bytes_done: None,
        bytes_total: None,
    });
    let outcome = {
        let store = Arc::clone(store);
        let staging_for_ingest = staging.clone();
        let project_id = project_id.clone();
        let hub_package_id = hub_package_id.clone();
        let peer_device = peer_device.clone();
        tokio::task::spawn_blocking(move || -> Result<super::ProjectIngestOutcome> {
            let conn = store.lock_conn();
            super::project_ingest::ingest_project_package(
                &conn,
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
    emit_event(emitter, "sync-finished", &SyncFinishedEvent {
        package_id: hub_package_id.clone(),
        direction: super::Direction::Received,
        outcome: finished_outcome.to_string(),
        peer_device,
        ok_count: outcome.ok_count as u32,
        failed: outcome.failed,
        new_count: 0,
        duplicate_count: 0,
        project_id: Some(project_id.clone()),
    });

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
        self.inner.lock().await.as_ref().map(|s| Arc::clone(&s.transport))
    }

    /// The running receiver's [`InboundControl`], if started (Task 12). The
    /// command layer ([`cancel_incoming_package`](crate::api::sync::cancel_incoming_package))
    /// uses it to request cancellation of an inbound package. `None` before the
    /// transport is started (nothing is being received, so there is nothing to
    /// cancel through the in-memory signal — a persisted `Cancelled` row still
    /// covers the restart case).
    pub async fn inbound_control(&self) -> Option<Arc<InboundControl>> {
        self.inner.lock().await.as_ref().map(|s| Arc::clone(&s.inbound_control))
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

        tracing::info!(ticket_len = info.pairing_ticket.len(), "sync runtime started (dev pairing)");
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
    use crate::sharing::loopback::LoopbackNetwork;
    use crate::sharing::types::{PackageAnnounce, PackageId};
    use std::sync::Mutex;

    /// Captures the events the receiver emits so a test can assert the rejection
    /// path fired.
    #[derive(Default)]
    struct RecordingEmitter {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }
    impl ProgressEmitter for RecordingEmitter {
        fn emit_json(&self, name: &str, payload: serde_json::Value) {
            self.events.lock().unwrap().push((name.to_string(), payload));
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
        handle_announce(
            &store,
            &staging_root,
            &incoming,
            &transport,
            Arc::clone(&emitter) as Arc<dyn ProgressEmitter>,
            &control,
            [7u8; 32],
            announce,
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
            ProjectReceiveHooks { gate: Some(project_gate), ..Default::default() },
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
            assert_eq!(s.len(), 1, "the unsafe package_id never reached the gate; the safe one did");
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
        sender.announce(receiver_node, &announce).await.unwrap();

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
        assert!(fetch_with_bytes, "a fetching progress tick carried bytes: {events:?}");
        let file_ticks = events.iter().filter(|(n, _)| n == "sync-file-progress").count();
        assert!(file_ticks >= 1, "at least one sync-file-progress event: {events:?}");
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
        sender.announce(receiver_node, &announce).await.unwrap();

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
        assert!(active_empty, "a Failed row drops out of the active set — never stuck non-terminal");
    }

    // ── Task 12: receiver-side cancel ───────────────────────────────────────

    /// Drain a peer endpoint's event stream until the next `AckReceived`,
    /// returning its `(package_id, receipts)`. Times out with a panic.
    async fn recv_ack(
        events: &mut tokio::sync::mpsc::Receiver<TransportEvent>,
    ) -> (PackageId, Vec<FrameReceipt>) {
        for _ in 0..400 {
            match tokio::time::timeout(std::time::Duration::from_millis(20), events.recv()).await {
                Ok(Some(TransportEvent::AckReceived { package_id, receipts, .. })) => {
                    return (package_id, receipts)
                }
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
    /// receipt log without re-fetching — discriminated from the epilogue by the
    /// finished event's `okCount` (replay = frame_count; epilogue = 0), and the
    /// outcome is never "ingested" (mandatory carry-over item 1).
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
        sender.announce(receiver_node, &announce).await.unwrap();

        // (b) The sender observes an all-Cancelled ack, one receipt per frame.
        let (ack_pkg, ack_receipts) = recv_ack(&mut sender_events).await;
        assert_eq!(ack_pkg.0, announce.package_id.0);
        assert_eq!(
            ack_receipts.len(),
            announce.frame_count as usize,
            "a cancel ack carries a receipt per manifest frame"
        );
        assert!(
            ack_receipts.iter().all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled)),
            "every receipt in a cancel ack is Cancelled"
        );

        // (c) The inbound row reaches the terminal Cancelled state.
        let row = poll_inbound(&store, &announce.package_id.0, InboundState::Cancelled).await;
        assert!(row.finished_at.is_some(), "a Cancelled row stamps finished_at");
        let active_empty = {
            let conn = store.lock_conn();
            inbound_active(&conn).unwrap().is_empty()
        };
        assert!(active_empty, "a Cancelled row drops out of the active set");

        // (a) No payload files landed under the incoming root.
        assert!(no_files_landed(&incoming), "cancel lands no payload files under {incoming:?}");

        // The first (epilogue) finished event: outcome "cancelled", ok_count 0.
        wait_for_finished(&recorder, 1).await;
        let first = finished_events(&recorder);
        assert_eq!(first[0]["outcome"], "cancelled", "the epilogue emits a cancelled outcome");
        assert_eq!(
            first[0]["okCount"].as_u64().unwrap(),
            0,
            "the epilogue accepts no frames"
        );

        // (d) A second announce replays the cancel from the receipt log WITHOUT
        //     re-fetching — the replay path emits ok_count == frame_count (the
        //     epilogue would emit 0), so this discriminates replay from re-fetch.
        sender.announce(receiver_node, &announce).await.unwrap();
        let (_pkg2, ack2) = recv_ack(&mut sender_events).await;
        assert!(
            ack2.iter().all(|r| matches!(r.outcome, ReceiptOutcome::Cancelled)),
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
            announce.frame_count as u64,
            "the replay guard (not the epilogue) handled the re-announce — no re-fetch"
        );

        // Still no files, still Cancelled.
        assert!(no_files_landed(&incoming), "the replay lands no files either");
        let row2 = {
            let conn = store.lock_conn();
            get_inbound(&conn, &announce.package_id.0).unwrap().unwrap()
        };
        assert_eq!(row2.state, InboundState::Cancelled, "the row stays Cancelled across the replay");
    }
}
