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
use crate::sharing::types::{NodeId, PackageAnnounce, ReceiptOutcome, StartInfo, TransportEvent};
use crate::sharing::SharingTransport;

use super::ingest::{self, IngestOutcome};
use super::store::CatalogSyncStore;

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
                            emitter.as_ref(),
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
                    // Collab exchange (slice 4): a receive-role member asked us to
                    // serve a project package. Serving lands in Task 6; log + drop.
                    TransportEvent::ProjectRequestReceived {
                        from,
                        project_id,
                        package_id,
                    } => {
                        tracing::info!(
                            from = %super::node_id_hex(&from),
                            project_id,
                            package_id,
                            "project package requested — serving lands in Task 6"
                        );
                    }
                    // `AckReceived` / `FetchProgress` are the sender/UI halves —
                    // the receiver loop does not consume them.
                    _ => {}
                }
            }
            tracing::info!("sync receiver event stream closed; loop stopping");
        });

        Ok((info, SyncReceiverHandle { join }))
    }
}

/// Handle one announced package: ack-replay guard, else fetch → ingest → ack,
/// emitting stage progress and a single finished event.
async fn handle_announce(
    store: &Arc<CatalogSyncStore>,
    staging_root: &Path,
    incoming: &IncomingResolver,
    transport: &dyn SharingTransport,
    emitter: &dyn ProgressEmitter,
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
    // announce, emit a failed outcome, ingest nothing.
    if let Err(e) = crate::package::validate_package_id(&package_id) {
        tracing::warn!(
            from = %peer_device,
            package_id = %package_id,
            error = %e,
            "sync receiver rejected announce with unsafe package_id"
        );
        emit_event(emitter, "sync-finished", &SyncFinishedEvent {
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

    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "received".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: None,
    });

    // Ack-replay guard: a fully-receipted package is re-acked from the log,
    // skipping the fetch and ingest entirely. Counts only non-Rejected
    // receipts as "satisfied" — a package with a pending Rejected receipt must
    // fall through to fetch+ingest below so that frame gets a real redelivery
    // attempt, not a replay of its stale rejection (fix-review finding #1).
    let satisfied_count = store.count_satisfied_receipts(&announce.package_id)?;
    if announce.frame_count > 0 && satisfied_count == announce.frame_count {
        let receipts = store.load_receipts(&announce.package_id)?;
        transport
            .ack(from, &announce.package_id, receipts)
            .await
            .context("ack (replayed)")?;
        tracing::info!(package_id = %package_id, count = satisfied_count, "sync receiver replayed ack from receipt log");
        // Terminal for the receiver: drop the fetched blobs. A lost-ack resend
        // may have re-downloaded them; release is idempotent. Never fails the
        // (successful) receive — log-and-continue on error.
        if let Err(e) = transport.release(&announce.package_id).await {
            tracing::warn!(package_id = %package_id, error = %format!("{e:#}"), "receiver blob release failed");
        }
        emit_event(emitter, "sync-finished", &SyncFinishedEvent {
            package_id,
            direction: super::Direction::Received,
            outcome: "replayed".to_string(),
            peer_device: peer_device.clone(),
            ok_count: satisfied_count,
            failed: Vec::new(),
            new_count: 0,
            duplicate_count: 0,
            project_id: None,
        });
        return Ok(());
    }

    // Fetch the package into a per-package staging dir under the staging root
    // (out of the user-visible landing tree, so a half-fetched package never
    // shows up in the designated sync_incoming folder).
    let staging = staging_root.join("staging").join(&package_id);
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "fetching".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: None,
    });
    transport
        .fetch(from, &announce, &staging)
        .await
        .with_context(|| format!("fetch package {package_id}"))?;

    // Resolve the landing root LIVE, per package: a `sync_incoming` designation
    // (or clear) since the last package is honored here — not frozen at transport
    // start. Falls back to the caller's app-data default when none is designated.
    let incoming_root = incoming();

    // Ingest on a blocking thread (file I/O + SQLite); never block the runtime.
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "ingesting".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
        project_id: None,
    });
    let outcome = {
        let store = Arc::clone(store);
        let staging_for_ingest = staging.clone();
        let announce = announce.clone();
        let peer_device = peer_device.clone();
        tokio::task::spawn_blocking(move || -> Result<IngestOutcome> {
            let conn = store.lock_conn();
            ingest::ingest_package(&conn, &incoming_root, &staging_for_ingest, &announce, &peer_device)
        })
        .await
        .context("ingest join")??
    };

    // Ack the per-frame receipts, then emit the single finished event.
    transport
        .ack(from, &announce.package_id, outcome.receipts.clone())
        .await
        .with_context(|| format!("ack package {package_id}"))?;

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
    emit_event(emitter, "sync-finished", &SyncFinishedEvent {
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
    });
    transport
        .fetch(from, &announce, &staging)
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

