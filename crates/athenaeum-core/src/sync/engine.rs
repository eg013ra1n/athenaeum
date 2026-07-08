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
//!   "peer is asleep when we queue" case — is retryable, not fatal. The package
//!   keeps a pending retry slot with a deadline and stays `Queued` until an
//!   announce succeeds, so [`handle_timeouts`](Worker::handle_timeouts) walks it
//!   toward [`SyncConfig::max_attempts`] → `Failed`. It either eventually
//!   announces (peer came online) or terminalizes — it is never left
//!   non-terminal with no retry slot.
//! - The sender observes no `FetchProgress` on loopback (fetch is
//!   receiver-driven), so `Transferring` is marked on the first *successful*
//!   announce — the in-flight window during which we await the ack. A
//!   `FetchProgress` arm remains for real transports and is a no-op here.
//! - `AckReceived → Confirmed` (idempotent: a second ack for a package no longer
//!   in the in-flight map is logged at debug and dropped).
//! - Per-package ack/retry timeout → `bump_attempts` + re-announce, until
//!   [`SyncConfig::max_attempts`] → `Failed`. A persistently-erroring
//!   re-announce re-arms its deadline every time, so it advances toward `Failed`
//!   rather than busy-spinning.
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

use crate::events::{emit_event, ProgressEmitter};
use crate::package::{self, ManifestRecord};
use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageId, ReceiptOutcome, TransportEvent,
};
use crate::sharing::SharingTransport;

