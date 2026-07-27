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
//! - **Scheduled** ([`Mode::Scheduled`], 0.5.1 §3): a **wall-clock calendar**.
//!   Files accumulate all night and the whole set flushes at each configured
//!   local time (`06:00`, `14:30`, …) as one `scheduled` batch. Unlike the auto
//!   quiet timer, a newly stabilized file does **not** move the fire time: the
//!   schedule is a property of the clock, not of the capture activity. A point
//!   that elapses with nothing pending is a no-op flush that still counts as a
//!   fire (see [`fire_scheduled`]), and a point missed while the agent was down
//!   is caught up **once** shortly after startup (see [`startup_catch_up`] and
//!   [`catch_up_grace`] — the grace exists because a catch-up fired at the
//!   batcher's very first instant would always find an empty accumulator).
//!
//! The mode is read **live** from a [`watch`] channel, so a web-side edit takes
//! effect on the running batcher with no restart: switching to Manual disarms
//! both timers, switching to Auto re-arms the quiet timer if anything is already
//! pending, and switching to Scheduled (or editing its times) re-derives the next
//! fire from the wall clock.
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
//! silently lost.
//!
//! A flush that ships nothing has exactly two fates, and they turn on whether
//! retrying could ever help. A **zero-target** delivery or a package that could
//! not be **written** (a file deleted mid-build, a full disk) re-queues its
//! drained files — they were never recorded seen, so the next flush retries them
//! and nothing is lost. A batch whose every file was individually unbuildable
//! ([`NoBuildableFiles`]) is dropped with a `warn!` instead: re-queuing frames
//! that cannot be parsed would spin the accumulator forever, and they stay unseen
//! either way. The batcher loop never fails on a bad batch — a single flush error
//! is logged and the loop continues.
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
use chrono::{DateTime, Local};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Duration, Instant};

use crate::batch_store::{BatchStore, KEY_LAST_SCHEDULED_FIRE};
use crate::config::{Config, Mode, SendCfg};
use crate::run::{build_batch_package, enqueue_package_to_all, record_seen, NoBuildableFiles};
use crate::schedule;
use crate::seen::SeenStore;

/// Capacity of the manual-flush signal channel. A handful of buffered "send now"
/// clicks is plenty — each drains the whole pending set, so extra signals past
/// the first collapse into cheap empty no-op flushes.
const FLUSH_CHANNEL_CAP: usize = 8;

/// Longest single sleep the scheduled arm ever takes, however far away the next
/// fire is.
///
/// The fire time is a **calendar** instant; `sleep_until` waits on a
/// **monotonic** one. Sleeping the whole gap in one go would bake the current
/// clock offset into a timer that survives NTP steps, a suspended laptop and an
/// operator fixing the date — a machine that resumes an hour late would still
/// wait out its original ten hours. So the loop sleeps in short hops and
/// re-derives the remaining time from the wall clock on every wake: any clock
/// jump is absorbed within one tick, forward (fire promptly, at the first wake
/// where the point is in the past) and backward alike (the target simply stays
/// in the future and is waited for again).
///
/// A minute is the resolution the schedule itself has (points are `HH:MM`), so
/// this costs at most one wasted wake per minute per agent and can never make a
/// fire more than a tick late.
const SCHEDULE_TICK: Duration = Duration::from_secs(60);

/// How long after a scheduled point that delivered NOTHING the batcher tries
/// that point again (spec §3). A point whose flush re-queued its files (no target
/// was reachable, the package could not be written) has not happened as far as
/// the operator is concerned — without a retry the night would sit in the
/// accumulator until tomorrow's point, because nothing else re-arms the calendar.
const SCHEDULE_RETRY_BACKOFF: Duration = SCHEDULE_TICK;

/// How many times such a point is retried before it is left to the next ordinary
/// point. Bounded on purpose: an agent whose only target has been offline for an
/// hour must not rebuild the same package every minute all night. The fire is
/// never stamped along the way, so a restart still owes the catch-up.
const SCHEDULE_RETRY_MAX: u32 = 5;

/// The batcher's view of the local wall clock — [`chrono::Local::now`] in
/// production, a tokio-driven fake in the tests.
///
/// Scheduled mode is the only consumer and the whole reason for the
/// indirection: `tokio`'s `start_paused` moves `Instant` but never
/// `Local::now()`, so without an injectable clock a calendar scheduler is
/// untestable — the tests would have to sleep in real seconds and could never
/// reach tomorrow's 06:00 at all.
pub(crate) type WallClock = Arc<dyn Fn() -> DateTime<Local> + Send + Sync>;

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

/// What one flush did, as its callers need to see it.
///
/// Richer than an `Option` because the SCHEDULED arm has to tell "the point
/// delivered everything it could" (an empty accumulator, a batch of individually
/// bad frames) from "the point delivered nothing and its files are still
/// waiting". Only the latter must leave `last_scheduled_fire` unstamped and arm a
/// retry — stamping it would park the night until tomorrow's point and tell every
/// future restart the point was served.
#[derive(Debug)]
enum FlushResult {
    /// Nothing was pending: no package, no rows, nothing to retry.
    Empty,
    /// A package reached at least one target.
    Sent(FlushOutcome),
    /// Nothing shipped and the drained files went BACK into the accumulator — a
    /// zero-target delivery, or a package that could not be written. Both causes
    /// are transient, so the next flush (or the next point) retries them.
    Requeued,
    /// Nothing shipped and there was nothing to retry: every file in the batch
    /// was individually unbuildable ([`NoBuildableFiles`]), so re-queuing them
    /// would only spin the accumulator. They stay unseen and are picked up again
    /// on the next detection / restart.
    Dropped,
}

/// Drain the pending set and, if non-empty, build one package, fan it to every
/// target, and record the files + the batch.
///
/// This is the whole flush body, factored out of the loop so tests can drive it
/// directly against a loopback engine without waiting on the timer. The four
/// outcomes ([`FlushResult`]):
///
/// - **empty** pending set — nothing to do ([`Empty`](FlushResult::Empty));
/// - **delivered** — a package was built, reached ≥1 target and a batch row was
///   written ([`Sent`](FlushResult::Sent));
/// - **zero-target** delivery, or a package that could not be **written** — the
///   files were never [`record_seen`]'d, so they are **re-inserted into
///   `pending`** and retried on the next flush
///   ([`Requeued`](FlushResult::Requeued));
/// - **nothing buildable** — every file in the batch is individually unparseable
///   / unhashable; dropped with a `warn!` rather than re-queued, since retrying
///   them changes nothing and would spin ([`Dropped`](FlushResult::Dropped)).
///
/// A single bad flush never propagates: the caller (the loop) keeps running.
async fn flush_once(
    mode: Mode,
    pending: &Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>>,
    ctx: &SendContext,
) -> FlushResult {
    // Drain under the lock, then release it before any await (never hold a std
    // Mutex across .await).
    let files: Vec<(PathBuf, PathBuf)> = {
        let mut guard = pending.lock().expect("batcher pending mutex poisoned");
        if guard.is_empty() {
            return FlushResult::Empty;
        }
        std::mem::take(&mut *guard).into_iter().collect()
    };

    match deliver_batch(ctx, &ctx.engines, &files, mode_str(mode)).await {
        Delivery::Sent(outcome) => FlushResult::Sent(outcome),
        Delivery::Unbuildable(error) if error.downcast_ref::<NoBuildableFiles>().is_some() => {
            // Every file in the batch is individually unbuildable (vanished,
            // won't parse, won't hash). There is nothing to retry — putting them
            // back would rebuild and re-fail them every quiet window forever.
            tracing::warn!(
                %error,
                count = files.len(),
                mode = mode_str(mode),
                "batch had no buildable files; dropping"
            );
            FlushResult::Dropped
        }
        Delivery::Unbuildable(error) => {
            // The records existed but the PACKAGE could not be written: the whole
            // batch vanished between the two build passes, the disk is full, the
            // packages dir is unwritable. Transient by construction — and the
            // files are in no store at all right now (not seen, not pending), so
            // dropping them would strand a night behind the watcher's emitted set
            // until the next restart. Put them back.
            tracing::error!(
                %error,
                count = files.len(),
                mode = mode_str(mode),
                "batch could not be packaged; re-queuing its files for retry"
            );
            requeue(pending, files);
            FlushResult::Requeued
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
            requeue(pending, files);
            FlushResult::Requeued
        }
    }
}

