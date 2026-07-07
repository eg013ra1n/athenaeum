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
    /// Start the receiver over `transport`, landing files under `incoming_root`
    /// and ingesting into `store`. Returns the transport's [`StartInfo`] (its
    /// node id + pairing ticket) plus a handle to the spawned loop.
    ///
    /// `transport.start()` is awaited here (so the ticket is available to the
    /// caller); the loop then takes the event stream exactly once.
    pub async fn spawn(
        store: Arc<CatalogSyncStore>,
        incoming_root: PathBuf,
        transport: Arc<dyn SharingTransport>,
        emitter: Arc<dyn ProgressEmitter>,
    ) -> Result<(StartInfo, SyncReceiverHandle)> {
        let info = transport.start().await.context("start receiver transport")?;
        std::fs::create_dir_all(&incoming_root)
            .with_context(|| format!("create incoming root {}", incoming_root.display()))?;

        let mut events = transport.events().await;
        let loop_transport = Arc::clone(&transport);
        let join = tokio::spawn(async move {
            tracing::info!(incoming_root = %incoming_root.display(), "sync receiver online");
            while let Some(ev) = events.recv().await {
                if let TransportEvent::AnnounceReceived { from, announce } = ev {
                    if let Err(e) = handle_announce(
                        &store,
                        &incoming_root,
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
    incoming_root: &Path,
    transport: &dyn SharingTransport,
    emitter: &dyn ProgressEmitter,
    from: NodeId,
    announce: PackageAnnounce,
) -> Result<()> {
    let peer_device = hex32(&from);
    let package_id = announce.package_id.0.clone();
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "received".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
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
        emit_event(emitter, "sync-finished", &SyncFinishedEvent {
            package_id,
            direction: super::Direction::Received,
            outcome: "replayed".to_string(),
            peer_device: peer_device.clone(),
            ok_count: satisfied_count,
            failed: Vec::new(),
        });
        return Ok(());
    }

    // Fetch the package into a per-package staging dir under incoming_root.
    let staging = incoming_root.join(".staging").join(&package_id);
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "fetching".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
    });
    transport
        .fetch(from, &announce, &staging)
        .await
        .with_context(|| format!("fetch package {package_id}"))?;

    // Ingest on a blocking thread (file I/O + SQLite); never block the runtime.
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
        direction: super::Direction::Received,
        stage: "ingesting".to_string(),
        peer_device: peer_device.clone(),
        frame_count: announce.frame_count,
    });
    let outcome = {
        let store = Arc::clone(store);
        let incoming_root = incoming_root.to_path_buf();
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
    });
    Ok(())
}

/// Lowercase-hex rendering of a 32-byte node id (64 chars).
fn hex32(id: &NodeId) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        s.push_str(&format!("{b:02x}"));
    }
    s
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
        let transport = crate::sharing::iroh::IrohTransport::new(
            secret,
            relay_mode,
            crate::sharing::iroh::BlobStore::Fs(sync_dir.join("blobs")),
        )
        .await
        .context("build iroh transport for receiver")?;
        let transport: Arc<dyn SharingTransport> = Arc::new(transport);

        let store = Arc::new(
            CatalogSyncStore::open(&db_path)
                .with_context(|| format!("open catalog sync store {}", db_path.display()))?,
        );
        let incoming_root = sync_dir.join("incoming");
        let (info, receiver) =
            SyncReceiver::spawn(store, incoming_root, Arc::clone(&transport), emitter).await?;

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
