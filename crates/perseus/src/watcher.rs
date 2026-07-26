//! Capture-directory watcher with write-stability debounce.
//!
//! # Why not reuse `athenaeum_core::monitor`
//!
//! Core's `monitor` module is **not** reusable here on two counts, both checked
//! against the source:
//!
//! 1. It is catalog-coupled: `MonitorService::run_loop` takes a
//!    `ServiceContext` (DB pool + settings + scanner) and drives
//!    `scanner::run_registered_scan` against `scan_roots` rows. Perseus is
//!    headless — no catalog, no DB of frames — so none of that applies.
//! 2. It is *polling*, not filesystem-watching (its own doc comment: "Polling,
//!    not filesystem watchers … Watchers are unreliable on NAS/SMB/NFS"). The
//!    brief's premise that it "uses the `notify` crate" does not match the code;
//!    there is no debounce/stability logic in it to copy.
//!
//! So this is a fresh `notify`-crate watcher purpose-built for a *local* capture
//! directory (where capture software writes progressively to fast local disk —
//! the case `notify` handles well). The write-stability rule is the part that
//! matters and it lives in [`StabilityTracker`], a pure, clock-injectable unit
//! that is exercised without any real timing or filesystem.
//!
//! # Shape
//!
//! `notify` events only *seed* candidate paths; all `stat`ing happens on a
//! periodic tick. A file is emitted as "stable" (ready to enqueue) once its
//! `(size, mtime)` has held steady for the configured quiet window. A file that
//! keeps growing keeps resetting the window, so a half-written FITS is never
//! sent. Each path is emitted at most once — until it is **deleted**, at which
//! point the deleter un-emits it through [`WatcherForget`] so a re-capture at
//! that path is sent again without waiting for a restart (spec §2's "reappears"
//! row; the durable half of that row lives in the seen store).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

use crate::seen::{mtime_millis, SeenStore};

/// Filesystem identity of a payload file at one instant — the pair the stability
/// check compares across observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

impl FileStat {
    fn from_metadata(m: &std::fs::Metadata) -> Self {
        Self {
            size: m.len(),
            mtime: m.modified().ok(),
        }
    }
}

/// One candidate file's most recent observation.
struct Observed {
    stat: FileStat,
    /// When the current `(size, mtime)` was first seen. Reset whenever the stat
    /// changes; the quiet window is measured from here.
    since: Instant,
}

/// Pure write-stability state machine. Time is injected (`now: Instant`) so the
/// whole thing is deterministically testable with a fabricated clock — no sleeps,
/// no filesystem. Instants are only ever *advanced* by the caller (`now + Δ`).
pub struct StabilityTracker {
    stability: Duration,
    pending: HashMap<PathBuf, Observed>,
    /// Paths already emitted as stable; never re-emitted (enqueue-once).
    emitted: HashSet<PathBuf>,
}

impl StabilityTracker {
    pub fn new(stability: Duration) -> Self {
        Self {
            stability,
            pending: HashMap::new(),
            emitted: HashSet::new(),
        }
    }

    /// Record an observation of `path` with its current `stat` at time `now`.
    ///
    /// A stat identical to the last observation keeps the quiet-window clock
    /// running (a re-fired `notify` event on an unchanged file must not reset
    /// it). A changed stat (still-growing file) resets the clock. A path that has
    /// already been emitted is ignored.
    pub fn observe(&mut self, path: &Path, stat: FileStat, now: Instant) {
        if self.emitted.contains(path) {
            return;
        }
        match self.pending.get_mut(path) {
            Some(obs) if obs.stat == stat => { /* unchanged: keep `since` */ }
            Some(obs) => {
                obs.stat = stat;
                obs.since = now;
            }
            None => {
                self.pending.insert(
                    path.to_path_buf(),
                    Observed { stat, since: now },
                );
            }
        }
    }

    /// Drop a candidate (e.g. the file vanished before stabilizing).
    pub fn forget(&mut self, path: &Path) {
        self.pending.remove(path);
    }

    /// Forget every trace of `paths` — both the in-progress observation state and
    /// the **emitted** set — so a file re-created at one of those paths is
    /// discovered and enqueued again *within this process run*. Returns how many
    /// of them the tracker actually knew about.
    ///
    /// This is the in-process half of spec §2's "file reappears" row; the durable
    /// half is [`SeenStore::mark_deleted`](crate::seen::SeenStore::mark_deleted).
    /// Both are needed and neither substitutes for the other: the seen store is
    /// consulted only *after* [`contains_emitted`](Self::contains_emitted) has
    /// already let the path through, so a still-emitted path is dropped by both
    /// the event arm and the poll sweep before any store lookup happens. Without
    /// this call the re-created file is invisible — no error, no log — until the
    /// agent restarts, which on an observatory Pi means never.
    ///
    /// Called only for paths that are **gone from disk** (unlinked by the library
    /// delete route or a retention pass, or found already missing), so re-arming
    /// discovery cannot re-send a file that is still there and already sent.
    pub fn forget_paths(&mut self, paths: &[PathBuf]) -> usize {
        let mut known = 0usize;
        for path in paths {
            let was_pending = self.pending.remove(path).is_some();
            let was_emitted = self.emitted.remove(path);
            if was_pending || was_emitted {
                known += 1;
            }
        }
        known
    }

