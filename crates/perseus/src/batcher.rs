//! The send batcher: accumulate the watcher's stable capture files, flush them
//! as **one** package on the auto quiet-timer or an explicit manual signal, fan
//! that package out to every target, and record each file + the batch.
//!
//! This replaces the old per-file `spawn_enqueue_consumer`: instead of building
//! and sending a fresh package the instant each file stabilizes, the batcher
//! collects stabilized files in a [`BTreeSet`] and flushes the whole set at once.
//! Two things trigger a flush:
//!
//! - **Auto** ([`Mode::Auto`]): a quiet timer. Every newly stabilized file
//!   (re)arms a deadline `now + auto_quiet_secs`; when the deadline elapses with
//!   the pending set non-empty, the set is flushed as one `auto` batch. A steady
//!   drip of captures keeps resetting the timer, so a whole imaging run coalesces
//!   into a handful of packages rather than one per sub-exposure.
//! - **Manual** ([`Mode::Manual`]): no timer at all. Files accumulate until the
//!   operator hits "Send N pending" on the web page, which calls
//!   [`BatcherHandle::flush_now`] — the whole pending set goes out as one
//!   `manual` batch. A `flush_now` with nothing pending is a no-op.
//!
//! The mode is read **live** from a [`watch`] channel, so a web-side edit that
//! flips Auto↔Manual (or changes the quiet window) takes effect on the running
//! batcher with no restart: switching to Manual disarms the timer, switching to
//! Auto re-arms it if anything is already pending.
//!
//! # Delivery + bookkeeping (the flush)
//!
//! One flush builds one package from the drained files
//! ([`build_batch_package`]), fans it to every target
//! ([`enqueue_package_to_all`]), and — only when at least one target accepted it
//! — marks the files it **actually packaged** [`record_seen`] and writes a
//! [`BatchStore`] row. A file that was present but unbuildable (won't parse /
//! stat / hash) is dropped from the package at build time and is deliberately
//! left unseen, so it is retried on the next detection / restart rather than
//! silently lost. A flush that reaches **zero** targets re-queues its files
//! (they were never recorded as seen) so the next flush retries them; a flush
//! whose files all vanished is dropped with a `warn!` (there is nothing left to
//! send). The batcher loop never fails on a bad batch — a single flush error is
//! logged and the loop continues.
//!
//! # The third sender: an explicit browser batch
//!
//! The library page can send an arbitrary selection *now*
//! ([`BatcherHandle::send_explicit`], batch mode `browser`, 0.5.1 §1a). That is
//! not a flush — the files are the operator's pick, not the accumulator — but it
//! must deliver and record identically, so both paths run the same
//! [`deliver_batch`] body with the same [`SendContext`] (engines, stores,
//! config, `origin_device`). The differences are exactly two, and both are in
//! the failure direction: a browser send takes its files OUT of the pending set
//! so the next auto/scheduled flush cannot send them twice, and it never
//! re-queues a batch that reached no target (it only undoes its own removal).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use athenaeum_core::sync::{SharedPackageCleanup, SyncEngineHandle};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Duration, Instant};

use crate::batch_store::BatchStore;
use crate::config::{Config, Mode, SendCfg};
use crate::run::{build_batch_package, enqueue_package_to_all, record_seen};
use crate::seen::SeenStore;

/// Capacity of the manual-flush signal channel. A handful of buffered "send now"
/// clicks is plenty — each drains the whole pending set, so extra signals past
/// the first collapse into cheap empty no-op flushes.
const FLUSH_CHANNEL_CAP: usize = 8;

/// A cheap, cloneable control handle to a running batcher. Holds the shared
/// pending set (so the web status page can show what is queued) and the
/// manual-flush signal sender (so "Send N pending" can trigger a flush).
///
/// [`Clone`] hands out an additional owner: the [`crate::run::Agent`] keeps one,
/// the web layer (Task 6) clones another. The batcher loop owns the matching
/// `Arc<Mutex<_>>` clone and the receiving half of the flush channel, so a handle
/// dropped by every owner does not stop the loop — only the watcher channel
/// closing does.
#[derive(Clone)]
pub struct BatcherHandle {
    /// The accumulated, not-yet-flushed `(capture_dir, file)` pairs. A
    /// [`BTreeSet`] so a file stabilized twice before a flush is deduped, and the
    /// snapshot is deterministically ordered. Shared with the batcher loop, which
    /// drains it on flush.
    pending: Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>>,
    /// Fire a manual flush. `()` is the whole message — the batcher flushes its
    /// entire pending set as one `manual` batch (a no-op when empty).
    flush_tx: mpsc::Sender<()>,
    /// Nudge the loop to re-derive its auto quiet deadline.
    ///
    /// The loop arms that deadline from the events it sees, and a handle-side
    /// [`restore_pending`](Self::restore_pending) is not one of them: those pairs
    /// go back into the accumulator behind the loop's back. Without this signal
    /// an Auto-mode agent whose browser send failed after the quiet window had
    /// already elapsed would hold the returned files with **no armed timer** —
    /// until the next capture happened to arrive, which on a finished night is
    /// never. [`Notify::notify_one`] stores a permit when the loop is busy
    /// elsewhere, so a nudge is never lost.
    rearm: Arc<tokio::sync::Notify>,
    /// Everything a delivery needs besides the files (engines, stores, config,
    /// this node's device id, the cleanup coordinator). Shared with the loop, so
    /// a browser send ([`send_explicit`](Self::send_explicit)) builds and records
    /// EXACTLY what an auto/manual flush would.
    ctx: Arc<SendContext>,
}

