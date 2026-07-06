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
//! - `enqueue → Queued` (handle, synchronously) → the worker `serve`s + announces
//!   → `Announced` → `Transferring`.
//! - The sender observes no `FetchProgress` on loopback (fetch is
//!   receiver-driven), so `Transferring` is marked immediately after a
//!   successful announce — the in-flight window during which we await the ack.
//!   A `FetchProgress` arm remains for real transports and is a no-op here.
//! - `AckReceived → Confirmed` (idempotent: a second ack for a package no longer
//!   in the in-flight map is logged at debug and dropped).
//! - Per-package ack timeout → `bump_attempts` + re-announce, until
//!   [`SyncConfig::max_attempts`] → `Failed`.
//! - [`cancel`](SyncEngineHandle::cancel) → `Failed` with a `cancelled` history
//!   outcome.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::package::{self, ManifestRecord};
use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, ReceiptOutcome, TransportEvent,
};
use crate::sharing::SharingTransport;

use super::models::{Direction, HistoryRow, OutboundRow, OutboundState};
use super::store::SyncStore;
use super::{node_id_hex, now_iso};

/// Default cap on announce attempts before a package is marked `Failed`. Five
/// attempts balances resilience to transient peer/transport hiccups against
/// wedging forever on a truly unreachable peer.
pub const MAX_ATTEMPTS: u32 = 5;

/// Default per-attempt wait for the peer's ack before retrying.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Far-future sleep target used when nothing is in flight (any command/event
/// wakes the worker before it elapses).
const IDLE_SLEEP: Duration = Duration::from_secs(3600);

/// Tunables for the engine worker.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// How long to wait for an ack before re-announcing.
    pub ack_timeout: Duration,
    /// Announce attempts before giving up ([`OutboundState::Failed`]).
    pub max_attempts: u32,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            ack_timeout: DEFAULT_ACK_TIMEOUT,
            max_attempts: MAX_ATTEMPTS,
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