    /// Return every pending path whose quiet window has fully elapsed by `now`,
    /// moving each into the emitted set so it is never returned twice. Sorted for
    /// deterministic ordering.
    pub fn collect_stable(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, obs)| now.saturating_duration_since(obs.since) >= self.stability)
            .map(|(p, _)| p.clone())
            .collect();
        ready.sort();
        for p in &ready {
            self.pending.remove(p);
            self.emitted.insert(p.clone());
        }
        ready
    }

    /// Mark a path as already handled without emitting it. Used to *baseline*
    /// the files already present when the watcher starts, so the directory-poll
    /// fallback never re-sends them (and a restart does not re-send the whole
    /// capture dir).
    pub fn mark_emitted(&mut self, path: &Path) {
        self.pending.remove(path);
        self.emitted.insert(path.to_path_buf());
    }

    /// Whether `path` has already been emitted or baselined.
    pub fn contains_emitted(&self, path: &Path) -> bool {
        self.emitted.contains(path)
    }

    /// Paths currently awaiting stabilization (test/introspection helper).
    #[cfg(test)]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// Eligible capture files directly under or nested within `dir`, sorted.
///
/// Infallible by contract: a walk error (unreadable entry, or — the case that
/// matters here — an offline network mount, where the *root itself* yields one
/// `Err`) is logged and skipped, never propagated. Callers that want to notice
/// a persistently failing sweep use [`scan_eligible_reporting`].
fn scan_eligible(dir: &Path) -> Vec<PathBuf> {
    scan_eligible_reporting(dir).0
}

/// [`scan_eligible`] plus the number of **root-level** walk failures.
///
/// The count is the offline-share signal, and it counts ONLY errors reported at
/// `depth() == 0` — walkdir's precise way of saying "the capture directory
/// itself could not be read". An unreachable mount makes `WalkDir` fail on the
/// root, so the sweep returns `(vec![], 1)` rather than a legitimately empty
/// directory's `(vec![], 0)`, and the watcher loop turns that into a
/// rate-limited warning instead of silently discovering nothing.
///
/// Errors on *child* entries are deliberately NOT counted. A perfectly healthy
/// macOS volume root carries `.Trashes` / `.Spotlight-V100` directories that
/// deny traversal, so counting those would pin the "mount may be offline"
/// warning on forever for a root that is entirely fine. Child errors stay a
/// `debug!` line, exactly as they were before the offline-share signal existed.
fn scan_eligible_reporting(dir: &Path) -> (Vec<PathBuf>, usize) {
    let mut out = Vec::new();
    let mut root_errors = 0usize;
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        match entry {
            Ok(e) if e.file_type().is_file() && is_eligible(e.path()) => {
                out.push(e.path().to_path_buf())
            }
            Ok(_) => {}
            Err(e) => {
                if e.depth() == 0 {
                    root_errors += 1;
                }
                tracing::debug!(error = %e, depth = e.depth(), "walk capture dir entry failed");
            }
        }
    }
    (out, root_errors)
}

/// FITS/XISF payload extensions Perseus enqueues. Case-insensitive.
const CAPTURE_EXTENSIONS: &[&str] = &["fits", "fit", "fts", "xisf"];

/// Whether `path` is a capture payload Perseus should sync (by extension only —
/// existence/type is checked separately when staged).
pub fn is_eligible(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let lower = ext.to_ascii_lowercase();
            CAPTURE_EXTENSIONS.contains(&lower.as_str())
        }
        None => false,
    }
}

/// Handle to the running watcher task. Dropping or calling [`shutdown`] stops it.
///
/// [`shutdown`]: WatcherHandle::shutdown
pub struct WatcherHandle {
    shutdown_tx: mpsc::Sender<()>,
    /// Deleted paths to drop from the tracker's emitted set (see
    /// [`StabilityTracker::forget_paths`]). Unbounded so a deleter — which may be
    /// running on a blocking thread — never awaits the watcher's loop.
    forget_tx: mpsc::UnboundedSender<Vec<PathBuf>>,
    join: tokio::task::JoinHandle<()>,
}

impl WatcherHandle {
    /// Ask the watcher to stop and await its exit.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(()).await;
        let _ = self.join.await;
    }

    /// A sender for this watcher's forget channel, for building the aggregate
    /// [`WatcherForget`] the deleters hold.
    pub fn forget_sender(&self) -> mpsc::UnboundedSender<Vec<PathBuf>> {
        self.forget_tx.clone()
    }

    /// Hard-kill: abort the watcher task immediately, no graceful handshake.
    /// Test-only — production shutdown should always go through
    /// [`shutdown`](Self::shutdown). Used by the crash-resume e2e test to
    /// simulate a killed process rather than a detached-but-still-running task.
    #[doc(hidden)]
    pub fn abort_for_test(self) {
        self.join.abort();
    }
}

/// The deleters' handle on every running watcher's forget channel.
///
/// # Why a fan-out and not a routed send
///
/// A forget is broadcast to ALL watchers rather than routed to the one owning
/// the path's capture root. Forgetting a path a watcher never emitted is a pure
/// no-op ([`StabilityTracker::forget_paths`] just fails to find it), so routing
/// would buy nothing and cost a capture-root resolution at every delete site —
/// including the retention pass, which knows a source path but not which root it
/// came from.
///
/// # Path spelling is a contract
///
/// Paths must arrive **canonical**, because that is the only spelling the tracker
/// ever keys on: the watcher canonicalizes its capture root at spawn and both
/// discovery paths (the `notify` event arm and the poll sweep) derive from it.
/// Both callers already satisfy this — the library deleter's victim is
/// `canonical parent + own name` (`library::delete::victim_path`) and retention's
/// source paths come from the seen store, recorded from the watcher's own
/// canonical emission. A non-canonical spelling is not an error; it simply
/// forgets nothing, which is why the contract is documented rather than asserted.
///
/// Cloneable and cheap; an empty one (no watchers — a detached web page, or an
/// agent started without `watch`) is a silent no-op.
#[derive(Clone, Default)]
pub struct WatcherForget {
    senders: Arc<Vec<mpsc::UnboundedSender<Vec<PathBuf>>>>,
}