impl BatcherHandle {
    /// A point-in-time snapshot of the pending (accumulated, not-yet-sent) files,
    /// as `(capture_dir, file)` pairs in the set's deterministic order. Used by
    /// the web status page to render "N pending" and by tests to assert the set
    /// drained after a flush.
    pub fn pending_snapshot(&self) -> Vec<(PathBuf, PathBuf)> {
        self.pending
            .lock()
            .expect("batcher pending mutex poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Take exactly these `(capture_dir, file)` pairs out of the pending
    /// accumulator, returning how many were actually in it.
    ///
    /// The double-send guard behind both browser actions on the library: a file
    /// sent explicitly ([`send_explicit`](Self::send_explicit)) or deleted must
    /// not also go out with the next auto/scheduled flush. Matching is by
    /// **exact pair**, so callers must spell the capture dir the way the watcher
    /// does — canonicalized, which is what
    /// [`expand_selection`](crate::library::expand_selection) returns. A pair
    /// that is not pending is silently not counted: the caller's selection is a
    /// list of files to act on, not an assertion about the accumulator.
    pub fn remove_pending(&self, files: &[(PathBuf, PathBuf)]) -> usize {
        self.take_pending(files).len()
    }

    /// [`remove_pending`](Self::remove_pending), returning **which** pairs were
    /// actually taken — so a caller whose operation then ships nothing can put
    /// exactly those back and leave the accumulator as it found it.
    fn take_pending(&self, files: &[(PathBuf, PathBuf)]) -> Vec<(PathBuf, PathBuf)> {
        let mut guard = self.pending.lock().expect("batcher pending mutex poisoned");
        files
            .iter()
            .filter(|pair| guard.remove(*pair))
            .cloned()
            .collect()
    }

    /// Put back pairs taken by [`take_pending`](Self::take_pending) /
    /// [`remove_pending`](Self::remove_pending) after an attempt that shipped
    /// nothing — a browser send that reached no target, or a library delete that
    /// failed and left the file on disk — and nudge the loop to re-arm its quiet
    /// timer for them ([`rearm`](Self::rearm) — the loop never saw these pairs
    /// arrive, so nothing else will).
    ///
    /// `pub(crate)` for that second caller: nothing outside this crate may plant
    /// entries in the accumulator, but a caller that took some out and then did
    /// nothing with them must be able to leave it as it found it.
    pub(crate) fn restore_pending(&self, files: Vec<(PathBuf, PathBuf)>) {
        if files.is_empty() {
            return;
        }
        {
            let mut guard = self.pending.lock().expect("batcher pending mutex poisoned");
            for pair in files {
                guard.insert(pair);
            }
        }
        // Outside the lock: never signal while holding a std Mutex the loop wants.
        self.rearm.notify_one();
    }

    /// Signal the batcher to flush the whole pending set now as one `manual`
    /// batch. The operator's "Send N pending" action. The batcher itself guards
    /// the empty case, so calling this with nothing pending records no batch.
    ///
    /// Best-effort: if the batcher has already stopped (its receiver dropped) the
    /// send is silently discarded — there is nothing left to flush.
    pub async fn flush_now(&self) {
        let _ = self.flush_tx.send(()).await;
    }

    /// Send an explicit, caller-chosen set of files as ONE `browser` batch
    /// (0.5.1 §1a) — the library page's "Send to…".
    ///
    /// Not a flush: `files` is whatever the operator picked, pending or not,
    /// already-sent or not (re-sending is the point of the button; the receiver's
    /// dedup handshake decides what actually travels). The selected files are
    /// **removed from the pending accumulator first**, so the next auto or
    /// scheduled flush cannot send them a second time — the guard lives here,
    /// inside the one entry point, rather than in each caller.
    ///
    /// Delivery and bookkeeping are the batcher's own ([`deliver_batch`]): the
    /// same build (from the batcher's own spawn-time config and `origin_device`),
    /// the same fan-out, the same record-seen-only-what-shipped rule, the same
    /// batch rows — with one deliberate difference on failure. A flush that
    /// reaches no target re-queues its whole batch for the next quiet window; a
    /// browser send does **not**: the operator is standing right there and
    /// retries explicitly, and enrolling library files they picked once into the
    /// automatic path would send something they were told had failed.
    ///
    /// What an attempt that ships **nothing** does do is undo its own pending
    /// removal — exactly the pairs it took, no others — so a failed send is a
    /// no-op on the accumulator rather than quietly dropping tonight's frames out
    /// of the next flush. (A file dropped at build time inside an otherwise
    /// successful batch is NOT put back, matching the flush path: it stays unseen
    /// and is retried on the next detection / restart instead of spinning
    /// in-session.) Returned files also re-arm the auto quiet timer, so they are
    /// picked up by the next window rather than waiting on a capture that may
    /// never come — see [`rearm`](Self::rearm).
    ///
    /// `engines` overrides the fan-out set for a targeted send; `None` means
    /// every configured target, exactly as a flush does.
    ///
    /// Runs on the CALLER's task, not the batcher loop — a large selection's
    /// hashing and copying must not stall the accumulator.
    pub async fn send_explicit(
        &self,
        files: &[(PathBuf, PathBuf)],
        engines: Option<&[Arc<SyncEngineHandle>]>,
    ) -> Delivery {
        let taken = self.take_pending(files);
        if !taken.is_empty() {
            tracing::info!(
                count = taken.len(),
                "browser send took files out of the pending set"
            );
        }
        let engines = engines.unwrap_or(&self.ctx.engines);
        let delivery = deliver_batch(&self.ctx, engines, files, BROWSER_MODE).await;
        if !matches!(delivery, Delivery::Sent(_)) && !taken.is_empty() {
            tracing::warn!(
                count = taken.len(),
                "browser send shipped nothing; returning its files to the pending set"
            );
            self.restore_pending(taken);
        }
        delivery
    }
}

/// The [`BatchStore`] mode string of a batch sent explicitly from the library
/// page, beside the batcher's own `auto` / `manual`.
pub const BROWSER_MODE: &str = "browser";

/// Everything one delivery needs besides the files: where to send, where to
/// record, and how to build. Shared (behind an [`Arc`]) by the batcher loop and
/// every [`BatcherHandle`], so a browser send cannot drift from a flush — same
/// engines, same stores, same config, same `origin_device`.
pub struct SendContext {
    /// One handle per configured send target; a built package is fanned out to
    /// all of them.
    engines: Vec<Arc<SyncEngineHandle>>,
    /// The stat-aware dedup authority: only files that actually shipped are
    /// recorded here.
    seen: Arc<SeenStore>,
    /// Per-batch send history (`perseus_batch` + `perseus_batch_files`).
    batches: Arc<BatchStore>,
    /// The config the packages are built from (packages dir, capture roots for
    /// `rel_path` labelling). Spawn-time, like the rest of the batcher's view.
    config: Config,
    /// This node's device id (hex), stamped into every manifest record.
    origin_device: String,
    /// Shared-payload cleanup coordinator, `Some` only for a ≥2-target fan-out.
    cleanup: Option<Arc<SharedPackageCleanup>>,
}

/// The result of one non-empty, delivered flush: the package it produced and how
/// many files it carried. `None` from [`flush_once`] means "nothing was
/// recorded" — an empty pending set, an all-vanished batch, or a zero-target
/// delivery (whose files were re-queued for retry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushOutcome {
    pub package_ref: String,
    pub file_count: usize,
}

/// What one [`deliver_batch`] attempt did. The three arms are the only outcomes,
/// and each caller decides what its own mode makes of them (a flush re-queues on
/// [`NoTarget`](Delivery::NoTarget); a browser send reports it and stops).
#[derive(Debug)]
pub enum Delivery {
    /// At least one target durably queued the package; the shipped files are
    /// recorded seen and the batch rows are written.
    Sent(FlushOutcome),
    /// Nothing in the batch could be built (every file vanished or won't parse),
    /// so no package was written and nothing was recorded. Carries the build
    /// error for the caller's own message.
    Unbuildable(anyhow::Error),
    /// A package was built but reached ZERO targets. Its files were NOT recorded
    /// seen and no batch row exists; the staged dir has already been released
    /// (registered with the fan-out coordinator at `0`, or removed outright), so
    /// nothing leaks whatever the caller decides next.
    NoTarget {
        package_ref: String,
        file_count: usize,
    },
}

/// Build ONE package from `files`, fan it out to `engines`, and — only when a
/// target accepted it — record the shipped files as seen and write the batch
/// rows. The single delivery path: the batcher's auto/manual flush and the
/// library page's browser send both go through here, so their bookkeeping cannot
/// diverge.
///
/// The order is load-bearing and mirrors the legacy per-file consumer:
/// build → enqueue-to-all → register cleanup → record seen (ONLY the files that
/// actually made it into the package) → record batch row → record file linkage.
/// A drained file dropped at build time (present but unbuildable) is deliberately
/// left unseen so it is retried on the next detection / restart rather than
/// silently lost; a failed history write only costs a status-page row, never a
/// frame, so both are `warn!`-and-continue.
async fn deliver_batch(
    ctx: &SendContext,
    engines: &[Arc<SyncEngineHandle>],
    files: &[(PathBuf, PathBuf)],
    mode: &str,
) -> Delivery {
    let built = match build_batch_package(&ctx.config, files, &ctx.origin_device) {
        Ok(built) => built,
        Err(error) => return Delivery::Unbuildable(error),
    };
    // The packaged record count == the number of files that actually shipped.
    // Some given files may have been dropped at build time (present but
    // unbuildable); those are deliberately absent from `included`.
    let n = built.included.len();

    let (first_id, delivered) = enqueue_package_to_all(
        engines,
        &built.pkg_dir,
        Some(&built.display_name),
        &built.files,
    )
    .await;
    // Fan-out only: register the delivered target count so the shared payload is
    // freed exactly once, after every target is terminal (mirrors the per-file
    // path). Single-target agents keep the engine's in-line cleanup (`None`).
    // `delivered == 0` registers an expected of 0 → the orphaned copy is cleaned
    // immediately (no target's retry can ever need it).
    if let Some(coord) = &ctx.cleanup {
        coord.register(&built.pkg_dir, delivered);
    }
    let pkg_dir = built.pkg_dir.clone();
    let package_ref = pkg_dir.to_string_lossy().into_owned();

    if first_id.is_none() {
        // Reached no target. The files were NOT recorded as seen — what happens
        // to them next is the caller's call. The staged package is orphaned
        // either way (any next attempt mints a fresh dir): with a fan-out
        // `cleanup` the zero `register` above already freed it; a single-target
        // agent (`None`) must drop it here, or an Auto-mode dead-worker leaks one
        // dir per quiet window. Best-effort — a failure just warns.
        if ctx.cleanup.is_none() {
            if let Err(error) = std::fs::remove_dir_all(&pkg_dir) {
                tracing::warn!(%error, package_ref = %package_ref, "failed to clean orphaned zero-target package dir");
            }
        }
        return Delivery::NoTarget {
            package_ref,
            file_count: n,
        };
    }

    // At least one target durably queued the package. Mark seen ONLY the files
    // that actually made it into the package (so a restart never re-baselines
    // them) and record the batch. A given file that was dropped at build time
    // (present-but-unbuildable) is intentionally left unseen: it never shipped,
    // so it must stay enqueue-eligible and get retried on the next detection /
    // restart rather than be silently lost (the durable seen store is the dedup
    // authority). This is not the zero-target path — we do NOT put it back into
    // any queue (that would spin a permanently-corrupt file forever in-session);
    // simply not marking it seen matches the legacy per-file behavior exactly.
    for file in &built.included {
        record_seen(&ctx.seen, file, &package_ref);
    }
    if let Err(error) = ctx.batches.record(&package_ref, mode, &now_rfc3339(), n) {
        // The files are already durably queued + recorded seen; a failed history
        // write only loses a status-page row, never a frame.
        tracing::warn!(%error, package_ref = %package_ref, "failed to record batch row");
    }
    // Durable rel_path → source linkage for a later confirmed-package rebuild.
    // Same best-effort contract as the batch row above.
    if let Err(error) = ctx.batches.record_files(&package_ref, &built.rel_sources) {
        tracing::warn!(%error, package_ref = %package_ref, "failed to record batch file linkage");
    }
    tracing::info!(
        package_ref = %package_ref,
        delivered,
        targets = engines.len(),
        file_count = n,
        mode,
        "batch flushed"
    );
    Delivery::Sent(FlushOutcome {
        package_ref,
        file_count: n,
    })
}

/// The auto quiet window as a [`Duration`], **floored at 1s**. `auto_quiet_secs`
/// may legitimately be `0` in the config (Task 1 puts no floor on it); a zero
/// timer would fire on the very next tick and degenerate the batcher back into a
/// per-file sender, defeating the whole point. Flooring here guarantees auto mode
/// always coalesces over at least one second.
fn quiet_window(cfg: &SendCfg) -> Duration {
    Duration::from_secs(cfg.auto_quiet_secs.max(1))
}

/// The string a [`Mode`] is recorded as in [`BatchStore`]. Kept in one place so
/// the batcher and the web page (Task 6) agree on the literal.
fn mode_str(mode: Mode) -> &'static str {
    match mode {
        Mode::Auto => "auto",
        Mode::Manual => "manual",
        Mode::Scheduled => "scheduled",
    }
}

