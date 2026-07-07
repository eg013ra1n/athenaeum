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
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgressEvent {
    pub package_id: String,
    /// Coarse stage: `received`, `fetching`, `ingesting`.
    pub stage: String,
    /// Sending peer node id (hex).
    pub peer_device: String,
    pub frame_count: u32,
}

/// `sync-finished` payload: emitted once per package at the end of processing.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncFinishedEvent {
    pub package_id: String,
    /// `ingested` (all accepted), `partial` (some rejected), `failed` (all
    /// rejected), or `replayed` (re-acked from the receipt log, no ingest).
    pub outcome: String,
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
            outcome: "replayed".to_string(),
            ok_count: satisfied_count,
            failed: Vec::new(),
        });
        return Ok(());
    }

    // Fetch the package into a per-package staging dir under incoming_root.
    let staging = incoming_root.join(".staging").join(&package_id);
    emit_event(emitter, "sync-progress", &SyncProgressEvent {
        package_id: package_id.clone(),
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
        outcome: finished_outcome.to_string(),
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

/// Snapshot of the receiver runtime for the `get_sync_status` command.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// Whether the dev pairing flag (`sync.dev_ticket_pairing`) is enabled.
    pub dev_pairing_enabled: bool,
    /// Whether the transport + receiver are running (a ticket has been minted).
    pub transport_started: bool,
    /// This device's pairing ticket, once started.
    pub pairing_ticket: Option<String>,
    /// Total frames received (history rows with `direction = received`).
    pub received_total: u32,
}

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
    pub async fn ensure_started(
        &self,
        sync_dir: PathBuf,
        db_path: PathBuf,
        emitter: Arc<dyn ProgressEmitter>,
    ) -> Result<String> {
        let mut guard = self.inner.lock().await;
        if let Some(started) = guard.as_ref() {
            return Ok(started.ticket.clone());
        }

        std::fs::create_dir_all(&sync_dir)
            .with_context(|| format!("create sync dir {}", sync_dir.display()))?;
        let secret = load_or_create_device_key(&sync_dir.join("device_key"))?;
        let transport = crate::sharing::iroh::IrohTransport::new(
            secret,
            iroh::RelayMode::Default,
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

/// Load the persisted 32-byte device secret, creating it (mode 0600 on unix) on
/// first run. Mirrors the Perseus loader — the identity secret must never be
/// group/world-readable.
fn load_or_create_device_key(path: &Path) -> Result<[u8; 32]> {
    if path.exists() {
        #[cfg(unix)]
        tighten_permissions_if_needed(path)?;
        let bytes = std::fs::read(path)
            .with_context(|| format!("read device key {}", path.display()))?;
        let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "device key {} is {} bytes, expected 32 — delete it to regenerate",
                path.display(),
                bytes.len()
            )
        })?;
        Ok(arr)
    } else {
        let secret = crate::sharing::iroh::random_secret();
        write_secret_0600(path, &secret)?;
        tracing::info!(path = %path.display(), "generated new sync device key");
        Ok(secret)
    }
}

#[cfg(unix)]
fn tighten_permissions_if_needed(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)
        .with_context(|| format!("stat device key {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("tighten device key permissions {}", path.display()))?;
        tracing::warn!(path = %path.display(), old_mode = format!("{mode:o}"), "sync device key permissions tightened");
    }
    Ok(())
}

#[cfg(unix)]
fn write_secret_0600(path: &Path, secret: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create device key {}", path.display()))?;
    f.write_all(secret)
        .with_context(|| format!("write device key {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_0600(path: &Path, secret: &[u8; 32]) -> Result<()> {
    std::fs::write(path, secret)
        .with_context(|| format!("write device key {}", path.display()))?;
    Ok(())
}