impl WatcherForget {
    /// Aggregate over one sender per running watcher.
    pub fn new(senders: Vec<mpsc::UnboundedSender<Vec<PathBuf>>>) -> Self {
        Self {
            senders: Arc::new(senders),
        }
    }

    /// The no-watcher aggregate: every [`forget`](Self::forget) is a no-op.
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether this aggregate reaches no watcher at all.
    pub fn is_empty(&self) -> bool {
        self.senders.is_empty()
    }

    /// Ask every watcher to forget `paths` (see the type docs for the spelling
    /// contract). One batched send per watcher — deleters pass a whole pass's
    /// worth of paths, never one call per file.
    ///
    /// Fire-and-forget: a closed channel (its watcher has already shut down) is
    /// ignored, since a stopped watcher has no emitted set left to correct.
    pub fn forget(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() || self.senders.is_empty() {
            return;
        }
        for tx in self.senders.iter() {
            let _ = tx.send(paths.clone());
        }
    }
}

/// How many consecutive failing poll sweeps pass between "capture dir sweep is
/// failing" warnings. At the default 2s poll interval that is ~2 minutes — loud
/// enough that an offline share is visible in the log, quiet enough that a NAS
/// unplugged overnight does not fill the file.
const SCAN_ERROR_WARN_EVERY_TICKS: u64 = 60;

/// What one poll tick's sweep outcome should put in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepLog {
    /// Nothing to say — a healthy sweep after a healthy one, or a failing sweep
    /// still inside the rate-limit window.
    Silent,
    /// Emit the "capture dir sweep failing — mount may be offline" warning.
    Warn,
    /// Emit the "capture dir sweep recovered" line, carrying how many
    /// consecutive sweeps had been failing.
    Recovered(u64),
}

/// Fold one sweep outcome into the consecutive-failing-sweep counter, returning
/// the new counter and what to log.
///
/// Pure, so the rate-limit arithmetic is testable with no clock, no filesystem
/// and no runtime. Failing sweeps warn on the very first one and then every
/// [`SCAN_ERROR_WARN_EVERY_TICKS`]th after it (#1, #61, #121, …). A successful
/// sweep resets the counter to zero, which is what makes the NEXT outage warn
/// immediately rather than inheriting the previous outage's rate-limit phase.
fn fold_sweep(failing_sweeps: u64, sweep_failed: bool) -> (u64, SweepLog) {
    if sweep_failed {
        let log = if failing_sweeps % SCAN_ERROR_WARN_EVERY_TICKS == 0 {
            SweepLog::Warn
        } else {
            SweepLog::Silent
        };
        (failing_sweeps.saturating_add(1), log)
    } else if failing_sweeps > 0 {
        (0, SweepLog::Recovered(failing_sweeps))
    } else {
        (0, SweepLog::Silent)
    }
}

