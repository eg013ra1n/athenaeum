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
        transport: Arc<dyn SharingTransport>,
        emitter: Arc<dyn ProgressEmitter>,
    ) -> Result<(StartInfo, SyncReceiverHandle)> {
        let info = transport.start().await.context("start receiver transport")?;
        std::fs::create_dir_all(&staging_root)
            .with_context(|| format!("create staging root {}", staging_root.display()))?;

        let mut events = transport.events().await;
        let loop_transport = Arc::clone(&transport);
        let join = tokio::spawn(async move {
            tracing::info!(staging_root = %staging_root.display(), "sync receiver online");
            while let Some(ev) = events.recv().await {
                if let TransportEvent::AnnounceReceived { from, announce } = ev {
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
        });
        return Ok(());
    }

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
    });
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
        // The receiver's blob store is `blobs`; the sender's is a SEPARATE
        // `blobs_out` (see `api::sync::ensure_sender_engine`). Both halves may
        // run in one process, and one `FsStore` per dir keeps the sender's
        // startup `delete_all` sweep from ever wiping this receiver's live tags.
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
        // Staging lives under the sync dir; the landing root is resolved live per
        // package by the caller-supplied resolver (task 5).
        let (info, receiver) =
            SyncReceiver::spawn(store, sync_dir.clone(), incoming, Arc::clone(&transport), emitter)
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
}