/// One started-transport bundle held by [`SyncRuntime`]. `_transport` /
/// `_receiver` are lifetime anchors — kept so the endpoint and its event loop
/// live for the runtime's lifetime, not read directly.
struct Started {
    _transport: Arc<dyn SharingTransport>,
    ticket: String,
    _receiver: SyncReceiverHandle,
}

/// App-lifecycle holder for the receive side. Lives in the host `AppState`
/// (desktop + web) and is reached by the `sync` commands. Cheap to construct;
/// the transport is built lazily on the first
/// [`get_sync_pairing_ticket`](crate::api::sync::get_pairing_ticket) call behind
/// the dev flag.
pub struct SyncRuntime {
    inner: tokio::sync::Mutex<Option<Started>>,
}

impl SyncRuntime {
    /// A fresh, unstarted runtime.
    pub fn new() -> Self {
        Self { inner: tokio::sync::Mutex::new(None) }
    }

    /// Whether the transport has been started (a ticket exists).
    pub async fn is_started(&self) -> bool {
        self.inner.lock().await.is_some()
    }

    /// The current pairing ticket, if started.
    pub async fn ticket(&self) -> Option<String> {
        self.inner.lock().await.as_ref().map(|s| s.ticket.clone())
    }

    /// Lazily build the iroh transport under `sync_dir`, spawn one receiver that
    /// ingests into the catalog at `db_path`, and return the pairing ticket.
    /// Idempotent — a second call returns the existing ticket without starting a
    /// second transport.
    ///
    /// `relay_mode` is resolved by the caller (task M1): the hub's relay map when
    /// signed in, the last cached map for an offline start, or iroh's default
    /// relays as the ultimate fallback (see [`crate::sync::pairing`]).
    pub async fn ensure_started(
        &self,
        sync_dir: PathBuf,
        db_path: PathBuf,
        relay_mode: iroh::RelayMode,
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
        // The ONE device identity (task B4, spec D-5): the account layer and the
        // transport share this exact key file. Loaded through `account::keys` so
        // a second identity can never be minted.
        let secret = crate::account::keys::DeviceKey::load_or_create(
            &crate::account::keys::device_key_path(&sync_dir),
        )?
        .secret_bytes();
        let store = Arc::new(
            CatalogSyncStore::open(&db_path)
                .with_context(|| format!("open catalog sync store {}", db_path.display()))?,
        );
        // A running receiver answers a peer's pre-Announce dedup handshake from
        // its own catalog: the transport's control channel routes inbound
        // Offer/FullHashes to this responder (spec §7, task 4).
        let responder: Arc<dyn crate::sync::DedupResponder> =
            Arc::new(crate::sync::CatalogDedupResponder::new(Arc::clone(&store)));

        // The receiver's blob store is `blobs`; the sender's is a SEPARATE
        // `blobs_out` (see `api::sync::ensure_sender_engine`). Both halves may
        // run in one process, and one `FsStore` per dir keeps the sender's
        // startup `delete_all` sweep from ever wiping this receiver's live tags.
        let transport = crate::sharing::iroh::IrohTransport::new(
            secret,
            relay_mode,
            crate::sharing::iroh::BlobStore::Fs(sync_dir.join("blobs")),
            Some(responder),
        )
        .await
        .context("build iroh transport for receiver")?;
        // Install the connection-level authorization gate on the concrete
        // transport BEFORE it is boxed (slice 4) — `set_connect_gate` is not on
        // the `SharingTransport` trait. Absent ⇒ the transport admits all.
        if let Some(gate) = hooks.connect_gate {
            transport.set_connect_gate(gate);
        }
        let transport: Arc<dyn SharingTransport> = Arc::new(transport);

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
            },
            Arc::clone(&transport),
            emitter,
        )
        .await?;

        tracing::info!(ticket_len = info.pairing_ticket.len(), "sync runtime started (dev pairing)");
        *guard = Some(Started {
            _transport: transport,
            ticket: info.pairing_ticket.clone(),
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
        let emitter = RecordingEmitter::default();

        let evil = tmp.path().join("evil_escape");
        let announce = PackageAnnounce {
            package_id: PackageId(evil.to_string_lossy().into_owned()),
            root_hash: "0".repeat(64),
            byte_size: 0,
            frame_count: 1,
        };

        handle_announce(
            &store,
            &staging_root,
            &incoming,
            &transport,
            &emitter,
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
}
