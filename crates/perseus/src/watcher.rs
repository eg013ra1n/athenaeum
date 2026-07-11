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
//! sent. Each path is emitted at most once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use notify::{Event, RecursiveMode, Watcher as _};
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
fn scan_eligible(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        match entry {
            Ok(e) if e.file_type().is_file() && is_eligible(e.path()) => {
                out.push(e.path().to_path_buf())
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "walk capture dir entry failed"),
        }
    }
    out
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
    join: tokio::task::JoinHandle<()>,
}

impl WatcherHandle {
    /// Ask the watcher to stop and await its exit.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(()).await;
        let _ = self.join.await;
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

/// Start watching `capture_dir` (recursively). Stable capture files are sent on
/// `stable_tx` as `(owning_capture_dir, file_path)`; the consumer builds a
/// package (with a `rel_path` relative to that capture dir) and enqueues it.
/// Returns once the `notify` watcher is armed.
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
    // Canonicalize the watched root so both discovery sources — `notify` events
    // and the directory poll — speak the same path spelling. Without this, macOS
    // reports `notify` paths under `/private/var/...` while a walkdir of a
    // `/var/...` tempdir yields the other spelling of the same file, and the two
    // would be tracked (and enqueued) as two distinct files.
    let capture_dir = std::fs::canonicalize(&capture_dir).unwrap_or(capture_dir);

    // `notify`'s callback runs on its own thread; forward raw event paths into
    // the async task over an unbounded channel (a bounded one could deadlock the
    // fs-event thread).
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<PathBuf>();
    let mut watcher =
        notify::recommended_watcher(move |res: notify::Result<Event>| match res {
            Ok(event) => {
                for path in event.paths {
                    if is_eligible(&path) {
                        let _ = raw_tx.send(path);
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "notify watcher error"),
        })
        .context("create filesystem watcher")?;
    watcher
        .watch(&capture_dir, RecursiveMode::Recursive)
        .with_context(|| format!("watch capture dir {}", capture_dir.display()))?;

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

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
            "capture watcher armed"
        );

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    tracing::info!("capture watcher stopping");
                    break;
                }
                Some(path) = raw_rx.recv() => {
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
                    for path in scan_eligible(&capture_dir) {
                        if !tracker.contains_emitted(&path) {
                            candidates.insert(path);
                        }
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

    Ok(WatcherHandle { shutdown_tx, join })
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