/// Put a failed flush's drained files back into the accumulator. The caller has
/// already logged WHY; this is only the bookkeeping.
fn requeue(pending: &Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>>, files: Vec<(PathBuf, PathBuf)>) {
    let mut guard = pending.lock().expect("batcher pending mutex poisoned");
    for item in files {
        guard.insert(item);
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
    stable_rx: mpsc::Receiver<(PathBuf, PathBuf)>,
    engines: Vec<Arc<SyncEngineHandle>>,
    seen: Arc<SeenStore>,
    batches: Arc<BatchStore>,
    config: Config,
    origin_device: String,
    cleanup: Option<Arc<SharedPackageCleanup>>,
    send_cfg_rx: watch::Receiver<SendCfg>,
) -> (BatcherHandle, JoinHandle<()>) {
    spawn_batcher_with_clock(
        stable_rx,
        engines,
        seen,
        batches,
        config,
        origin_device,
        cleanup,
        send_cfg_rx,
        Arc::new(Local::now),
    )
}

/// [`spawn_batcher`] with the wall clock the scheduled arm reads injected — the
/// production entry point passes [`chrono::Local::now`]; the tests pass a clock
/// driven by tokio's paused timer, which is the only way to watch a calendar
/// schedule reach 06:00 inside a unit test (see [`WallClock`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_batcher_with_clock(
    mut stable_rx: mpsc::Receiver<(PathBuf, PathBuf)>,
    engines: Vec<Arc<SyncEngineHandle>>,
    seen: Arc<SeenStore>,
    batches: Arc<BatchStore>,
    config: Config,
    origin_device: String,
    cleanup: Option<Arc<SharedPackageCleanup>>,
    mut send_cfg_rx: watch::Receiver<SendCfg>,
    clock: WallClock,
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
        // The scheduled arm's target, in WALL-CLOCK time (`None` = disarmed: any
        // mode but Scheduled, or Scheduled with no points). Kept as a calendar
        // instant, never as a monotonic one, so every wake can re-check it against
        // the real clock — see [`SCHEDULE_TICK`].
        //
        // Seeded before the first select! so a catch-up owed from a point missed
        // while the agent was down (spec §3) is armed exactly once, at startup: the
        // earlier of that owed fire and the ordinary next point.
        //
        // …but never BEFORE the startup settle grace ends. An agent restarted at
        // 05:59 with a 06:00 point would otherwise fire a minute later into an
        // accumulator the watchers have not filled yet — by construction, since a
        // file only arrives after a full write-stability window — flush nothing,
        // and stamp the point as served. The point still happens; it just waits
        // for the watchers to re-baseline, exactly like the catch-up fire does
        // (see [`catch_up_grace`]).
        let settle_end = settle_until(clock(), &ctx.config);
        let mut sched_fire: Option<DateTime<Local>> = [
            startup_catch_up(&cfg, &ctx, &clock),
            arm_schedule(&cfg, &clock),
        ]
        .into_iter()
        .flatten()
        .min()
        .map(|fire| fire.max(settle_end));
        // Consecutive retries of a scheduled point that delivered nothing (S4),
        // reset by the first one that does. Bounded by [`SCHEDULE_RETRY_MAX`].
        let mut sched_retries: u32 = 0;
        // Guards to disable a select! branch whose channel has closed, so a closed
        // channel never spins the loop. In practice both senders outlive the loop
        // (the watcher channel closes first, breaking us out), but this is cheap
        // insurance against a hot loop.
        let mut cfg_open = true;
        let mut flush_open = true;

        loop {
            // The scheduled arm's tokio deadline, re-derived from the wall clock on
            // every turn (cheap: one `Local::now()` only while the schedule is
            // armed). Deriving it here rather than caching an `Instant` is what
            // makes a clock jump self-correcting — see [`SCHEDULE_TICK`].
            let sched_wake = sched_fire.as_ref().map(|fire| schedule_wake(fire, &clock));

            tokio::select! {
                // A watcher stabilized a file. Accumulate it; in auto mode this
                // (re)arms the quiet timer — a steady drip keeps pushing it out.
                // In SCHEDULED mode it deliberately does not: the fire time is a
                // property of the calendar, not of the capture activity.
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

                // The scheduled arm (spec §3). This wakes MUCH more often than it
                // fires: `sched_wake` is `min(fire, now + SCHEDULE_TICK)`, so most
                // wakes are just "re-read the wall clock". The point is due only
                // when the calendar says so — never when the monotonic timer alone
                // says so, which is what keeps a suspended laptop or an NTP step
                // from firing a day early.
                () = async {
                    match sched_wake {
                        Some(at) => sleep_until(at).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let now = clock();
                    // Re-derive on EVERY wake, fired or not: after a fire this is
                    // the next point (strictly after `now`, so a fire can never
                    // re-arm itself), and after a plain tick it is a no-op — unless
                    // the clock moved, which is precisely the case it exists to
                    // absorb.
                    //
                    // Corollary of re-deriving AFTER the flush: a flush that runs
                    // long enough to overrun a closely-following point (06:00 and
                    // 06:05 with a five-minute send) skips that point — its files
                    // simply ride the next one. Deliberate: the alternative is a
                    // fire that immediately re-fires into a set the flush it just
                    // waited for has already drained.
                    if sched_fire.as_ref().is_some_and(|fire| *fire <= now) {
                        if fire_scheduled(now, &loop_pending, &ctx).await {
                            sched_retries = 0;
                            sched_fire = arm_schedule(&cfg, &clock);
                        } else {
                            // The point delivered NOTHING and its files are back in
                            // the accumulator (no target, or the package could not
                            // be written). Nothing was stamped, so re-arming the
                            // ordinary next point here would park the night for a
                            // day; arm a bounded short retry instead.
                            sched_fire = schedule_retry(&cfg, &clock, &mut sched_retries);
                        }
                    } else if sched_retries == 0 {
                        sched_fire = arm_schedule(&cfg, &clock);
                    }
                    // (A plain tick while a retry is armed leaves that retry alone —
                    // re-deriving would replace it with the next ordinary point,
                    // which is exactly what the retry exists to avoid.)
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
                //
                // `sched_fire` is deliberately NOT touched here: a scheduled fire
                // is a wall-clock appointment, not an activity-derived window, so
                // returned files change *what* the next point sends, never *when*
                // it happens. (Recomputing would be harmless but misleading — it
                // would suggest the accumulator's contents move the calendar.)
                () = rearm_signal.notified() => {
                    deadline = rearm(&loop_pending, &cfg);
                }

                // A live config edit (Task 6). Adopt the new mode/quiet window/
                // schedule: Manual disarms both timers; Auto re-arms the quiet
                // timer iff something is already pending; Scheduled re-derives its
                // next fire from the wall clock (which is also how a live edit of
                // `schedule_times` takes effect).
                changed = send_cfg_rx.changed(), if cfg_open => {
                    match changed {
                        Ok(()) => {
                            cfg = send_cfg_rx.borrow_and_update().clone();
                            deadline = match cfg.mode {
                                // The AUTO quiet timer is off in both of the other
                                // modes; scheduled mode drives `sched_fire` below.
                                Mode::Manual | Mode::Scheduled => None,
                                Mode::Auto => rearm(&loop_pending, &cfg),
                            };
                            // A point that is ALREADY DUE must not vanish into a
                            // config edit that won the select! race against its own
                            // fire arm. `next_fire` is strictly-after `now`, so
                            // re-deriving over a due arm would silently defer that
                            // send by up to a day (nothing is stamped, so a restart
                            // still catches it up — but only a restart). Serve it
                            // first instead, exactly as the fire arm would have.
                            let mut served_due: Option<bool> = None;
                            if let Some(fire) = sched_fire.filter(|fire| *fire <= clock()) {
                                if arm_schedule(&cfg, &clock).is_some() {
                                    served_due =
                                        Some(fire_scheduled(clock(), &loop_pending, &ctx).await);
                                } else {
                                    // The edit left scheduled mode (or emptied the
                                    // points): there is no schedule left to serve
                                    // the point on, so it is dropped — out loud.
                                    tracing::warn!(
                                        dropped_fire = %fire,
                                        "config edit dropped a due scheduled point"
                                    );
                                }
                            }
                            // No catch-up here, deliberately: catch-up answers
                            // "did a point elapse while the agent was DOWN", and
                            // this process has been up the whole time. A flip into
                            // scheduled mode simply waits for the next point (spec
                            // §3: a non-empty pending set is taken by that point,
                            // nothing is lost or double-armed).
                            //
                            // An edit landing inside the startup catch-up's short
                            // grace window therefore replaces that owed fire with
                            // the ordinary next point. It is not lost, only
                            // deferred: nothing was stamped, so the next start
                            // finds the same missed point and catches up then.
                            // (Once that owed fire comes DUE the guard above serves
                            // it instead of replacing it.)
                            sched_fire = match served_due {
                                // The point we just served delivered nothing: keep
                                // it alive on a bounded retry rather than deferring
                                // it to the edited schedule's next point — the same
                                // rule the fire arm applies.
                                Some(false) => schedule_retry(&cfg, &clock, &mut sched_retries),
                                _ => {
                                    sched_retries = 0;
                                    arm_schedule(&cfg, &clock)
                                }
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

/// The next wall-clock fire in [`Mode::Scheduled`], or `None` — any other mode,
/// or scheduled with no points (an empty `schedule_times` **disarms** rather
/// than guessing a time; validation refuses to persist that combination, so it
/// can only arrive from a hand-built config).
///
/// Unlike [`rearm`] this does not look at the pending set: a schedule point is
/// due whether or not anything accumulated, and a point that finds nothing
/// pending is a no-op flush that still counts as a fire (see
/// [`fire_scheduled`]).
fn arm_schedule(cfg: &SendCfg, clock: &WallClock) -> Option<DateTime<Local>> {
    if cfg.mode != Mode::Scheduled {
        return None;
    }
    schedule::next_fire(&cfg.schedule_times, clock())
}

/// The tokio deadline for the next scheduled wake: `min(fire, now +
/// SCHEDULE_TICK)`, translated from the calendar into the monotonic clock at the
/// last possible moment.
///
/// A `fire` already in the past yields `Instant::now()` (zero remaining), so an
/// overdue point is served on the very next loop turn instead of waiting out a
/// tick.
fn schedule_wake(fire: &DateTime<Local>, clock: &WallClock) -> Instant {
    let remaining = (*fire - clock())
        .to_std()
        .unwrap_or(Duration::ZERO)
        .min(SCHEDULE_TICK);
    Instant::now() + remaining
}

/// One scheduled fire: the same drain-and-flush the manual button performs,
/// recorded as a `scheduled` batch, followed by the `last_scheduled_fire` stamp.
/// Returns whether the point **delivered** — `false` only when the flush shipped
/// nothing and put its files back, which is the one case the caller must retry.
///
/// Three things are deliberate here:
///
/// - **An empty pending set still counts as a fire.** No batch row is written
///   (the batcher never records empty batches), but the stamp advances — the
///   stamp records "the schedule reached this point", not "a package went out".
///   Without that, a quiet night would look like a missed point forever and
///   every restart would catch up again.
/// - **A point that shipped NOTHING and re-queued its files does not stamp.** It
///   did not reach the peer, the frames are still queued, and stamping would both
///   park them until tomorrow's point and tell every future restart the point was
///   served. Leaving the stamp alone is what makes catch-up owe it.
///   ([`FlushResult::Dropped`] does stamp: those files are unbuildable, so the
///   point delivered everything it ever could.)
/// - **The stamp is written after the flush, never before.** A crash in between
///   costs one redundant catch-up on the next start (whose files the receiver's
///   dedup handshake trims anyway); the opposite order would silently swallow
///   the fire.
async fn fire_scheduled(
    at: DateTime<Local>,
    pending: &Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>>,
    ctx: &SendContext,
) -> bool {
    let result = flush_once(Mode::Scheduled, pending, ctx).await;
    let delivered = !matches!(result, FlushResult::Requeued);
    if delivered {
        stamp_scheduled_fire(&ctx.batches, at);
    }
    tracing::info!(
        at = %at.to_rfc3339(),
        file_count = match &result {
            FlushResult::Sent(outcome) => outcome.file_count,
            _ => 0,
        },
        delivered,
        "scheduled fire"
    );
    delivered
}

/// Arm the next attempt at a scheduled point that delivered nothing, or give up
/// on it: `min(now + `[`SCHEDULE_RETRY_BACKOFF`]`, next ordinary point)` until
/// [`SCHEDULE_RETRY_MAX`] attempts are spent, then the ordinary next point.
///
/// Bounded and never stamped: a target that has been offline for an hour must not
/// make the agent rebuild the same package every minute all night, and a point
/// abandoned here is still owed — the files stay in the accumulator for the next
/// point, and a restart in between catches it up.
///
/// `None` disarms, exactly as [`arm_schedule`] does: an edit that left scheduled
/// mode (or emptied the points) leaves no schedule to retry on.
fn schedule_retry(
    cfg: &SendCfg,
    clock: &WallClock,
    retries: &mut u32,
) -> Option<DateTime<Local>> {
    let next_point = arm_schedule(cfg, clock)?;
    if *retries >= SCHEDULE_RETRY_MAX {
        tracing::warn!(
            attempts = *retries,
            next_point = %next_point.to_rfc3339(),
            "a scheduled point never delivered; leaving its files to the next point (the fire stays unstamped, so a restart catches it up)"
        );
        *retries = 0;
        return Some(next_point);
    }
    *retries += 1;
    let now = clock();
    let at = now
        .checked_add_signed(
            chrono::TimeDelta::from_std(SCHEDULE_RETRY_BACKOFF).unwrap_or_default(),
        )
        .unwrap_or(now)
        .min(next_point);
    // Once, loudly: the operator needs to know the 06:00 send did not happen. The
    // attempts after it are the same fact repeating.
    if *retries == 1 {
        tracing::warn!(
            retry_at = %at.to_rfc3339(),
            "a scheduled point delivered nothing; its files are still queued and it will be retried shortly"
        );
    } else {
        tracing::debug!(attempt = *retries, retry_at = %at.to_rfc3339(), "retrying a scheduled point");
    }
    Some(at)
}

/// Persist the time of the last scheduled fire (the catch-up window's lower
/// bound). Best-effort: a failed write only costs one redundant catch-up on the
/// next start, so it warns rather than derailing a delivered batch — but it is
/// never swallowed, because a *permanently* unwritable stamp means every restart
/// re-fires.
fn stamp_scheduled_fire(batches: &BatchStore, at: DateTime<Local>) {
    let value = at.to_rfc3339();
    if let Err(error) = batches.meta_set(KEY_LAST_SCHEDULED_FIRE, &value) {
        tracing::warn!(
            %error,
            value = %value,
            "failed to stamp the last scheduled fire; a restart may catch up again"
        );
    }
}

/// How long after startup an owed catch-up fire waits before flushing.
///
/// A catch-up fired at the batcher's very first instant would flush an **empty**
/// set every time and be a stamp, not a send: at that moment the watchers have
/// not yet re-detected last night's unsent frames, and by construction cannot
/// have — a file only reaches the accumulator once it has been seen unchanged
/// for the write-stability window. So the owed fire is armed a short settle
/// period out, derived from the operator's own watcher knobs (one full stability
/// window plus a few poll ticks of slack) rather than a new invented one.
///
/// Files that take longer than that to settle (a very large tree on a slow
/// share) are not lost — they simply go out at the next point, or on the
/// operator's "Send N pending now".
fn catch_up_grace(config: &Config) -> Duration {
    config.stability() + 3 * config.poll_interval()
}

/// The wall-clock instant the startup settle grace ends: `now + `
/// [`catch_up_grace`]. Every fire armed at startup — the owed catch-up AND an
/// ordinary point that happens to fall inside the grace — is deferred to at least
/// this instant, so no fire runs against an accumulator the watchers have not
/// refilled yet.
fn settle_until(now: DateTime<Local>, config: &Config) -> DateTime<Local> {
    now.checked_add_signed(
        chrono::TimeDelta::from_std(catch_up_grace(config)).unwrap_or_default(),
    )
    .unwrap_or(now)
}

/// The startup catch-up check (spec §3), run **once** before the first arm:
/// returns the instant an owed catch-up fire should happen at, or `None` when
/// nothing is owed.
///
/// One fire, never one per missed point — [`schedule::missed_point`] returns
/// only the LATEST point in `(last_fire, now]`, so a week of downtime owes a
/// single send (the pending set is drained whole either way; N fires would be
/// N-1 empty no-ops with N-1 misleading history rows).
///
/// The "never fired before" case is **not** a catch-up: nothing can have been
/// missed before the feature was switched on, and treating an absent stamp as
/// "infinitely behind" would make the first start in scheduled mode always fire.
/// It writes the stamp as a *baseline* instead, so the very next start can tell a
/// genuinely missed point from a fresh install — including a crash-loop that
/// restarts across a point.
fn startup_catch_up(
    cfg: &SendCfg,
    ctx: &SendContext,
    clock: &WallClock,
) -> Option<DateTime<Local>> {
    if cfg.mode != Mode::Scheduled {
        return None;
    }
    let now = clock();
    let stamp = match ctx.batches.meta_get(KEY_LAST_SCHEDULED_FIRE) {
        Ok(stamp) => stamp,
        Err(error) => {
            // Unknown window: neither catch up (it could fire for a point that
            // already fired) nor baseline (it could clobber a good stamp).
            tracing::warn!(%error, "failed to read the last scheduled fire; skipping the catch-up check");
            return None;
        }
    };
    let last = match stamp.as_deref().map(DateTime::parse_from_rfc3339) {
        Some(Ok(last)) => Some(last.with_timezone(&Local)),
        Some(Err(error)) => {
            tracing::warn!(
                %error,
                value = %stamp.unwrap_or_default(),
                "unparseable last_scheduled_fire; treating this start as the first"
            );
            None
        }
        None => None,
    };
    let Some(last) = last else {
        stamp_scheduled_fire(&ctx.batches, now);
        tracing::info!(
            at = %now.to_rfc3339(),
            "scheduled mode has no previous fire on record; recording a baseline instead of catching up"
        );
        return None;
    };
    if !cfg.schedule_catchup {
        tracing::debug!(
            last_fire = %last.to_rfc3339(),
            "schedule_catchup is off; not checking for a missed point"
        );
        return None;
    }
    let missed = schedule::missed_point(&cfg.schedule_times, last, now)?;
    let at = settle_until(now, &ctx.config);
    tracing::info!(
        last_fire = %last.to_rfc3339(),
        missed_point = %missed.to_rfc3339(),
        at = %at.to_rfc3339(),
        "catching up one schedule point missed while the agent was down"
    );
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
    use athenaeum_core::fits_writer::write_fits_f32;
    use athenaeum_core::sharing::loopback::LoopbackNetwork;
    use athenaeum_core::sync::store::{StandaloneSyncStore, SyncStore};
    use athenaeum_core::sync::SyncEngine;
    use chrono::TimeZone;
    use std::path::Path;

    /// A fixed local wall-clock instant on 2026-07-26/27 — a plain summer weekend
    /// with no DST transition in any real zone at these hours, so the schedule
    /// tests are deterministic whatever the machine's time zone is. (A zone that
    /// somehow had a gap here would `expect`-fail loudly rather than skew a
    /// deadline silently.)
    fn local_at(day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 7, day, hour, minute, 0)
            .earliest()
            .expect("2026-07-{day} {hour}:{minute} local exists in this zone")
    }

    /// [`local_at`] with seconds — for the tests that care where inside a minute
    /// the agent booted.
    fn local_at_s(day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 7, day, hour, minute, second)
            .earliest()
            .expect("2026-07-{day} {hour}:{minute}:{second} local exists in this zone")
    }

    /// The startup settle grace the harness config produces (`stability_secs=30`
    /// + 3 × `poll_interval_secs=5`) — the window every startup-armed fire is
    /// deferred past. Spelled out so a test asserting around it says WHY.
    const SETTLE_GRACE: Duration = Duration::from_secs(45);

    /// Arm the build's mid-build test seam ([`crate::run::MID_BUILD_HOOK`]) with a
    /// one-shot closure. It fires between the pre-copy check and the payload copy
    /// of the next package built ON THIS THREAD — which, under a current-thread
    /// runtime, is the batcher task's own build.
    fn arm_mid_build<F: FnOnce() + 'static>(hook: F) {
        crate::run::MID_BUILD_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    /// A [`WallClock`] driven by tokio's (paused) timer: `base + elapsed`.
    ///
    /// This is the whole reason the batcher takes an injectable clock.
    /// `start_paused` moves `Instant`, never `chrono::Local::now()`, so a
    /// scheduler test driven by the real clock could only ever assert "it did not
    /// fire yet". Deriving the wall clock from the same virtual timeline makes
    /// `tokio::time::advance(2h)` mean "two hours passed" to the calendar too.
    fn virtual_clock(base: DateTime<Local>) -> WallClock {
        skewable_clock(base).0
    }

    /// [`virtual_clock`] plus a test-settable **skew** — the only way to move the
    /// calendar *independently* of the monotonic timer, which is exactly the
    /// divergence [`SCHEDULE_TICK`] exists to absorb (a suspended machine waking
    /// hours later, an NTP step, an operator fixing the date).
    fn skewable_clock(base: DateTime<Local>) -> (WallClock, Arc<Mutex<chrono::TimeDelta>>) {
        let skew = Arc::new(Mutex::new(chrono::TimeDelta::zero()));
        let origin = Instant::now();
        let handle = Arc::clone(&skew);
        let clock: WallClock = Arc::new(move || {
            let skew = *handle.lock().expect("skew mutex poisoned");
            base + chrono::TimeDelta::from_std(origin.elapsed()).unwrap_or_default() + skew
        });
        (clock, skew)
    }

    /// A scheduled-mode [`SendCfg`] over `times`, with catch-up as given.
    fn scheduled_cfg(times: &[(u8, u8)], catchup: bool) -> SendCfg {
        SendCfg {
            mode: Mode::Scheduled,
            auto_quiet_secs: 60,
            schedule_times: times.to_vec(),
            schedule_catchup: catchup,
        }
    }

    /// The non-default knobs a test's batcher is spawned with.
    #[derive(Default)]
    struct Opts {
        /// Give the batcher an EMPTY engine set — the zero-target agent (dead
        /// worker / no configured peer), where every delivery reaches nobody.
        no_targets: bool,
        /// Drive the batcher's wall clock from the paused tokio timer (see
        /// [`virtual_clock`] / [`skewable_clock`]). `None` = the real clock.
        clock: Option<WallClock>,
        /// Seed `perseus_meta.last_scheduled_fire` before the loop starts — the
        /// state a restart finds.
        last_fire: Option<DateTime<Local>>,
    }

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
        /// The live send-config sender. Held so the batcher's
        /// `send_cfg_rx.changed()` branch never sees a dropped sender for the life
        /// of the test, and used by the mode-flip test to push an edit.
        cfg_tx: watch::Sender<SendCfg>,
    }

    impl Harness {
        /// Spawn a batcher seeded with `send_cfg`, over a fresh temp tree with
        /// three parseable fixture FITS in the capture dir and one loopback engine
        /// as the sole target. The temp tree is intentionally leaked (`keep`) so
        /// the built package survives the assertions.
        fn spawn(send_cfg: SendCfg) -> Self {
            Self::spawn_with(send_cfg, Opts::default())
        }

        /// [`spawn`](Self::spawn) with the non-default knobs in [`Opts`].
        fn spawn_with(send_cfg: SendCfg, opts: Opts) -> Self {
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

            // `stability_secs` + `poll_interval_secs` are spelled out (rather than
            // defaulted) because they define the startup settle grace
            // (`catch_up_grace` = stability + 3 × poll = 45 s here), which the
            // scheduled tests measure against. A named 45 s reads far better in
            // those assertions than an implicit 16 s.
            let toml = format!(
                "capture_dir=\"{}\"\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"auto\"\n\
                 stability_secs=30\npoll_interval_secs=5\n\
                 [retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
                capture.display(),
                data.display()
            );
            let config = Config::from_toml_str(&toml).unwrap();

            let seen = Arc::new(SeenStore::open(config.db_path()).unwrap());
            let batches = Arc::new(BatchStore::open(config.db_path()).unwrap());
            let store = Arc::new(StandaloneSyncStore::open(config.db_path()).unwrap());
            // The state a restart finds: what the last scheduled fire was, if any.
            if let Some(last) = opts.last_fire {
                batches
                    .meta_set(KEY_LAST_SCHEDULED_FIRE, &last.to_rfc3339())
                    .unwrap();
            }

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

            let engines = if opts.no_targets {
                Vec::new()
            } else {
                vec![Arc::clone(&engine)]
            };
            let clock: WallClock = opts.clock.unwrap_or_else(|| Arc::new(Local::now));
            let (handle, task) = spawn_batcher_with_clock(
                stable_rx,
                engines,
                Arc::clone(&seen),
                Arc::clone(&batches),
                config,
                "aa".repeat(32),
                None,
                cfg_rx,
                clock,
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
                cfg_tx,
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

        /// The persisted `last_scheduled_fire`, parsed back into local time.
        /// Panics if it was never written — every scheduled-mode start records at
        /// least a baseline.
        fn last_fire(&self) -> DateTime<Local> {
            let raw = self
                .batches
                .meta_get(KEY_LAST_SCHEDULED_FIRE)
                .unwrap()
                .expect("a scheduled-mode batcher always records a fire time");
            DateTime::parse_from_rfc3339(&raw)
                .expect("the stamp is RFC-3339")
                .with_timezone(&Local)
        }

        /// The rel_paths the batch with `mode` carried, sorted — "which files
        /// actually went out in *that* batch".
        fn files_in_batch(&self, mode: &str) -> Vec<String> {
            let rows = self.batches.list().unwrap();
            let row = rows
                .iter()
                .find(|r| r.mode == mode)
                .unwrap_or_else(|| panic!("no {mode} batch was recorded"));
            let mut rels: Vec<String> = self
                .batches
                .files_for(&row.package_ref)
                .unwrap()
                .into_iter()
                .map(|(rel, _)| rel)
                .collect();
            rels.sort();
            rels
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
        let h = Harness::spawn_with(
            SendCfg {
                mode: Mode::Manual,
                auto_quiet_secs: 60,
                ..SendCfg::default()
            },
            Opts {
                no_targets: true, // no engines: every delivery reaches nobody
                ..Opts::default()
            },
        );
        h.feed(&[0]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 1).await;

        let outcome = flush_once(Mode::Auto, &h.handle.pending, &h.handle.ctx).await;
        assert!(
            matches!(outcome, FlushResult::Requeued),
            "a package that reached no target is not a recorded flush, and its files are put back: {outcome:?}"
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

    /// A file deleted **mid-build** — after the batch was hashed, while its
    /// payloads are being copied — costs only itself. The rest of the batch
    /// ships.
    ///
    /// This is the Library's headline feature happening at the worst possible
    /// moment (and equally an EIO on one frame, or the retention sweep landing
    /// mid-flush). The copy pass used to be all-or-nothing: one `fs::copy`
    /// failure failed the whole package, and the drained files ended up in NO
    /// store — not seen, not pending — with the watcher's emitted set blocking
    /// re-detection until a restart. On a scheduled multi-minute build that is a
    /// whole night lost to one deleted junk frame.
    #[tokio::test]
    async fn a_file_deleted_mid_build_costs_only_itself() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });
        h.feed(&[0, 1, 2]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 3).await;

        // The exact race: `b.fits` survives the manifest pass (it is parsed,
        // stat-ed and hashed) and is gone by the time its payload is copied.
        let doomed = h.files[1].clone();
        arm_mid_build(move || std::fs::remove_file(&doomed).expect("delete mid-build"));

        h.handle.flush_now().await;
        wait_until(|| h.batch_count() == 1).await;

        assert_eq!(
            h.files_in_batch("manual"),
            vec!["a.fits", "c.fits"],
            "the batch shipped without the file that vanished under it"
        );
        assert_eq!(h.batches.list().unwrap()[0].file_count, 2);
        assert!(
            h.handle.pending_snapshot().is_empty(),
            "the flush still drained the accumulator"
        );
        // The two that shipped are deduped; nothing is left half-recorded.
        for i in [0usize, 2] {
            let (size, mtime) = stat_size_mtime(&h.files[i]);
            assert!(
                !h.seen.should_enqueue(&h.files[i], size, mtime).unwrap(),
                "file {i} shipped, so it is recorded seen"
            );
        }
    }

    /// A batch whose PACKAGE could not be written at all is re-queued, not
    /// dropped: the cause (every file gone at once, a full disk, an unwritable
    /// packages dir) is transient, and the drained files are in no store while it
    /// lasts. The partial package dir is cleaned up behind it.
    ///
    /// The contrast with `dropped_but_present_file_is_not_marked_seen` is the
    /// whole point: individually unbuildable frames are DROPPED (re-queuing them
    /// would spin), while a failed write is RETRIED.
    #[tokio::test]
    async fn a_batch_that_cannot_be_packaged_is_requeued_and_leaves_no_dir() {
        let h = Harness::spawn(SendCfg {
            mode: Mode::Manual,
            auto_quiet_secs: 60,
            ..SendCfg::default()
        });
        h.feed(&[0, 1]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 2).await;

        // Every file in the batch vanishes between the two build passes: no
        // package can be written at all.
        let doomed: Vec<PathBuf> = h.files[..2].to_vec();
        arm_mid_build(move || {
            for f in &doomed {
                std::fs::remove_file(f).expect("delete mid-build");
            }
        });

        h.handle.flush_now().await;
        wait_until(|| h.handle.pending_snapshot().len() == 2).await;

        assert_eq!(h.batch_count(), 0, "no batch row for a package never written");
        assert_eq!(
            h.handle.pending_snapshot(),
            vec![
                (h.capture.clone(), h.files[0].clone()),
                (h.capture.clone(), h.files[1].clone()),
            ],
            "the drained files went back for the next flush"
        );
        let packages = h.handle.ctx.config.packages_dir();
        let leftovers: Vec<PathBuf> = std::fs::read_dir(&packages)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "the partially-written package dir was cleaned up: {leftovers:?}"
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

    // ---- Scheduled mode (0.5.1 §3) ----

    /// The core of scheduled mode: files that stabilize before the point WAIT for
    /// it (no quiet-timer send, no per-file send), and the point ships the whole
    /// accumulated set as one `scheduled` batch and stamps the fire.
    ///
    /// The clock crosses one [`SCHEDULE_TICK`] boundary before the point, so this
    /// also pins that a tick wake is not a fire.
    #[tokio::test(start_paused = true)]
    async fn scheduled_fires_at_the_point_and_not_before() {
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(virtual_clock(local_at(26, 5, 58))),
                ..Opts::default()
            },
        );

        h.feed(&[0, 1]).await;
        settle().await;

        // 05:59 — one tick later, still a minute short of the point.
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(h.batch_count(), 0, "a tick wake is not a fire");
        assert_eq!(
            h.handle.pending_snapshot().len(),
            2,
            "the set is still accumulating for the point"
        );

        // 06:00:30 — past the point.
        tokio::time::advance(Duration::from_secs(90)).await;
        wait_until(|| h.batch_count() == 1).await;

        let rows = h.batches.list().unwrap();
        assert_eq!(rows[0].mode, "scheduled");
        assert_eq!(rows[0].file_count, 2, "the whole set went out as one batch");
        assert!(
            h.handle.pending_snapshot().is_empty(),
            "the point drained the accumulator"
        );
        assert!(
            h.last_fire() >= local_at(26, 6, 0),
            "the fire is stamped at or after the point it served, got {}",
            h.last_fire()
        );
    }

    /// A point that arrives with nothing pending is a no-op flush — no batch row
    /// (the batcher never records empty batches) — but it STILL stamps: the stamp
    /// records that the schedule reached the point, not that a package went out.
    /// Without that, a quiet night would look like a missed point forever and
    /// every restart would catch it up again.
    #[tokio::test(start_paused = true)]
    async fn a_point_with_nothing_pending_records_no_batch_but_still_stamps() {
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(virtual_clock(local_at(26, 5, 59))),
                ..Opts::default()
            },
        );
        settle().await;
        let baseline = h.last_fire();
        assert!(
            baseline < local_at(26, 6, 0),
            "the startup baseline predates it"
        );

        tokio::time::advance(Duration::from_secs(120)).await; // 06:01
        wait_until(|| h.last_fire() >= local_at(26, 6, 0)).await;

        assert_eq!(
            h.batch_count(),
            0,
            "an empty point writes no batch row — empty batches never exist"
        );
    }

    /// Collision (spec §3): a manual "Send N pending" mid-window drains what was
    /// accumulated; the scheduled point that follows carries ONLY what stabilized
    /// after that drain. By construction (the accumulator is drained at flush),
    /// but it is the owner's stated requirement, so it is pinned.
    #[tokio::test(start_paused = true)]
    async fn a_manual_drain_leaves_only_later_files_for_the_point() {
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(virtual_clock(local_at(26, 5, 58))),
                ..Opts::default()
            },
        );

        // Two files accumulate and the operator sends them by hand.
        h.feed(&[0, 1]).await;
        settle().await;
        h.handle.flush_now().await;
        wait_until(|| h.batch_count() == 1).await;

        // A third stabilizes after the manual drain.
        h.feed(&[2]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 1).await;

        tokio::time::advance(Duration::from_secs(150)).await; // 06:00:30
        wait_until(|| h.batch_count() == 2).await;

        assert_eq!(
            h.files_in_batch("manual"),
            vec!["a.fits", "b.fits"],
            "the manual batch took what was pending when it ran"
        );
        assert_eq!(
            h.files_in_batch("scheduled"),
            vec!["c.fits"],
            "the point carries ONLY the file that stabilized after the drain"
        );
    }

    /// Catch-up (spec §3): a point that elapsed while the agent was down fires
    /// **once** at startup, shortly after boot — late enough for the restarted
    /// watchers to have put last night's unsent frames back into the accumulator
    /// (see [`catch_up_grace`]), which is the whole point of catching up.
    #[tokio::test(start_paused = true)]
    async fn a_point_missed_while_down_is_caught_up_once_at_startup() {
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                // Booted at 09:00; last fired yesterday at 05:00, so today's 06:00
                // passed unattended.
                clock: Some(virtual_clock(local_at(26, 9, 0))),
                last_fire: Some(local_at(25, 5, 0)),
                ..Opts::default()
            },
        );

        // The restarted watcher re-detects the night's unsent frames.
        h.feed(&[0, 1]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 2).await;

        tokio::time::advance(Duration::from_secs(60)).await; // past the settle grace
        wait_until(|| h.batch_count() == 1).await;

        let rows = h.batches.list().unwrap();
        assert_eq!(rows[0].mode, "scheduled");
        assert_eq!(rows[0].file_count, 2, "the caught-up set went out whole");
        assert!(
            h.last_fire() >= local_at(26, 9, 0),
            "the catch-up advanced the stamp past the missed point, so the next \
             start does not fire again, got {}",
            h.last_fire()
        );

        // Exactly one fire: the ordinary next point is tomorrow's 06:00, so a long
        // wait adds nothing.
        tokio::time::advance(Duration::from_secs(3600)).await;
        settle().await;
        assert_eq!(
            h.batch_count(),
            1,
            "catch-up fires once, not once per point"
        );
    }

    /// `schedule_catchup = false` skips the missed point entirely — and leaves the
    /// recorded fire time alone, so turning the flag back on later still measures
    /// downtime from the last real fire.
    #[tokio::test(start_paused = true)]
    async fn catch_up_disabled_skips_the_missed_point() {
        let last = local_at(25, 5, 0);
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], false),
            Opts {
                clock: Some(virtual_clock(local_at(26, 9, 0))),
                last_fire: Some(last),
                ..Opts::default()
            },
        );
        h.feed(&[0, 1]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 2).await;

        tokio::time::advance(Duration::from_secs(300)).await;
        settle().await;

        assert_eq!(h.batch_count(), 0, "no catch-up when the flag is off");
        assert_eq!(
            h.last_fire(),
            last,
            "and the recorded fire time is untouched"
        );
        assert_eq!(
            h.handle.pending_snapshot().len(),
            2,
            "the files wait for the next real point"
        );
    }

    /// The FIRST start in scheduled mode has no recorded fire, and that is not
    /// "infinitely behind": nothing can have been missed before the feature was
    /// switched on. It records a baseline instead of firing, so the very next
    /// start can tell a genuinely missed point from a fresh install.
    #[tokio::test(start_paused = true)]
    async fn a_first_run_records_a_baseline_instead_of_catching_up() {
        let boot = local_at(26, 9, 0);
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(virtual_clock(boot)),
                last_fire: None, // never fired: a fresh install
                ..Opts::default()
            },
        );
        h.feed(&[0, 1]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 2).await;

        tokio::time::advance(Duration::from_secs(300)).await;
        settle().await;

        assert_eq!(
            h.batch_count(),
            0,
            "a fresh install has nothing to catch up"
        );
        let stamp = h.last_fire();
        assert!(
            stamp >= boot && stamp < boot + chrono::TimeDelta::minutes(1),
            "the baseline is the boot instant, got {stamp}"
        );
    }

    /// A wall clock that jumps forward without the monotonic timer following it —
    /// a laptop resuming from suspend, an NTP step, a Pi getting its first real
    /// time from the network — still fires within one [`SCHEDULE_TICK`].
    ///
    /// This is what the capped, re-derived sleep buys. A single
    /// `sleep_until(fire_instant)` armed at 20:00 for tomorrow's 06:00 would wait
    /// out its full ten monotonic hours no matter what the calendar said, so a
    /// machine that slept through the night would send at 16:00.
    #[tokio::test(start_paused = true)]
    async fn a_forward_clock_jump_is_absorbed_within_one_tick() {
        let (clock, skew) = skewable_clock(local_at(26, 20, 0));
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(clock),
                ..Opts::default()
            },
        );
        h.feed(&[0]).await;
        settle().await;
        assert_eq!(h.batch_count(), 0, "the point is ten hours out");

        // The machine suspends and resumes past the point: the calendar moved,
        // the monotonic clock did not.
        *skew.lock().unwrap() = chrono::TimeDelta::hours(10) + chrono::TimeDelta::minutes(5);

        // ONE tick of monotonic time — not the ten hours the naive arm would need.
        tokio::time::advance(SCHEDULE_TICK).await;
        wait_until(|| h.batch_count() == 1).await;
        assert_eq!(h.batches.list().unwrap()[0].mode, "scheduled");
        assert!(
            h.last_fire() >= local_at(27, 6, 0),
            "the fire is stamped in the resumed present, got {}",
            h.last_fire()
        );
    }

    /// A point that is already DUE survives a config edit racing its own fire arm:
    /// the batch still goes out once, and the edited schedule is armed after it.
    ///
    /// Without the guard the edit's re-derive would swallow the point silently —
    /// `next_fire` is strictly-after, so the send would defer up to a day with an
    /// untouched stamp. The race is staged deterministically by moving only the
    /// CALENDAR past the point (the monotonic tick sleep is not ready, so the
    /// config branch is the only runnable arm — no select! coin flip).
    #[tokio::test(start_paused = true)]
    async fn a_due_point_survives_a_config_edit_race() {
        let (clock, skew) = skewable_clock(local_at(26, 5, 58));
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(clock),
                ..Opts::default()
            },
        );
        h.feed(&[0]).await;
        settle().await;
        assert_eq!(h.batch_count(), 0, "the point is two minutes out");

        // 06:03 on the calendar only: the armed 06:00 point is due, but the
        // batcher is still parked on its (monotonic) tick sleep.
        *skew.lock().unwrap() = chrono::TimeDelta::minutes(5);
        // The edit lands first — it adds a second point, so "the new schedule was
        // armed" is observable below.
        h.cfg_tx
            .send(scheduled_cfg(&[(6, 0), (21, 0)], true))
            .unwrap();

        wait_until(|| h.batch_count() == 1).await;
        assert_eq!(
            h.batches.list().unwrap()[0].mode,
            "scheduled",
            "the due point was served as a scheduled batch, not dropped"
        );
        assert_eq!(h.batches.list().unwrap()[0].file_count, 1);
        assert!(
            h.handle.pending_snapshot().is_empty(),
            "and it drained the accumulator"
        );
        assert!(
            h.last_fire() >= local_at(26, 6, 0),
            "the fire is stamped, so no restart re-catches it up, got {}",
            h.last_fire()
        );

        // The edit's own schedule is live: the newly added 21:00 point fires too.
        h.feed(&[1]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 1).await;
        *skew.lock().unwrap() = chrono::TimeDelta::hours(15) + chrono::TimeDelta::minutes(5);
        tokio::time::advance(SCHEDULE_TICK).await;
        wait_until(|| h.batch_count() == 2).await;
        assert!(
            h.last_fire() >= local_at(26, 21, 0),
            "the point the config edit ADDED fired, got {}",
            h.last_fire()
        );
    }

    /// A scheduled point that delivers NOTHING must not be stamped as served —
    /// and must be retried within the minute rather than left to tomorrow.
    ///
    /// The stamp is the catch-up window's lower bound: advancing it over a point
    /// whose files are still sitting in the accumulator both parks the night for
    /// 24 h (nothing re-arms the calendar until the next point) and tells every
    /// future restart that point was served. Here the first fire cannot write its
    /// package (its files vanish under it); the retry a minute later finds them
    /// back and ships them.
    #[tokio::test(start_paused = true)]
    async fn a_point_that_delivered_nothing_is_not_stamped_and_is_retried() {
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(virtual_clock(local_at(26, 5, 58))),
                ..Opts::default()
            },
        );
        h.feed(&[0, 1]).await;
        settle().await;
        let baseline = h.last_fire();
        assert!(baseline < local_at(26, 6, 0), "the startup baseline predates the point");

        // The 06:00 fire cannot package anything: both files vanish between the
        // manifest pass and the copy pass.
        let doomed: Vec<PathBuf> = h.files[..2].to_vec();
        let restore: Vec<PathBuf> = doomed.clone();
        arm_mid_build(move || {
            for f in &doomed {
                std::fs::remove_file(f).expect("delete mid-build");
            }
        });

        // `settle` (not a `wait_until` on the pending count): the set is already 2
        // and stays 2 across the failed fire, so only "the loop has run out of
        // work" tells us the point has actually been served.
        tokio::time::advance(Duration::from_secs(150)).await; // 06:00:30
        settle().await;

        assert_eq!(h.batch_count(), 0, "the point shipped nothing");
        assert_eq!(
            h.handle.pending_snapshot().len(),
            2,
            "its files went back into the accumulator"
        );
        assert_eq!(
            h.last_fire(),
            baseline,
            "a point that delivered nothing must NOT advance the stamp — catch-up still owes it"
        );

        // Whatever made the write fail is over (here: the files are back).
        for path in &restore {
            write_fixture_fits(path, "M42");
        }

        // One retry backoff later the SAME point is tried again — without it the
        // next arm would be tomorrow's 06:00.
        tokio::time::advance(SCHEDULE_RETRY_BACKOFF + Duration::from_secs(5)).await;
        wait_until(|| h.batch_count() == 1).await;

        let rows = h.batches.list().unwrap();
        assert_eq!(rows[0].mode, "scheduled");
        assert_eq!(rows[0].file_count, 2, "the retry shipped the whole waiting set");
        assert!(
            h.last_fire() >= local_at(26, 6, 0),
            "the retry that delivered is what finally stamps, got {}",
            h.last_fire()
        );
        assert!(h.handle.pending_snapshot().is_empty(), "and drained");
    }

    /// A restart shortly before a point must not turn that point into an empty,
    /// stamped no-op.
    ///
    /// At 05:59:50 the watchers have not re-detected last night's unsent frames
    /// and by construction cannot have (a file reaches the accumulator only after
    /// a full write-stability window). Firing 06:00 ten seconds later would flush
    /// an EMPTY set, write no batch, stamp the point as served — and the frames
    /// would then wait for tomorrow. So every startup-armed fire, ordinary or
    /// catch-up, is deferred past the same settle grace the catch-up already uses.
    #[tokio::test(start_paused = true)]
    async fn a_point_inside_the_startup_settle_grace_waits_for_the_watchers() {
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                // Boot ten seconds before the point: 06:00 falls inside the 45 s
                // settle grace.
                clock: Some(virtual_clock(local_at_s(26, 5, 59, 50))),
                ..Opts::default()
            },
        );
        settle().await;

        // +40 s: the point itself has passed, but the grace has not — and the
        // watchers have only just now put the night's frames back.
        tokio::time::advance(Duration::from_secs(40)).await;
        settle().await;
        assert_eq!(
            h.batch_count(),
            0,
            "the point did not fire into an empty accumulator inside the grace"
        );
        h.feed(&[0, 1]).await;
        wait_until(|| h.handle.pending_snapshot().len() == 2).await;

        // Past the grace (05:59:50 + 45 s = 06:00:35): the point fires now, with
        // the files it exists to send.
        tokio::time::advance(SETTLE_GRACE).await;
        wait_until(|| h.batch_count() == 1).await;

        let rows = h.batches.list().unwrap();
        assert_eq!(rows[0].mode, "scheduled");
        assert_eq!(
            rows[0].file_count, 2,
            "the deferred point carried the frames the watchers had settled by then"
        );
        assert!(
            h.last_fire() >= local_at(26, 6, 0),
            "stamped once, at or after the point, got {}",
            h.last_fire()
        );

        // ONE fire: the deferral does not leave a second, ordinary arm behind it.
        let stamp = h.last_fire();
        tokio::time::advance(Duration::from_secs(3600)).await;
        settle().await;
        assert_eq!(h.batch_count(), 1, "the deferred point fired exactly once");
        assert_eq!(h.last_fire(), stamp, "and stamped exactly once");
    }

    /// The schedule follows a LIVE mode edit, both ways: flipping to Manual
    /// disarms it (the point passes with nothing sent), flipping back re-arms it
    /// from the wall clock and the next point fires.
    #[tokio::test(start_paused = true)]
    async fn a_live_mode_flip_disarms_and_re_arms_the_schedule() {
        let h = Harness::spawn_with(
            scheduled_cfg(&[(6, 0)], true),
            Opts {
                clock: Some(virtual_clock(local_at(26, 5, 58))),
                ..Opts::default()
            },
        );
        h.feed(&[0]).await;
        settle().await;

        // Flip to Manual before the point.
        h.cfg_tx
            .send(SendCfg {
                mode: Mode::Manual,
                ..scheduled_cfg(&[(6, 0)], true)
            })
            .unwrap();
        settle().await;
        tokio::time::advance(Duration::from_secs(180)).await; // 06:01 — the point passed
        settle().await;
        assert_eq!(h.batch_count(), 0, "manual mode ignores the schedule");
        assert_eq!(
            h.handle.pending_snapshot().len(),
            1,
            "and nothing was drained"
        );

        // Flip back: the schedule re-arms for the next point (tomorrow's 06:00).
        h.cfg_tx.send(scheduled_cfg(&[(6, 0)], true)).unwrap();
        settle().await;
        tokio::time::advance(Duration::from_secs(24 * 3600)).await;
        wait_until(|| h.batch_count() == 1).await;
        assert_eq!(h.batches.list().unwrap()[0].mode, "scheduled");
        assert_eq!(
            h.batches.list().unwrap()[0].file_count,
            1,
            "the file that waited through the flip went out on the next point"
        );
    }
}