/// Arm a `notify` watcher on `capture_dir`, or `None` if the filesystem cannot
/// provide one.
///
/// Establishing a watch fails on plenty of *supported* setups — SMB/NFS mounts
/// most of all, but also inotify-instance exhaustion on Linux. That is not a
/// fatal condition for this watcher: `notify` events only ever *seed* candidate
/// paths, and the periodic tick runs a full `scan_eligible` sweep, so discovery
/// is complete without any events at all. Returning `None` therefore means
/// "poll-only mode", not "broken root" — the failure costs latency, never
/// correctness.
fn establish_watcher(
    capture_dir: &Path,
    raw_tx: mpsc::UnboundedSender<PathBuf>,
) -> Option<RecommendedWatcher> {
    // `notify`'s callback runs on its own thread; it only forwards eligible raw
    // event paths to the async loop, which does all the stat'ing.
    let sink = move |res: notify::Result<Event>| match res {
        Ok(event) => {
            for path in event.paths {
                if is_eligible(&path) {
                    let _ = raw_tx.send(path);
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "notify watcher error"),
    };
    let degrade = |error: notify::Error| {
        tracing::warn!(
            path = %capture_dir.display(),
            %error,
            "filesystem watch unavailable — running poll-only (normal for SMB/NFS mounts)"
        );
    };

    let mut watcher = match notify::recommended_watcher(sink) {
        Ok(w) => w,
        Err(error) => {
            degrade(error);
            return None;
        }
    };
    if let Err(error) = watcher.watch(capture_dir, RecursiveMode::Recursive) {
        degrade(error);
        return None;
    }
    Some(watcher)
}

/// Start watching `capture_dir` (recursively). Stable capture files are sent on
/// `stable_tx` as `(owning_capture_dir, file_path)`; the consumer builds a
/// package (with a `rel_path` relative to that capture dir) and enqueues it.
/// Returns once the watcher task is spawned.
///
/// If the `notify` watch cannot be established (network filesystem, inotify
/// limits) the root degrades to **poll-only mode** — a `warn!` and then normal
/// service on the tick sweep alone — rather than failing. The `Result` is kept
/// for call-site stability; it is currently always `Ok`.
///
/// The owning capture dir is the *canonicalized* root this watcher watches (see
/// the canonicalize note below): pairing it with each stable path lets the
/// enqueue consumer compute the capture-dir-relative `rel_path` without guessing
/// which of several configured roots the file came from.
///
/// `seen_store` is the durable, stat-aware "already enqueued this exact file"
/// record (see [`crate::seen`]) — it, not an in-process baseline, decides
/// whether a file discovered at startup or mid-run is a genuinely new arrival.
/// A file recorded with a matching `(size, mtime)` is skipped; anything new,
/// changed, or never recorded flows through the normal stability pipeline.
pub fn spawn_watcher(
    capture_dir: PathBuf,
    stability: Duration,
    poll_interval: Duration,
    stable_tx: mpsc::Sender<(PathBuf, PathBuf)>,
    seen_store: Arc<SeenStore>,
) -> Result<WatcherHandle> {
    Ok(spawn_watcher_with_options(
        capture_dir,
        stability,
        poll_interval,
        stable_tx,
        seen_store,
        false,
    ))
}

/// [`spawn_watcher`]'s body, with the poll-only degradation forceable.
///
/// `force_poll_only` skips watch establishment entirely, which is exactly the
/// state a failed `watch()` leaves the loop in. It exists so the poll-only path
/// is testable deterministically, without depending on how a given platform's
/// `notify` backend reacts to a contrived path. Production always passes
/// `false` and reaches poll-only via a genuine establishment failure.
///
/// `pub(crate)` for tests outside this module (the web layer's end-to-end delete
/// → re-capture test drives a real watcher): discovery is complete on the tick
/// sweep alone, and skipping `notify` keeps those tests both deterministic and
/// quick — establishing a real FSEvents watch costs seconds on macOS.
pub(crate) fn spawn_watcher_with_options(
    capture_dir: PathBuf,
    stability: Duration,
    poll_interval: Duration,
    stable_tx: mpsc::Sender<(PathBuf, PathBuf)>,
    seen_store: Arc<SeenStore>,
    force_poll_only: bool,
) -> WatcherHandle {
    // Canonicalize the watched root so both discovery sources — `notify` events
    // and the directory poll — speak the same path spelling. Without this, macOS
    // reports `notify` paths under `/private/var/...` while a walkdir of a
    // `/var/...` tempdir yields the other spelling of the same file, and the two
    // would be tracked (and enqueued) as two distinct files.
    //
    // An unreachable/absent root simply keeps its configured spelling here; the
    // poll sweep retries it every tick, so a share that mounts later is picked
    // up without a restart.
    let capture_dir = std::fs::canonicalize(&capture_dir).unwrap_or(capture_dir);

    // Raw event paths cross from `notify`'s own thread into the async task over
    // an unbounded channel (a bounded one could deadlock the fs-event thread).
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<PathBuf>();
    let watcher = if force_poll_only {
        None
    } else {
        establish_watcher(&capture_dir, raw_tx)
    };
    // No watcher → no events will ever arrive; the loop runs on the tick sweep
    // alone (see `establish_watcher`).
    let watch_active = watcher.is_some();

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    // Deleted paths to un-emit (spec §2's reappear row). Unbounded: the senders
    // are deleters running on blocking threads, and they must never await this
    // loop — which may itself be blocked stat'ing an offline mount.
    let (forget_tx, mut forget_rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();

    let join = tokio::spawn(async move {
        // Move the watcher into the task so it lives exactly as long as the loop.
        let _watcher = watcher;
        let mut tracker = StabilityTracker::new(stability);
        let mut candidates: HashSet<PathBuf> = HashSet::new();

        // Baseline: files already present when we start are checked against the
        // durable seen store, NOT assumed handled. A file recorded there with a
        // matching (size, mtime) is a genuine repeat — skip it (hot-cache via
        // mark_emitted). Anything unrecorded or changed (e.g. written during a
        // crash/restart window) is left untouched here and falls through to the
        // normal per-tick discovery below, so it is enqueued exactly like a
        // brand-new arrival. This is what closes the restart-window gap: the
        // old code marked every baseline file emitted unconditionally, silently
        // losing anything captured while the agent was down.
        let baseline = scan_eligible(&capture_dir);
        let mut baseline_new = 0usize;
        for path in &baseline {
            if let Ok(m) = std::fs::metadata(path) {
                if m.is_file() {
                    let stat = FileStat::from_metadata(&m);
                    match seen_store.should_enqueue(path, stat.size, mtime_millis(stat.mtime)) {
                        Ok(false) => tracker.mark_emitted(path),
                        Ok(true) => baseline_new += 1,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                path = %path.display(),
                                "seen-store lookup failed at startup; will enqueue to be safe"
                            );
                            baseline_new += 1;
                        }
                    }
                }
            }
        }

        let mut tick = tokio::time::interval(poll_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(
            path = %capture_dir.display(),
            stability_secs = stability.as_secs(),
            baseline = baseline.len(),
            baseline_new,
            poll_only = !watch_active,
            "capture watcher armed"
        );

        // Consecutive poll sweeps that hit walk errors (offline share). Warns are
        // rate-limited off this counter and it resets — silently, no error state
        // to clear — the moment a sweep succeeds again.
        let mut failing_sweeps: u64 = 0;

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    tracing::info!("capture watcher stopping");
                    break;
                }
                // A file was deleted (library route or retention pass): drop it
                // from the emitted set so a re-capture at the same path is
                // discovered again in THIS run. Handled before the tick so a
                // forget that lands between two sweeps takes effect on the very
                // next one.
                Some(paths) = forget_rx.recv() => {
                    let forgotten = tracker.forget_paths(&paths);
                    for path in &paths {
                        candidates.remove(path);
                    }
                    tracing::debug!(
                        count = paths.len(),
                        forgotten,
                        "watcher forgot deleted paths"
                    );
                }
                // Disabled outright in poll-only mode: with no watcher the raw
                // sender is already dropped, and an always-`None` branch would
                // be polled on every select! turn for nothing.
                Some(path) = raw_rx.recv(), if watch_active => {
                    // notify gives low-latency discovery. Canonicalize so the
                    // path matches the poll's spelling (see the note in
                    // `spawn_watcher`); skip if it vanished or is already handled.
                    if let Ok(path) = std::fs::canonicalize(&path) {
                        if !tracker.contains_emitted(&path) {
                            candidates.insert(path);
                        }
                    }
                }
                _ = tick.tick() => {
                    let now = Instant::now();

                    // Directory-poll fallback: `notify` can miss or coalesce
                    // events (FSEvents on macOS, and NAS/SMB/NFS where core's
                    // monitor eschews watchers entirely). Rescanning each tick
                    // guarantees new files are discovered even if no event fired.
                    // In poll-only mode this is the ONLY discovery path.
                    let (found, scan_errors) = scan_eligible_reporting(&capture_dir);
                    for path in found {
                        if !tracker.contains_emitted(&path) {
                            candidates.insert(path);
                        }
                    }

                    // An offline share fails the sweep every tick; say so once
                    // per SCAN_ERROR_WARN_EVERY_TICKS rather than every 2s, and
                    // recover with a single line when the mount comes back. The
                    // arithmetic lives in `fold_sweep` so it is unit-tested.
                    let (next_failing, sweep_log) = fold_sweep(failing_sweeps, scan_errors > 0);
                    failing_sweeps = next_failing;
                    match sweep_log {
                        SweepLog::Silent => {}
                        // `failing_sweeps` is the post-fold counter, so this
                        // reads 1 on the first failure and 61, 121, … on the
                        // rate-limited repeats — how long the outage has run.
                        // A failing root yields exactly one root-level error,
                        // so the raw `scan_errors` would be a constant 1.
                        SweepLog::Warn => tracing::warn!(
                            path = %capture_dir.display(),
                            failing_sweeps,
                            "capture dir sweep failing — mount may be offline"
                        ),
                        SweepLog::Recovered(failed) => tracing::info!(
                            path = %capture_dir.display(),
                            count = failed,
                            "capture dir sweep recovered"
                        ),
                    }

                    // Re-stat every candidate: a growing file resets its window,
                    // a vanished (or already-handled) one leaves the set. Before
                    // tracking it for stability, consult the durable seen store —
                    // this is the authoritative "already enqueued this exact
                    // file" check (across restarts, not just within this run).
                    let snapshot: Vec<PathBuf> = candidates.iter().cloned().collect();
                    for path in snapshot {
                        if tracker.contains_emitted(&path) {
                            candidates.remove(&path);
                            continue;
                        }
                        match std::fs::metadata(&path) {
                            Ok(m) if m.is_file() => {
                                let stat = FileStat::from_metadata(&m);
                                let mtime_ms = mtime_millis(stat.mtime);
                                match seen_store.should_enqueue(&path, stat.size, mtime_ms) {
                                    Ok(true) => tracker.observe(&path, stat, now),
                                    Ok(false) => {
                                        tracker.mark_emitted(&path);
                                        candidates.remove(&path);
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            %error,
                                            path = %path.display(),
                                            "seen-store lookup failed; enqueueing to be safe"
                                        );
                                        tracker.observe(&path, stat, now);
                                    }
                                }
                            }
                            _ => {
                                tracker.forget(&path);
                                candidates.remove(&path);
                            }
                        }
                    }

                    for path in tracker.collect_stable(now) {
                        candidates.remove(&path);
                        tracing::info!(path = %path.display(), "capture file stable; enqueuing");
                        // Pair the file with the (canonicalized) capture dir it
                        // came from, so the consumer can compute a rel_path
                        // relative to it.
                        if stable_tx.send((capture_dir.clone(), path)).await.is_err() {
                            tracing::warn!("stable-file consumer gone; watcher stopping");
                            return;
                        }
                    }
                }
            }
        }
    });

    WatcherHandle {
        shutdown_tx,
        forget_tx,
        join,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(size: u64) -> FileStat {
        FileStat {
            size,
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000)),
        }
    }

    #[test]
    fn eligible_extensions() {
        assert!(is_eligible(Path::new("/x/a.fits")));
        assert!(is_eligible(Path::new("/x/a.FIT")));
        assert!(is_eligible(Path::new("/x/a.Fts")));
        assert!(is_eligible(Path::new("/x/a.xisf")));
        assert!(!is_eligible(Path::new("/x/a.txt")));
        assert!(!is_eligible(Path::new("/x/a.fits.tmp")));
        assert!(!is_eligible(Path::new("/x/noext")));
    }

    #[test]
    fn not_stable_until_quiet_window_elapses() {
        let mut t = StabilityTracker::new(Duration::from_secs(10));
        let t0 = Instant::now();
        let p = PathBuf::from("/cap/a.fits");

        t.observe(&p, stat(100), t0);
        assert!(t.collect_stable(t0 + Duration::from_secs(5)).is_empty());

        // A re-fired notify event on the UNCHANGED file must not reset the clock.
        t.observe(&p, stat(100), t0 + Duration::from_secs(6));
        assert_eq!(
            t.collect_stable(t0 + Duration::from_secs(11)),
            vec![p.clone()],
            "should stabilize 10s after first sight, despite the re-observe"
        );
    }

    #[test]
    fn emitted_once_only() {
        let mut t = StabilityTracker::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let p = PathBuf::from("/cap/a.fits");
        t.observe(&p, stat(100), t0);
        assert_eq!(t.collect_stable(t0 + Duration::from_secs(2)), vec![p.clone()]);
        // Later observations of the same path never re-emit it.
        t.observe(&p, stat(200), t0 + Duration::from_secs(3));
        assert!(t.collect_stable(t0 + Duration::from_secs(10)).is_empty());
        assert_eq!(t.pending_len(), 0);
    }

    #[test]
    fn growing_file_resets_the_window() {
        let mut t = StabilityTracker::new(Duration::from_secs(10));
        let t0 = Instant::now();
        let p = PathBuf::from("/cap/a.fits");

        t.observe(&p, stat(100), t0);
        // Still growing at +8s → window resets to +8s.
        t.observe(&p, stat(200), t0 + Duration::from_secs(8));
        assert!(
            t.collect_stable(t0 + Duration::from_secs(9)).is_empty(),
            "only 1s since the last change"
        );
        assert_eq!(
            t.collect_stable(t0 + Duration::from_secs(19)),
            vec![p],
            "11s of quiet since the last change → stable"
        );
    }

    #[test]
    fn forget_drops_a_pending_candidate() {
        let mut t = StabilityTracker::new(Duration::from_secs(5));
        let t0 = Instant::now();
        let p = PathBuf::from("/cap/gone.fits");
        t.observe(&p, stat(10), t0);
        assert_eq!(t.pending_len(), 1);
        t.forget(&p);
        assert_eq!(t.pending_len(), 0);
        assert!(t.collect_stable(t0 + Duration::from_secs(10)).is_empty());
    }

    /// Poll-only mode, forced deterministically (no dependence on how a given
    /// platform's `notify` backend reacts to an odd path): a file created
    /// *after* spawn is still discovered and emitted by the tick sweep alone.
    #[tokio::test]
    async fn poll_only_mode_discovers_new_files() {
        let tmp = tempfile::tempdir().unwrap();
        let capture = tmp.path().join("capture");
        std::fs::create_dir_all(&capture).unwrap();
        let seen = Arc::new(SeenStore::open(tmp.path().join("perseus.db")).unwrap());
        let (tx, mut rx) = mpsc::channel::<(PathBuf, PathBuf)>(8);

        let handle = spawn_watcher_with_options(
            capture.clone(),
            Duration::from_millis(50),
            Duration::from_millis(50),
            tx,
            seen,
            /* force_poll_only = */ true,
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
        std::fs::write(capture.join("l1.fits"), b"data").unwrap();

        let (dir, file) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("poll fallback must find the file")
            .expect("stable-file channel closed");
        assert!(
            file.ends_with("l1.fits"),
            "unexpected stable file: {}",
            file.display()
        );
        assert!(
            file.starts_with(&dir),
            "emitted file must sit under the reported capture dir"
        );
        handle.shutdown().await;
    }

    /// A capture root whose `notify` watch cannot be established (here: the
    /// directory does not exist at spawn — the portable stand-in for the
    /// SMB/NFS case where `watch()` errors) must NOT kill the watcher. The
    /// per-tick `scan_eligible` sweep is a complete discovery path on its own,
    /// so the root degrades to poll-only and still emits files that appear
    /// later.
    #[tokio::test]
    async fn unwatchable_dir_degrades_to_poll_only() {
        let tmp = tempfile::tempdir().unwrap();
        let seen = Arc::new(SeenStore::open(tmp.path().join("perseus.db")).unwrap());
        // Does not exist yet → notify's `watch()` fails for this path.
        let capture = tmp.path().join("capture");
        let (tx, mut rx) = mpsc::channel::<(PathBuf, PathBuf)>(8);

        let handle = spawn_watcher(
            capture.clone(),
            Duration::from_millis(50),
            Duration::from_millis(50),
            tx,
            seen,
        )
        .expect("an unwatchable capture dir must degrade, not fail the spawn");

        // The mount "comes back": the directory and a capture file appear.
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::write(capture.join("l1.fits"), b"data").unwrap();

        let (_dir, file) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("poll fallback must find the file")
            .expect("stable-file channel closed");
        assert!(
            file.ends_with("l1.fits"),
            "unexpected stable file: {}",
            file.display()
        );
        handle.shutdown().await;
    }

    /// An unreachable root is what the offline-share signal is FOR: walkdir
    /// fails on the root entry itself (`depth() == 0`), so the sweep reports
    /// exactly one failure — distinguishable from a legitimately empty dir.
    #[test]
    fn missing_root_counts_as_one_sweep_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let (found, errors) = scan_eligible_reporting(&tmp.path().join("not-mounted"));
        assert!(found.is_empty());
        assert_eq!(errors, 1, "an unreadable root is one root-level failure");

        // …and a real, empty directory reports zero, which is the contrast the
        // whole signal rests on.
        let (found, errors) = scan_eligible_reporting(tmp.path());
        assert!(found.is_empty());
        assert_eq!(errors, 0, "an empty but readable dir is not a failure");
    }

    /// Review finding: a *child* entry that cannot be traversed must NOT be
    /// counted. A healthy macOS volume root carries `.Trashes` /
    /// `.Spotlight-V100` (traversal denied), and counting those pinned the
    /// "mount may be offline" warning on forever for a perfectly fine root.
    /// Only `depth() == 0` — the root itself — is the offline signal.
    #[cfg(unix)]
    #[test]
    fn child_walk_errors_do_not_count_as_offline() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.fits"), b"x").unwrap();
        let denied = root.join(".Trashes");
        std::fs::create_dir(&denied).unwrap();
        std::fs::write(denied.join("hidden.fits"), b"x").unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Running as root ignores the mode bits — skip rather than assert a
        // property the environment cannot produce.
        if std::fs::read_dir(&denied).is_ok() {
            std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let (found, errors) = scan_eligible_reporting(root);
        // Restore before any assertion can unwind, so tempdir cleanup succeeds.
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            errors, 0,
            "an untraversable child must not read as an offline mount"
        );
        assert_eq!(found.len(), 1, "the readable payload is still discovered");
        assert!(found[0].ends_with("a.fits"));
    }

    /// The rate-limit arithmetic, exercised without a clock, a filesystem or a
    /// runtime: the first failing sweep warns, the next 59 stay silent, and
    /// sweep #61 warns again.
    #[test]
    fn fold_sweep_warns_on_the_first_then_every_sixtieth() {
        // Sweep #1.
        let (n, log) = fold_sweep(0, true);
        assert_eq!((n, log), (1, SweepLog::Warn), "the first failure warns");

        // Sweeps #2..#60 are inside the rate-limit window.
        let mut n = n;
        for i in 2..=SCAN_ERROR_WARN_EVERY_TICKS {
            let (next, log) = fold_sweep(n, true);
            assert_eq!(log, SweepLog::Silent, "sweep #{i} must stay quiet");
            n = next;
        }
        assert_eq!(n, SCAN_ERROR_WARN_EVERY_TICKS);

        // Sweep #61 — one full window later — warns again.
        let (n, log) = fold_sweep(n, true);
        assert_eq!(log, SweepLog::Warn, "the window has elapsed; warn again");
        assert_eq!(n, SCAN_ERROR_WARN_EVERY_TICKS + 1);
    }

    /// A healthy sweep after a healthy sweep says nothing at all.
    #[test]
    fn fold_sweep_is_silent_while_healthy() {
        assert_eq!(fold_sweep(0, false), (0, SweepLog::Silent));
    }

    /// Recovery emits ONE line carrying the outage length and resets the counter
    /// to zero — which is precisely what makes the NEXT outage warn immediately
    /// instead of inheriting the previous outage's rate-limit phase.
    #[test]
    fn fold_sweep_recovery_resets_so_the_next_outage_warns_immediately() {
        // Three failing sweeps, then the mount returns.
        let mut n = 0;
        for _ in 0..3 {
            n = fold_sweep(n, true).0;
        }
        assert_eq!(n, 3);
        let (n, log) = fold_sweep(n, false);
        assert_eq!(
            log,
            SweepLog::Recovered(3),
            "recovery reports the outage length"
        );
        assert_eq!(n, 0, "counter resets on recovery");

        // A second outage warns on its very first sweep, not 60 ticks later.
        assert_eq!(fold_sweep(n, true), (1, SweepLog::Warn));
    }

    /// The whole point of the counter, end to end: a root that goes away
    /// mid-run must not kill the watcher — the sweep keeps failing, and when
    /// the mount returns the file that appeared on it is still discovered.
    ///
    /// Poll-only is forced so discovery cannot come from a `notify` event (a
    /// vanished+recreated directory invalidates the original watch anyway); the
    /// assertion is convergent — it fails only if recovery never happens.
    #[tokio::test]
    async fn vanished_root_recovers_when_the_mount_returns() {
        let tmp = tempfile::tempdir().unwrap();
        let capture = tmp.path().join("capture");
        std::fs::create_dir_all(&capture).unwrap();
        let seen = Arc::new(SeenStore::open(tmp.path().join("perseus.db")).unwrap());
        let (tx, mut rx) = mpsc::channel::<(PathBuf, PathBuf)>(8);

        let handle = spawn_watcher_with_options(
            capture.clone(),
            Duration::from_millis(50),
            Duration::from_millis(50),
            tx,
            seen,
            /* force_poll_only = */ true,
        );

        // The share drops: the root disappears under the running watcher, so
        // every sweep now fails on the root entry.
        std::fs::remove_dir_all(&capture).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        // The mount returns, carrying a new capture.
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::write(capture.join("l1.fits"), b"data").unwrap();

        let (_dir, file) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the watcher must recover once the root is back")
            .expect("stable-file channel closed");
        assert!(
            file.ends_with("l1.fits"),
            "unexpected stable file: {}",
            file.display()
        );
        handle.shutdown().await;
    }

    /// The unit half of the forget seam: it clears BOTH the emitted set and any
    /// in-progress observation, and reports how many paths it actually knew.
    #[test]
    fn forget_paths_clears_emitted_and_pending() {
        let mut t = StabilityTracker::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let emitted = PathBuf::from("/cap/emitted.fits");
        let growing = PathBuf::from("/cap/growing.fits");
        let unknown = PathBuf::from("/cap/never-seen.fits");

        t.observe(&emitted, stat(100), t0);
        assert_eq!(
            t.collect_stable(t0 + Duration::from_secs(2)),
            vec![emitted.clone()]
        );
        t.observe(&growing, stat(50), t0 + Duration::from_secs(2));
        assert!(t.contains_emitted(&emitted));
        assert_eq!(t.pending_len(), 1);

        let known = t.forget_paths(&[emitted.clone(), growing.clone(), unknown]);
        assert_eq!(known, 2, "only the two the tracker knew about count");
        assert!(
            !t.contains_emitted(&emitted),
            "the emitted set must no longer block this path"
        );
        assert_eq!(t.pending_len(), 0, "in-progress observation is dropped too");
    }

    /// Why the seam has to exist, pinned as a test rather than a comment: the
    /// emitted set — not the seen store — is what silently swallows a file
    /// re-created at a path this run already sent. The store is consulted only
    /// AFTER `contains_emitted` has let the path through, so the durable
    /// `mark_deleted` fix alone cannot heal this within one process run.
    #[test]
    fn emitted_blocks_a_recreation_until_the_path_is_forgotten() {
        let mut t = StabilityTracker::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let p = PathBuf::from("/cap/a.fits");

        t.observe(&p, stat(100), t0);
        assert_eq!(
            t.collect_stable(t0 + Duration::from_secs(2)),
            vec![p.clone()]
        );

        // Deleted, then re-copied from the camera media: byte-identical, so even
        // a stat-aware check would see the same `(size, mtime)`.
        t.observe(&p, stat(100), t0 + Duration::from_secs(3));
        assert!(
            t.collect_stable(t0 + Duration::from_secs(10)).is_empty(),
            "documents the defect: the emitted set drops it, invisibly"
        );

        // …and with the seam, the very same recreation is emitted again.
        t.forget_paths(&[p.clone()]);
        t.observe(&p, stat(100), t0 + Duration::from_secs(11));
        assert_eq!(t.collect_stable(t0 + Duration::from_secs(13)), vec![p]);
    }

    /// The load-bearing one, through a RUNNING watcher: create → emitted →
    /// delete (the deleter's two steps: `mark_deleted` + forget) → re-create
    /// identical bytes → emitted AGAIN, inside one process run.
    ///
    /// Poll-only so discovery is the deterministic tick sweep rather than a
    /// platform-dependent `notify` event.
    #[tokio::test]
    async fn a_forgotten_path_is_emitted_again_after_recreation() {
        let tmp = tempfile::tempdir().unwrap();
        let capture = tmp.path().join("capture");
        std::fs::create_dir_all(&capture).unwrap();
        let seen = Arc::new(SeenStore::open(tmp.path().join("perseus.db")).unwrap());
        let (tx, mut rx) = mpsc::channel::<(PathBuf, PathBuf)>(8);

        let handle = spawn_watcher_with_options(
            capture.clone(),
            Duration::from_millis(50),
            Duration::from_millis(50),
            tx,
            Arc::clone(&seen),
            /* force_poll_only = */ true,
        );

        std::fs::write(capture.join("l1.fits"), b"0123456789").unwrap();
        let (_dir, first) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the first capture must be emitted")
            .expect("stable-file channel closed");
        assert!(first.ends_with("l1.fits"));

        // Record it exactly as the enqueue path does, so the durable store would
        // dedup a stat-identical recreation if `mark_deleted` were skipped —
        // this is what makes the assertion below about the EMITTED set.
        let meta = std::fs::metadata(&first).unwrap();
        seen.mark_enqueued(
            &first,
            meta.len(),
            mtime_millis(meta.modified().ok()),
            "/pkg/u1",
        )
        .unwrap();

        // The deleter's two steps, in its order: unlink, stamp the seen row,
        // then un-emit the path.
        std::fs::remove_file(&first).unwrap();
        seen.mark_deleted(&first).unwrap();
        let forget = WatcherForget::new(vec![handle.forget_sender()]);
        forget.forget(vec![first.clone()]);

        // Re-copied from the camera media: same bytes at the same path.
        std::fs::write(&first, b"0123456789").unwrap();

        let (_dir, again) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a re-created capture must be emitted again within the same run")
            .expect("stable-file channel closed");
        assert_eq!(again, first, "the same path, enqueued a second time");
        handle.shutdown().await;
    }

    /// An empty aggregate (no watchers: a detached web page, or an agent started
    /// without `watch`) is a silent no-op, not a panic or an error.
    #[test]
    fn an_empty_forget_aggregate_is_a_no_op() {
        let forget = WatcherForget::none();
        assert!(forget.is_empty());
        forget.forget(vec![PathBuf::from("/cap/a.fits")]);
    }

    #[test]
    fn independent_files_track_separately() {
        let mut t = StabilityTracker::new(Duration::from_secs(10));
        let t0 = Instant::now();
        let a = PathBuf::from("/cap/a.fits");
        let b = PathBuf::from("/cap/b.fits");
        t.observe(&a, stat(1), t0);
        t.observe(&b, stat(1), t0 + Duration::from_secs(5));
        // At +11s a is stable (11s) but b is not (6s).
        assert_eq!(t.collect_stable(t0 + Duration::from_secs(11)), vec![a]);
        // At +16s b is stable too.
        assert_eq!(
            t.collect_stable(t0 + Duration::from_secs(16)),
            vec![b]
        );
    }
}