/// In-flight bookkeeping for one package awaiting its ack. Keyed in the worker's
/// map by the (per-session) `package_id` the peer will ack with.
struct Pending {
    id: i64,
    dir: PathBuf,
    announce: PackageAnnounce,
    /// When the transfer-start history row was written, reused as `started_at`
    /// on the confirm/terminal rows.
    started_at: String,
    /// When to give up waiting for the ack and retry.
    deadline: Instant,
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
    /// timeouts). Starts a tokio task running the worker loop and returns a
    /// handle to it.
    pub fn spawn_with_config(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        config: SyncConfig,
    ) -> SyncEngineHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(64);
        let worker = Worker {
            store: Arc::clone(&store),
            transport,
            peer,
            config,
            pending: HashMap::new(),
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
    pending: HashMap<String, Pending>,
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

        // Crash-resume: re-drive every non-terminal row left by a prior engine.
        match self.store.non_terminal() {
            Ok(rows) => {
                for row in rows {
                    let dir = PathBuf::from(&row.package_ref);
                    if let Err(e) = self.start_package(row.id, dir, row.state).await {
                        tracing::error!(package_id = row.id, error = %e, "resume re-announce failed");
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "crash-resume enumeration failed"),
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

    /// Serve + announce a package and record the transfer-start milestone (only
    /// the first time it reaches `Transferring`; a resume just re-announces).
    async fn start_package(
        &mut self,
        id: i64,
        dir: PathBuf,
        prior_state: OutboundState,
    ) -> Result<()> {
        let announce =
            announce_for_dir(&dir).with_context(|| format!("build announce for {}", dir.display()))?;

        // Provider side: register the served dir, then advertise it to the peer.
        self.transport
            .serve(&announce, &dir)
            .await
            .context("serve package")?;
        self.transport
            .announce(self.peer, &announce)
            .await
            .context("announce package")?;

        let started_at = now_iso();
        if matches!(prior_state, OutboundState::Queued | OutboundState::Announced) {
            self.store.set_state(id, OutboundState::Announced)?;
            tracing::info!(package_id = id, state = "announced", "sync state");
            self.store.set_state(id, OutboundState::Transferring)?;
            tracing::info!(package_id = id, state = "transferring", "sync state");
            self.append_started_history(id, &dir, &started_at)?;
        } else {
            // Resume: the row is already Transferring — re-announce only.
            tracing::info!(
                package_id = id,
                state = prior_state.as_str(),
                "sync resume re-announce"
            );
        }

        let deadline = Instant::now() + self.config.ack_timeout;
        self.pending.insert(
            announce.package_id.0.clone(),
            Pending {
                id,
                dir,
                announce,
                started_at,
                deadline,
            },
        );
        Ok(())
    }

    /// Dispatch a transport event. Synchronous — no `.await` — so a package can
    /// never be confirmed twice by interleaving.
    fn handle_event(&mut self, ev: TransportEvent) -> Result<()> {
        match ev {
            TransportEvent::AckReceived {
                package_id,
                receipts,
                ..
            } => self.on_ack(package_id, receipts),
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
        }
    }

    /// Confirm a package on ack. Idempotent: an ack for a package no longer in
    /// the in-flight map (already confirmed, or unknown) is dropped at debug.
    fn on_ack(&mut self, package_id: PackageId, receipts: Vec<FrameReceipt>) -> Result<()> {
        let Some(pending) = self.pending.remove(&package_id.0) else {
            tracing::debug!(package_id = %package_id.0, "duplicate/late ack ignored");
            return Ok(());
        };
        self.store
            .confirm(pending.id, &receipts)
            .context("confirm outbound")?;
        self.append_confirmed_history(&pending, &receipts)?;
        tracing::info!(package_id = pending.id, state = "confirmed", "sync state");
        Ok(())
    }

    /// Handle every in-flight package whose ack deadline has elapsed: bump its
    /// attempt count, then either fail it (attempts exhausted) or re-announce.
    async fn handle_timeouts(&mut self) -> Result<()> {
        let now = Instant::now();
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(k, _)| k.clone())
            .collect();

        for key in due {
            let Some((id, dir, announce)) = self
                .pending
                .get(&key)
                .map(|p| (p.id, p.dir.clone(), p.announce.clone()))
            else {
                continue;
            };

            let attempts = self.store.bump_attempts(id).context("bump attempts")?;
            if attempts >= self.config.max_attempts {
                self.fail_package(&key, id, &dir, "max_attempts_exhausted")?;
            } else {
                tracing::warn!(package_id = id, attempts, "sync ack timeout; re-announcing");
                self.transport
                    .serve(&announce, &dir)
                    .await
                    .context("re-serve package")?;
                self.transport
                    .announce(self.peer, &announce)
                    .await
                    .context("re-announce package")?;
                if let Some(p) = self.pending.get_mut(&key) {
                    p.deadline = Instant::now() + self.config.ack_timeout;
                }
            }
        }
        Ok(())
    }

    /// Mark a package `Failed` after exhausting attempts and record a `failed`
    /// history outcome.
    fn fail_package(&mut self, key: &str, id: i64, dir: &Path, _reason: &str) -> Result<()> {
        self.pending.remove(key);
        self.store.set_state(id, OutboundState::Failed)?;
        tracing::error!(package_id = id, state = "failed", "sync state");
        self.append_terminal_history(id, dir, "failed")?;
        Ok(())
    }

    /// Cancel a package → `Failed` with a `cancelled` outcome. Idempotent: a
    /// no-op if the package is already terminal / unknown.
    fn cancel_package(&mut self, id: i64) -> Result<()> {
        // Resolve the package dir: prefer the in-flight entry, else a live row.
        let dir = if let Some(p) = self.pending.values().find(|p| p.id == id) {
            Some(p.dir.clone())
        } else {
            self.store
                .non_terminal()?
                .into_iter()
                .find(|r| r.id == id)
                .map(|r| PathBuf::from(r.package_ref))
        };

        // Drop any in-flight entries for this id so no timeout fires later.
        let keys: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| p.id == id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            self.pending.remove(&k);
        }

        let Some(dir) = dir else {
            tracing::debug!(package_id = id, "cancel ignored (already terminal or unknown)");
            return Ok(());
        };

        self.store.set_state(id, OutboundState::Failed)?;
        tracing::info!(
            package_id = id,
            state = "failed",
            reason = "cancelled",
            "sync state"
        );
        self.append_terminal_history(id, &dir, "cancelled")?;
        Ok(())
    }

    /// Append one `sent` (transfer-started) history row per manifest frame.
    fn append_started_history(&self, id: i64, dir: &Path, started_at: &str) -> Result<()> {
        let records = package::read_manifest(dir)
            .with_context(|| format!("read manifest for started history {}", dir.display()))?;
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
            let (filename, object, bytes) = match by_uuid.get(rec.frame_uuid.as_str()) {
                Some(m) => (filename_of(&m.rel_path), object_of(m), m.byte_size),
                None => (rec.frame_uuid.clone(), None, 0),
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
            })?;
        }
        Ok(())
    }

    /// Append one terminal history row (`failed` / `cancelled`) per manifest
    /// frame. A missing/unreadable manifest is logged, not fatal.
    fn append_terminal_history(&self, id: i64, dir: &Path, outcome: &str) -> Result<()> {
        let records = match package::read_manifest(dir) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(package_id = id, error = %e, "sync history: manifest unreadable at terminal");
                return Ok(());
            }
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