/// RFC-3339 UTC millisecond timestamp — the same rendering the sync tables use,
/// so `perseus_batch.created_at` sorts lexicographically the way [`BatchStore`]
/// lists it (newest-first).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Drain the pending set and, if non-empty, build one package, fan it to every
/// target, and record the files + the batch.
///
/// This is the whole flush body, factored out of the loop so tests can drive it
/// directly against a loopback engine without waiting on the timer. Returns
/// `Some(FlushOutcome)` only when a package was built, reached ≥1 target, and a
/// batch row was written. Returns `None` (recording nothing) for each of:
///
/// - **empty** pending set — nothing to do;
/// - **all-vanished** batch — every file failed to build; the files are gone, so
///   they are dropped with a `warn!` (nothing to retry);
/// - **zero-target** delivery — the package reached no target; its files were
///   never [`record_seen`]'d, so they are **re-inserted into `pending`** to be
///   retried on the next flush, with an `error!`.
///
/// A single bad flush never propagates: the caller (the loop) keeps running.
async fn flush_once(
    mode: Mode,
    pending: &Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>>,
    ctx: &SendContext,
) -> Option<FlushOutcome> {
    // Drain under the lock, then release it before any await (never hold a std
    // Mutex across .await).
    let files: Vec<(PathBuf, PathBuf)> = {
        let mut guard = pending.lock().expect("batcher pending mutex poisoned");
        if guard.is_empty() {
            return None;
        }
        std::mem::take(&mut *guard).into_iter().collect()
    };

    match deliver_batch(ctx, &ctx.engines, &files, mode_str(mode)).await {
        Delivery::Sent(outcome) => Some(outcome),
        Delivery::Unbuildable(error) => {
            // Every file in the batch vanished / won't parse. There is nothing on
            // disk to send and nothing to retry — drop the batch.
            tracing::warn!(
                %error,
                count = files.len(),
                mode = mode_str(mode),
                "batch had no buildable files; dropping"
            );
            None
        }
        Delivery::NoTarget {
            package_ref,
            file_count,
        } => {
            // Reached no target. The files were NOT recorded as seen, so put them
            // back to retry on the next flush rather than silently losing them.
            // (A browser send deliberately does not — see
            // [`BatcherHandle::send_explicit`].)
            tracing::error!(
                targets = ctx.engines.len(),
                file_count,
                package_ref = %package_ref,
                "batch reached no target; re-queuing its files for retry"
            );
            let mut guard = pending.lock().expect("batcher pending mutex poisoned");
            for item in files {
                guard.insert(item);
            }
            None
        }
    }
}