use super::models::{Direction, HistoryRow, OutboundRow, OutboundState};
use super::receiver::{SyncFinishedEvent, SyncProgressEvent};
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
    /// When to give up waiting (for the ack, or to retry a failed announce).
    deadline: Instant,
    /// Frame uuids the most recent ack rejected, if any (task A7 fix-review).
    /// Empty until a partial ack arrives; overwritten (not accumulated) by each
    /// subsequent partial ack so it always reflects the latest verdict. Named
    /// in the terminal history outcome if the package is eventually `Failed`.
    last_rejected: Vec<String>,
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

    /// The full constructor both convenience spawners delegate to. Starts a
    /// tokio task running the worker loop and returns a handle to it.
    pub fn spawn_with_config_and_emitter(
        store: Arc<dyn SyncStore>,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        config: SyncConfig,
        emitter: Option<Arc<dyn ProgressEmitter>>,
    ) -> SyncEngineHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(64);
        let worker = Worker {
            store: Arc::clone(&store),
            transport,
            peer,
            config,
            pending: HashMap::new(),
            emitter,
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
            emit_event(em.as_ref(), "sync-progress", &SyncProgressEvent {
                package_id: id.to_string(),
                direction: Direction::Sent,
                stage: stage.to_string(),
                peer_device: node_id_hex(&self.peer),
                frame_count,
            });
        }
    }

    /// Emit the single send-side `sync-finished` event for a package (task M3).
    fn emit_finished(&self, id: i64, outcome: &str, ok_count: u32, failed: Vec<String>) {
        if let Some(em) = &self.emitter {
            emit_event(em.as_ref(), "sync-finished", &SyncFinishedEvent {
                package_id: id.to_string(),
                direction: Direction::Sent,
                outcome: outcome.to_string(),
                peer_device: node_id_hex(&self.peer),
                ok_count,
                failed,
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
                last_rejected: Vec::new(),
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
    /// arms the ack-wait deadline. On a build/serve/announce failure it logs and
    /// arms a *retry* deadline instead of returning `Err`, so the row keeps its
    /// slot and `handle_timeouts` walks it toward `max_attempts` rather than
    /// wedging (C1). Because it always re-arms a deadline, a persistently
    /// failing re-announce cannot busy-spin (M1).
    async fn attempt(&mut self, id: i64) -> Result<()> {
        // Snapshot from the slot; drop the borrow before any await / mutation.
        let Some((dir, existing, started)) = self
            .pending
            .get(&id)
            .map(|p| (p.dir.clone(), p.announce.clone(), p.started))
        else {
            // Slot gone (cancelled/confirmed concurrently) — nothing to do.
            return Ok(());
        };

        // Reuse the minted announce across retries (stable `package_id` for ack
        // correlation) or build a fresh one if we don't have one yet.
        let announce = match existing {
            Some(a) => a,
            None => match announce_for_dir(&dir) {
                Ok(a) => a,
                Err(e) => {
                    // `{e:#}` (alternate Display) — a bare `%e` prints only the
                    // outermost `.context(...)` layer, hiding the actual cause
                    // (fix-review: field diagnosis shouldn't need a debugger).
                    tracing::error!(package_id = id, error = %format!("{e:#}"), "sync build announce failed; will retry");
                    self.arm_retry(id);
                    return Ok(());
                }
            },
        };

        // Provider side: register the served dir, then advertise it to the peer.
        // A failure here (e.g. the peer is offline) is retryable, not fatal:
        // remember the announce and arm a retry deadline.
        let serve_announce = async {
            self.transport
                .serve(&announce, &dir)
                .await
                .context("serve package")?;
            self.transport
                .announce(self.peer, &announce)
                .await
                .context("announce package")
        }
        .await;
        if let Err(e) = serve_announce {
            // `{e:#}` — same rationale as the build-announce branch above: the
            // bare `.context("announce package")` layer alone hid the real
            // cause (e.g. "peer not started: <hex>") in production logs.
            tracing::error!(package_id = id, error = %format!("{e:#}"), "sync serve/announce failed; will retry");
            if let Some(p) = self.pending.get_mut(&id) {
                p.announce = Some(announce);
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
            self.append_started_history(id, &dir, &started_at)?;
            // One coarse in-flight tick per package (the first successful
            // announce); retries re-announce but do NOT re-emit — bounded, never
            // per-byte.
            self.emit_progress(id, "transferring", announce.frame_count);
        } else {
            tracing::info!(package_id = id, state = "transferring", "sync resume/retry re-announce");
        }

        // Arm the ack-wait deadline and persist the announce/milestone on the slot.
        if let Some(p) = self.pending.get_mut(&id) {
            p.announce = Some(announce);
            p.started = true;
            p.started_at = started_at;
            p.deadline = Instant::now() + self.config.ack_timeout;
        }
        Ok(())
    }

    /// Arm a retry deadline on a still-pending package after a failed
    /// build/serve/announce attempt. Leaves the milestone untouched — the row
    /// stays `Queued` until an announce actually succeeds.
    fn arm_retry(&mut self, id: i64) {
        if let Some(p) = self.pending.get_mut(&id) {
            p.deadline = Instant::now() + self.config.ack_timeout;
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

    /// Handle an ack: confirm the package ONLY if every receipt is non-`Rejected`
    /// (task A7 fix-review — `Confirmed` means "all frames ingested-or-duplicate").
    /// An ack carrying any `Rejected` receipt is a partial delivery: log the
    /// rejected frame uuids and leave the package's pending slot untouched (no
    /// confirm, no history, deadline unchanged) — the existing ack-timeout
    /// deadline elapses normally and `handle_timeouts`' ordinary retry path
    /// re-announces (redelivery) or, once `max_attempts` is exhausted, fails the
    /// package with a history outcome naming the rejected frame(s).
    ///
    /// Idempotent for a fully-accepted ack: one for a package no longer in the
    /// in-flight map (already confirmed, or unknown) is dropped at debug.
    fn on_ack(&mut self, package_id: PackageId, receipts: Vec<FrameReceipt>) -> Result<()> {
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

        let pending = self.pending.remove(&key).expect("key from live find");
        self.store
            .confirm(pending.id, &receipts)
            .context("confirm outbound")?;
        self.append_confirmed_history(&pending, &receipts)?;
        // Terminal: the package is confirmed; drop its served blobs so they do
        // not outlive the transfer. `package_id` is the id the ack correlated
        // against (== pending.announce.package_id). Fire-and-forget, never fails
        // the confirm.
        self.spawn_release(package_id);
        tracing::info!(package_id = pending.id, state = "confirmed", "sync state");
        self.emit_finished(pending.id, "confirmed", receipts.len() as u32, Vec::new());
        Ok(())
    }

    /// Handle every in-flight package whose deadline has elapsed (an ack that
    /// never came, or a retry of a failed announce): bump its attempt count,
    /// then either fail it (attempts exhausted) or re-attempt.
    async fn handle_timeouts(&mut self) -> Result<()> {
        let now = Instant::now();
        let due: Vec<i64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(k, _)| *k)
            .collect();

        for id in due {
            let attempts = self.store.bump_attempts(id).context("bump attempts")?;
            if attempts >= self.config.max_attempts {
                self.fail_package(id)?;
            } else {
                tracing::warn!(package_id = id, attempts, "sync timeout; re-attempting");
                // `attempt` always re-arms a deadline (ack-wait on success, retry
                // on failure), so a re-announce that keeps erroring can no longer
                // busy-spin (M1) and still advances toward max_attempts. Guard the
                // rare store-error path with a retry so the deadline is never left
                // stale.
                if let Err(e) = self.attempt(id).await {
                    tracing::error!(package_id = id, error = %e, "sync re-attempt failed");
                    self.arm_retry(id);
                }
            }
        }
        Ok(())
    }

    /// Mark a package `Failed` after exhausting attempts and record a `failed`
    /// history outcome. When the package's last known ack rejected one or more
    /// frames, the outcome names them (task A7 fix-review) instead of the bare
    /// `failed` string — the recorded reason for terminal failure was the
    /// receiver's rejection, not just an unreachable peer.
    fn fail_package(&mut self, id: i64) -> Result<()> {
        let removed = self.pending.remove(&id);
        let (dir, last_rejected, pkg_id) = match removed {
            Some(p) => (Some(p.dir), p.last_rejected, p.announce.map(|a| a.package_id)),
            None => (None, Vec::new(), None),
        };
        self.store.set_state(id, OutboundState::Failed)?;
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
            self.append_terminal_history(id, &dir, &outcome)?;
        }
        self.emit_finished(id, &outcome, 0, last_rejected);
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
        let (dir, pkg_id) = if let Some(p) = self.pending.remove(&id) {
            (Some(p.dir), p.announce.map(|a| a.package_id))
        } else {
            (
                self.store
                    .non_terminal()?
                    .into_iter()
                    .find(|r| r.id == id)
                    .map(|r| PathBuf::from(r.package_ref)),
                None,
            )
        };

        let Some(dir) = dir else {
            tracing::debug!(package_id = id, "cancel ignored (already terminal or unknown)");
            return Ok(());
        };

        self.store.set_state(id, OutboundState::Failed)?;
        // Terminal: release any served blobs (fire-and-forget).
        if let Some(pid) = pkg_id {
            self.spawn_release(pid);
        }
        tracing::info!(
            package_id = id,
            state = "failed",
            reason = "cancelled",
            "sync state"
        );
        self.append_terminal_history(id, &dir, "cancelled")?;
        self.emit_finished(id, "cancelled", 0, Vec::new());
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
