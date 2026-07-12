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

    /// Signal the batcher to flush the whole pending set now as one `manual`
    /// batch. The operator's "Send N pending" action. The batcher itself guards
    /// the empty case, so calling this with nothing pending records no batch.
    ///
    /// Best-effort: if the batcher has already stopped (its receiver dropped) the
    /// send is silently discarded — there is nothing left to flush.
    pub async fn flush_now(&self) {
        let _ = self.flush_tx.send(()).await;
    }
}

/// The result of one non-empty, delivered flush: the package it produced and how
/// many files it carried. `None` from [`flush_once`] means "nothing was
/// recorded" — an empty pending set, an all-vanished batch, or a zero-target
/// delivery (whose files were re-queued for retry).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FlushOutcome {
    package_ref: String,
    file_count: usize,
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
#[allow(clippy::too_many_arguments)]
async fn flush_once(
    mode: Mode,
    pending: &Arc<Mutex<BTreeSet<(PathBuf, PathBuf)>>>,
    engines: &[Arc<SyncEngineHandle>],
    seen: &SeenStore,
    batches: &BatchStore,
    config: &Config,
    origin_device: &str,
    cleanup: &Option<Arc<SharedPackageCleanup>>,
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

    let (pkg_dir, included) = match build_batch_package(config, &files, origin_device) {
        Ok(built) => built,
        Err(error) => {
            // Every file in the batch vanished / won't parse. There is nothing on
            // disk to send and nothing to retry — drop the batch.
            tracing::warn!(
                %error,
                count = files.len(),
                mode = mode_str(mode),
                "batch had no buildable files; dropping"
            );
            return None;
        }
    };
    // The packaged record count == the number of files that actually shipped.
    // Some drained files may have been dropped at build time (present but
    // unbuildable); those are deliberately absent from `included`.
    let n = included.len();

    let (first_id, delivered) = enqueue_package_to_all(engines, &pkg_dir).await;
    // Fan-out only: register the delivered target count so the shared payload is
    // freed exactly once, after every target is terminal (mirrors the per-file
    // path). Single-target agents keep the engine's in-line cleanup (`None`).
    // `delivered == 0` registers an expected of 0 → the orphaned copy is cleaned
    // immediately (no target's retry can ever need it).
    if let Some(coord) = cleanup {
        coord.register(&pkg_dir, delivered);
    }
    let package_ref = pkg_dir.to_string_lossy().into_owned();

    match first_id {
        Some(_) => {
            // At least one target durably queued the package. Mark seen ONLY the
            // files that actually made it into the package (so a restart never
            // re-baselines them) and record the batch. A drained file that was
            // dropped at build time (present-but-unbuildable) is intentionally
            // left unseen: it never shipped, so it must stay enqueue-eligible and
            // get retried on the next detection / restart rather than be silently
            // lost (the durable seen store is the dedup authority). This is not
            // the zero-target re-queue path — we do NOT re-insert it into
            // `pending` (that would spin a permanently-corrupt file forever
            // in-session); simply not marking it seen matches the legacy per-file
            // behavior exactly.
            for file in &included {
                record_seen(seen, file, &package_ref);
            }
            if let Err(error) = batches.record(&package_ref, mode_str(mode), &now_rfc3339(), n) {
                // The files are already durably queued + recorded seen; a failed
                // history write only loses a status-page row, never a frame.
                tracing::warn!(%error, package_ref = %package_ref, "failed to record batch row");
            }
            tracing::info!(
                package_ref = %package_ref,
                delivered,
                targets = engines.len(),
                file_count = n,
                mode = mode_str(mode),
                "batch flushed"
            );
            Some(FlushOutcome {
                package_ref,
                file_count: n,
            })
        }
        None => {
            // Reached no target. The files were NOT recorded as seen, so put them
            // back to retry on the next flush rather than silently losing them.
            tracing::error!(
                targets = engines.len(),
                file_count = n,
                package_ref = %package_ref,
                "batch reached no target; re-queuing its files for retry"
            );
            let mut guard = pending.lock().expect("batcher pending mutex poisoned");
            for item in files {
                guard.insert(item);
            }
            drop(guard);
            // The staged package is now orphaned (its files are re-queued and the
            // next flush mints a fresh dir). With a fan-out `cleanup` the zero
            // `register` above already frees it; a single-target agent (`None`)
            // must drop it here, or an Auto-mode dead-worker leaks one dir per
            // quiet window. Best-effort — a failure just warns.
            if cleanup.is_none() {
                if let Err(error) = std::fs::remove_dir_all(&pkg_dir) {
                    tracing::warn!(%error, package_ref = %package_ref, "failed to clean orphaned zero-target package dir");
                }
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
    let handle = BatcherHandle {
        pending: Arc::clone(&pending),
        flush_tx,
    };

    let loop_pending = Arc::clone(&pending);
    let task = tokio::spawn(async move {
        // The live send config. Seeded from the watch channel (production seeds it
        // with `config.send_cfg()`), then updated in place on every change.
        let mut cfg = *send_cfg_rx.borrow();
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
                    flush_once(
                        Mode::Auto, &loop_pending, &engines, &seen, &batches,
                        &config, &origin_device, &cleanup,
                    ).await;
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
                    flush_once(
                        Mode::Manual, &loop_pending, &engines, &seen, &batches,
                        &config, &origin_device, &cleanup,
                    ).await;
                    deadline = rearm(&loop_pending, &cfg);
                }

                // A live config edit (Task 6). Adopt the new mode/quiet window:
                // Manual disarms the timer; Auto re-arms it iff something is
                // already pending.
                changed = send_cfg_rx.changed(), if cfg_open => {
                    match changed {
                        Ok(()) => {
                            cfg = *send_cfg_rx.borrow_and_update();
                            deadline = match cfg.mode {
                                Mode::Manual => None,
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

/// The post-flush / mode-change deadline: `Some(now + quiet)` in auto mode when
/// files remain pending, else `None`. This is what makes a zero-target flush
/// retry (its re-queued files keep the timer armed) and a fully-drained flush go
/// idle.
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

            let (handle, task) = spawn_batcher(
                stable_rx,
                vec![Arc::clone(&engine)],
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

    /// The current `(size, mtime_ms)` of a file on disk, in the same shape the
    /// seen store keys on — so a test can ask `should_enqueue` the exact question
    /// the watcher would.
    fn stat_size_mtime(path: &Path) -> (u64, i64) {
        let m = std::fs::metadata(path).expect("stat test file");
        (m.len(), crate::seen::mtime_millis(m.modified().ok()))
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
}