/// Spawn the batcher: accumulate the watcher's stable files, flush on the auto
/// quiet-timer or a manual signal, and fan each batch out to every target.
/// Returns the [`BatcherHandle`] (kept by the agent + cloned to the web layer)
/// and the loop's [`JoinHandle`].
///
/// The loop exits when `stable_rx` closes — i.e. when the last watcher drops its
/// sender (graceful shutdown). Any files still pending at that point are not
/// force-flushed: they were never [`record_seen`]'d, so the next run's watcher
/// re-detects them and the batcher re-batches them. No frame is lost.
#[allow(clippy::too_many_arguments)]
pub fn spawn_batcher(
    mut stable_rx: mpsc::Receiver<(PathBuf, PathBuf)>,
    engines: Vec<Arc<SyncEngineHandle>>,
    seen: Arc<SeenStore>,
    batches: Arc<BatchStore>,
    config: Config,
    origin_device: String,
    cleanup: Option<Arc<SharedPackageCleanup>>,
    mut send_cfg_rx: watch::Receiver<SendCfg>,
) -> (BatcherHandle, JoinHandle<()>) {
    let pending: Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let (flush_tx, mut flush_rx) = mpsc::channel::<()>(FLUSH_CHANNEL_CAP);
    // One delivery context, shared by the loop and every handle clone: a browser
    // send through `BatcherHandle::send_explicit` builds and records exactly what
    // this loop's own flush would.
    let ctx = Arc::new(SendContext {
        engines,
        seen,
        batches,
        config,
        origin_device,
        cleanup,
    });
    // The restore nudge: signalled by a handle that puts files back, awaited by
    // the loop's re-arm branch.
    let rearm_signal = Arc::new(tokio::sync::Notify::new());
    let handle = BatcherHandle {
        pending: Arc::clone(&pending),
        flush_tx,
        rearm: Arc::clone(&rearm_signal),
        ctx: Arc::clone(&ctx),
    };

    let loop_pending = Arc::clone(&pending);
    let task = tokio::spawn(async move {
        // The live send config. Seeded from the watch channel (production seeds it
        // with `config.send_cfg()`), then updated in place on every change.
        let mut cfg = send_cfg_rx.borrow().clone();
        // The auto quiet deadline. `None` = disarmed (manual mode, or auto with an
        // empty pending set). Recreated as a fresh `sleep_until` each loop turn, so
        // reassigning it here *is* the timer reset.
        let mut deadline: Option<Instant> = None;
        // Guards to disable a select! branch whose channel has closed, so a closed
        // channel never spins the loop. In practice both senders outlive the loop
        // (the watcher channel closes first, breaking us out), but this is cheap
        // insurance against a hot loop.
        let mut cfg_open = true;
        let mut flush_open = true;

        loop {
            tokio::select! {
                // A watcher stabilized a file. Accumulate it; in auto mode this
                // (re)arms the quiet timer — a steady drip keeps pushing it out.
                maybe = stable_rx.recv() => {
                    let Some(item) = maybe else {
                        // All watchers gone → graceful shutdown. See the fn docs:
                        // any still-pending files are re-detected next run.
                        break;
                    };
                    loop_pending
                        .lock()
                        .expect("batcher pending mutex poisoned")
                        .insert(item);
                    if cfg.mode == Mode::Auto {
                        deadline = Some(Instant::now() + quiet_window(&cfg));
                    }
                }

                // The quiet timer elapsed. Flush as an auto batch, then re-derive
                // the deadline: a fully-drained set disarms it; a set left
                // non-empty (a zero-target flush re-queued its files) re-arms it so
                // the retry happens after another quiet window.
                () = async {
                    match deadline {
                        Some(at) => sleep_until(at).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    flush_once(Mode::Auto, &loop_pending, &ctx).await;
                    deadline = rearm(&loop_pending, &cfg);
                }

                // The operator hit "Send N pending". Flush the whole set as a
                // manual batch regardless of mode.
                maybe = flush_rx.recv(), if flush_open => {
                    if maybe.is_none() {
                        // Every handle dropped; stop listening on this branch.
                        flush_open = false;
                        continue;
                    }
                    flush_once(Mode::Manual, &loop_pending, &ctx).await;
                    deadline = rearm(&loop_pending, &cfg);
                }

                // A handle put files BACK into the accumulator (a browser send
                // that shipped nothing). The loop never saw them arrive, so
                // nothing armed the quiet timer for them — re-derive it, exactly
                // as a post-flush or a mode change does.
                () = rearm_signal.notified() => {
                    deadline = rearm(&loop_pending, &cfg);
                }

                // A live config edit (Task 6). Adopt the new mode/quiet window:
                // Manual disarms the timer; Auto re-arms it iff something is
                // already pending.
                changed = send_cfg_rx.changed(), if cfg_open => {
                    match changed {
                        Ok(()) => {
                            cfg = send_cfg_rx.borrow_and_update().clone();
                            deadline = match cfg.mode {
                                // T12 note: `Scheduled` is inert here on purpose —
                                // this arm is the AUTO quiet timer, and the
                                // scheduler's own wall-clock arm is T13. Until it
                                // lands, scheduled mode behaves as manual: files
                                // accumulate and only an explicit flush ships them
                                // (`POST /api/send-now` works in this mode; the web
                                // page hides its own button until the scheduler UI
                                // lands, so the interim flush path is the route, or
                                // switching mode back in the TOML). Safe
                                // degradation: nothing sends early, nothing is lost.
                                Mode::Manual | Mode::Scheduled => None,
                                Mode::Auto => rearm(&loop_pending, &cfg),
                            };
                        }
                        Err(_) => {
                            // Sender dropped; stop polling this branch.
                            cfg_open = false;
                        }
                    }
                }
            }
        }
        tracing::debug!("batcher stopped");
    });

    (handle, task)
}

/// The post-flush / mode-change / post-restore deadline: `Some(now + quiet)` in
/// auto mode when files remain pending, else `None`. This is what makes a
/// zero-target flush retry (its re-queued files keep the timer armed), a failed
/// browser send's returned files get a window of their own, and a fully-drained
/// flush go idle.
fn rearm(pending: &Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>>, cfg: &SendCfg) -> Option<Instant> {
    if cfg.mode != Mode::Auto {
        return None;
    }
    let has_pending = !pending
        .lock()
        .expect("batcher pending mutex poisoned")
        .is_empty();
    has_pending.then(|| Instant::now() + quiet_window(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
    use athenaeum_core::fits_writer::write_fits_f32;
    use athenaeum_core::sharing::loopback::LoopbackNetwork;
    use athenaeum_core::sync::store::{StandaloneSyncStore, SyncStore};
    use athenaeum_core::sync::SyncEngine;
    use std::path::Path;

    /// Yield the scheduler enough times for the single-threaded test runtime to
    /// drive the batcher task to a parked state (all currently-queued channel
    /// items processed). Used instead of a time-based sleep so it works under
    /// `start_paused` without advancing the clock.
    async fn settle() {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    /// Poll `pred`, yielding between checks, until it holds. Yield-based (never
    /// `tokio::time::sleep`) so it makes progress under a paused clock.
    async fn wait_until<F: FnMut() -> bool>(mut pred: F) {
        for _ in 0..2000 {
            if pred() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition never became true");
    }

    /// Write a minimal, parseable single-frame FITS via core's writer.
    fn write_fixture_fits(path: &Path, object: &str) {
        let cards = HeaderBuilder::new(FrameKind::Light)
            .object(object)
            .exptime(60.0)
            .filter("Ha")
            .instrume("TestCam")
            .build()
            .expect("build header");
        let data = vec![0.0f32; 8 * 8];
        write_fits_f32(path, 8, 8, 1, &data, &cards).expect("write fixture fits");
    }

    /// A running batcher wired to a loopback engine, plus the levers a test needs:
    /// feed stable files, snapshot the pending set, read the recorded batches.
    struct Harness {
        capture: PathBuf,
        files: Vec<PathBuf>,
        batches: Arc<BatchStore>,
        /// A clone of the batcher's seen store, so a test can assert which files
        /// were (or were not) recorded seen after a flush.
        seen: Arc<SeenStore>,
        /// The durable sync store the loopback engine writes into, so a test can
        /// assert the enqueued row's `display_name` and per-file rows.
        store: Arc<StandaloneSyncStore>,
        stable_tx: mpsc::Sender<(PathBuf, PathBuf)>,
        handle: BatcherHandle,
        _task: JoinHandle<()>,
        _engine: Arc<SyncEngineHandle>,
        // Held so the batcher's `send_cfg_rx.changed()` branch never sees a
        // dropped sender for the life of the test (the tests set mode via the
        // seed only, not by pushing edits).
        _cfg_tx: watch::Sender<SendCfg>,
    }

    impl Harness {
        /// Spawn a batcher seeded with `send_cfg`, over a fresh temp tree with
        /// three parseable fixture FITS in the capture dir and one loopback engine
        /// as the sole target. The temp tree is intentionally leaked (`keep`) so
        /// the built package survives the assertions.
        fn spawn(send_cfg: SendCfg) -> Self {
            Self::spawn_with_targets(send_cfg, true)
        }

        /// [`spawn`](Self::spawn), but `targets == false` gives the batcher an
        /// EMPTY engine set — the zero-target agent (dead worker / no configured
        /// peer), where every delivery reaches nobody.
        fn spawn_with_targets(send_cfg: SendCfg, targets: bool) -> Self {
            let tmp = tempfile::tempdir().unwrap().keep();
            let capture = tmp.join("capture");
            let data = tmp.join("data");
            std::fs::create_dir_all(&capture).unwrap();
            std::fs::create_dir_all(&data).unwrap();

            let files: Vec<PathBuf> = ["a.fits", "b.fits", "c.fits"]
                .iter()
                .map(|name| {
                    let p = capture.join(name);
                    write_fixture_fits(&p, "M42");
                    p
                })
                .collect();

            let toml = format!(
                "capture_dir=\"{}\"\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"auto\"\n\
                 [retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
                capture.display(),
                data.display()
            );
            let config = Config::from_toml_str(&toml).unwrap();

            let seen = Arc::new(SeenStore::open(config.db_path()).unwrap());
            let batches = Arc::new(BatchStore::open(config.db_path()).unwrap());
            let store = Arc::new(StandaloneSyncStore::open(config.db_path()).unwrap());

            // A loopback engine with a live worker: `enqueue_package` durably
            // queues (delivered == 1) without needing a started receiver, which is
            // all a flush needs to record a batch.
            let net = LoopbackNetwork::new();
            let engine = Arc::new(SyncEngine::spawn(
                Arc::clone(&store) as Arc<dyn SyncStore>,
                Arc::new(net.endpoint()),
                [1u8; 32],
            ));

            let (stable_tx, stable_rx) = mpsc::channel::<(PathBuf, PathBuf)>(64);
            let (cfg_tx, cfg_rx) = watch::channel(send_cfg);

            let engines = if targets {
                vec![Arc::clone(&engine)]
            } else {
                Vec::new()
            };
            let (handle, task) = spawn_batcher(
                stable_rx,
                engines,
                Arc::clone(&seen),
                Arc::clone(&batches),
                config,
                "aa".repeat(32),
                None,
                cfg_rx,
            );

            Harness {
                capture,
                files,
                batches,
                seen,
                store,
                stable_tx,
                handle,
                _task: task,
                _engine: engine,
                _cfg_tx: cfg_tx,
            }
        }

        /// Feed the fixture files at `indices` into the batcher as stable
        /// `(capture_dir, file)` pairs, as a watcher would.
        async fn feed(&self, indices: &[usize]) {
            for &i in indices {
                self.stable_tx
                    .send((self.capture.clone(), self.files[i].clone()))
                    .await
                    .unwrap();
            }
        }

        fn batch_count(&self) -> usize {
            self.batches.list().unwrap().len()
        }
    }

    /// Auto mode flushes only after the quiet window fully elapses, and the flush
    /// carries every accumulated file as one `auto` batch, draining the pending
    /// set.
    #[tokio::test(start_paused = true)]
    async fn auto_flushes_after_quiet_period_not_before() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Auto,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });

        h.feed(&[0, 1]).await; // two files arrive at t0
        settle().await; // let the batcher accumulate + arm the deadline

        tokio::time::advance(Duration::from_secs(59)).await;
        settle().await;
        assert_eq!(h.batch_count(), 0, "no flush before the quiet window elapses");

        tokio::time::advance(Duration::from_secs(2)).await; // cross 60s of quiet
        wait_until(|| h.batch_count() == 1).await;

        let rows = h.batches.list().unwrap();
        assert_eq!(rows[0].file_count, 2, "the batch carried both files");
        assert_eq!(rows[0].mode, "auto");
        assert!(
            h.handle.pending_snapshot().is_empty(),
            "the pending set drained on flush"
        );
    }

    /// Every new file resets the quiet timer: a file at t0 and another at t50 must
    /// flush ~60s after the SECOND file (≈t110), not at t60.
    #[tokio::test(start_paused = true)]
    async fn a_new_file_resets_the_quiet_timer() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Auto,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });

        h.feed(&[0]).await; // first file at t0 → deadline t60
        settle().await;

        tokio::time::advance(Duration::from_secs(50)).await; // t50
        settle().await;
        assert_eq!(h.batch_count(), 0, "still within the first window");

        h.feed(&[1]).await; // second file at t50 → deadline resets to t110
        settle().await;

        tokio::time::advance(Duration::from_secs(11)).await; // t61 — past the ORIGINAL t60
        settle().await;
        assert_eq!(
            h.batch_count(),
            0,
            "timer was reset by the second file; no flush at the original deadline"
        );

        tokio::time::advance(Duration::from_secs(50)).await; // t111 > t110
        wait_until(|| h.batch_count() == 1).await;
        assert_eq!(
            h.batches.list().unwrap()[0].file_count,
            2,
            "both files went out in the one batch after the reset window"
        );
    }

    /// Manual mode never auto-flushes; `flush_now` sends the whole pending set as
    /// one `manual` batch, and a second `flush_now` with nothing pending is a
    /// no-op.
    #[tokio::test]
    async fn manual_flush_sends_whole_pending_and_empty_is_noop() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });

        h.feed(&[0, 1, 2]).await;
        settle().await;
        assert_eq!(h.batch_count(), 0, "manual mode does not auto-flush");

        h.handle.flush_now().await;
        wait_until(|| h.batch_count() == 1).await;

        let rows = h.batches.list().unwrap();
        assert_eq!(rows[0].file_count, 3, "the whole pending set went out at once");
        assert_eq!(rows[0].mode, "manual");
        assert!(
            h.handle.pending_snapshot().is_empty(),
            "pending drained after the manual flush"
        );

        // A second flush with nothing pending records no new batch.
        h.handle.flush_now().await;
        settle().await;
        assert_eq!(h.batch_count(), 1, "an empty manual flush is a no-op");
    }

    /// A flushed batch enqueues with its derived human `display_name` and the
    /// full per-file manifest, so the sender's durable `sync_outbound_files`
    /// rows exist from enqueue time — and the `perseus_batch_files` source
    /// linkage (for a later confirmed-package rebuild) is recorded beside the
    /// batch row.
    #[tokio::test]
    async fn flush_records_display_name_and_per_file_rows() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });

        h.feed(&[0, 1, 2]).await;
        settle().await;
        h.handle.flush_now().await;
        wait_until(|| h.batch_count() == 1).await;

        let rows = h.store.all_outbound(10).unwrap();
        assert_eq!(rows.len(), 1, "one outbound row for the single target");
        let row = &rows[0];
        let name = row.display_name.clone().expect("batch has a display name");
        assert!(
            name.ends_with("(3 files)"),
            "3 flat files derive a dated fallback name, got {name:?}"
        );

        let files = h.store.list_outbound_files(row.id).unwrap();
        let mut rels: Vec<String> = files.iter().map(|f| f.rel_path.clone()).collect();
        rels.sort();
        assert_eq!(
            rels,
            vec!["a.fits", "b.fits", "c.fits"],
            "per-file rows written at enqueue time"
        );

        // The rebuild source linkage points every rel_path at its capture file.
        let package_ref = h.batches.list().unwrap()[0].package_ref.clone();
        let linkage = h.batches.files_for(&package_ref).unwrap();
        assert_eq!(linkage.len(), 3);
        assert!(
            linkage
                .iter()
                .all(|(rel, src)| src == &h.capture.join(rel)),
            "each rel_path links back to its source capture file"
        );
    }

    /// The current `(size, mtime_ms)` of a file on disk, in the same shape the
    /// seen store keys on — so a test can ask `should_enqueue` the exact question
    /// the watcher would.
    fn stat_size_mtime(path: &Path) -> (u64, i64) {
        let m = std::fs::metadata(path).expect("stat test file");
        (m.len(), crate::seen::mtime_millis(m.modified().ok()))
    }

    /// `remove_pending` takes exactly the named `(capture_dir, file)` pairs out of
    /// the accumulator and reports how many it found. A pair that is not pending
    /// (never fed, or already flushed) is simply not counted — the caller's own
    /// selection is not an assertion about what the batcher holds.
    #[tokio::test]
    async fn remove_pending_removes_exactly_the_named_pairs() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });
        h.feed(&[0, 1, 2]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 3).await;

        let removed = h.handle.remove_pending(&[
            (h.capture.clone(), h.files[0].clone()),
            (h.capture.clone(), h.files[2].clone()),
            // Never pending: same file, a different capture dir.
            (h.capture.join("elsewhere"), h.files[1].clone()),
        ]);
        assert_eq!(removed, 2, "only the two truly-pending pairs were removed");
        assert_eq!(
            h.handle.pending_snapshot(),
            vec![(h.capture.clone(), h.files[1].clone())],
            "the untouched file stays pending"
        );
    }

    /// A browser send delivers the given files as ONE `browser` batch through the
    /// same build → enqueue → record-seen → record-batch path a flush uses, and
    /// takes those files out of the pending accumulator first, so the next
    /// auto/scheduled flush cannot send them a second time (spec §1a).
    #[tokio::test]
    async fn send_explicit_delivers_a_browser_batch_and_drains_those_pending() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });
        h.feed(&[0, 1]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 2).await;

        let picked = vec![
            (h.capture.clone(), h.files[0].clone()),
            // Deliberately NOT pending: a browser send may pick any library file.
            (h.capture.clone(), h.files[2].clone()),
        ];
        let outcome = match h.handle.send_explicit(&picked, None).await {
            Delivery::Sent(outcome) => outcome,
            other => panic!("expected a delivered batch, got {other:?}"),
        };
        assert_eq!(outcome.file_count, 2);

        let rows = h.batches.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mode, "browser", "the batch is recorded as browser");
        assert_eq!(rows[0].file_count, 2);
        assert_eq!(rows[0].package_ref, outcome.package_ref);

        assert_eq!(
            h.handle.pending_snapshot(),
            vec![(h.capture.clone(), h.files[1].clone())],
            "the sent file left the pending set; the unselected one stayed"
        );

        // Both sent files are recorded seen (dedup authority), so a restart never
        // re-baselines them.
        for i in [0usize, 2] {
            let (size, mtime) = stat_size_mtime(&h.files[i]);
            assert!(
                !h.seen.should_enqueue(&h.files[i], size, mtime).unwrap(),
                "file {i} recorded seen"
            );
        }
    }

    /// A browser send that reaches ZERO targets leaves no staged package dir
    /// behind, records nothing, and is a **no-op on the accumulator**: the file
    /// it took out of pending goes back (it never shipped), while a file that was
    /// never pending is not injected into the automatic path.
    #[tokio::test]
    async fn send_explicit_with_no_targets_drops_the_package_and_restores_pending() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });
        h.feed(&[0]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 1).await;

        let picked = vec![
            (h.capture.clone(), h.files[0].clone()),
            // Never pending: a failed send must not enroll it into the auto path.
            (h.capture.clone(), h.files[1].clone()),
        ];
        // An explicit EMPTY engine set is the zero-target case.
        let package_ref = match h.handle.send_explicit(&picked, Some(&[])).await {
            Delivery::NoTarget {
                package_ref,
                file_count,
            } => {
                assert_eq!(file_count, 2);
                package_ref
            }
            other => panic!("expected a zero-target delivery, got {other:?}"),
        };
        assert!(
            !Path::new(&package_ref).exists(),
            "the orphaned package dir was removed"
        );
        assert_eq!(
            h.handle.pending_snapshot(),
            vec![(h.capture.clone(), h.files[0].clone())],
            "the taken file is back; the never-pending one was not added"
        );
        assert!(
            h.batches.list().unwrap().is_empty(),
            "no batch row for an undelivered package"
        );
        for i in [0usize, 1] {
            let (size, mtime) = stat_size_mtime(&h.files[i]);
            assert!(
                h.seen.should_enqueue(&h.files[i], size, mtime).unwrap(),
                "nothing shipped, so nothing is recorded seen"
            );
        }
    }

    /// A present-but-unbuildable file (garbage, non-FITS) drained in a batch is
    /// **not** marked seen when the batch delivers — only the files that actually
    /// made it into the package are. So the good file is deduped (never re-sent)
    /// while the corrupt-but-present file stays enqueue-eligible and is retried on
    /// the next detection / restart, exactly as the legacy per-file path did.
    /// Without this the corrupt file would be marked seen despite never shipping,
    /// and the durable seen store would silently lose it forever.
    #[tokio::test]
    async fn dropped_but_present_file_is_not_marked_seen() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });

        let good = h.files[0].clone();
        let bad = h.files[1].clone();
        // Corrupt the second fixture in place: it still EXISTS on disk, but
        // `build_batch_package` can no longer parse it, so it is dropped from the
        // package (present-but-unbuildable) rather than vanishing.
        std::fs::write(&bad, b"this is not a FITS file").expect("clobber fixture");

        h.feed(&[0, 1]).await;
        settle().await;
        h.handle.flush_now().await;
        wait_until(|| h.batch_count() == 1).await;

        // Only the good file was packaged.
        let rows = h.batches.list().unwrap();
        assert_eq!(rows[0].file_count, 1, "only the buildable file was packaged");

        // The good file is now seen (deduped — never re-sent).
        let (gs, gm) = stat_size_mtime(&good);
        assert!(
            !h.seen.should_enqueue(&good, gs, gm).unwrap(),
            "the packaged file is recorded seen"
        );

        // The corrupt-but-present file is NOT seen — it will be retried, not lost.
        let (bs, bm) = stat_size_mtime(&bad);
        assert!(
            h.seen.should_enqueue(&bad, bs, bm).unwrap(),
            "a dropped-but-present file is left enqueue-eligible for retry"
        );
    }

    /// A FLUSH that reaches zero targets re-queues its files — the batcher's
    /// oldest and most load-bearing rule, and the one thing the browser path
    /// deliberately does NOT inherit. Without it a night's frames are drained
    /// out of the accumulator, never recorded seen, and never retried: silently
    /// lost until the next restart re-detects them.
    ///
    /// Driven through [`flush_once`] directly (the same entry the loop's timer
    /// branch uses) so the assertion cannot race the scheduler.
    #[tokio::test]
    async fn a_zero_target_flush_requeues_its_files_and_records_nothing() {
        let h = Harness::spawn_with_targets(
            SendCfg {
                mode: Mode::Manual,
                auto_quiet_secs: 60,
                ..SendCfg::default()
            },
            false, // no engines: every delivery reaches nobody
        );
        h.feed(&[0]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 1).await;

        let outcome = flush_once(Mode::Auto, &h.handle.pending, &h.handle.ctx).await;
        assert!(
            outcome.is_none(),
            "a package that reached no target is not a recorded flush"
        );
        assert_eq!(
            h.handle.pending_snapshot(),
            vec![(h.capture.clone(), h.files[0].clone())],
            "the drained file is back in the accumulator for the next flush"
        );
        assert_eq!(
            h.batch_count(),
            0,
            "no batch row for an undelivered package"
        );
        let (size, mtime) = stat_size_mtime(&h.files[0]);
        assert!(
            h.seen.should_enqueue(&h.files[0], size, mtime).unwrap(),
            "nothing shipped, so nothing is recorded seen"
        );
    }

    /// Files returned to the accumulator by a failed browser send must ARM the
    /// auto quiet timer. The loop never saw them arrive, so if the window had
    /// already elapsed while they were out (the send was still hashing) nothing
    /// else would ever re-arm it — on a finished night those frames would sit
    /// pending until the next restart.
    #[tokio::test(start_paused = true)]
    async fn a_restore_re_arms_the_auto_quiet_timer() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Auto,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });
        h.feed(&[0]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 1).await;

        // The state a browser send leaves while it is delivering: its files are
        // OUT of the accumulator, so the quiet window elapses over an empty set
        // and the loop disarms the timer.
        let taken = h
            .handle
            .take_pending(&[(h.capture.clone(), h.files[0].clone())]);
        assert_eq!(taken.len(), 1);
        tokio::time::advance(Duration::from_secs(61)).await;
        settle().await;
        assert_eq!(
            h.batch_count(),
            0,
            "nothing was pending when the timer fired"
        );

        // The send failed: its file comes back, and nothing else ever arrives.
        h.handle.restore_pending(taken);
        settle().await;
        tokio::time::advance(Duration::from_secs(61)).await;
        wait_until(|| h.batch_count() == 1).await;

        assert_eq!(
            h.batches.list().unwrap()[0].file_count,
            1,
            "the returned file went out on the re-armed window"
        );
        assert!(h.handle.pending_snapshot().is_empty(), "and drained");
    }
}
